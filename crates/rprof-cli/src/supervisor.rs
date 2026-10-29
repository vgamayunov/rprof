//! Linux workload supervision. Each workload gets its own process group and
//! inherited stdio. When stdin is the foreground terminal, foreground ownership
//! is handed to the workload before it is allowed to run, then restored on exit.
//!
//! The subreaper adopts orphaned grandchildren so that members of the original
//! process group can be reaped even after the leader exits. A descendant that
//! deliberately calls `setsid`/`setpgid` to escape that group cannot be reliably
//! identified after reparenting; such daemonized descendants are not supervised.
//! Only one supervisor may be active per process; background capture writers are
//! supported. Process-wide signal and subreaper state are owned for this lifetime.

#![cfg(target_os = "linux")]

use std::{
    ffi::OsString,
    io,
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, ExitStatus},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use signal_hook::{
    consts::signal::{SIGHUP, SIGINT, SIGQUIT, SIGTERM},
    iterator::Signals,
};

#[derive(Clone, Debug)]
pub struct Config {
    pub grace: Duration,
    pub poll_interval: Duration,
    pub environment: Vec<(OsString, OsString)>,
    pub heartbeat_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(2),
            poll_interval: Duration::from_millis(10),
            environment: Vec::new(),
            heartbeat_interval: Duration::from_millis(100),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Started { pid: u32 },
    Heartbeat { elapsed_ns: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub interrupted_by: Option<i32>,
    pub duration_ns: u64,
}

fn elapsed_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

fn group_signal(pgid: libc::pid_t, signal: i32) -> io::Result<()> {
    // The supervisor is never a member of this process group.
    if unsafe { libc::kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

fn group_exists(pgid: libc::pid_t) -> io::Result<bool> {
    if unsafe { libc::kill(-pgid, 0) } == 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ESRCH) => Ok(false),
        Some(libc::EPERM) => Ok(true),
        _ => Err(error),
    }
}

fn reap_group(
    pgid: libc::pid_t,
    child: &mut Child,
    root_status: &mut Option<ExitStatus>,
) -> io::Result<()> {
    // Never use waitpid(-1): other CLI children may belong to another owner.
    loop {
        // Peek first: a generic waitpid(-pgid) could consume the leader's
        // status before std::process::Child can collect it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::waitid(
                libc::P_PGID,
                pgid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result != 0 {
            match io::Error::last_os_error().raw_os_error() {
                Some(libc::ECHILD) => return Ok(()),
                Some(libc::EINTR) => continue,
                _ => return Err(io::Error::last_os_error()),
            }
        }
        let pid = unsafe { info.si_pid() };
        if pid == 0 {
            return Ok(());
        }
        if pid == child.id() as libc::pid_t {
            *root_status = child.try_wait()?;
        } else {
            let mut status = 0;
            let result = unsafe { libc::waitpid(pid, &mut status, 0) };
            if result < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ECHILD) {
                return Err(io::Error::last_os_error());
            }
        }
    }
}

struct Subreaper {
    previous: libc::c_int,
}

impl Subreaper {
    fn new() -> Result<Self> {
        let mut previous = 0;
        ensure!(
            unsafe { libc::prctl(libc::PR_GET_CHILD_SUBREAPER, &mut previous) } == 0,
            "reading child subreaper setting: {}",
            io::Error::last_os_error()
        );
        if previous == 0 {
            ensure!(
                unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) } == 0,
                "enabling child subreaper: {}",
                io::Error::last_os_error()
            );
        }
        Ok(Self { previous })
    }
}

impl Drop for Subreaper {
    fn drop(&mut self) {
        if self.previous == 0 {
            unsafe {
                libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 0);
            }
        }
    }
}

// tcsetpgrp from a background process raises SIGTTOU even when TOSTOP is off.
// Block it for the duration of the terminal transfer (including restoration).
fn set_foreground(fd: i32, pgid: libc::pid_t) -> io::Result<()> {
    unsafe {
        let mut blocked: libc::sigset_t = std::mem::zeroed();
        let mut previous: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut blocked);
        libc::sigaddset(&mut blocked, libc::SIGTTOU);
        if libc::sigprocmask(libc::SIG_BLOCK, &blocked, &mut previous) != 0 {
            return Err(io::Error::last_os_error());
        }
        let result = libc::tcsetpgrp(fd, pgid);
        let error = io::Error::last_os_error();
        libc::sigprocmask(libc::SIG_SETMASK, &previous, std::ptr::null_mut());
        if result == 0 { Ok(()) } else { Err(error) }
    }
}

struct Foreground {
    fd: i32,
    original: libc::pid_t,
}

impl Foreground {
    fn current() -> io::Result<Option<Self>> {
        let fd = io::stdin().as_raw_fd();
        let original = unsafe { libc::tcgetpgrp(fd) };
        if original < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::ENOTTY) {
                return Ok(None);
            }
            return Err(io::Error::last_os_error());
        }
        // Do not steal a terminal if the CLI itself is a background job.
        Ok((original == unsafe { libc::getpgrp() }).then_some(Self { fd, original }))
    }
}

impl Drop for Foreground {
    fn drop(&mut self) {
        let _ = set_foreground(self.fd, self.original);
    }
}

struct Workload {
    child: Child,
    pgid: libc::pid_t,
    // Must stay alive until workload cleanup has finished.
    foreground: Option<Foreground>,
    root_status: Option<ExitStatus>,
    group_gone: bool,
}

impl Workload {
    fn update(&mut self) -> Result<bool> {
        if self.root_status.is_none() {
            self.root_status = self
                .child
                .try_wait()
                .context("waiting for workload leader")?;
        }
        reap_group(self.pgid, &mut self.child, &mut self.root_status)
            .context("reaping workload descendants")?;
        if !self.group_gone {
            self.group_gone =
                !group_exists(self.pgid).context("checking workload process group")?;
        }
        Ok(self.root_status.is_some() && self.group_gone)
    }

    fn send(&self, signal: i32) -> Result<()> {
        if !self.group_gone {
            group_signal(self.pgid, signal).context("signaling workload process group")?;
        }
        Ok(())
    }

    fn cleanup(&mut self, grace: Duration, poll: Duration) {
        if self.update().unwrap_or(false) {
            return;
        }
        let _ = self.send(SIGTERM);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self.update().unwrap_or(false) {
                return;
            }
            std::thread::sleep(poll);
        }
        let _ = self.send(libc::SIGKILL);
        // Reap the leader and all group descendants before allowing its PGID
        // to be reused. An escaped daemon is outside the documented scope.
        loop {
            if self.update().unwrap_or(false) {
                return;
            }
            std::thread::sleep(poll);
        }
    }
}

impl Drop for Workload {
    fn drop(&mut self) {
        if !self.group_gone || self.root_status.is_none() {
            self.cleanup(Duration::from_millis(100), Duration::from_millis(10));
        }
        // Keep terminal ownership until all group members have stopped.
        let _ = self.foreground.take();
    }
}

pub fn supervise(
    command: &[OsString],
    config: Config,
    mut on_event: impl FnMut(Event) -> Result<()>,
) -> Result<Outcome> {
    ensure!(!command.is_empty(), "a workload command is required");
    let poll = config
        .poll_interval
        .clamp(Duration::from_millis(1), Duration::from_millis(100));
    let _subreaper = Subreaper::new()?;
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP, SIGQUIT])
        .context("installing supervisor signal handlers")?;
    let foreground = Foreground::current().context("checking foreground terminal")?;

    let mut process = Command::new(&command[0]);
    process.args(&command[1..]);
    process.envs(config.environment);
    let tty = foreground.as_ref().map(|value| value.fd);
    // SAFETY: setpgid and the foreground handoff use only async-signal-safe
    // syscalls. Transferring in the child before exec avoids a stdin/SIGTTIN
    // race without blocking Command::spawn's exec-error handshake.
    unsafe {
        process.pre_exec(move || {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            if let Some(fd) = tty {
                set_foreground(fd, libc::getpgrp())?;
            }
            Ok(())
        });
    }
    let start = Instant::now();
    let child = process
        .spawn()
        .with_context(|| format!("launching {:?}", command[0]))?;
    let pgid = child.id() as libc::pid_t;
    let mut workload = Workload {
        child,
        pgid,
        foreground,
        root_status: None,
        group_gone: false,
    };
    on_event(Event::Started {
        pid: workload.child.id(),
    })?;

    let mut interrupted_by = None;
    let mut deadline = None;
    let mut escalated = false;
    let mut next_heartbeat = start + config.heartbeat_interval;
    loop {
        // Establish whether the group is already gone before considering new
        // signals; never send a signal after all its members have been reaped.
        if workload.update()? {
            break;
        }
        for signal in signals.pending() {
            if interrupted_by.is_none() {
                interrupted_by = Some(signal);
                workload.send(signal)?;
                deadline = Some(Instant::now() + config.grace);
            }
        }
        if workload.root_status.is_some() && deadline.is_none() && !escalated {
            // Root exit does not imply its background children have exited.
            workload.send(SIGTERM)?;
            deadline = Some(Instant::now() + config.grace);
        }
        if deadline.is_some_and(|limit| Instant::now() >= limit) {
            workload.send(libc::SIGKILL)?;
            deadline = None;
            escalated = true;
        }
        if Instant::now() >= next_heartbeat {
            on_event(Event::Heartbeat {
                elapsed_ns: elapsed_ns(start),
            })?;
            next_heartbeat = Instant::now() + config.heartbeat_interval;
        }
        std::thread::sleep(poll);
    }
    let status = workload
        .root_status
        .as_ref()
        .context("workload exit status missing")?;
    use std::os::unix::process::ExitStatusExt;
    Ok(Outcome {
        exit_code: status.code(),
        signal: status.signal(),
        interrupted_by,
        duration_ns: elapsed_ns(start),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, sync::Mutex};

    // PR_SET_CHILD_SUBREAPER is process-wide; serialize tests that change it.
    static LOCK: Mutex<()> = Mutex::new(());

    fn shell(script: &str) -> Vec<OsString> {
        ["sh", "-c", script]
            .into_iter()
            .map(OsString::from)
            .collect()
    }

    #[test]
    fn reports_exit_status_and_events() {
        let _lock = LOCK.lock().unwrap();
        let mut events = Vec::new();
        let result = supervise(&shell("sleep .15; exit 23"), Config::default(), |event| {
            events.push(event);
            Ok(())
        })
        .unwrap();
        assert_eq!(result.exit_code, Some(23));
        assert_eq!(result.signal, None);
        assert_eq!(result.interrupted_by, None);
        assert!(result.duration_ns >= 100_000_000);
        assert!(matches!(events.first(), Some(Event::Started { .. })));
        assert!(events.iter().any(|e| matches!(e, Event::Heartbeat { .. })));
    }

    #[test]
    fn root_exit_terminates_and_reaps_background_group_members() {
        let _lock = LOCK.lock().unwrap();
        let path =
            std::env::temp_dir().join(format!("rprof-supervisor-test-{}", std::process::id()));
        let script = format!(
            "(trap '' TERM; while :; do sleep 1; done) & echo $! > '{}'; exit 7",
            path.display()
        );
        let result = supervise(
            &shell(&script),
            Config {
                grace: Duration::from_millis(100),
                ..Config::default()
            },
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(result.exit_code, Some(7));
        let pid: libc::pid_t = fs::read_to_string(&path).unwrap().trim().parse().unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn callback_failure_cleans_up_workload_without_reaping_other_children() {
        let _lock = LOCK.lock().unwrap();
        let mut unrelated = Command::new("sh")
            .args(["-c", "sleep .2; exit 9"])
            .spawn()
            .unwrap();
        let mut leader = None;
        let error = supervise(&shell("sleep 30"), Config::default(), |event| {
            if let Event::Started { pid } = event {
                leader = Some(pid);
            }
            anyhow::bail!("write failed")
        })
        .unwrap_err();
        assert!(error.to_string().contains("write failed"));
        let pid = leader.unwrap() as libc::pid_t;
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(unrelated.wait().unwrap().code(), Some(9));
    }

    #[test]
    fn forwards_external_signal_and_records_interrupt() {
        let _lock = LOCK.lock().unwrap();
        let supervisor_thread = unsafe { libc::pthread_self() };
        let mut sender = None;
        let result = supervise(
            &shell("trap 'exit 44' TERM; while :; do sleep .05; done"),
            Config::default(),
            |event| {
                if matches!(event, Event::Started { .. }) {
                    sender = Some(std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(60));
                        assert_eq!(unsafe { libc::pthread_kill(supervisor_thread, SIGTERM) }, 0);
                    }));
                }
                Ok(())
            },
        )
        .unwrap();
        sender.unwrap().join().unwrap();
        assert_eq!(result.interrupted_by, Some(SIGTERM));
        assert_eq!(result.exit_code, Some(44));
    }
}
