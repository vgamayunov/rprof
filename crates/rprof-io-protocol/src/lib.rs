//! Fixed-size, same-host IPC for the POSIX I/O preload collector. No allocation
//! or serialized strings cross the callback boundary. This is not a network format.

use std::sync::atomic::AtomicU64;

/// A common process-image identity for independently loaded native collectors.
/// Linux supplies fresh AT_RANDOM bytes at exec; including PID distinguishes fork.
/// Only a non-reversible hash is returned, never the loader's random seed bytes.
pub fn process_instance() -> u64 {
    use sha2::{Digest, Sha256};
    let address = unsafe { libc::getauxval(libc::AT_RANDOM) } as *const u8;
    let mut material = [0u8; 20];
    if !address.is_null() {
        for (index, byte) in material[..16].iter_mut().enumerate() {
            *byte = unsafe { *address.add(index) };
        }
    }
    material[16..].copy_from_slice(&(unsafe { libc::getpid() } as u32).to_ne_bytes());
    let digest = Sha256::digest(material);
    let mut id = [0u8; 8];
    id.copy_from_slice(&digest[..8]);
    u64::from_ne_bytes(id).max(1)
}

pub const PROCESS_MAGIC: u32 = 0x5250504e;
pub const PROCESS_VERSION: u16 = 1;

/// Optional metadata shared by the native transports, tied to the sending exec image.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcessEvent {
    pub magic: u32,
    pub version: u16,
    pub name_len: u16,
    pub pid: u32,
    pub error: i32,
    pub instance: u64,
    pub name: [u8; 256],
}
pub const PROCESS_EVENT_SIZE: usize = 280;
const _: () = assert!(std::mem::size_of::<ProcessEvent>() == PROCESS_EVENT_SIZE);

/// Capture the basename inside the process, not via a PID lookup after exit/exec.
/// Uses only bounded stack storage, atomics and syscalls, including after fork.
pub fn send_process_name(socket: i32, stats: &Stats, instance: u64) {
    use std::sync::atomic::Ordering;
    let saved_errno = unsafe { *libc::__errno_location() };
    let mut event: ProcessEvent = unsafe { std::mem::zeroed() };
    event.magic = PROCESS_MAGIC;
    event.version = PROCESS_VERSION;
    event.pid = unsafe { libc::getpid() } as u32;
    event.instance = instance;
    let mut path = [0u8; 4096];
    let length = unsafe {
        libc::syscall(
            libc::SYS_readlinkat,
            libc::AT_FDCWD,
            c"/proc/self/exe".as_ptr(),
            path.as_mut_ptr(),
            path.len(),
        )
    };
    if length < 0 {
        event.error = unsafe { *libc::__errno_location() };
    } else if length as usize >= path.len() {
        event.error = libc::ENAMETOOLONG;
    } else {
        let path = &path[..length as usize];
        let start = path
            .iter()
            .rposition(|byte| *byte == b'/')
            .map_or(0, |index| index + 1);
        let name = &path[start..];
        if name.is_empty() || name.len() > event.name.len() {
            event.error = libc::ENAMETOOLONG;
        } else {
            event.name[..name.len()].copy_from_slice(name);
            event.name_len = name.len() as u16;
        }
    }
    stats.attempted.fetch_add(1, Ordering::Relaxed);
    let sent = unsafe {
        libc::syscall(
            libc::SYS_sendto,
            socket,
            (&event as *const ProcessEvent).cast::<libc::c_void>(),
            PROCESS_EVENT_SIZE,
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            std::ptr::null::<libc::sockaddr>(),
            0 as libc::socklen_t,
        )
    };
    if sent == PROCESS_EVENT_SIZE as libc::c_long {
        stats.sent.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.dropped.fetch_add(1, Ordering::Relaxed);
    }
    unsafe { *libc::__errno_location() = saved_errno };
}

pub const MAGIC: u32 = 0x5250494f;
pub const VERSION: u16 = 2;
pub const STATS_MAGIC: u64 = 0x5250524f46494f31;
pub const PATH_CAPACITY: usize = 256;
pub const PATH_TRUNCATED: u16 = 1;
pub const PATH_UNAVAILABLE: u16 = 2;
pub const PATH_ARGUMENT: u16 = 4;
/// Zero values are not sentinels: only the corresponding validity bit makes a
/// kernel-supplied value usable.
pub const FS_MAGIC_VALID: u32 = 1;
pub const FS_DEVICE_VALID: u32 = 2;
pub const FS_MODE_VALID: u32 = 4;
pub const FS_MOUNT_ID_VALID: u32 = 8;
pub const FS_NAMESPACE_VALID: u32 = 16;

pub mod op {
    pub const OPEN: u16 = 1;
    pub const OPENAT: u16 = 2;
    pub const CLOSE: u16 = 3;
    pub const READ: u16 = 4;
    pub const WRITE: u16 = 5;
    pub const PREAD: u16 = 6;
    pub const PWRITE: u16 = 7;
    pub const FSYNC: u16 = 8;
    pub const FDATASYNC: u16 = 9;
    pub const DUP: u16 = 10;
    pub const DUP2: u16 = 11;
    pub const DUP3: u16 = 12;
    pub const LSEEK: u16 = 13;
    pub const CREAT: u16 = 14;

    pub fn name(value: u16) -> Option<&'static str> {
        Some(match value {
            OPEN => "open",
            OPENAT => "openat",
            CLOSE => "close",
            READ => "read",
            WRITE => "write",
            PREAD => "pread",
            PWRITE => "pwrite",
            FSYNC => "fsync",
            FDATASYNC => "fdatasync",
            DUP => "dup",
            DUP2 => "dup2",
            DUP3 => "dup3",
            LSEEK => "lseek",
            CREAT => "creat",
            _ => return None,
        })
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct IoEvent {
    pub magic: u32,
    pub version: u16,
    pub operation: u16,
    pub pid: u32,
    pub tid: u32,
    pub ppid: u32,
    pub flags: u32,
    pub instance: u64,
    pub resource: u64,
    pub start_ns: u64,
    pub end_ns: u64,
    pub requested: u64,
    pub result: i64,
    pub offset: i64,
    pub fd: i32,
    pub other_fd: i32,
    pub error: i32,
    pub path_len: u16,
    pub path_flags: u16,
    pub path: [u8; PATH_CAPACITY],
    pub fs_magic: u64,
    pub mount_namespace: u64,
    pub dev_major: u32,
    pub dev_minor: u32,
    pub mount_id: u32,
    pub file_mode: u32,
    pub fs_valid: u32,
}

pub const EVENT_SIZE: usize = 392;
const _: () = assert!(std::mem::size_of::<IoEvent>() == EVENT_SIZE);

/// Shared mmap counters survive fork/exec, _exit and fatal signals. The CLI owns
/// this mapping and checks it after workload process-group cleanup.
#[repr(C)]
pub struct Stats {
    pub magic: u64,
    pub initialized: AtomicU64,
    pub attempted: AtomicU64,
    pub sent: AtomicU64,
    pub dropped: AtomicU64,
    /// Optional source-side diagnostics (e.g. CUPTI drops), after the stable prefix.
    pub source_dropped: AtomicU64,
    pub source_errors: AtomicU64,
    pub finalized: AtomicU64,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            magic: STATS_MAGIC,
            initialized: AtomicU64::new(0),
            attempted: AtomicU64::new(0),
            sent: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            source_dropped: AtomicU64::new(0),
            source_errors: AtomicU64::new(0),
            finalized: AtomicU64::new(0),
        }
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;
    use std::{
        os::{
            fd::AsRawFd,
            unix::{ffi::OsStrExt, net::UnixDatagram},
        },
        sync::atomic::Ordering,
    };

    #[test]
    fn executable_metadata_is_captured_in_the_sending_image_without_changing_errno() {
        let (sender, receiver) = UnixDatagram::pair().unwrap();
        receiver.set_nonblocking(true).unwrap();
        let stats = Stats::default();
        let instance = process_instance();
        unsafe { *libc::__errno_location() = libc::ERANGE };
        send_process_name(sender.as_raw_fd(), &stats, instance);
        assert_eq!(unsafe { *libc::__errno_location() }, libc::ERANGE);
        let mut bytes = [0u8; PROCESS_EVENT_SIZE];
        assert_eq!(receiver.recv(&mut bytes).unwrap(), PROCESS_EVENT_SIZE);
        let event = unsafe { std::ptr::read_unaligned(bytes.as_ptr().cast::<ProcessEvent>()) };
        assert_eq!(event.magic, PROCESS_MAGIC);
        assert_eq!(event.pid, std::process::id());
        assert_eq!(event.instance, instance);
        assert_eq!(event.error, 0);
        assert_eq!(
            &event.name[..event.name_len as usize],
            std::env::current_exe()
                .unwrap()
                .file_name()
                .unwrap()
                .as_bytes()
        );
        assert_eq!(stats.attempted.load(Ordering::Relaxed), 1);
        assert_eq!(stats.sent.load(Ordering::Relaxed), 1);
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
    }
}
