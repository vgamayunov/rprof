use crate::{
    capture_sink::CaptureSink,
    native_collector::{Mapping, Outcome, pump},
    record::monotonic_ns,
};
use anyhow::Result;
use rprof_core::{
    Category, CounterSample, CounterValue, Span, TrackKind,
    stream::{Attribute, Event, Payload},
};
use rprof_mpi_protocol::*;
use std::{
    collections::{BTreeMap, HashMap},
    os::unix::net::UnixDatagram,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
};

pub(crate) fn receive(
    socket: &UnixDatagram,
    mapping: &Mapping,
    stop: &AtomicBool,
    sink: &Mutex<CaptureSink>,
    origin: u64,
    capacity: usize,
) -> Result<Outcome> {
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let pump = thread::Builder::new()
            .name("rprof-mpi-ingress".into())
            .spawn_scoped(scope, || pump(socket, mapping, stop, sender))?;
        let result = consume(receiver, mapping, sink, origin);
        if result.is_err() {
            stop.store(true, Ordering::Release);
        }
        pump.join()
            .map_err(|_| anyhow::anyhow!("MPI ingress thread panicked"))??;
        result
    })
}

fn string(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())])
        .into_owned()
}

#[derive(Default)]
struct Decode {
    request_tracks: HashMap<u64, u64>,
}

impl Decode {
    fn event(&mut self, raw: MpiEvent, out: &mut CaptureSink, origin: u64, at: u64) -> Result<()> {
        let name = string(&raw.name);
        let rank = u32::try_from(raw.rank).ok();
        let thread = out.native_thread(at, raw.pid, raw.instance, raw.tid, rank)?;
        let mut attrs = BTreeMap::from([
            ("mpi.rank".into(), Attribute::Signed(raw.rank as i64)),
            (
                "mpi.world_size".into(),
                Attribute::Signed(raw.world_size as i64),
            ),
            ("mpi.comm".into(), Attribute::Unsigned(raw.comm_id)),
            (
                "mpi.comm_scope".into(),
                Attribute::Text(
                    if raw.comm_id == 1 {
                        "MPI_COMM_WORLD"
                    } else if raw.comm_id == 2 {
                        "MPI_COMM_SELF"
                    } else if raw.comm_id == 0 {
                        "unknown"
                    } else {
                        "rank-local"
                    }
                    .into(),
                ),
            ),
            (
                "mpi.comm_rank".into(),
                Attribute::Signed(raw.comm_rank as i64),
            ),
            (
                "mpi.comm_size".into(),
                Attribute::Signed(raw.comm_size as i64),
            ),
            ("mpi.peer".into(), Attribute::Signed(raw.peer as i64)),
            ("mpi.tag".into(), Attribute::Signed(raw.tag as i64)),
            ("mpi.count".into(), Attribute::Signed(raw.count)),
            ("mpi.result".into(), Attribute::Signed(raw.result as i64)),
            (
                "mpi.thread_level".into(),
                Attribute::Signed(raw.thread_level as i64),
            ),
            (
                "mpi.tracking_limit".into(),
                Attribute::Bool(raw.flags & TRACKING_LIMIT != 0),
            ),
            (
                "mpi.detail_truncated".into(),
                Attribute::Bool(raw.flags & DETAIL_TRUNCATED != 0),
            ),
            (
                "mpi.request_identity_ambiguous".into(),
                Attribute::Bool(raw.flags & REQUEST_AMBIGUOUS != 0),
            ),
            (
                "mpi.intercommunicator".into(),
                Attribute::Bool(raw.flags & INTERCOMM != 0),
            ),
            ("process_instance".into(), Attribute::Unsigned(raw.instance)),
            ("pid".into(), Attribute::Unsigned(raw.pid as u64)),
            ("tid".into(), Attribute::Unsigned(raw.tid as u64)),
        ]);
        if raw.flags & BYTES_KNOWN != 0 {
            attrs.insert("mpi.bytes".into(), Attribute::Unsigned(raw.bytes));
        }
        let detail = string(&raw.detail);
        if !detail.is_empty() {
            attrs.insert("mpi.detail".into(), Attribute::Text(detail));
        }
        if raw.request_id != 0 {
            attrs.insert("mpi.request".into(), Attribute::Unsigned(raw.request_id));
        }
        if name == "MPI_Sendrecv" && raw.result == 0 {
            attrs.insert("mpi.recv_peer".into(), Attribute::Signed(raw.aux0));
            attrs.insert("mpi.recv_tag".into(), Attribute::Signed(raw.aux1 as i64));
            attrs.insert("mpi.recv_bytes".into(), Attribute::Unsigned(raw.aux2));
            attrs.insert("mpi.recv_count".into(), Attribute::Unsigned(raw.aux3));
        }
        match raw.kind {
            CALL => {
                attrs.insert("mpi.record_type".into(), Attribute::Text("call".into()));
                attrs.insert(
                    "mpi.abort_entry".into(),
                    Attribute::Bool(raw.flags & ABORT_ENTRY != 0),
                );
                let correlation = out.correlation("mpi", raw.instance, raw.call_id)?;
                out.emit(Event {
                    timestamp_ns: at,
                    attributes: attrs,
                    payload: Payload::Span(Span {
                        track_id: thread,
                        name,
                        category: Category::Mpi,
                        start_ns: raw.start_ns - origin,
                        duration_ns: raw.end_ns - raw.start_ns,
                        correlation_id: Some(correlation),
                    }),
                })?;
            }
            REQUEST_BEGIN | REQUEST_END => {
                let track = if let Some(track) = self.request_tracks.get(&thread) {
                    *track
                } else {
                    let track = out.track_ranked(
                        at,
                        thread,
                        "MPI request lifetimes".into(),
                        TrackKind::Thread {
                            tid: raw.tid as u64,
                        },
                        rank,
                    )?;
                    self.request_tracks.insert(thread, track);
                    track
                };
                let request = out.correlation("mpi", raw.instance, raw.request_id)?;
                let call = out.correlation("mpi", raw.instance, raw.call_id)?;
                attrs.insert("mpi.record_type".into(), Attribute::Text("request".into()));
                let state = if raw.kind == REQUEST_BEGIN {
                    "pending"
                } else if raw.flags & (DETACHED | TRACKING_LIMIT) != 0 {
                    "tracking_released"
                } else if raw.flags & CANCELLED != 0 {
                    "cancelled"
                } else if raw.result != 0 {
                    "completed_with_error"
                } else {
                    "completed"
                };
                attrs.insert("mpi.request_state".into(), Attribute::Text(state.into()));
                let payload = if raw.kind == REQUEST_BEGIN {
                    Payload::ActivityStart {
                        track_id: track,
                        name: format!("{name} request"),
                        correlation_id: request,
                        start_ns: Some(raw.start_ns - origin),
                    }
                } else {
                    Payload::Span(Span {
                        track_id: track,
                        name: format!("{name} request lifetime"),
                        category: Category::Mpi,
                        start_ns: raw.start_ns - origin,
                        duration_ns: raw.end_ns - raw.start_ns,
                        correlation_id: Some(request),
                    })
                };
                out.emit(Event {
                    timestamp_ns: at,
                    attributes: attrs,
                    payload,
                })?;
                let (from, to, relation) = if raw.kind == REQUEST_BEGIN {
                    (call, request, "initiates request")
                } else {
                    (
                        request,
                        call,
                        if raw.flags & DETACHED != 0 {
                            "releases request tracking"
                        } else {
                            "observes request completion"
                        },
                    )
                };
                out.emit(Event::new(
                    at,
                    Payload::Dependency {
                        from_correlation_id: from,
                        to_correlation_id: to,
                        relation: relation.into(),
                    },
                ))?;
            }
            CLOCK => {
                attrs.insert("mpi.record_type".into(), Attribute::Text("clock".into()));
                attrs.insert(
                    "mpi.clock.local_ns".into(),
                    Attribute::Unsigned(raw.start_ns),
                );
                attrs.insert("mpi.clock.offset_ns".into(), Attribute::Signed(raw.aux0));
                attrs.insert(
                    "mpi.clock.uncertainty_ns".into(),
                    Attribute::Unsigned(raw.aux1),
                );
                attrs.insert("mpi.clock.group".into(), Attribute::Unsigned(raw.aux2));
                attrs.insert("mpi.clock.stage".into(), Attribute::Unsigned(raw.aux3));
                out.reliable(Event {
                    timestamp_ns: at,
                    attributes: attrs,
                    payload: Payload::Counter(CounterSample {
                        track_id: thread,
                        timestamp_ns: raw.start_ns - origin,
                        name: "mpi.clock.uncertainty_ns".into(),
                        value: CounterValue::Integer(raw.aux1),
                        unit: "ns".into(),
                    }),
                })?;
            }
            COMM | METADATA => {
                attrs.insert(
                    "mpi.record_type".into(),
                    Attribute::Text(
                        if raw.kind == COMM {
                            "communicator"
                        } else {
                            "metadata"
                        }
                        .into(),
                    ),
                );
                out.emit(Event {
                    timestamp_ns: at,
                    attributes: attrs,
                    payload: Payload::Span(Span {
                        track_id: thread,
                        name: format!("{name} [metadata]"),
                        category: Category::Mpi,
                        start_ns: raw.end_ns - origin,
                        duration_ns: 0,
                        correlation_id: None,
                    }),
                })?;
            }
            _ => (),
        }
        Ok(())
    }
}

fn consume(
    receiver: Receiver<MpiEvent>,
    mapping: &Mapping,
    sink: &Mutex<CaptureSink>,
    origin: u64,
) -> Result<Outcome> {
    let mut decoder = Decode::default();
    let mut received = 0;
    for raw in receiver {
        if raw.magic != MAGIC
            || raw.version != VERSION
            || !(CALL..=METADATA).contains(&raw.kind)
            || raw.start_ns < origin
            || raw.end_ns < raw.start_ns
        {
            continue;
        }
        let now = monotonic_ns()?;
        if raw.end_ns > now {
            continue;
        }
        received += 1;
        let mut out = sink
            .lock()
            .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
        out.set_ingress_loss("mpi", mapping.stats().dropped.load(Ordering::Relaxed));
        decoder.event(raw, &mut out, origin, now - origin)?;
    }
    let stats = mapping.stats();
    let attempted = stats.attempted.load(Ordering::Acquire);
    let dropped = attempted.saturating_sub(received);
    sink.lock()
        .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?
        .set_ingress_loss("mpi", dropped);
    Ok(Outcome {
        attempted,
        received,
        dropped,
        initialized: stats.initialized.load(Ordering::Acquire),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rprof_capture::{Recorder, RecorderConfig};
    use rprof_core::{
        Track,
        stream::{Collector, End, EndReason, Header, Loss, Shard},
    };
    use uuid::Uuid;
    fn raw(kind: u16, name: &str, start: u64, end: u64, call: u64, request: u64) -> MpiEvent {
        let mut raw: MpiEvent = unsafe { std::mem::zeroed() };
        raw.magic = MAGIC;
        raw.version = VERSION;
        raw.kind = kind;
        raw.pid = 10;
        raw.tid = 10;
        raw.instance = 100;
        raw.rank = 0;
        raw.world_size = 2;
        raw.start_ns = start;
        raw.end_ns = end;
        raw.call_id = call;
        raw.request_id = request;
        raw.name[..name.len()].copy_from_slice(name.as_bytes());
        raw
    }
    #[test]
    fn request_lifetime_uses_source_start_and_detach_is_not_completion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mpi.jsonl");
        let id = Uuid::new_v4();
        let config = RecorderConfig::default();
        let recorder = Recorder::create(
            &path,
            Header::new(Uuid::new_v4()),
            Shard {
                shard_id: id,
                parent_shard_id: None,
                hostname: "host".into(),
                boot_id: "boot".into(),
                monotonic_origin_ns: 100,
                started_unix_ns: 0,
                command: vec![],
                supervisor_pid: 1,
                collectors: vec![Collector {
                    name: "mpi".into(),
                    version: "test".into(),
                }],
                config: config.metadata(),
            },
            config,
        )
        .unwrap();
        let mut sink = CaptureSink::new(recorder);
        sink.reliable(Event::new(
            0,
            Payload::Track(Track {
                id: 0,
                parent_id: None,
                name: "supervisor".into(),
                rank: Some(0),
                kind: TrackKind::Process { pid: 10 },
            }),
        ))
        .unwrap();
        let mut decode = Decode::default();
        decode
            .event(
                raw(REQUEST_BEGIN, "MPI_Irecv", 110, 120, 1, 2),
                &mut sink,
                100,
                1000,
            )
            .unwrap();
        decode
            .event(raw(CALL, "MPI_Irecv", 110, 120, 1, 2), &mut sink, 100, 1000)
            .unwrap();
        decode
            .event(raw(CALL, "MPI_Wait", 200, 220, 3, 0), &mut sink, 100, 1000)
            .unwrap();
        let mut completed = raw(REQUEST_END, "MPI_Irecv", 110, 220, 3, 2);
        completed.flags = BYTES_KNOWN;
        completed.bytes = 4;
        decode.event(completed, &mut sink, 100, 1000).unwrap();
        decode
            .event(
                raw(REQUEST_BEGIN, "MPI_Isend", 300, 310, 4, 5),
                &mut sink,
                100,
                1000,
            )
            .unwrap();
        let mut detached = raw(REQUEST_END, "MPI_Isend", 300, 350, 6, 5);
        detached.flags = DETACHED;
        decode.event(detached, &mut sink, 100, 1000).unwrap();
        sink.set_ingress_loss("io", 3);
        sink.set_ingress_loss("mpi", 5);
        sink.recorder
            .finish(End {
                shard_id: id,
                duration_ns: 1000,
                exit_code: Some(0),
                signal: None,
                interrupted_by: None,
                reason: EndReason::Completed,
                loss: Loss::default(),
            })
            .unwrap();
        let report = rprof_report::analyze(&path, Default::default()).unwrap();
        let lifetime = report
            .spans
            .iter()
            .find(|s| s.name == "MPI_Irecv request lifetime")
            .unwrap();
        assert_eq!(lifetime.start_ns, "10");
        assert_eq!(lifetime.duration_ns, "110");
        assert_eq!(lifetime.attributes["mpi.bytes"], "4");
        assert!(!lifetime.incomplete);
        assert!(
            report
                .spans
                .iter()
                .find(|s| s.name == "MPI_Isend request lifetime")
                .unwrap()
                .incomplete
        );
        assert_eq!(report.mpi.len(), 2);
        assert_eq!(report.summary.dropped, "8");
        assert_eq!(report.dependencies.len(), 4);
    }
}
