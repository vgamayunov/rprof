use crate::{IoTimeBin, ReportCounter};
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::{cmp::Ordering, collections::BTreeMap};

pub(crate) const FANOUT: u128 = 8;
pub(crate) fn time(value: u128) -> [u8; 16] {
    value.to_be_bytes()
}
pub(crate) fn integer(bytes: Vec<u8>) -> Result<u128> {
    Ok(u128::from_be_bytes(bytes.try_into().map_err(|_| {
        anyhow::anyhow!("invalid indexed timestamp")
    })?))
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Io {
    pub rb: u128,
    pub wb: u128,
    pub ro: u64,
    pub wo: u64,
    pub other: u64,
    pub errors: u64,
    pub rl: u128,
    pub wl: u128,
    pub ol: u128,
    pub max: u64,
}
impl Io {
    pub fn add(&mut self, other: Self) {
        self.rb += other.rb;
        self.wb += other.wb;
        self.ro += other.ro;
        self.wo += other.wo;
        self.other += other.other;
        self.errors += other.errors;
        self.rl += other.rl;
        self.wl += other.wl;
        self.ol += other.ol;
        self.max = self.max.max(other.max);
    }
    pub fn bin(self, start: u128, duration: u128) -> IoTimeBin {
        IoTimeBin {
            start_ns: start.to_string(),
            duration_ns: duration.to_string(),
            read_bytes: self.rb.to_string(),
            write_bytes: self.wb.to_string(),
            read_ops: self.ro.to_string(),
            write_ops: self.wo.to_string(),
            other_ops: self.other.to_string(),
            errors: self.errors.to_string(),
            read_latency_ns: self.rl.to_string(),
            write_latency_ns: self.wl.to_string(),
            other_latency_ns: self.ol.to_string(),
            max_latency_ns: self.max.to_string(),
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) enum Number {
    Integer(u64),
    Float(f64),
}
impl Number {
    fn compare(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Integer(a), Self::Integer(b)) => a.cmp(b),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(b).unwrap_or(Ordering::Equal),
            _ => Ordering::Equal,
        }
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Point {
    pub id: i64,
    pub at: u128,
    pub value: Number,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Gauge {
    pub first: Point,
    pub last: Point,
    pub min: Point,
    pub max: Point,
    pub count: u64,
}
impl Gauge {
    pub fn new(id: i64, at: u128, counter: &ReportCounter) -> Result<Self> {
        let value = if counter.value_type == "integer" {
            Number::Integer(counter.value.parse()?)
        } else {
            Number::Float(counter.value.parse()?)
        };
        let point = Point { id, at, value };
        Ok(Self {
            first: point.clone(),
            last: point.clone(),
            min: point.clone(),
            max: point,
            count: 1,
        })
    }
    pub fn add(&mut self, other: Self) {
        if (other.first.at, other.first.id) < (self.first.at, self.first.id) {
            self.first = other.first.clone();
        }
        if (other.last.at, other.last.id) > (self.last.at, self.last.id) {
            self.last = other.last.clone();
        }
        if other.min.value.compare(&self.min.value) == Ordering::Less {
            self.min = other.min;
        }
        if other.max.value.compare(&self.max.value) == Ordering::Greater {
            self.max = other.max;
        }
        self.count += other.count;
    }
}

pub(crate) fn configure(db: &Connection) -> Result<()> {
    use rusqlite::functions::FunctionFlags;
    db.create_scalar_function(
        "merge_io",
        2,
        FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_UTF8,
        |ctx| {
            let merge = || -> Result<String> {
                let mut a: Io = serde_json::from_str(&ctx.get::<String>(0)?)?;
                a.add(serde_json::from_str(&ctx.get::<String>(1)?)?);
                Ok(serde_json::to_string(&a)?)
            };
            merge().map_err(|e| rusqlite::Error::UserFunctionError(e.into()))
        },
    )?;
    db.create_scalar_function(
        "merge_gauge",
        2,
        FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_UTF8,
        |ctx| {
            let merge = || -> Result<String> {
                let mut a: Gauge = serde_json::from_str(&ctx.get::<String>(0)?)?;
                a.add(serde_json::from_str(&ctx.get::<String>(1)?)?);
                Ok(serde_json::to_string(&a)?)
            };
            merge().map_err(|e| rusqlite::Error::UserFunctionError(e.into()))
        },
    )?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct Pending {
    io: BTreeMap<(i64, u128), Io>,
    gauges: BTreeMap<(i64, u128), Gauge>,
}
impl Pending {
    pub fn io(&mut self, series: i64, bucket: u128, value: Io) {
        self.io.entry((series, bucket)).or_default().add(value);
    }
    pub fn gauge(&mut self, series: i64, bucket: u128, value: Gauge) {
        self.gauges
            .entry((series, bucket))
            .and_modify(|g| g.add(value.clone()))
            .or_insert(value);
    }
    pub fn maybe_flush(&mut self, db: &Connection) -> Result<()> {
        if self.io.len() + self.gauges.len() >= 16_384 {
            self.flush(db)?;
        }
        Ok(())
    }
    pub fn flush(&mut self, db: &Connection) -> Result<()> {
        let mut io = db.prepare_cached("INSERT INTO bins(series,level,bucket,data) VALUES(?1,0,?2,?3) ON CONFLICT(series,level,bucket) DO UPDATE SET data=merge_io(data,excluded.data)")?;
        for ((series, bucket), value) in std::mem::take(&mut self.io) {
            io.execute(params![
                series,
                time(bucket),
                serde_json::to_string(&value)?
            ])?;
        }
        let mut gauges = db.prepare_cached("INSERT INTO bins(series,level,bucket,data) VALUES(?1,0,?2,?3) ON CONFLICT(series,level,bucket) DO UPDATE SET data=merge_gauge(data,excluded.data)")?;
        for ((series, bucket), value) in std::mem::take(&mut self.gauges) {
            gauges.execute(params![
                series,
                time(bucket),
                serde_json::to_string(&value)?
            ])?;
        }
        Ok(())
    }
}

pub(crate) fn pyramid(db: &Connection) -> Result<()> {
    let mut statement = db.prepare("SELECT id,kind,base,end FROM series ORDER BY id")?;
    let mut series = statement.query([])?;
    while let Some(row) = series.next()? {
        let id: i64 = row.get(0)?;
        let kind: String = row.get(1)?;
        let mut width = integer(row.get(2)?)?;
        let end = integer(row.get(3)?)?;
        let mut level = 0;
        while width < end {
            let mut source = db.prepare(
                "SELECT bucket,data FROM bins WHERE series=?1 AND level=?2 ORDER BY bucket",
            )?;
            let mut rows = source.query(params![id, level])?;
            let mut io: Option<(u128, Io)> = None;
            let mut gauge: Option<(u128, Gauge)> = None;
            let mut insert = db
                .prepare_cached("INSERT INTO bins(series,level,bucket,data) VALUES(?1,?2,?3,?4)")?;
            while let Some(row) = rows.next()? {
                let bucket = integer(row.get(0)?)? / FANOUT;
                let json: String = row.get(1)?;
                if kind == "io" {
                    let value: Io = serde_json::from_str(&json)?;
                    if io.as_ref().is_some_and(|(key, _)| *key != bucket) {
                        let (key, value) = io.take().unwrap();
                        insert.execute(params![
                            id,
                            level + 1,
                            time(key),
                            serde_json::to_string(&value)?
                        ])?;
                    }
                    io.get_or_insert_with(|| (bucket, Io::default()))
                        .1
                        .add(value);
                } else {
                    let value: Gauge = serde_json::from_str(&json)?;
                    if gauge.as_ref().is_some_and(|(key, _)| *key != bucket) {
                        let (key, value) = gauge.take().unwrap();
                        insert.execute(params![
                            id,
                            level + 1,
                            time(key),
                            serde_json::to_string(&value)?
                        ])?;
                    }
                    if let Some((_, current)) = &mut gauge {
                        current.add(value);
                    } else {
                        gauge = Some((bucket, value));
                    }
                }
            }
            if let Some((key, value)) = io {
                insert.execute(params![
                    id,
                    level + 1,
                    time(key),
                    serde_json::to_string(&value)?
                ])?;
            }
            if let Some((key, value)) = gauge {
                insert.execute(params![
                    id,
                    level + 1,
                    time(key),
                    serde_json::to_string(&value)?
                ])?;
            }
            level += 1;
            width = width
                .checked_mul(FANOUT)
                .context("index pyramid time overflow")?;
        }
    }
    Ok(())
}
