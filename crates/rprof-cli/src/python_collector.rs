use crate::{capture_sink::CaptureSink, record::monotonic_ns};
use anyhow::{Result, ensure};
use rprof_core::{
    Category, CounterSample, CounterValue, Span,
    stream::{Attribute, Event, Payload},
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fs, io,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
};

const MAX_PACKET: usize = 8192;
const MAX_SOURCES: usize = 4096;
#[derive(Default, Deserialize)]
#[serde(default)]
struct Message {
    v: u8,
    kind: String,
    source: String,
    pid: u32,
    tid: u32,
    thread: u64,
    image: u64,
    interpreter: u64,
    seq: u64,
    attempted: u64,
    dropped: u64,
    omitted: u64,
    unmatched: u64,
    errors: u64,
    at: u64,
    call: u64,
    parent: u64,
    start: u64,
    end: u64,
    self_ns: u64,
    symbol: u64,
    line: u32,
    name: String,
    file: String,
    execution_kind: String,
    boundary: String,
    truncated: bool,
    backend: String,
    version: String,
    implementation: String,
    clock: String,
    detail: String,
    initialized: bool,
    finalized: bool,
    abandoned: u64,
    inflight: u64,
    ppid: u32,
    forked: bool,
}
pub struct Prepared {
    directory: tempfile::TempDir,
    socket: OwnedFd,
    environment: Vec<(OsString, OsString)>,
    capacity: usize,
}
impl Prepared {
    pub fn new(
        roots: &[std::path::PathBuf],
        backend: &str,
        capacity: usize,
        inline: bool,
    ) -> Result<Self> {
        ensure!(
            capacity <= 32768,
            "Python supports at most 32768 --native-buffer-events"
        );
        let directory = tempfile::Builder::new().prefix("rprof-python-").tempdir()?;
        let package = directory.path().join("rprof_python");
        fs::create_dir(&package)?;
        for (name, content) in [
            (
                "sitecustomize.py",
                include_str!("../../../python/sitecustomize.py"),
            ),
            (
                "rprof_python/__init__.py",
                include_str!("../../../python/rprof_python/__init__.py"),
            ),
            (
                "rprof_python/_runtime.py",
                include_str!("../../../python/rprof_python/_runtime.py"),
            ),
            (
                "rprof_python/torch.py",
                include_str!("../../../python/rprof_python/torch.py"),
            ),
        ] {
            fs::write(directory.path().join(name), content)?;
        }
        let address = directory.path().join("events.sock");
        let fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        ensure!(
            fd >= 0,
            "creating Python collector socket: {}",
            io::Error::last_os_error()
        );
        let socket = unsafe { OwnedFd::from_raw_fd(fd) };
        let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        addr.sun_family = libc::AF_UNIX as _;
        let bytes = address.as_os_str().as_bytes();
        ensure!(
            bytes.len() < addr.sun_path.len(),
            "Python collector socket path is too long"
        );
        for (out, byte) in addr.sun_path.iter_mut().zip(bytes) {
            *out = *byte as libc::c_char;
        }
        ensure!(
            unsafe {
                libc::bind(
                    fd,
                    (&addr as *const libc::sockaddr_un).cast(),
                    std::mem::size_of_val(&addr) as _,
                )
            } == 0,
            "binding Python collector socket: {}",
            io::Error::last_os_error()
        );
        ensure!(
            unsafe { libc::listen(fd, 128) } == 0,
            "listening on Python collector socket: {}",
            io::Error::last_os_error()
        );
        let bytes = 4 * 1024 * 1024i32;
        ensure!(
            unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    (&bytes as *const i32).cast(),
                    std::mem::size_of::<i32>() as libc::socklen_t,
                )
            } == 0,
            "configuring Python receive buffer"
        );
        let roots = if roots.is_empty() {
            vec![std::env::current_dir()?]
        } else {
            roots
                .iter()
                .map(|p| p.canonicalize())
                .collect::<io::Result<Vec<_>>>()?
        };
        let roots = serde_json::to_string(
            &roots
                .iter()
                .map(|p| p.to_string_lossy())
                .collect::<Vec<_>>(),
        )?;
        ensure!(
            roots.len() <= 4096,
            "Python include paths exceed 4096 encoded bytes"
        );
        ensure!(
            std::env::var_os("RPROF_PYTHON_SOCKET").is_none(),
            "a Python capture is already active; nested record --collect python is not supported"
        );
        let mut path = directory.path().as_os_str().to_os_string();
        if let Some(prior) = std::env::var_os("PYTHONPATH").filter(|p| !p.is_empty()) {
            path.push(":");
            path.push(prior);
        }
        let environment = vec![
            ("PYTHONPATH".into(), path),
            ("RPROF_PYTHON_SOCKET".into(), address.into_os_string()),
            ("RPROF_PYTHON_INCLUDE".into(), roots.into()),
            ("RPROF_PYTHON_BACKEND".into(), backend.into()),
            (
                "RPROF_PYTHON_INLINE".into(),
                if inline { "1" } else { "0" }.into(),
            ),
        ];
        Ok(Self {
            directory,
            socket,
            environment,
            capacity,
        })
    }
    pub fn environment(&self) -> Vec<(OsString, OsString)> {
        self.environment.clone()
    }
    pub fn start(self, sink: Arc<Mutex<CaptureSink>>, origin: u64) -> Result<Running> {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker=thread::Builder::new().name("rprof-python".into()).spawn(move||{
            let queue_drops=AtomicU64::new(0);
            let result=thread::scope(|scope|{
                let (sender,receiver)=mpsc::sync_channel::<Vec<u8>>(self.capacity);
                let pump_stop=&stopping;let socket=&self.socket;let drops=&queue_drops;
                let pump=scope.spawn(move||->Result<()>{
                    let mut buffer=[0u8;MAX_PACKET+1];let mut drain=0;let mut clients:Vec<OwnedFd>=Vec::new();
                    loop{
                        let stopped=pump_stop.load(Ordering::Acquire);
                        let mut polls=Vec::with_capacity(clients.len()+1);
                        polls.push(libc::pollfd{fd:socket.as_raw_fd(),events:libc::POLLIN,revents:0});
                        polls.extend(clients.iter().map(|fd|libc::pollfd{fd:fd.as_raw_fd(),events:libc::POLLIN,revents:0}));
                        let ready=unsafe{libc::poll(polls.as_mut_ptr(),polls.len() as _,if stopped{0}else{20})};
                        if ready<0{let e=io::Error::last_os_error();if e.kind()==io::ErrorKind::Interrupted{continue;}return Err(e.into());}
                        if ready==0{if stopped{break;}continue;}
                        // Process old connections before accepting new ones, keeping indexes stable.
                        for index in (0..clients.len()).rev(){
                            if polls[index+1].revents==0{continue;}
                            let mut closed=false;
                            for _ in 0..64{
                                let n=unsafe{libc::recv(clients[index].as_raw_fd(),buffer.as_mut_ptr().cast(),buffer.len(),libc::MSG_DONTWAIT)};
                                if n==0{closed=true;break;}
                                if n<0 {
                                    let e=io::Error::last_os_error();
                                    if e.kind()==io::ErrorKind::Interrupted {continue;}
                                    if e.kind()!=io::ErrorKind::WouldBlock {closed=true;}
                                    break;
                                }
                                if stopped{drain+=1;ensure!(drain<=65536,"Python producers remained active after workload cleanup");}
                                match sender.try_send(buffer[..n as usize].to_vec()){
                                    Ok(())=>(),Err(mpsc::TrySendError::Full(_))=>{drops.fetch_add(1,Ordering::Relaxed);},Err(mpsc::TrySendError::Disconnected(_))=>return Ok(()),
                                }
                            }
                            if closed{clients.swap_remove(index);}
                        }
                        if polls[0].revents&libc::POLLIN!=0{
                            for _ in 0..128{
                                let fd=unsafe{libc::accept4(socket.as_raw_fd(),std::ptr::null_mut(),std::ptr::null_mut(),libc::SOCK_CLOEXEC|libc::SOCK_NONBLOCK)};
                                if fd<0 {
                                    let e=io::Error::last_os_error();
                                    if e.kind()==io::ErrorKind::WouldBlock {break;}
                                    if e.kind()==io::ErrorKind::Interrupted {continue;}
                                    return Err(e.into());
                                }
                                let fd=unsafe{OwnedFd::from_raw_fd(fd)};
                                ensure!(clients.len()<4096,"Python connection limit exceeded");clients.push(fd);
                            }
                        }
                    }Ok(())
                });
                let result=consume(receiver,&sink,origin,&queue_drops);
                if result.is_err(){stopping.store(true,Ordering::Release);}
                pump.join().map_err(|_|anyhow::anyhow!("Python ingress thread panicked"))??;result
            });
            drop(self.directory);result
        })?;
        Ok(Running {
            stop,
            worker: Some(worker),
        })
    }
}
pub struct Running {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<Outcome>>>,
}
pub struct Outcome {
    pub initialized: u64,
    pub errors: u64,
    pub received: u64,
    pub dropped: u64,
}
impl Running {
    pub fn check(&self) -> Result<()> {
        ensure!(
            !self.worker.as_ref().is_some_and(|w| w.is_finished()),
            "Python collector stopped unexpectedly"
        );
        Ok(())
    }
    pub fn finish(mut self) -> Result<Outcome> {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow::anyhow!("Python collector panicked"))?
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct Source {
    namespace: u64,
    pid: u32,
    image: u64,
    interpreter: u64,
    received: u64,
    attempted: u64,
    dropped: u64,
    omitted: u64,
    unmatched: u64,
    errors: u64,
    initialized: bool,
    finalized: bool,
    abandoned: u64,
    inflight: u64,
    streams: HashMap<u64, (u32, u64, u64)>,
}
fn attributes(m: &Message) -> BTreeMap<String, Attribute> {
    BTreeMap::from([
        ("python.source".into(), Attribute::Text(m.source.clone())),
        (
            "python.interpreter".into(),
            if m.interpreter == u64::MAX {
                Attribute::Text("unknown".into())
            } else {
                Attribute::Unsigned(m.interpreter)
            },
        ),
        (
            "python.thread_instance".into(),
            Attribute::Unsigned(m.thread),
        ),
        ("python.symbol".into(), Attribute::Unsigned(m.symbol)),
        ("python.file".into(), Attribute::Text(m.file.clone())),
        ("python.line".into(), Attribute::Unsigned(m.line as u64)),
        (
            "python.execution_kind".into(),
            Attribute::Text(m.execution_kind.clone()),
        ),
        (
            "python.boundary".into(),
            Attribute::Text(m.boundary.clone()),
        ),
        ("python.truncated".into(), Attribute::Bool(m.truncated)),
        ("pid".into(), Attribute::Unsigned(m.pid as u64)),
        ("tid".into(), Attribute::Unsigned(m.tid as u64)),
        ("process_instance".into(), Attribute::Unsigned(m.image)),
        ("python.attempted".into(), Attribute::Unsigned(m.attempted)),
        (
            "python.source_dropped".into(),
            Attribute::Unsigned(m.dropped),
        ),
        (
            "python.omitted_callbacks".into(),
            Attribute::Unsigned(m.omitted),
        ),
        ("python.unmatched".into(), Attribute::Unsigned(m.unmatched)),
        ("python.errors".into(), Attribute::Unsigned(m.errors)),
    ])
}
fn metadata(
    out: &mut CaptureSink,
    track: u64,
    at: u64,
    name: &str,
    a: BTreeMap<String, Attribute>,
) -> Result<()> {
    out.reliable(Event {
        timestamp_ns: at,
        attributes: a,
        payload: Payload::Counter(CounterSample {
            track_id: track,
            timestamp_ns: at,
            name: name.into(),
            value: CounterValue::Integer(0),
            unit: "metadata".into(),
        }),
    })
}
fn consume(
    receiver: mpsc::Receiver<Vec<u8>>,
    sink: &Mutex<CaptureSink>,
    origin: u64,
    queue_drops: &AtomicU64,
) -> Result<Outcome> {
    let mut sources = HashMap::<String, Source>::new();
    let mut malformed = 0u64;
    let mut received = 0u64;
    let mut streams_seen = 0usize;
    let mut known_loss = 0u64;
    for bytes in receiver {
        let m: Message = match serde_json::from_slice(&bytes) {
            Ok(m) if bytes.len() <= MAX_PACKET => m,
            _ => {
                malformed += 1;
                continue;
            }
        };
        if m.v != 1
            || m.source.len() != 32
            || !m.source.bytes().all(|b| b.is_ascii_hexdigit())
            || m.pid == 0
            || m.tid == 0
            || m.image == 0
            || m.seq == 0
            || m.thread == 0
            || m.at < origin
        {
            malformed += 1;
            continue;
        }
        if !matches!(
            m.kind.as_str(),
            "runtime" | "diagnostic" | "begin" | "end" | "range_begin" | "range_end" | "final"
        ) || m.name.len() > 1024
            || m.file.len() > 3072
            || m.detail.len() > 8192
        {
            malformed += 1;
            continue;
        }
        ensure!(
            sources.contains_key(&m.source) || sources.len() < MAX_SOURCES,
            "Python interpreter/source limit exceeded"
        );
        let next = sources.len() as u64 + 1;
        let source = sources.entry(m.source.clone()).or_insert_with(|| Source {
            namespace: next,
            pid: m.pid,
            image: m.image,
            interpreter: m.interpreter,
            ..Default::default()
        });
        ensure!(
            (source.pid, source.image, source.interpreter) == (m.pid, m.image, m.interpreter),
            "Python source identity changed"
        );
        if !source.streams.contains_key(&m.thread) {
            ensure!(
                streams_seen < 65536,
                "Python thread-lifetime limit exceeded"
            );
            streams_seen += 1;
        }
        let stream = source.streams.entry(m.thread).or_insert((m.tid, 0, 0));
        if stream.0 != m.tid || m.seq <= stream.1 {
            malformed += 1;
            continue;
        }
        stream.1 = m.seq;
        stream.2 += 1;
        source.received += 1;
        received += 1;
        known_loss = known_loss
            .saturating_add(m.dropped.saturating_sub(source.dropped))
            .saturating_add(m.omitted.saturating_sub(source.omitted));
        source.attempted = source.attempted.max(m.attempted);
        source.dropped = source.dropped.max(m.dropped);
        source.omitted = source.omitted.max(m.omitted);
        source.unmatched = source.unmatched.max(m.unmatched);
        source.errors = source.errors.max(m.errors);
        let now = monotonic_ns()?;
        if m.at > now {
            malformed += 1;
            continue;
        }
        let at = now - origin;
        let mut out = sink
            .lock()
            .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
        out.set_ingress_loss(
            "python",
            known_loss
                .saturating_add(queue_drops.load(Ordering::Relaxed))
                .saturating_add(malformed),
        );
        let process = out.native_process(at, m.pid, m.image, None)?;
        let mut a = attributes(&m);
        match m.kind.as_str() {
            "runtime" => {
                source.initialized |= m.initialized;
                for (key, value) in [
                    ("python.backend", m.backend),
                    ("python.version", m.version),
                    ("python.implementation", m.implementation),
                    ("python.clock", m.clock),
                    ("python.detail", m.detail),
                ] {
                    a.insert(key.into(), Attribute::Text(value));
                }
                a.insert("python.role".into(), Attribute::Text("runtime".into()));
                a.insert("python.initialized".into(), Attribute::Bool(m.initialized));
                a.insert("python.ppid".into(), Attribute::Unsigned(m.ppid as u64));
                a.insert("python.forked".into(), Attribute::Bool(m.forked));
                metadata(&mut out, process, at, "python.meta.runtime", a)?;
            }
            "diagnostic" => {
                a.insert("python.role".into(), Attribute::Text("error".into()));
                a.insert("python.detail".into(), Attribute::Text(m.detail));
                metadata(&mut out, process, at, "python.meta.error", a)?;
            }
            "final" => {
                source.finalized = m.finalized;
                source.abandoned = m.abandoned;
                source.inflight = m.inflight;
                a.insert("python.role".into(), Attribute::Text("source".into()));
                a.insert(
                    "python.initialized".into(),
                    Attribute::Bool(source.initialized),
                );
                a.insert("python.finalized".into(), Attribute::Bool(m.finalized));
                a.insert("python.abandoned".into(), Attribute::Unsigned(m.abandoned));
                a.insert("python.inflight".into(), Attribute::Unsigned(m.inflight));
                a.insert(
                    "python.received".into(),
                    Attribute::Unsigned(source.received),
                );
                metadata(&mut out, process, at, "python.meta.final", a)?;
            }
            _ => {
                let range = m.kind.starts_with("range");
                a.insert(
                    "python.role".into(),
                    Attribute::Text(if range { "range" } else { "function" }.into()),
                );
                a.insert("python.name".into(), Attribute::Text(m.name.clone()));
                if m.start < origin || m.start > now || m.call == 0 || m.call >= (1u64 << 48) {
                    malformed += 1;
                    continue;
                }
                let track = out.native_thread(at, m.pid, m.image, m.tid, None)?;
                let correlation = out.correlation("python", source.namespace, m.call)?;
                if m.kind.ends_with("begin") {
                    out.emit(Event {
                        timestamp_ns: at,
                        attributes: a,
                        payload: Payload::ActivityStart {
                            track_id: track,
                            name: m.name,
                            correlation_id: correlation,
                            start_ns: Some(m.start - origin),
                        },
                    })?;
                    if m.parent > 0 && m.parent < (1u64 << 48) {
                        let parent = out.correlation("python", source.namespace, m.parent)?;
                        out.emit(Event::new(
                            at,
                            Payload::Dependency {
                                from_correlation_id: parent,
                                to_correlation_id: correlation,
                                relation: if range {
                                    "Python range begins in parent context"
                                } else {
                                    "Python call nesting"
                                }
                                .into(),
                            },
                        ))?;
                    }
                } else if m.end >= m.start && m.end <= now {
                    if !range {
                        a.insert(
                            "python.self_ns".into(),
                            Attribute::Unsigned(m.self_ns.min(m.end - m.start)),
                        );
                    }
                    out.emit(Event {
                        timestamp_ns: at,
                        attributes: a,
                        payload: Payload::Span(Span {
                            track_id: track,
                            name: m.name,
                            category: Category::Python,
                            start_ns: m.start - origin,
                            duration_ns: m.end - m.start,
                            correlation_id: Some(correlation),
                        }),
                    })?;
                } else {
                    malformed += 1;
                }
            }
        }
    }
    let mut out = sink
        .lock()
        .map_err(|_| anyhow::anyhow!("capture writer lock poisoned"))?;
    let at = monotonic_ns()?.saturating_sub(origin);
    let mut dropped = queue_drops
        .load(Ordering::Acquire)
        .saturating_add(malformed);
    let mut initialized = 0;
    let mut errors = 0;
    for (id, s) in sources {
        dropped = dropped.saturating_add(s.dropped).saturating_add(s.omitted);
        initialized += u64::from(s.initialized);
        errors += s.errors;
        let process = out.native_process(at, s.pid, s.image, None)?;
        let gaps = s.streams.values().fold(0u64, |sum, (_, seq, count)| {
            sum.saturating_add(seq.saturating_sub(*count))
        });
        let a = BTreeMap::from([
            ("python.role".into(), Attribute::Text("source".into())),
            ("python.source".into(), Attribute::Text(id)),
            ("pid".into(), Attribute::Unsigned(s.pid as u64)),
            ("process_instance".into(), Attribute::Unsigned(s.image)),
            (
                "python.interpreter".into(),
                if s.interpreter == u64::MAX {
                    Attribute::Text("unknown".into())
                } else {
                    Attribute::Unsigned(s.interpreter)
                },
            ),
            ("python.initialized".into(), Attribute::Bool(s.initialized)),
            ("python.finalized".into(), Attribute::Bool(s.finalized)),
            ("python.attempted".into(), Attribute::Unsigned(s.attempted)),
            ("python.received".into(), Attribute::Unsigned(s.received)),
            (
                "python.source_dropped".into(),
                Attribute::Unsigned(s.dropped),
            ),
            (
                "python.omitted_callbacks".into(),
                Attribute::Unsigned(s.omitted),
            ),
            ("python.sequence_gaps".into(), Attribute::Unsigned(gaps)),
            ("python.unmatched".into(), Attribute::Unsigned(s.unmatched)),
            ("python.errors".into(), Attribute::Unsigned(s.errors)),
            ("python.abandoned".into(), Attribute::Unsigned(s.abandoned)),
            ("python.inflight".into(), Attribute::Unsigned(s.inflight)),
        ]);
        metadata(&mut out, process, at, "python.meta.source", a)?;
    }
    for (name, value) in [
        (
            "python.receiver_dropped",
            queue_drops.load(Ordering::Acquire),
        ),
        ("python.malformed", malformed),
    ] {
        let mut e = Event::new(
            at,
            Payload::Counter(CounterSample {
                track_id: 0,
                timestamp_ns: at,
                name: name.into(),
                value: CounterValue::Integer(value),
                unit: "records".into(),
            }),
        );
        e.attributes
            .insert("python.role".into(), Attribute::Text("diagnostic".into()));
        out.reliable(e)?;
    }
    out.set_ingress_loss("python", dropped);
    Ok(Outcome {
        initialized,
        errors,
        received,
        dropped,
    })
}
