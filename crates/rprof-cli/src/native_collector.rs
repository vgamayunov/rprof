use std::{
    collections::{BTreeMap, HashMap},
    ffi::{CString, OsString},
    fs::File,
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, net::UnixDatagram},
    },
    path::{Path, PathBuf},
    ptr::NonNull,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use rprof_core::{
    Category, Span, TrackKind,
    stream::{Attribute, Event, Payload},
};
use rprof_io_protocol::{
    IoEvent, MAGIC, PATH_ARGUMENT, PATH_CAPACITY, PATH_TRUNCATED, PATH_UNAVAILABLE, Stats, VERSION,
    op,
};

use crate::{capture_sink::CaptureSink, record::monotonic_ns};

pub(crate) struct Mapping(NonNull<Stats>);
// The mapping lives until all receiver/supervisor references are dropped. The
// immutable magic and atomic counters are the only fields shared across threads.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}
impl Mapping {
    pub(crate) fn stats(&self) -> &Stats {
        unsafe { self.0.as_ref() }
    }
}
impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            libc::munmap(self.0.as_ptr().cast(), std::mem::size_of::<Stats>());
        }
    }
}

pub struct Prepared {
    receiver: UnixDatagram,
    sender: File,
    stats_file: File,
    mapping: Arc<Mapping>,
    library: PathBuf,
    buffer_events: usize,
    kind: Kind,
}

#[derive(Clone, Copy)]
enum Kind {
    Io,
    Mpi,
    Openmp,
}
impl Kind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Io => "RPROF_IO",
            Self::Mpi => "RPROF_MPI",
            Self::Openmp => "RPROF_OMP",
        }
    }
    fn library(self) -> &'static str {
        match self {
            Self::Io => "librprof_io.so",
            Self::Mpi => "librprof_mpi.so",
            Self::Openmp => "librprof_ompt.so",
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Io => "io",
            Self::Mpi => "mpi",
            Self::Openmp => "openmp",
        }
    }
}

fn inherited_copy(fd: i32) -> Result<File> {
    let copied = unsafe { libc::fcntl(fd, libc::F_DUPFD, 198) };
    ensure!(
        copied >= 0,
        "reserving collector descriptor: {}",
        io::Error::last_os_error()
    );
    Ok(unsafe { File::from_raw_fd(copied) })
}

impl Prepared {
    pub fn new(library: Option<&Path>, buffer_events: usize) -> Result<Self> {
        Self::configured(library, buffer_events, Kind::Io)
    }
    pub fn mpi(library: Option<&Path>, buffer_events: usize) -> Result<Self> {
        Self::configured(library, buffer_events, Kind::Mpi)
    }
    pub fn openmp(library: Option<&Path>, buffer_events: usize) -> Result<Self> {
        Self::configured(library, buffer_events, Kind::Openmp)
    }
    fn configured(library: Option<&Path>, buffer_events: usize, kind: Kind) -> Result<Self> {
        let library = match library {
            Some(path) => path.to_path_buf(),
            None => std::env::current_exe()?
                .parent()
                .context("executable directory missing")?
                .join(kind.library()),
        };
        let library = library.canonicalize().with_context(|| {
            format!(
                "finding {}; build the collector with `make preload`, `make mpi`, or `make ompt`, or set its --*-library path",
                library.display()
            )
        })?;
        ensure!(library.is_file(), "collector library must be a file");
        ensure!(
            !library
                .as_os_str()
                .as_bytes()
                .iter()
                .any(|b| b.is_ascii_whitespace() || *b == b':'),
            "LD_PRELOAD library path cannot contain whitespace or ':'"
        );
        let (receiver, sender) = UnixDatagram::pair()?;
        receiver.set_read_timeout(Some(Duration::from_millis(20)))?;
        let capacity = 4 * 1024 * 1024i32;
        // The kernel may clamp socket budgets to its configured maximum.
        for (fd, option) in [
            (receiver.as_raw_fd(), libc::SO_RCVBUF),
            (sender.as_raw_fd(), libc::SO_SNDBUF),
        ] {
            ensure!(
                unsafe {
                    libc::setsockopt(
                        fd,
                        libc::SOL_SOCKET,
                        option,
                        (&capacity as *const i32).cast(),
                        std::mem::size_of::<i32>() as libc::socklen_t,
                    )
                } == 0,
                "configuring collector socket: {}",
                io::Error::last_os_error()
            );
        }
        let sender = inherited_copy(sender.as_raw_fd())?;
        let name = CString::new("rprof-io-stats")?;
        let fd = unsafe {
            libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING)
        };
        ensure!(
            fd >= 0,
            "creating collector counters: {}",
            io::Error::last_os_error()
        );
        let original = unsafe { File::from_raw_fd(fd) };
        original.set_len(std::mem::size_of::<Stats>() as u64)?;
        ensure!(
            unsafe {
                libc::fcntl(
                    fd,
                    libc::F_ADD_SEALS,
                    libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
                )
            } == 0,
            "sealing collector counters: {}",
            io::Error::last_os_error()
        );
        let stats_file = inherited_copy(fd)?;
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                std::mem::size_of::<Stats>(),
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        ensure!(
            address != libc::MAP_FAILED,
            "mapping collector counters: {}",
            io::Error::last_os_error()
        );
        let pointer = NonNull::new(address.cast::<Stats>()).context("null counter mapping")?;
        unsafe {
            pointer.as_ptr().write(Stats::default());
        }
        Ok(Self {
            receiver,
            sender,
            stats_file,
            mapping: Arc::new(Mapping(pointer)),
            library,
            buffer_events,
            kind,
        })
    }

    pub fn library(&self) -> &Path {
        &self.library
    }
    pub fn environment(&self) -> Vec<(OsString, OsString)> {
        vec![
            (
                format!("{}_FD", self.kind.prefix()).into(),
                self.sender.as_raw_fd().to_string().into(),
            ),
            (
                format!("{}_STATS_FD", self.kind.prefix()).into(),
                self.stats_file.as_raw_fd().to_string().into(),
            ),
        ]
    }

    pub fn start(self, sink: Arc<Mutex<CaptureSink>>, origin: u64) -> Result<Running> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let mapping = Arc::clone(&self.mapping);
        let wake = self.sender.try_clone()?;
        let worker = thread::Builder::new()
            .name(format!("rprof-{}-receiver", self.kind.name()))
            .spawn(move || {
                let receive = match self.kind {
                    Kind::Mpi => crate::mpi_collector::receive,
                    Kind::Openmp => crate::openmp_collector::receive,
                    Kind::Io => receive,
                };
                let result = receive(
                    &self.receiver,
                    &mapping,
                    &stopping,
                    &sink,
                    origin,
                    self.buffer_events,
                );
                // self keeps the sender and counter descriptors alive throughout launch.
                drop(self);
                result
            })?;
        Ok(Running {
            stop,
            wake,
            worker: Some(worker),
        })
    }
}

pub struct Running {
    stop: Arc<AtomicBool>,
    wake: File,
    worker: Option<JoinHandle<Result<Outcome>>>,
}
pub struct Outcome {
    pub attempted: u64,
    pub received: u64,
    pub dropped: u64,
    pub initialized: u64,
}
impl Running {
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        unsafe {
            libc::send(
                self.wake.as_raw_fd(),
                std::ptr::null(),
                0,
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            );
        }
    }
    pub fn check(&self) -> Result<()> {
        ensure!(
            !self
                .worker
                .as_ref()
                .is_some_and(|worker| worker.is_finished()),
            "native collector receiver stopped unexpectedly"
        );
        Ok(())
    }
    pub fn finish(mut self) -> Result<Outcome> {
        self.stop();
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow::anyhow!("native collector receiver panicked"))?
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn receive(
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
            .name("rprof-io-ingress".into())
            .spawn_scoped(scope, || pump(socket, mapping, stop, sender))?;
        let result = consume(receiver, mapping, sink, origin);
        if result.is_err() {
            stop.store(true, Ordering::Release);
        }
        pump.join()
            .map_err(|_| anyhow::anyhow!("I/O ingress thread panicked"))??;
        result
    })
}

pub(crate) fn pump<T: Copy + Send>(
    socket: &UnixDatagram,
    mapping: &Mapping,
    stop: &AtomicBool,
    sender: SyncSender<T>,
) -> Result<()> {
    // Instantiated only with the fixed integer/byte-array POD wire structs.
    let size = std::mem::size_of::<T>();
    ensure!(size <= 1023, "native wire event exceeds buffer");
    let mut buffer = [0u8; 1024];
    let mut drain_count = 0;
    let mut draining = false;
    loop {
        if stop.load(Ordering::Acquire) && !draining {
            socket.set_nonblocking(true)?;
            draining = true;
        }
        match socket.recv(&mut buffer[..size + 1]) {
            Ok(length) => {
                if stop.load(Ordering::Acquire) {
                    drain_count += 1;
                    ensure!(
                        drain_count <= 65_536,
                        "I/O producers remained active after workload cleanup"
                    );
                }
                if length != size {
                    continue;
                }
                let raw = unsafe { std::ptr::read_unaligned(buffer.as_ptr().cast::<T>()) };
                match sender.try_send(raw) {
                    Ok(()) => (),
                    Err(TrySendError::Full(_)) => {
                        mapping.stats().dropped.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                if stop.load(Ordering::Acquire) {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("receiving native I/O events"),
        }
    }
    Ok(())
}

fn consume(
    receiver: Receiver<IoEvent>,
    mapping: &Mapping,
    sink: &Mutex<CaptureSink>,
    origin: u64,
) -> Result<Outcome> {
    let mut files = HashMap::<(u64, Vec<u8>, u16), u64>::new();
    let mut received = 0u64;
    for raw in receiver {
        let Some(operation) = op::name(raw.operation) else {
            continue;
        };
        if raw.magic != MAGIC
            || raw.version != VERSION
            || raw.path_len as usize > PATH_CAPACITY
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
        let at = now - origin;
        let path_bytes = &raw.path[..raw.path_len as usize];
        let path = if path_bytes.is_empty() {
            format!("<unavailable fd {}>", raw.fd)
        } else {
            String::from_utf8_lossy(path_bytes).into_owned()
        };
        let mut out = sink
            .lock()
            .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
        out.set_ingress_loss("io", mapping.stats().dropped.load(Ordering::Relaxed));
        let thread = out.native_thread(at, raw.pid, raw.instance, raw.tid, None)?;
        let mut file_identity = if path_bytes.is_empty() {
            path.as_bytes().to_vec()
        } else {
            path_bytes.to_vec()
        };
        if raw.path_flags & (PATH_TRUNCATED | PATH_UNAVAILABLE) != 0 {
            file_identity.extend_from_slice(&raw.resource.to_le_bytes());
        }
        let key = (thread, file_identity, raw.path_flags);
        let track = match files.get(&key) {
            Some(id) => *id,
            None => {
                let suffix = if raw.path_flags & PATH_TRUNCATED != 0 {
                    " (truncated path)"
                } else {
                    ""
                };
                let id = out.track(
                    at,
                    thread,
                    format!("{path}{suffix}"),
                    TrackKind::Io {
                        resource: path.clone(),
                    },
                )?;
                files.insert(key, id);
                id
            }
        };
        let completed = if matches!(raw.operation, op::READ | op::WRITE | op::PREAD | op::PWRITE)
            && raw.result > 0
        {
            raw.result as u64
        } else {
            0
        };
        let mut attrs = BTreeMap::from([
            ("io.operation".into(), Attribute::Text(operation.into())),
            ("io.file".into(), Attribute::Text(path)),
            ("io.fd".into(), Attribute::Signed(raw.fd as i64)),
            ("io.other_fd".into(), Attribute::Signed(raw.other_fd as i64)),
            ("io.resource".into(), Attribute::Unsigned(raw.resource)),
            (
                "io.requested_bytes".into(),
                Attribute::Unsigned(raw.requested),
            ),
            ("io.completed_bytes".into(), Attribute::Unsigned(completed)),
            ("io.result".into(), Attribute::Signed(raw.result)),
            ("io.errno".into(), Attribute::Signed(raw.error as i64)),
            ("io.offset".into(), Attribute::Signed(raw.offset)),
            ("io.flags".into(), Attribute::Unsigned(raw.flags as u64)),
            (
                "io.path_truncated".into(),
                Attribute::Bool(raw.path_flags & PATH_TRUNCATED != 0),
            ),
            (
                "io.path_unavailable".into(),
                Attribute::Bool(raw.path_flags & PATH_UNAVAILABLE != 0),
            ),
            (
                "io.path_from_argument".into(),
                Attribute::Bool(raw.path_flags & PATH_ARGUMENT != 0),
            ),
            (
                "io.lifetime_tracked".into(),
                Attribute::Bool(raw.resource != 0 && raw.resource & (1 << 63) == 0),
            ),
            ("pid".into(), Attribute::Unsigned(raw.pid as u64)),
            ("tid".into(), Attribute::Unsigned(raw.tid as u64)),
            ("ppid".into(), Attribute::Unsigned(raw.ppid as u64)),
            ("process_instance".into(), Attribute::Unsigned(raw.instance)),
        ]);
        if std::str::from_utf8(path_bytes).is_err() {
            attrs.insert(
                "io.path_raw_hex".into(),
                Attribute::Text(path_bytes.iter().map(|b| format!("{b:02x}")).collect()),
            );
        }
        out.emit(Event {
            timestamp_ns: at,
            attributes: attrs,
            payload: Payload::Span(Span {
                track_id: track,
                name: operation.into(),
                category: Category::Io,
                start_ns: raw.start_ns - origin,
                duration_ns: raw.end_ns - raw.start_ns,
                correlation_id: None,
            }),
        })?;
    }
    let stats = mapping.stats();
    let attempted = stats.attempted.load(Ordering::Acquire);
    let dropped = attempted.saturating_sub(received);
    let mut out = sink
        .lock()
        .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
    out.set_ingress_loss("io", dropped);
    Ok(Outcome {
        attempted,
        received,
        dropped,
        initialized: stats.initialized.load(Ordering::Acquire),
    })
}
