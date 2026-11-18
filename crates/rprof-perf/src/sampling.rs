//! User-space, frame-pointer cpu-clock sampling over the initial CPU affinity.
//! Per-CPU rings allow inheritance; a cpu=-1 inherited event cannot be mmapped.
use super::{ChildSetup, Counter, EventSpec, MAX_HANDOFF, Prepared};
use std::{
    io,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::net::UnixDatagram,
    },
    ptr::NonNull,
    sync::{
        Arc,
        atomic::{Ordering, fence},
    },
    time::Duration,
};

const PAGE_HEADER: usize = 1024;
const DATA_PAGES: usize = 8;

#[derive(Clone, Debug)]
pub(crate) struct Settings {
    pub period_ns: u64,
    pub cpus: Vec<i32>,
}

#[derive(Debug, PartialEq)]
pub enum Record {
    Sample {
        pid: u32,
        tid: u32,
        time: u64,
        period: u64,
        ips: Vec<u64>,
    },
    Mapping {
        pid: u32,
        address: u64,
        length: u64,
        offset: u64,
        path: String,
        device: u64,
        inode: u64,
        time: u64,
    },
    Comm {
        pid: u32,
        name: String,
        exec: bool,
        time: u64,
    },
    Fork {
        pid: u32,
        parent: u32,
        tid: u32,
        parent_tid: u32,
        time: u64,
    },
    Exit {
        pid: u32,
        tid: u32,
        time: u64,
    },
    Lost {
        count: u64,
        time: u64,
    },
    Throttled {
        time: u64,
    },
}
impl Record {
    pub fn time(&self) -> u64 {
        match self {
            Self::Sample { time, .. }
            | Self::Mapping { time, .. }
            | Self::Comm { time, .. }
            | Self::Fork { time, .. }
            | Self::Exit { time, .. }
            | Self::Lost { time, .. }
            | Self::Throttled { time } => *time,
        }
    }
}

impl Prepared {
    pub fn stacks(hz: u32) -> io::Result<Self> {
        if !cfg!(target_arch = "x86_64") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "stack sampling currently supports Linux x86-64",
            ));
        }
        if !(1..=1000).contains(&hz) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "stack frequency must be 1..=1000 Hz",
            ));
        }
        let mut mask: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::sched_getaffinity(0, std::mem::size_of_val(&mask), &mut mask) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let cpus: Vec<_> = (0..libc::CPU_SETSIZE as usize)
            .filter(|&cpu| unsafe { libc::CPU_ISSET(cpu, &mask) })
            .map(|cpu| cpu as i32)
            .collect();
        if cpus.is_empty() || cpus.len() > MAX_HANDOFF {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "stack sampling requires an initial affinity of 1..=240 CPUs; bind this rank to fewer CPUs",
            ));
        }
        let (receiver, sender) = UnixDatagram::pair()?;
        receiver.set_read_timeout(Some(Duration::from_secs(5)))?;
        sender.set_read_timeout(Some(Duration::from_secs(5)))?;
        Ok(Self {
            receiver,
            child: ChildSetup {
                socket: Arc::new(sender),
                specs: cpus
                    .iter()
                    .map(|_| EventSpec {
                        name: "cpu-clock stacks".into(),
                        kind: 1,
                        config: 0,
                    })
                    .collect(),
                inherit: true,
                kernel: false,
                sampling: Some(Settings {
                    cpus,
                    period_ns: 1_000_000_000 / hz as u64,
                }),
            },
        })
    }

    pub fn receive_stacks(self) -> io::Result<Sampler> {
        let counters = self.receive_descriptors()?;
        let mut rings = Vec::with_capacity(counters.len());
        for Counter { fd, .. } in counters {
            rings.push(Ring::new(fd.map_err(io::Error::from_raw_os_error)?)?);
        }
        self.receiver.send(&[1])?;
        Ok(Sampler {
            rings,
            pending: Vec::new(),
        })
    }
}

pub struct Sampler {
    rings: Vec<Ring>,
    pending: Vec<Record>,
}
impl Sampler {
    pub fn read(&mut self, mut observe: impl FnMut(Record) -> io::Result<()>) -> io::Result<()> {
        let mut now: libc::timespec = unsafe { std::mem::zeroed() };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let cutoff = now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64;
        for ring in &mut self.rings {
            ring.read(&mut |record| {
                if self.pending.len() >= 65_536 {
                    return Err(io::Error::new(
                        io::ErrorKind::OutOfMemory,
                        "perf sampling batch limit exceeded",
                    ));
                }
                self.pending.push(record);
                Ok(())
            })?;
        }
        self.pending.sort_by_key(Record::time);
        let end = self
            .pending
            .partition_point(|record| record.time() <= cutoff);
        for record in self.pending.drain(..end) {
            observe(record)?;
        }
        Ok(())
    }
}

struct Ring {
    _fd: OwnedFd,
    base: NonNull<u8>,
    length: usize,
    offset: usize,
    size: usize,
    tail: u64,
}
impl Ring {
    fn new(fd: OwnedFd) -> io::Result<Self> {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page < 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported perf page size",
            ));
        }
        let length = (DATA_PAGES + 1) * page as usize;
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let base = NonNull::new(address.cast::<u8>()).unwrap();
        let offset =
            unsafe { std::ptr::read_volatile(base.as_ptr().add(PAGE_HEADER + 16).cast::<u64>()) }
                as usize;
        let size =
            unsafe { std::ptr::read_volatile(base.as_ptr().add(PAGE_HEADER + 24).cast::<u64>()) }
                as usize;
        if offset < page as usize
            || size == 0
            || !size.is_power_of_two()
            || offset.checked_add(size).is_none_or(|end| end > length)
        {
            unsafe {
                libc::munmap(base.as_ptr().cast(), length);
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid perf ring layout",
            ));
        }
        Ok(Self {
            _fd: fd,
            base,
            length,
            offset,
            size,
            tail: 0,
        })
    }
    fn copy(&self, at: u64, output: &mut [u8]) {
        let index = (at % self.size as u64) as usize;
        let first = output.len().min(self.size - index);
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.base.as_ptr().add(self.offset + index),
                output.as_mut_ptr(),
                first,
            );
            if first < output.len() {
                std::ptr::copy_nonoverlapping(
                    self.base.as_ptr().add(self.offset),
                    output.as_mut_ptr().add(first),
                    output.len() - first,
                );
            }
        }
    }
    fn read(&mut self, observe: &mut impl FnMut(Record) -> io::Result<()>) -> io::Result<()> {
        let head =
            unsafe { std::ptr::read_volatile(self.base.as_ptr().add(PAGE_HEADER).cast::<u64>()) };
        fence(Ordering::Acquire);
        if head.wrapping_sub(self.tail) > self.size as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "perf ring overrun; sample coverage is unknown",
            ));
        }
        while self.tail != head {
            let mut header = [0u8; 8];
            if head.wrapping_sub(self.tail) < 8 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "incomplete perf record",
                ));
            }
            self.copy(self.tail, &mut header);
            let size = u16::from_ne_bytes(header[6..8].try_into().unwrap()) as usize;
            if size < 8 || size > self.size || size as u64 > head.wrapping_sub(self.tail) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid perf record size",
                ));
            }
            let mut bytes = vec![0; size];
            self.copy(self.tail, &mut bytes);
            let parsed = parse(&bytes).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "perf record type {} ({size} bytes): {error}",
                        u32::from_ne_bytes(header[..4].try_into().unwrap())
                    ),
                )
            })?;
            if let Some(record) = parsed {
                observe(record)?;
            }
            self.tail = self.tail.wrapping_add(size as u64);
        }
        fence(Ordering::Release);
        unsafe {
            std::ptr::write_volatile(
                self.base.as_ptr().add(PAGE_HEADER + 8).cast::<u64>(),
                self.tail,
            );
        }
        Ok(())
    }
}
impl Drop for Ring {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.base.as_ptr().cast(), self.length);
        }
    }
}

fn parse(bytes: &[u8]) -> io::Result<Option<Record>> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "malformed perf stack record");
    let u32_at = |at: usize| -> io::Result<u32> {
        Ok(u32::from_ne_bytes(
            bytes
                .get(at..at + 4)
                .ok_or_else(invalid)?
                .try_into()
                .unwrap(),
        ))
    };
    let u64_at = |at: usize| -> io::Result<u64> {
        Ok(u64::from_ne_bytes(
            bytes
                .get(at..at + 8)
                .ok_or_else(invalid)?
                .try_into()
                .unwrap(),
        ))
    };
    let text = |at: usize| -> io::Result<String> {
        let data = bytes.get(at..).ok_or_else(invalid)?;
        let end = data.iter().position(|&v| v == 0).ok_or_else(invalid)?;
        Ok(String::from_utf8_lossy(&data[..end]).into_owned())
    };
    Ok(match u32_at(0)? {
        9 => {
            let ip = u64_at(8)?;
            let depth = usize::try_from(u64_at(40)?).map_err(|_| invalid())?;
            if depth > 4096 {
                return Err(invalid());
            }
            let mut ips = Vec::new();
            for at in 0..depth {
                let pc = u64_at(48 + at * 8)?;
                if pc > 0 && pc < u64::MAX - 4095 {
                    ips.push(pc);
                }
            }
            if ips.first() != Some(&ip) && ip != 0 {
                ips.insert(0, ip);
            }
            ips.truncate(128);
            Some(Record::Sample {
                pid: u32_at(16)?,
                tid: u32_at(20)?,
                time: u64_at(24)?,
                period: u64_at(32)?,
                ips,
            })
        }
        kind @ (1 | 10) => Some(Record::Mapping {
            pid: u32_at(8)?,
            address: u64_at(16)?,
            length: u64_at(24)?,
            offset: u64_at(32)?,
            path: text(if kind == 10 { 72 } else { 40 })?,
            device: if kind == 10 {
                libc::makedev(u32_at(40)?, u32_at(44)?)
            } else {
                0
            },
            inode: if kind == 10 { u64_at(48)? } else { 0 },
            time: u64_at(bytes.len().checked_sub(8).ok_or_else(invalid)?)?,
        }),
        3 => Some(Record::Comm {
            pid: u32_at(8)?,
            name: text(16)?,
            exec: u16::from_ne_bytes(bytes.get(4..6).ok_or_else(invalid)?.try_into().unwrap())
                & (1 << 13)
                != 0,
            time: u64_at(bytes.len().checked_sub(8).ok_or_else(invalid)?)?,
        }),
        7 => Some(Record::Fork {
            pid: u32_at(8)?,
            parent: u32_at(12)?,
            tid: u32_at(16)?,
            parent_tid: u32_at(20)?,
            time: u64_at(24)?,
        }),
        4 => Some(Record::Exit {
            pid: u32_at(8)?,
            tid: u32_at(16)?,
            time: u64_at(24)?,
        }),
        2 => Some(Record::Lost {
            count: u64_at(16)?,
            time: u64_at(bytes.len().checked_sub(8).ok_or_else(invalid)?)?,
        }),
        5 => Some(Record::Throttled { time: u64_at(8)? }),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sample_order_context_markers_and_truncated_records() {
        let mut bytes = vec![9, 0, 0, 0, 0, 0, 72, 0];
        bytes.extend(0x1234u64.to_ne_bytes());
        bytes.extend(12u32.to_ne_bytes());
        bytes.extend(13u32.to_ne_bytes());
        bytes.extend(900u64.to_ne_bytes());
        bytes.extend(10_000_000u64.to_ne_bytes());
        bytes.extend(3u64.to_ne_bytes());
        bytes.extend((-512i64).to_ne_bytes());
        bytes.extend(0x1234u64.to_ne_bytes());
        bytes.extend(0x5678u64.to_ne_bytes());
        assert_eq!(
            parse(&bytes).unwrap(),
            Some(Record::Sample {
                pid: 12,
                tid: 13,
                time: 900,
                period: 10_000_000,
                ips: vec![0x1234, 0x5678]
            })
        );
        for end in 0..bytes.len() {
            assert!(parse(&bytes[..end]).is_err());
        }
    }
}
