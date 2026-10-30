//! Allocation-free transport half of the target-built PMPI adapter.

use rprof_io_protocol::{STATS_MAGIC, Stats};
use rprof_mpi_protocol::{EVENT_SIZE, MAGIC, MpiEvent, VERSION};
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, Ordering};

static SOCKET: AtomicI32 = AtomicI32::new(-1);
static STATS: AtomicPtr<Stats> = AtomicPtr::new(std::ptr::null_mut());
static INSTANCE: AtomicU64 = AtomicU64::new(0);
static NEXT: AtomicU64 = AtomicU64::new(1);

#[unsafe(no_mangle)]
pub extern "C" fn rprof_mpi_now() -> u64 {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return 0;
    }
    (time.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(time.tv_nsec as u64)
}

#[unsafe(no_mangle)]
pub extern "C" fn rprof_mpi_next_id() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

unsafe fn env_fd(name: &'static [u8]) -> Option<i32> {
    let value = unsafe { libc::getenv(name.as_ptr().cast()) };
    if value.is_null() {
        return None;
    }
    let mut number = 0i32;
    for index in 0..11 {
        let byte = unsafe { *value.add(index) } as u8;
        if byte == 0 {
            return (index > 0).then_some(number);
        }
        if !byte.is_ascii_digit() {
            return None;
        }
        number = number.checked_mul(10)?.checked_add((byte - b'0') as i32)?;
    }
    None
}

#[unsafe(no_mangle)]
pub extern "C" fn rprof_mpi_init() {
    let Some(fd) = (unsafe { env_fd(b"RPROF_MPI_FD\0") }) else {
        return;
    };
    let Some(stats_fd) = (unsafe { env_fd(b"RPROF_MPI_STATS_FD\0") }) else {
        return;
    };
    let mut kind = 0i32;
    let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut kind as *mut i32).cast(),
            &mut length,
        )
    } != 0
        || kind != libc::SOCK_DGRAM
    {
        return;
    }
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(stats_fd, &mut stat) } != 0
        || stat.st_size < std::mem::size_of::<Stats>() as i64
    {
        return;
    }
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            std::mem::size_of::<Stats>(),
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            stats_fd,
            0,
        )
    };
    if address == libc::MAP_FAILED {
        return;
    }
    let stats = address.cast::<Stats>();
    if unsafe { (*stats).magic } != STATS_MAGIC {
        unsafe {
            libc::munmap(address, std::mem::size_of::<Stats>());
        }
        return;
    }
    let instance = rprof_io_protocol::process_instance();
    INSTANCE.store(instance, Ordering::Relaxed);
    STATS.store(stats, Ordering::Release);
    unsafe {
        (*stats).initialized.fetch_add(1, Ordering::Relaxed);
    }
    SOCKET.store(fd, Ordering::Release);
}

/// # Safety
/// `event` is writable aligned storage and `name` is a valid NUL-terminated
/// adapter-owned C string. No application buffer is inspected here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rprof_mpi_begin(
    event: *mut MpiEvent,
    kind: u16,
    name: *const libc::c_char,
) -> i32 {
    if SOCKET.load(Ordering::Acquire) < 0 || event.is_null() {
        return 0;
    }
    unsafe {
        std::ptr::write_bytes(event, 0, 1);
    }
    let event = unsafe { &mut *event };
    event.magic = MAGIC;
    event.version = VERSION;
    event.kind = kind;
    event.pid = unsafe { libc::getpid() } as u32;
    event.tid = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
    event.instance = INSTANCE.load(Ordering::Relaxed);
    event.call_id = rprof_mpi_next_id();
    event.rank = -1;
    event.world_size = -1;
    event.comm_rank = -1;
    event.comm_size = -1;
    event.peer = -1;
    event.tag = -1;
    event.count = -1;
    event.thread_level = -1;
    if !name.is_null() {
        for index in 0..event.name.len() - 1 {
            let byte = unsafe { *name.add(index) } as u8;
            if byte == 0 {
                break;
            }
            event.name[index] = byte;
        }
    }
    event.start_ns = rprof_mpi_now();
    1
}

/// # Safety
/// `event` is initialized by the adapter, with its identity originating from a
/// successful begin call. It contains no process pointers or opaque MPI handles.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rprof_mpi_emit(event: *mut MpiEvent) {
    let stats = STATS.load(Ordering::Acquire);
    if stats.is_null() || event.is_null() {
        return;
    }
    let event = unsafe { &mut *event };
    if event.instance == 0 {
        return;
    }
    event.magic = MAGIC;
    event.version = VERSION;
    if event.end_ns == 0 {
        event.end_ns = rprof_mpi_now();
    }
    let stats = unsafe { &*stats };
    stats.attempted.fetch_add(1, Ordering::Relaxed);
    let sent = unsafe {
        libc::syscall(
            libc::SYS_sendto,
            SOCKET.load(Ordering::Acquire),
            (event as *const MpiEvent).cast::<libc::c_void>(),
            EVENT_SIZE,
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            std::ptr::null::<libc::sockaddr>(),
            0 as libc::socklen_t,
        )
    };
    if sent == EVENT_SIZE as libc::c_long {
        stats.sent.fetch_add(1, Ordering::Relaxed);
    } else {
        stats.dropped.fetch_add(1, Ordering::Relaxed);
    }
}
