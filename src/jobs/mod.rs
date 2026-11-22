//! The job engine: runs one shell command in many folders, a few at a time, with live output,
//! a status per folder, timeouts and cancelling.
//!
//! A [`Run`] is shared between the backend (which fills it in) and the UI (which reads it each
//! frame and can cancel it directly).

pub mod output;

use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::Semaphore;
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;

use output::{MAX_LINE_BYTES, OutputBuffer, Stream, clean_line};

/// How long a process group gets to exit after SIGTERM before SIGKILL.
const TERM_GRACE: Duration = Duration::from_secs(3);
/// How long to keep reading after the shell exits; background children may hold the pipes open.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

#[derive(Clone, Debug)]
pub struct Target {
    /// `owner/name`, or the folder name.
    pub name: String,
    pub path: PathBuf,
    pub tags: Vec<String>,
    /// Extra environment variables for this target only.
    pub env: Vec<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct RunSpec {
    /// What the run is called in the UI, e.g. the command itself or "Clone 5 repos".
    pub title: String,
    pub command: String,
    pub shell: String,
    pub targets: Vec<Target>,
    /// How many run at once.
    pub parallel: usize,
    pub timeout: Option<Duration>,
    /// Lines kept per target.
    pub line_limit: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetStatus {
    Queued,
    Running {
        since: Instant,
    },
    Exited {
        code: i32,
        took: Duration,
    },
    /// Ended by a signal that didn't come from this app.
    Killed {
        signal: i32,
        took: Duration,
    },
    TimedOut {
        took: Duration,
    },
    /// `took` is `None` when it was cancelled before it started.
    Cancelled {
        took: Option<Duration>,
    },
    /// The reason is in the output.
    FailedToStart,
}

impl TargetStatus {
    pub fn is_finished(self) -> bool {
        !matches!(self, TargetStatus::Queued | TargetStatus::Running { .. })
    }

    pub fn failed(self) -> bool {
        match self {
            TargetStatus::Exited { code, .. } => code != 0,
            TargetStatus::Killed { .. } | TargetStatus::TimedOut { .. } => true,
            TargetStatus::FailedToStart => true,
            _ => false,
        }
    }

    /// How long it ran (so far, if still running).
    pub fn took(self) -> Option<Duration> {
        match self {
            TargetStatus::Running { since } => Some(since.elapsed()),
            TargetStatus::Exited { took, .. }
            | TargetStatus::Killed { took, .. }
            | TargetStatus::TimedOut { took } => Some(took),
            TargetStatus::Cancelled { took } => took,
            TargetStatus::Queued | TargetStatus::FailedToStart => None,
        }
    }
}

pub struct Slot {
    pub status: TargetStatus,
    pub output: OutputBuffer,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub queued: usize,
    pub running: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub cancelled: usize,
}

impl Summary {
    pub fn is_finished(&self) -> bool {
        self.queued == 0 && self.running == 0
    }
}

pub struct Run {
    pub id: u64,
    pub title: String,
    pub command: String,
    pub shell: String,
    pub targets: Vec<Target>,
    pub parallel: usize,
    pub timeout: Option<Duration>,
    pub started: Instant,
    finished: Mutex<Option<Instant>>,
    cancel: CancellationToken,
    target_cancels: Vec<CancellationToken>,
    slots: Vec<Mutex<Slot>>,
}

impl Run {
    /// Locks one target's status and output. Keep the guard short-lived.
    pub fn slot(&self, index: usize) -> MutexGuard<'_, Slot> {
        lock(&self.slots[index])
    }

    pub fn status(&self, index: usize) -> TargetStatus {
        self.slot(index).status
    }

    /// Cancels everything still queued or running.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    pub fn cancel_target(&self, index: usize) {
        self.target_cancels[index].cancel();
    }

    pub fn summary(&self) -> Summary {
        let mut summary = Summary::default();
        for index in 0..self.targets.len() {
            match self.status(index) {
                TargetStatus::Queued => summary.queued += 1,
                TargetStatus::Running { .. } => summary.running += 1,
                TargetStatus::Cancelled { .. } => summary.cancelled += 1,
                status if status.failed() => summary.failed += 1,
                _ => summary.succeeded += 1,
            }
        }
        summary
    }

    pub fn is_finished(&self) -> bool {
        lock(&self.finished).is_some()
    }

    /// Time from start until every target finished, or until now.
    pub fn elapsed(&self) -> Duration {
        match *lock(&self.finished) {
            Some(end) => end - self.started,
            None => self.started.elapsed(),
        }
    }
}

/// Called whenever a run's state or output changes (from background threads).
pub type OnChange = Arc<dyn Fn() + Send + Sync>;

/// Starts a run. Must be called within a tokio runtime.
/// The returned handle completes once every target has finished.
pub fn start(id: u64, spec: RunSpec, on_change: OnChange) -> (Arc<Run>, JoinHandle<()>) {
    let cancel = CancellationToken::new();
    let run = Arc::new(Run {
        id,
        target_cancels: spec.targets.iter().map(|_| cancel.child_token()).collect(),
        slots: spec
            .targets
            .iter()
            .map(|_| {
                Mutex::new(Slot {
                    status: TargetStatus::Queued,
                    output: OutputBuffer::new(spec.line_limit),
                })
            })
            .collect(),
        title: spec.title,
        command: spec.command,
        shell: spec.shell,
        targets: spec.targets,
        parallel: spec.parallel.max(1),
        timeout: spec.timeout,
        started: Instant::now(),
        finished: Mutex::new(None),
        cancel,
    });

    let handle = tokio::spawn({
        let run = run.clone();
        async move {
            let limit = Arc::new(Semaphore::new(run.parallel));
            let mut tasks = JoinSet::new();
            for index in 0..run.targets.len() {
                tasks.spawn(run_target(
                    run.clone(),
                    index,
                    limit.clone(),
                    on_change.clone(),
                ));
            }
            while let Some(joined) = tasks.join_next().await {
                if let Err(e) = joined {
                    tracing::error!("a run task crashed: {e}");
                }
            }
            *lock(&run.finished) = Some(Instant::now());
            on_change();
        }
    });
    (run, handle)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

enum Stop {
    Cancelled,
    TimedOut,
}

async fn run_target(run: Arc<Run>, index: usize, limit: Arc<Semaphore>, on_change: OnChange) {
    let token = run.target_cancels[index].clone();
    let permit = tokio::select! {
        biased;
        _ = token.cancelled() => None,
        permit = limit.acquire_owned() => permit.ok(),
    };
    let Some(_permit) = permit else {
        run.slot(index).status = TargetStatus::Cancelled { took: None };
        on_change();
        return;
    };

    let since = Instant::now();
    run.slot(index).status = TargetStatus::Running { since };
    on_change();

    let mut child = match spawn(&run.shell, &run.command, &run.targets[index]) {
        Ok(child) => child,
        Err(e) => {
            let mut slot = run.slot(index);
            slot.output
                .push(Stream::Stderr, format!("couldn't start {}: {e}", run.shell));
            slot.status = TargetStatus::FailedToStart;
            drop(slot);
            on_change();
            return;
        }
    };
    let pid = child.id();
    let readers = [
        child.stdout.take().map(|out| {
            tokio::spawn(pump(
                out,
                Stream::Stdout,
                run.clone(),
                index,
                on_change.clone(),
            ))
        }),
        child.stderr.take().map(|err| {
            tokio::spawn(pump(
                err,
                Stream::Stderr,
                run.clone(),
                index,
                on_change.clone(),
            ))
        }),
    ];

    let timeout = async {
        match run.timeout {
            Some(limit) => tokio::time::sleep(limit).await,
            None => std::future::pending().await,
        }
    };
    let ended = tokio::select! {
        status = child.wait() => Ok(status),
        _ = token.cancelled() => Err(Stop::Cancelled),
        _ = timeout => Err(Stop::TimedOut),
    };
    if ended.is_err() {
        terminate(&mut child, pid).await;
    }

    for reader in readers.into_iter().flatten() {
        let abort = reader.abort_handle();
        if tokio::time::timeout(DRAIN_GRACE, reader).await.is_err() {
            abort.abort();
        }
    }

    let took = since.elapsed();
    let status = match ended {
        Ok(Ok(status)) => exit_status(status, took),
        Ok(Err(e)) => {
            run.slot(index)
                .output
                .push(Stream::Stderr, format!("lost track of the process: {e}"));
            TargetStatus::FailedToStart
        }
        Err(Stop::Cancelled) => TargetStatus::Cancelled { took: Some(took) },
        Err(Stop::TimedOut) => TargetStatus::TimedOut { took },
    };
    run.slot(index).status = status;
    on_change();
}

fn spawn(shell: &str, command: &str, target: &Target) -> std::io::Result<Child> {
    let mut command_builder = Command::new(shell);
    command_builder
        .arg("-c")
        .arg(command)
        .current_dir(&target.path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("DEPO_REPO", &target.name)
        .env("DEPO_PATH", &target.path)
        .env("DEPO_TAGS", target.tags.join(","))
        .envs(target.env.iter().map(|(k, v)| (k, v)))
        .kill_on_drop(true);
    // Its own session (and so its own process group): cancelling reaches everything it
    // started, and nothing can read from the terminal the app was started in. Without a
    // terminal, ssh asks for passphrases through SSH_ASKPASS instead.
    in_new_session(&mut command_builder);
    command_builder.spawn()
}

/// Makes the process start its own session (and process group), with no controlling terminal.
pub fn in_new_session(command: &mut Command) {
    // SAFETY: setsid is async-signal-safe and touches no memory of the parent.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid().map_err(std::io::Error::from)?;
            Ok(())
        });
    }
}

/// SIGTERM to the whole process group, then SIGKILL if it hasn't exited in time.
async fn terminate(child: &mut Child, pid: Option<u32>) {
    let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) else {
        return;
    };
    let group = Pid::from_raw(pid);
    let _ = killpg(group, Signal::SIGTERM);
    if tokio::time::timeout(TERM_GRACE, child.wait())
        .await
        .is_err()
    {
        let _ = killpg(group, Signal::SIGKILL);
        let _ = child.wait().await;
    }
    // Anything left in the group ignored SIGTERM and outlived the shell.
    let _ = killpg(group, Signal::SIGKILL);
}

fn exit_status(status: ExitStatus, took: Duration) -> TargetStatus {
    use std::os::unix::process::ExitStatusExt;
    match status.code() {
        Some(code) => TargetStatus::Exited { code, took },
        None => TargetStatus::Killed {
            signal: status.signal().unwrap_or(0),
            took,
        },
    }
}

/// Reads a pipe into the target's output, a line at a time.
async fn pump(
    mut reader: impl AsyncRead + Unpin,
    stream: Stream,
    run: Arc<Run>,
    index: usize,
    on_change: OnChange,
) {
    let mut chunk = vec![0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        let read = match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let mut lines = Vec::new();
        for &byte in &chunk[..read] {
            if byte == b'\n' {
                lines.push(clean_line(&pending));
                pending.clear();
            } else {
                pending.push(byte);
                if pending.len() >= MAX_LINE_BYTES {
                    lines.push(clean_line(&pending));
                    pending.clear();
                }
            }
        }
        if !lines.is_empty() {
            let mut slot = run.slot(index);
            for line in lines {
                slot.output.push(stream, line);
            }
            drop(slot);
            on_change();
        }
    }
    if !pending.is_empty() {
        run.slot(index).output.push(stream, clean_line(&pending));
        on_change();
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    /// Temporary folders to run in, removed on drop.
    struct Folders {
        root: PathBuf,
        targets: Vec<Target>,
    }

    impl Folders {
        fn new(test: &str, names: &[&str]) -> Self {
            let root =
                std::env::temp_dir().join(format!("depository-jobs-{test}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            let targets = names
                .iter()
                .map(|name| {
                    let path = root.join(name.replace('/', "_"));
                    fs::create_dir_all(&path).unwrap();
                    Target {
                        name: (*name).to_owned(),
                        path,
                        tags: vec!["work".into(), "rust".into()],
                        env: vec![("DEPO_EXTRA".into(), format!("extra-{name}"))],
                    }
                })
                .collect();
            Self { root, targets }
        }
    }

    impl Drop for Folders {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn spec(command: &str, targets: &[Target]) -> RunSpec {
        RunSpec {
            title: command.into(),
            command: command.into(),
            shell: "/bin/sh".into(),
            targets: targets.to_vec(),
            parallel: 4,
            timeout: None,
            line_limit: 1000,
        }
    }

    fn lines(run: &Run, index: usize) -> Vec<(Stream, String)> {
        run.slot(index)
            .output
            .iter()
            .map(|l| (l.stream, l.text.clone()))
            .collect()
    }

    async fn finish(spec: RunSpec) -> Arc<Run> {
        let (run, handle) = start(1, spec, Arc::new(|| {}));
        tokio::time::timeout(Duration::from_secs(20), handle)
            .await
            .expect("run took too long")
            .unwrap();
        run
    }

    async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("timed out waiting for {what}");
    }

    #[tokio::test]
    async fn exit_codes_and_streams() {
        let folders = Folders::new("exit", &["a/one", "b/two"]);
        let run = finish(spec(
            r#"echo "out $DEPO_REPO"; echo err >&2; [ "$DEPO_REPO" = a/one ] && exit 3; true"#,
            &folders.targets,
        ))
        .await;

        assert!(matches!(
            run.status(0),
            TargetStatus::Exited { code: 3, .. }
        ));
        assert!(matches!(
            run.status(1),
            TargetStatus::Exited { code: 0, .. }
        ));
        let mut first = lines(&run, 0);
        first.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(
            first,
            [
                (Stream::Stderr, "err".to_owned()),
                (Stream::Stdout, "out a/one".to_owned())
            ]
        );
        let summary = run.summary();
        assert_eq!((summary.succeeded, summary.failed), (1, 1));
        assert!(summary.is_finished() && run.is_finished());
    }

    #[tokio::test]
    async fn environment_and_working_directory() {
        let folders = Folders::new("env", &["me/thing"]);
        let run = finish(spec(
            r#"echo "$DEPO_PATH|$DEPO_TAGS|$GIT_TERMINAL_PROMPT|$DEPO_EXTRA"; pwd -P; printf 'no newline'"#,
            &folders.targets,
        ))
        .await;
        let path = &folders.targets[0].path;
        let out: Vec<String> = lines(&run, 0).into_iter().map(|(_, l)| l).collect();
        assert_eq!(
            out[0],
            format!("{}|work,rust|0|extra-me/thing", path.display())
        );
        assert_eq!(Path::new(&out[1]), fs::canonicalize(path).unwrap());
        assert_eq!(out[2], "no newline");
    }

    #[tokio::test]
    async fn commands_run_in_their_own_session() {
        let folders = Folders::new("session", &["a"]);
        // The session ID equals the process's own PID only for a session leader.
        let run = finish(spec(
            r#"[ "$(ps -o sid= -p $$ | tr -d ' ')" = "$$" ] && echo leader"#,
            &folders.targets,
        ))
        .await;
        assert_eq!(lines(&run, 0), [(Stream::Stdout, "leader".to_owned())]);
    }

    #[tokio::test]
    async fn parallel_limit_is_respected() {
        let folders = Folders::new("parallel", &["a", "b", "c"]);
        let mut one_at_a_time = spec("sleep 0.2", &folders.targets);
        one_at_a_time.parallel = 1;
        let run = finish(one_at_a_time).await;
        assert!(
            run.elapsed() >= Duration::from_millis(580),
            "{:?}",
            run.elapsed()
        );
    }

    #[tokio::test]
    async fn timeout_stops_a_slow_command() {
        let folders = Folders::new("timeout", &["slow"]);
        let mut slow = spec("sleep 30", &folders.targets);
        slow.timeout = Some(Duration::from_millis(300));
        let run = finish(slow).await;
        assert!(matches!(run.status(0), TargetStatus::TimedOut { .. }));
        assert!(run.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn cancel_kills_the_whole_process_group() {
        let folders = Folders::new("cancel", &["x"]);
        let pid_file = folders.targets[0].path.join("pid");
        let (run, handle) = start(
            1,
            spec("sleep 30 & echo $! > pid; wait", &folders.targets),
            Arc::new(|| {}),
        );
        wait_until("the background sleep to start", || pid_file.exists()).await;
        let pid: u32 = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();

        run.cancel();
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            run.status(0),
            TargetStatus::Cancelled { took: Some(_) }
        ));
        // The grandchild must be gone too (allowing a moment for it to be reaped).
        wait_until("the background sleep to die", || {
            fs::read_to_string(format!("/proc/{pid}/stat"))
                .map_or(true, |stat| stat.contains(") Z "))
        })
        .await;
    }

    #[tokio::test]
    async fn cancelling_skips_queued_targets() {
        let folders = Folders::new("queued", &["a", "b"]);
        let mut serial = spec("sleep 30", &folders.targets);
        serial.parallel = 1;
        let (run, handle) = start(1, serial, Arc::new(|| {}));
        wait_until("the first to start", || {
            matches!(run.status(0), TargetStatus::Running { .. })
        })
        .await;
        run.cancel();
        handle.await.unwrap();
        assert!(matches!(
            run.status(0),
            TargetStatus::Cancelled { took: Some(_) }
        ));
        assert_eq!(run.status(1), TargetStatus::Cancelled { took: None });
        assert_eq!(run.summary().cancelled, 2);
    }

    #[tokio::test]
    async fn cancelling_one_target_leaves_the_rest() {
        let folders = Folders::new("one", &["a", "b"]);
        let (run, handle) = start(1, spec("sleep 30", &folders.targets), Arc::new(|| {}));
        wait_until("both to start", || {
            matches!(run.summary(), Summary { running: 2, .. })
        })
        .await;
        run.cancel_target(0);
        wait_until("the first to stop", || run.status(0).is_finished()).await;
        assert!(matches!(run.status(1), TargetStatus::Running { .. }));
        run.cancel();
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn output_is_bounded() {
        let folders = Folders::new("bounded", &["a"]);
        let mut many = spec(
            "seq 1 100; head -c 10000 /dev/zero | tr '\\0' a",
            &folders.targets,
        );
        many.line_limit = 10;
        let run = finish(many).await;
        let slot = run.slot(0);
        assert_eq!(slot.output.len(), 10);
        assert_eq!(slot.output.dropped(), 93);
        let texts: Vec<usize> = slot.output.iter().map(|l| l.text.len()).collect();
        // The last three lines are the 10,000-character line split into chunks.
        assert_eq!(texts[7..], [4096, 4096, 1808]);
        assert_eq!(slot.output.get(6).unwrap().text, "100");
    }

    #[tokio::test]
    async fn missing_shell_is_reported() {
        let folders = Folders::new("noshell", &["a"]);
        let mut bad = spec("true", &folders.targets);
        bad.shell = "/nonexistent/shell".into();
        let run = finish(bad).await;
        assert_eq!(run.status(0), TargetStatus::FailedToStart);
        assert!(run.status(0).failed());
        assert!(
            lines(&run, 0)[0]
                .1
                .starts_with("couldn't start /nonexistent/shell")
        );
    }
}
