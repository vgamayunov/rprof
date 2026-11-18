//! Optional call-site snapshots shared by the native transports. No unwinder,
//! allocation, symbol lookup or unguarded application-memory access occurs here.
use crate::Stats;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const MAGIC: u32 = 0x52505354;
pub const VERSION: u16 = 1;
pub const DEPTH: usize = 64;
pub const IO: u16 = 1;
pub const MPI: u16 = 2;
pub const CUDA: u16 = 3;
static ENABLED: AtomicBool = AtomicBool::new(false);
static NEXT: AtomicU64 = AtomicU64::new(1);

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Snapshot {
    pub magic: u32,
    pub version: u16,
    pub source: u16,
    pub pid: u32,
    pub tid: u32,
    pub instance: u64,
    pub token: u64,
    pub depth: u32,
    pub truncated: u32,
    pub addresses: [u64; DEPTH],
}
pub const SIZE: usize = 552;
const _: () = assert!(std::mem::size_of::<Snapshot>() == SIZE);

pub fn configure() {
    let value = unsafe { libc::getenv(c"RPROF_STACKS".as_ptr()) };
    let enabled =
        !value.is_null() && unsafe { *value == b'1' as libc::c_char && *value.add(1) == 0 };
    ENABLED.store(enabled, Ordering::Relaxed);
}
pub fn next_token() -> u64 {
    if ENABLED.load(Ordering::Relaxed) {
        NEXT.fetch_add(1, Ordering::Relaxed)
    } else {
        0
    }
}

pub fn initialized(stats: &Stats) {
    if ENABLED.load(Ordering::Relaxed) {
        stats.stack_images.fetch_add(1, Ordering::Relaxed);
    }
}

/// `frame` originates from the C adapter's __builtin_frame_address(0).
/// Even an invalid/corrupt frame chain is copied through the kernel, not dereferenced.
pub fn send(socket: i32, stats: &Stats, source: u16, instance: u64, token: u64, mut frame: usize) {
    if !ENABLED.load(Ordering::Relaxed) || token == 0 {
        return;
    }
    let saved_errno = unsafe { *libc::__errno_location() };
    let mut snapshot: Snapshot = unsafe { std::mem::zeroed() };
    snapshot.magic = MAGIC;
    snapshot.version = VERSION;
    snapshot.source = source;
    snapshot.pid = unsafe { libc::getpid() } as u32;
    snapshot.tid = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
    snapshot.instance = instance;
    snapshot.token = token;
    while frame != 0 && snapshot.depth < DEPTH as u32 {
        let mut pair = [0usize; 2];
        let local = libc::iovec {
            iov_base: pair.as_mut_ptr().cast(),
            iov_len: std::mem::size_of_val(&pair),
        };
        let remote = libc::iovec {
            iov_base: frame as *mut libc::c_void,
            iov_len: local.iov_len,
        };
        let copied = unsafe {
            libc::syscall(
                libc::SYS_process_vm_readv,
                snapshot.pid,
                &local,
                1usize,
                &remote,
                1usize,
                0usize,
            )
        };
        if copied != local.iov_len as libc::c_long {
            snapshot.truncated = 1;
            break;
        }
        if pair[1] != 0 {
            snapshot.addresses[snapshot.depth as usize] = pair[1] as u64;
            snapshot.depth += 1;
        }
        if pair[0] == 0 {
            break;
        }
        if pair[0] <= frame || pair[0] - frame > 1024 * 1024 {
            snapshot.truncated = 1;
            break;
        }
        frame = pair[0];
    }
    if snapshot.depth == DEPTH as u32 {
        snapshot.truncated = 1;
    }
    stats.attempted.fetch_add(1, Ordering::Relaxed);
    let sent = unsafe {
        libc::syscall(
            libc::SYS_sendto,
            socket,
            (&snapshot as *const Snapshot).cast::<libc::c_void>(),
            SIZE,
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            std::ptr::null::<libc::sockaddr>(),
            0 as libc::socklen_t,
        )
    };
    if sent == SIZE as libc::c_long {
        stats.sent.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.dropped.fetch_add(1, Ordering::Relaxed);
    }
    unsafe {
        *libc::__errno_location() = saved_errno;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::{fd::AsRawFd, unix::net::UnixDatagram};
    #[test]
    fn invalid_frame_is_reported_without_dereferencing_or_changing_errno() {
        ENABLED.store(true, Ordering::Relaxed);
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        let stats = Stats::default();
        unsafe {
            *libc::__errno_location() = libc::EBUSY;
        }
        send(sender.as_raw_fd(), &stats, IO, 12, 42, 1);
        assert_eq!(unsafe { *libc::__errno_location() }, libc::EBUSY);
        let mut bytes = [0; SIZE];
        assert_eq!(receiver.recv(&mut bytes).unwrap(), SIZE);
        let value = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<Snapshot>()) };
        assert_eq!((value.token, value.depth, value.truncated), (42, 0, 1));
        assert_eq!(stats.sent.load(Ordering::Relaxed), 1);
        ENABLED.store(false, Ordering::Relaxed);
    }
}
