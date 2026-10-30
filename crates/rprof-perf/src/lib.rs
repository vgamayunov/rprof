//! Linux counting-only perf events. Installed in the workload before exec, enabled
//! by exec, and passed to the supervisor using SCM_RIGHTS. No attach/start race,
//! preload dependency, sampling interrupts, or allocation in the post-fork hook.
use std::{
    io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::UnixDatagram,
    },
    sync::Arc,
};

pub const MAX_EVENTS: usize = 32;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventSpec {
    pub name: String,
    pub kind: u32,
    pub config: u64,
}
impl EventSpec {
    pub fn parse(name: &str) -> Result<Self, String> {
        let name = match name {
            "cpu-cycles" => "cycles",
            "branch-instructions" => "branches",
            other => other,
        };
        let (kind, config) = match name {
            "cycles" => (0, 0),
            "instructions" => (0, 1),
            "cache-references" => (0, 2),
            "cache-misses" => (0, 3),
            "branches" => (0, 4),
            "branch-misses" => (0, 5),
            "bus-cycles" => (0, 6),
            "ref-cycles" => (0, 9),
            "cpu-clock" => (1, 0),
            "task-clock" => (1, 1),
            "page-faults" => (1, 2),
            "context-switches" => (1, 3),
            "cpu-migrations" => (1, 4),
            "minor-faults" => (1, 5),
            "major-faults" => (1, 6),
            _ => {
                let raw = name.strip_prefix('r').filter(|v| {
                    !v.is_empty() && v.len() <= 16 && v.bytes().all(|b| b.is_ascii_hexdigit())
                });
                let Some(raw) = raw else {
                    return Err(format!(
                        "unsupported PMU event {name:?}; use a documented generic event or rHEX raw encoding"
                    ));
                };
                (4, u64::from_str_radix(raw, 16).map_err(|e| e.to_string())?)
            }
        };
        Ok(Self {
            name: if kind == 4 {
                format!("r{config:x}")
            } else {
                name.into()
            },
            kind,
            config,
        })
    }
    pub fn unit(&self) -> &'static str {
        if self.kind == 1 && self.config <= 1 {
            "ns"
        } else {
            "count"
        }
    }
}

// PERF_ATTR_SIZE_VER0: stable 64-byte UAPI prefix. No bitfields cross the FFI.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Attr {
    kind: u32,
    size: u32,
    config: u64,
    period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup: u32,
    bp_type: u32,
    config1: u64,
}
const _: () = assert!(std::mem::size_of::<Attr>() == 64);

#[derive(Clone, Debug)]
pub struct ChildSetup {
    socket: Arc<UnixDatagram>,
    specs: Vec<EventSpec>,
    inherit: bool,
    kernel: bool,
}
pub struct Prepared {
    receiver: UnixDatagram,
    pub child: ChildSetup,
}
pub struct Counter {
    pub spec: EventSpec,
    pub fd: Result<OwnedFd, i32>,
}

// Native control buffer aligned for cmsghdr and large enough for MAX_EVENTS fds.
#[repr(C)]
struct Control {
    _alignment: [usize; 0],
    bytes: [u8; 256],
}
impl Prepared {
    pub fn new(specs: Vec<EventSpec>, inherit: bool, kernel: bool) -> io::Result<Self> {
        if specs.is_empty() || specs.len() > MAX_EVENTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "PMU requires 1..=32 events",
            ));
        }
        let (receiver, sender) = UnixDatagram::pair()?;
        receiver.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
        Ok(Self {
            receiver,
            child: ChildSetup {
                socket: Arc::new(sender),
                specs,
                inherit,
                kernel,
            },
        })
    }
    /// Called only after successful spawn: the child already sent exactly one message.
    pub fn receive(self) -> io::Result<Vec<Counter>> {
        let mut status = [0i32; MAX_EVENTS];
        let mut control = Control {
            _alignment: [],
            bytes: [0; 256],
        };
        let mut iov = libc::iovec {
            iov_base: status.as_mut_ptr().cast(),
            iov_len: self.child.specs.len() * 4,
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.bytes.as_mut_ptr().cast();
        msg.msg_controllen = control.bytes.len();
        let got = loop {
            let got = unsafe {
                libc::recvmsg(self.receiver.as_raw_fd(), &mut msg, libc::MSG_CMSG_CLOEXEC)
            };
            if got >= 0 {
                break got;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        };
        let mut fds = Vec::new();
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&msg);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET && (*cmsg).cmsg_type == libc::SCM_RIGHTS {
                    let count = ((*cmsg).cmsg_len - libc::CMSG_LEN(0) as usize) / 4;
                    for index in 0..count {
                        let fd = std::ptr::read_unaligned(
                            libc::CMSG_DATA(cmsg).cast::<i32>().add(index),
                        );
                        fds.push(OwnedFd::from_raw_fd(fd));
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&msg, cmsg);
            }
        }
        let count = self.child.specs.len();
        if got as usize != count * 4
            || msg.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
            || fds.len() != status[..count].iter().filter(|v| **v == 0).count()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid perf descriptor handoff",
            ));
        }
        let mut fds = fds.into_iter();
        Ok(self
            .child
            .specs
            .into_iter()
            .zip(status)
            .map(|(spec, error)| Counter {
                spec,
                fd: if error == 0 {
                    Ok(fds.next().unwrap())
                } else {
                    Err(error)
                },
            })
            .collect())
    }
}

impl ChildSetup {
    /// Post-fork/pre-exec: stack storage and Linux syscalls only. Individual open
    /// failures are sent as errno values; the application's exec still proceeds.
    pub fn install(&self) -> io::Result<()> {
        let mut status = [0i32; MAX_EVENTS];
        let mut fds = [-1i32; MAX_EVENTS];
        let mut count = 0;
        for (index, spec) in self.specs.iter().enumerate() {
            let attr = Attr {
                kind: spec.kind,
                size: 64,
                config: spec.config,
                read_format: 3,
                flags: 1
                    | ((self.inherit as u64) << 1)
                    | ((!self.kernel as u64) << 5)
                    | (1 << 6)
                    | (1 << 12),
                ..Default::default()
            };
            let fd = unsafe {
                libc::syscall(libc::SYS_perf_event_open, &attr, 0i32, -1i32, -1i32, 8u64)
            } as i32;
            if fd < 0 {
                status[index] = io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO);
            } else {
                fds[count] = fd;
                count += 1;
            }
        }
        let mut control = Control {
            _alignment: [],
            bytes: [0; 256],
        };
        let mut iov = libc::iovec {
            iov_base: status.as_mut_ptr().cast(),
            iov_len: self.specs.len() * 4,
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if count > 0 {
            unsafe {
                msg.msg_control = control.bytes.as_mut_ptr().cast();
                msg.msg_controllen = libc::CMSG_SPACE((count * 4) as u32) as usize;
                let header = libc::CMSG_FIRSTHDR(&msg);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN((count * 4) as u32) as usize;
                std::ptr::copy_nonoverlapping(
                    fds.as_ptr().cast::<u8>(),
                    libc::CMSG_DATA(header),
                    count * 4,
                );
            }
        }
        let result = loop {
            let sent = unsafe { libc::sendmsg(self.socket.as_raw_fd(), &msg, libc::MSG_NOSIGNAL) };
            if sent == iov.iov_len as isize {
                break Ok(());
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                break Err(error);
            }
        };
        for fd in &fds[..count] {
            unsafe {
                libc::close(*fd);
            }
        }
        result
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    pub raw: u64,
    pub enabled_ns: u64,
    pub running_ns: u64,
}
impl Reading {
    /// Integer estimate rounded to nearest; absent when never scheduled or invalid.
    pub fn scaled(self) -> Option<u128> {
        if self.running_ns == 0 || self.running_ns > self.enabled_ns {
            return None;
        }
        Some(
            (self.raw as u128 * self.enabled_ns as u128 + self.running_ns as u128 / 2)
                / self.running_ns as u128,
        )
    }
}
impl Counter {
    pub fn read(&self) -> io::Result<Reading> {
        let fd = self
            .fd
            .as_ref()
            .map_err(|e| io::Error::from_raw_os_error(*e))?;
        let mut values = [0u64; 3];
        loop {
            let n = unsafe { libc::read(fd.as_raw_fd(), values.as_mut_ptr().cast(), 24) };
            if n == 24 {
                return Ok(Reading {
                    raw: values[0],
                    enabled_ns: values[1],
                    running_ns: values[2],
                });
            }
            if n < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "perf counter returned a short read (possibly unschedulable)",
            ));
        }
    }
    pub fn disable(&self) -> io::Result<()> {
        let fd = self
            .fd
            .as_ref()
            .map_err(|e| io::Error::from_raw_os_error(*e))?;
        // PERF_EVENT_IOC_DISABLE = _IO('$', 1); propagates to inherited events.
        if unsafe { libc::ioctl(fd.as_raw_fd(), 0x2401, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
pub fn error_status(errno: i32) -> &'static str {
    match errno {
        libc::EACCES | libc::EPERM => "permission_denied",
        libc::ENOENT | libc::ENODEV | libc::EOPNOTSUPP | libc::ENOSYS => "unsupported",
        libc::EINVAL => "unsupported_or_invalid",
        libc::EBUSY | libc::EMFILE | libc::ENFILE | libc::ENOMEM | libc::ENOSPC => {
            "resource_unavailable"
        }
        _ => "open_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scaling_is_exact_above_js_precision_and_handles_unscheduled_events() {
        let r = Reading {
            raw: 9_007_199_254_740_993,
            enabled_ns: 10,
            running_ns: 3,
        };
        assert_eq!(r.scaled(), Some(30_023_997_515_803_310));
        assert_eq!(
            Reading {
                raw: 0,
                enabled_ns: 10,
                running_ns: 0
            }
            .scaled(),
            None
        );
        assert_eq!(
            Reading {
                raw: 1,
                enabled_ns: 1,
                running_ns: 2
            }
            .scaled(),
            None
        );
        assert_eq!(
            Reading {
                raw: u64::MAX,
                enabled_ns: u64::MAX,
                running_ns: 1
            }
            .scaled(),
            Some(u64::MAX as u128 * u64::MAX as u128)
        );
    }
    #[test]
    fn event_names_are_bounded_and_raw_encodings_preserve_identity() {
        assert_eq!(EventSpec::parse("cpu-cycles"), EventSpec::parse("cycles"));
        assert_eq!(
            EventSpec::parse("branch-instructions"),
            EventSpec::parse("branches")
        );
        assert_eq!(
            EventSpec::parse("r00C0").unwrap(),
            EventSpec {
                name: "rc0".into(),
                kind: 4,
                config: 192
            }
        );
        for bad in [
            "cycles:k",
            "r",
            "rxyz",
            "r12345678901234567",
            "cpu/event=1/",
        ] {
            assert!(EventSpec::parse(bad).is_err());
        }
        assert_eq!(EventSpec::parse("task-clock").unwrap().unit(), "ns");
    }
}
