//! CUDA adapter transport; CUPTI calls stay in the target-SDK C++ adapter.
use rprof_cuda_protocol::{CudaEvent, EVENT_SIZE, MAGIC, UNKNOWN, VERSION};
use rprof_io_protocol::{STATS_MAGIC, Stats};
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, Ordering};

static SOCKET: AtomicI32 = AtomicI32::new(-1);
static STATS: AtomicPtr<Stats> = AtomicPtr::new(std::ptr::null_mut());
static INSTANCE: AtomicU64 = AtomicU64::new(0);
static NEXT: AtomicU64 = AtomicU64::new(1);

#[unsafe(no_mangle)]
pub extern "C" fn rprof_cuda_now() -> u64 {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return 0;
    }
    (time.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(time.tv_nsec as u64)
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
pub extern "C" fn rprof_cuda_init() -> i32 {
    if SOCKET.load(Ordering::Acquire) >= 0 {
        return 1;
    }
    let Some(fd) = (unsafe { env_fd(b"RPROF_CUDA_FD\0") }) else {
        return 0;
    };
    let Some(stats_fd) = (unsafe { env_fd(b"RPROF_CUDA_STATS_FD\0") }) else {
        return 0;
    };
    let mut kind = 0i32;
    let mut size = std::mem::size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut kind as *mut i32).cast(),
            &mut size,
        )
    } != 0
        || kind != libc::SOCK_DGRAM
    {
        return 0;
    }
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(stats_fd, &mut stat) } != 0
        || stat.st_size < std::mem::size_of::<Stats>() as i64
    {
        return 0;
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
        return 0;
    }
    let stats = address.cast::<Stats>();
    if unsafe { (*stats).magic } != STATS_MAGIC {
        unsafe {
            libc::munmap(address, std::mem::size_of::<Stats>());
        }
        return 0;
    }
    INSTANCE.store(rprof_io_protocol::process_instance(), Ordering::Relaxed);
    STATS.store(stats, Ordering::Release);
    SOCKET.store(fd, Ordering::Release);
    1
}
#[unsafe(no_mangle)]
pub extern "C" fn rprof_cuda_activate() {
    let stats = STATS.load(Ordering::Acquire);
    if !stats.is_null() {
        unsafe {
            (*stats).initialized.fetch_add(1, Ordering::Relaxed);
        }
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn rprof_cuda_source_drop(records: u64) {
    let stats = STATS.load(Ordering::Acquire);
    if !stats.is_null() {
        unsafe {
            (*stats)
                .source_dropped
                .fetch_add(records, Ordering::Relaxed);
        }
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn rprof_cuda_source_error() {
    let stats = STATS.load(Ordering::Acquire);
    if !stats.is_null() {
        unsafe {
            (*stats).source_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn rprof_cuda_finalized() {
    let stats = STATS.load(Ordering::Acquire);
    if !stats.is_null() {
        unsafe {
            (*stats).finalized.fetch_add(1, Ordering::Release);
        }
    }
}

/// # Safety
/// `event` is aligned writable storage and `name` is an adapter-owned C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rprof_cuda_begin(
    event: *mut CudaEvent,
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
    event.activity_id = NEXT.fetch_add(1, Ordering::Relaxed);
    event.start_ns = rprof_cuda_now();
    event.end_ns = event.start_ns;
    event.device = UNKNOWN;
    event.context = UNKNOWN;
    event.stream = UNKNOWN;
    event.src_device = UNKNOWN;
    event.dst_device = UNKNOWN;
    if !name.is_null() {
        for i in 0..event.name.len() - 1 {
            let byte = unsafe { *name.add(i) } as u8;
            if byte == 0 {
                break;
            }
            event.name[i] = byte;
        }
    }
    1
}
/// # Safety
/// `event` contains a fully initialized pointer-free wire observation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rprof_cuda_emit(event: *mut CudaEvent) {
    let stats = STATS.load(Ordering::Acquire);
    if stats.is_null() || event.is_null() {
        return;
    }
    let stats = unsafe { &*stats };
    stats.attempted.fetch_add(1, Ordering::Relaxed);
    let sent = unsafe {
        libc::syscall(
            libc::SYS_sendto,
            SOCKET.load(Ordering::Acquire),
            event.cast::<libc::c_void>(),
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
