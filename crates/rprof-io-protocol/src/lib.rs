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

pub const MAGIC: u32 = 0x5250494f;
pub const VERSION: u16 = 1;
pub const STATS_MAGIC: u64 = 0x5250524f46494f31;
pub const PATH_CAPACITY: usize = 256;
pub const PATH_TRUNCATED: u16 = 1;
pub const PATH_UNAVAILABLE: u16 = 2;
pub const PATH_ARGUMENT: u16 = 4;

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
}

pub const EVENT_SIZE: usize = 352;
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
