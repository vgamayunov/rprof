//! Allocation-free OMPT callback transport. OMPT registration and ABI adaptation
//! live in C; interval reconstruction and reporting live outside the workload.
use rprof_io_protocol::{STATS_MAGIC, Stats};
use rprof_ompt_protocol::{EVENT_SIZE, MAGIC, OmpEvent, VERSION};
use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU64, Ordering};

static SOCKET: AtomicI32 = AtomicI32::new(-1);
static STATS: AtomicPtr<Stats> = AtomicPtr::new(std::ptr::null_mut());
static NEXT: AtomicU64 = AtomicU64::new(1);
static INSTANCE: AtomicU64 = AtomicU64::new(0);
static PID: AtomicI32 = AtomicI32::new(0);

/// Verify the exact target library selected when the GNU adapter was built.
/// # Safety
/// Both arguments are valid NUL-terminated strings supplied by the adapter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rprof_omp_runtime_matches(
    path: *const libc::c_char,
    expected: *const libc::c_char,
) -> i32 {
    use sha2::{Digest, Sha256};
    use std::{ffi::CStr, io::Read, os::unix::ffi::OsStrExt};
    let saved = unsafe { *libc::__errno_location() };
    let result = (|| -> Option<i32> {
        if path.is_null() || expected.is_null() {
            return None;
        }
        let path = unsafe { CStr::from_ptr(path) }.to_bytes();
        let expected = unsafe { CStr::from_ptr(expected) }.to_str().ok()?;
        if path.len() > 4096 || expected.len() != 64 {
            return None;
        }
        let mut file = std::fs::File::open(std::ffi::OsStr::from_bytes(path)).ok()?;
        if file.metadata().ok()?.len() > 128 * 1024 * 1024 {
            return None;
        }
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 16384];
        let mut total = 0usize;
        loop {
            let n = file.read(&mut buffer).ok()?;
            if n == 0 {
                break;
            }
            total += n;
            if total > 128 * 1024 * 1024 {
                return None;
            }
            hash.update(&buffer[..n]);
        }
        Some(i32::from(format!("{:x}", hash.finalize()) == expected))
    })()
    .unwrap_or(0);
    unsafe {
        *libc::__errno_location() = saved;
    }
    result
}

fn now() -> u64 {
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
pub extern "C" fn rprof_omp_init() -> i32 {
    if SOCKET.load(Ordering::Acquire) >= 0 {
        return 1;
    }
    let Some(fd) = (unsafe { env_fd(b"RPROF_OMP_FD\0") }) else {
        return 0;
    };
    let Some(stats_fd) = (unsafe { env_fd(b"RPROF_OMP_STATS_FD\0") }) else {
        return 0;
    };
    let mut kind = 0i32;
    let mut len = std::mem::size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut kind as *mut i32).cast(),
            &mut len,
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
    PID.store(unsafe { libc::getpid() }, Ordering::Relaxed);
    STATS.store(stats, Ordering::Release);
    SOCKET.store(fd, Ordering::Release);
    1
}
#[unsafe(no_mangle)]
pub extern "C" fn rprof_omp_activate() {
    let stats = STATS.load(Ordering::Acquire);
    if !stats.is_null() {
        unsafe {
            (*stats).initialized.fetch_add(1, Ordering::Relaxed);
        }
    }
}
#[unsafe(no_mangle)]
pub extern "C" fn rprof_omp_next_id() -> u64 {
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// # Safety
/// `event` is aligned writable storage and `name` is an adapter-owned C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rprof_omp_begin(
    event: *mut OmpEvent,
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
    let pid = unsafe { libc::getpid() };
    if PID.load(Ordering::Relaxed) != pid {
        INSTANCE.store(rprof_io_protocol::process_instance(), Ordering::Relaxed);
        PID.store(pid, Ordering::Relaxed);
    }
    event.magic = MAGIC;
    event.version = VERSION;
    event.kind = kind;
    event.pid = pid as u32;
    event.tid = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
    event.instance = INSTANCE.load(Ordering::Relaxed);
    event.timestamp_ns = now();
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
/// `event` contains a fully initialized, pointer-free wire observation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rprof_omp_emit(event: *mut OmpEvent) {
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
