//! Allocation-free Rust half of the Linux POSIX preload collector. C owns the
//! ABI/variadic wrappers and recursion guard. Completed observations are sent to
//! the external profiler over a nonblocking inherited datagram socket; serialization
//! and file output never run in the instrumented process.

#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
compile_error!("the initial I/O preload collector requires 64-bit Linux");

use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, AtomicU64, Ordering};

use rprof_io_protocol::{
    EVENT_SIZE, FS_DEVICE_VALID, FS_MAGIC_VALID, FS_MODE_VALID, FS_MOUNT_ID_VALID,
    FS_NAMESPACE_VALID, IoEvent, MAGIC, PATH_ARGUMENT, PATH_CAPACITY, PATH_TRUNCATED,
    PATH_UNAVAILABLE, STATS_MAGIC, Stats, VERSION, op,
};

const FD_SLOTS: usize = 65_536;
static SOCKET: AtomicI32 = AtomicI32::new(-1);
static STATS_FD: AtomicI32 = AtomicI32::new(-1);
static STATS: AtomicPtr<Stats> = AtomicPtr::new(std::ptr::null_mut());
static PID: AtomicU32 = AtomicU32::new(0);
static INSTANCE: AtomicU64 = AtomicU64::new(0);
static NEXT_RESOURCE: AtomicU64 = AtomicU64::new(1);
static RESOURCES: [AtomicU64; FD_SLOTS] = [const { AtomicU64::new(0) }; FD_SLOTS];
struct CachedPath {
    generation: AtomicU64,
    metadata: AtomicU64,
    words: [AtomicU64; PATH_CAPACITY / 8],
    fs_magic: AtomicU64,
    mount_namespace: AtomicU64,
    device: AtomicU64,
    mount_mode: AtomicU64,
    fs_valid: AtomicU32,
}
static PATHS: [CachedPath; FD_SLOTS] = [const {
    CachedPath {
        generation: AtomicU64::new(0),
        metadata: AtomicU64::new(0),
        words: [const { AtomicU64::new(0) }; PATH_CAPACITY / 8],
        fs_magic: AtomicU64::new(0),
        mount_namespace: AtomicU64::new(0),
        device: AtomicU64::new(0),
        mount_mode: AtomicU64::new(0),
        fs_valid: AtomicU32::new(0),
    }
}; FD_SLOTS];
const CACHE_BUSY: u64 = u64::MAX;

fn now() -> u64 {
    let mut time: libc::timespec = unsafe { std::mem::zeroed() };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) } != 0 {
        return 0;
    }
    (time.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(time.tv_nsec as u64)
}

fn nonce() -> u64 {
    rprof_io_protocol::process_instance()
}

unsafe fn env_fd(name: &'static [u8]) -> Option<i32> {
    let text = unsafe { libc::getenv(name.as_ptr().cast()) };
    if text.is_null() {
        return None;
    }
    let mut value = 0i32;
    for index in 0..11 {
        let byte = unsafe { *text.add(index) } as u8;
        if byte == 0 {
            return (index > 0).then_some(value);
        }
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add((byte - b'0') as i32)?;
    }
    None
}

#[unsafe(no_mangle)]
pub extern "C" fn rprof_io_init() {
    let Some(socket) = (unsafe { env_fd(b"RPROF_IO_FD\0") }) else {
        return;
    };
    let Some(stats_fd) = (unsafe { env_fd(b"RPROF_IO_STATS_FD\0") }) else {
        return;
    };
    let mut kind = 0i32;
    let mut length = std::mem::size_of::<i32>() as libc::socklen_t;
    if unsafe {
        libc::getsockopt(
            socket,
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
    let mapping = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            std::mem::size_of::<Stats>(),
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            stats_fd,
            0,
        )
    };
    if mapping == libc::MAP_FAILED {
        return;
    }
    let stats = mapping.cast::<Stats>();
    if unsafe { (*stats).magic } != STATS_MAGIC {
        unsafe {
            libc::munmap(mapping, std::mem::size_of::<Stats>());
        }
        return;
    }
    STATS.store(stats, Ordering::Release);
    STATS_FD.store(stats_fd, Ordering::Relaxed);
    PID.store(unsafe { libc::getpid() } as u32, Ordering::Relaxed);
    INSTANCE.store(nonce(), Ordering::Relaxed);
    unsafe {
        (*stats).initialized.fetch_add(1, Ordering::Relaxed);
    }
    SOCKET.store(socket, Ordering::Release);
}

/// Called by the C pthread_atfork child handler. No locks, allocation, or inherited
/// writer threads need repair. Each child starts a fresh descriptor/instance namespace.
#[unsafe(no_mangle)]
pub extern "C" fn rprof_io_after_fork() {
    if SOCKET.load(Ordering::Acquire) < 0 {
        return;
    }
    PID.store(unsafe { libc::getpid() } as u32, Ordering::Relaxed);
    INSTANCE.store(nonce(), Ordering::Relaxed);
    // Keep the inherited monotonic counter: new resource IDs cannot match stale
    // cached generations, without touching/copying the sparse path-cache pages.
    for slot in &RESOURCES {
        slot.store(0, Ordering::Relaxed);
    }
    let stats = STATS.load(Ordering::Acquire);
    if !stats.is_null() {
        unsafe {
            (*stats).initialized.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn rprof_io_fini() {
    // Other threads can still be exiting. Leave mappings to process teardown.
    SOCKET.store(-1, Ordering::Release);
}

fn resource(fd: i32) -> u64 {
    if fd < 0 {
        return 0;
    }
    if fd as usize >= FD_SLOTS {
        return (1u64 << 63) | fd as u64;
    }
    let slot = &RESOURCES[fd as usize];
    let current = slot.load(Ordering::Acquire);
    if current != 0 {
        return current;
    }
    let next = NEXT_RESOURCE.fetch_add(1, Ordering::Relaxed);
    match slot.compare_exchange(0, next, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => next,
        Err(current) => current,
    }
}

fn new_resource(fd: i32) -> u64 {
    if fd < 0 || fd as usize >= FD_SLOTS {
        return resource(fd);
    }
    let value = NEXT_RESOURCE.fetch_add(1, Ordering::Relaxed);
    RESOURCES[fd as usize].store(value, Ordering::Release);
    value
}

fn fd_filesystem(event: &mut IoEvent, fd: i32) {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::syscall(libc::SYS_fstat, fd, &mut stat) } == 0 {
        event.file_mode = stat.st_mode;
        event.fs_valid |= FS_MODE_VALID;
        let device = stat.st_dev as u64;
        event.dev_major = libc::major(device) as u32;
        event.dev_minor = libc::minor(device) as u32;
        event.fs_valid |= FS_DEVICE_VALID;
    }
    let mut fs: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::syscall(libc::SYS_fstatfs, fd, &mut fs) } == 0 {
        event.fs_magic = fs.f_type as u64;
        event.fs_valid |= FS_MAGIC_VALID;
    }
    let mut statx: libc::statx = unsafe { std::mem::zeroed() };
    if unsafe {
        libc::syscall(
            libc::SYS_statx,
            fd,
            c"".as_ptr(),
            libc::AT_EMPTY_PATH | libc::AT_STATX_SYNC_AS_STAT,
            libc::STATX_MNT_ID,
            &mut statx,
        )
    } == 0
        && statx.stx_mask & libc::STATX_MNT_ID != 0
        && statx.stx_mnt_id <= u32::MAX as u64
    {
        event.mount_id = statx.stx_mnt_id as u32;
        event.fs_valid |= FS_MOUNT_ID_VALID;
    }
    let mut namespace: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(c"/proc/self/ns/mnt".as_ptr(), &mut namespace) } == 0 {
        event.mount_namespace = namespace.st_ino;
        event.fs_valid |= FS_NAMESPACE_VALID;
    }
}

fn cached_filesystem(event: &mut IoEvent, cache: &CachedPath) {
    event.fs_magic = cache.fs_magic.load(Ordering::Relaxed);
    event.mount_namespace = cache.mount_namespace.load(Ordering::Relaxed);
    let device = cache.device.load(Ordering::Relaxed);
    event.dev_major = device as u32;
    event.dev_minor = (device >> 32) as u32;
    let mount_mode = cache.mount_mode.load(Ordering::Relaxed);
    event.mount_id = mount_mode as u32;
    event.file_mode = (mount_mode >> 32) as u32;
    event.fs_valid = cache.fs_valid.load(Ordering::Relaxed);
}

fn store_filesystem(event: &IoEvent, cache: &CachedPath) {
    cache.fs_magic.store(event.fs_magic, Ordering::Relaxed);
    cache
        .mount_namespace
        .store(event.mount_namespace, Ordering::Relaxed);
    cache.device.store(
        event.dev_major as u64 | (event.dev_minor as u64) << 32,
        Ordering::Relaxed,
    );
    cache.mount_mode.store(
        event.mount_id as u64 | (event.file_mode as u64) << 32,
        Ordering::Relaxed,
    );
    cache.fs_valid.store(event.fs_valid, Ordering::Relaxed);
}

fn fd_path(event: &mut IoEvent, fd: i32) {
    event.path_len = 0;
    event.path.fill(0);
    event.path_flags = PATH_UNAVAILABLE;
    if fd < 0 {
        return;
    }
    if (fd as usize) < FD_SLOTS && event.resource != 0 {
        let cache = &PATHS[fd as usize];
        let generation = cache.generation.load(Ordering::Acquire);
        if generation == event.resource && generation != CACHE_BUSY {
            let metadata = cache.metadata.load(Ordering::Relaxed);
            cached_filesystem(event, cache);
            for (word, bytes) in cache.words.iter().zip(event.path.chunks_exact_mut(8)) {
                bytes.copy_from_slice(&word.load(Ordering::Relaxed).to_ne_bytes());
            }
            std::sync::atomic::fence(Ordering::Acquire);
            if cache.generation.load(Ordering::Acquire) == generation {
                event.path_len = (metadata & 0xffff) as u16;
                event.path_flags = (metadata >> 16) as u16;
                return;
            }
            event.path.fill(0);
            event.fs_valid = 0;
        }
    }
    fd_filesystem(event, fd);
    let mut proc = [0u8; 40];
    let prefix = b"/proc/self/fd/";
    proc[..prefix.len()].copy_from_slice(prefix);
    let mut number = fd as u32;
    let mut digits = [0u8; 10];
    let mut count = 0;
    loop {
        digits[count] = b'0' + (number % 10) as u8;
        count += 1;
        number /= 10;
        if number == 0 {
            break;
        }
    }
    for i in 0..count {
        proc[prefix.len() + i] = digits[count - i - 1];
    }
    let length = unsafe {
        libc::syscall(
            libc::SYS_readlinkat,
            libc::AT_FDCWD,
            proc.as_ptr(),
            event.path.as_mut_ptr(),
            PATH_CAPACITY,
        )
    };
    if length >= 0 {
        event.path_len = length as u16;
        event.path_flags = if length as usize == PATH_CAPACITY {
            PATH_TRUNCATED
        } else {
            0
        };
    }
    if (fd as usize) < FD_SLOTS && event.resource != 0 {
        let cache = &PATHS[fd as usize];
        let previous = cache.generation.load(Ordering::Acquire);
        if previous < event.resource
            && RESOURCES[fd as usize].load(Ordering::Acquire) == event.resource
            && cache
                .generation
                .compare_exchange(previous, CACHE_BUSY, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        {
            // Atomic words make concurrent cache snapshots data-race-free,
            // even when a descriptor is being closed/reused by another thread.
            for (word, bytes) in cache.words.iter().zip(event.path.chunks_exact(8)) {
                let mut value = [0u8; 8];
                value.copy_from_slice(bytes);
                word.store(u64::from_ne_bytes(value), Ordering::Relaxed);
            }
            cache.metadata.store(
                event.path_len as u64 | ((event.path_flags as u64) << 16),
                Ordering::Relaxed,
            );
            store_filesystem(event, cache);
            cache.generation.store(event.resource, Ordering::Release);
        }
    }
}

unsafe fn argument_path(event: &mut IoEvent, path: *const libc::c_char) {
    event.path_flags = PATH_ARGUMENT | PATH_UNAVAILABLE;
    if path.is_null() {
        return;
    }
    // A failed application call may have an invalid path pointer. Reading it via
    // process_vm_readv preserves EFAULT behavior instead of crashing the workload.
    let local = libc::iovec {
        iov_base: event.path.as_mut_ptr().cast(),
        iov_len: PATH_CAPACITY,
    };
    let remote = libc::iovec {
        iov_base: path.cast_mut().cast(),
        iov_len: PATH_CAPACITY,
    };
    let length = unsafe {
        libc::syscall(
            libc::SYS_process_vm_readv,
            libc::getpid(),
            &local,
            1usize,
            &remote,
            1usize,
            0usize,
        )
    };
    if length <= 0 {
        return;
    }
    let read = length as usize;
    let end = event.path[..read].iter().position(|b| *b == 0);
    event.path_len = end.unwrap_or(read) as u16;
    event.path_flags = PATH_ARGUMENT | if end.is_none() { PATH_TRUNCATED } else { 0 };
    event.path[event.path_len as usize..].fill(0);
}

/// Prepare metadata before timing the real libc operation.
///
/// # Safety
/// `event` must point to writable, aligned storage for one IoEvent. `path` may be
/// an application-supplied invalid pointer; it is only read with a kernel-assisted copy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rprof_io_begin(
    event: *mut IoEvent,
    operation: u16,
    fd: i32,
    other_fd: i32,
    path: *const libc::c_char,
    flags: i32,
    offset: i64,
    requested: u64,
) -> i32 {
    let socket = SOCKET.load(Ordering::Acquire);
    if socket < 0 || fd == socket || fd == STATS_FD.load(Ordering::Relaxed) || event.is_null() {
        return 0;
    }
    unsafe {
        std::ptr::write_bytes(event, 0, 1);
    }
    let event = unsafe { &mut *event };
    event.magic = MAGIC;
    event.version = VERSION;
    event.operation = operation;
    event.pid = PID.load(Ordering::Relaxed);
    event.tid = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
    event.ppid = unsafe { libc::getppid() } as u32;
    event.instance = INSTANCE.load(Ordering::Relaxed);
    event.fd = fd;
    event.other_fd = other_fd;
    event.flags = flags as u32;
    event.offset = offset;
    event.requested = requested;
    if matches!(operation, op::OPEN | op::OPENAT | op::CREAT) {
        unsafe {
            argument_path(event, path);
        }
    } else {
        event.resource = resource(fd);
        fd_path(event, fd);
    }
    event.start_ns = now();
    i32::from(event.start_ns != 0)
}

/// Complete one observation. The C wrapper restores the application's errno after
/// this function returns, including errors from profiler syscalls.
///
/// # Safety
/// `event` must be an IoEvent initialized by a successful rprof_io_begin call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rprof_io_end(event: *mut IoEvent, result: i64, error: i32) {
    let end = now();
    let event = unsafe { &mut *event };
    event.end_ns = end;
    event.result = result;
    event.error = if result < 0 { error } else { 0 };
    let stats = STATS.load(Ordering::Acquire);
    if stats.is_null() {
        return;
    }
    let stats = unsafe { &*stats };
    stats.attempted.fetch_add(1, Ordering::Relaxed);
    match event.operation {
        op::OPEN | op::OPENAT | op::CREAT if result >= 0 => {
            event.fd = result as i32;
            event.resource = new_resource(event.fd);
            fd_path(event, event.fd);
        }
        op::CLOSE if error != libc::EBADF || result == 0 => {
            if event.fd >= 0 && (event.fd as usize) < FD_SLOTS {
                let _ = RESOURCES[event.fd as usize].compare_exchange(
                    event.resource,
                    0,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
        }
        op::DUP | op::DUP2 | op::DUP3 if result >= 0 => {
            event.other_fd = result as i32;
            if event.other_fd != event.fd {
                new_resource(event.other_fd);
            }
        }
        _ => (),
    }
    // Logging must not add a pthread cancellation point or allow a forced C
    // unwind through a Rust hook. The real libc operation owns cancellation.
    let sent = unsafe {
        libc::syscall(
            libc::SYS_sendto,
            SOCKET.load(Ordering::Acquire),
            (event as *const IoEvent).cast::<libc::c_void>(),
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
