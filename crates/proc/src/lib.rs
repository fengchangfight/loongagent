//! Running a child process safely: bounded output, a hard timeout, and teardown
//! of the **whole process tree**.
//!
//! Both the shell tool and the shell hook kind need exactly this, and it is
//! security-relevant plumbing, so it lives in one place. Two details are
//! load-bearing:
//!
//! * Output goes to **scratch files**, not pipes. A timeout cannot deadlock a
//!   reader when nobody is reading, and a grandchild that outlives its parent
//!   cannot hold a reader open.
//! * The child leads its own **process group**, so a timeout kills the tree —
//!   killing only `sh` would leave `sleep 30` running.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// Stdin payloads are written before the first poll and must stay well under the
/// 64 KiB pipe buffer, or a child that never reads stdin would block us.
pub const STDIN_LIMIT: usize = 32 * 1024;

pub struct RunSpec<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: &'a Path,
    pub stdin: Option<&'a [u8]>,
    pub timeout: Duration,
    /// Per stream.
    pub output_limit: usize,
    /// Installed in the forked child, before `exec` (the PEP's seam).
    pub confine: Option<&'a dyn Fn(&mut Command)>,
}

impl<'a> RunSpec<'a> {
    pub fn new(program: &'a str, args: &'a [String], cwd: &'a Path, timeout: Duration) -> Self {
        Self { program, args, cwd, stdin: None, timeout, output_limit: 64 * 1024, confine: None }
    }
    pub fn stdin(mut self, bytes: &'a [u8]) -> Self {
        self.stdin = Some(bytes);
        self
    }
    pub fn output_limit(mut self, limit: usize) -> Self {
        self.output_limit = limit;
        self
    }
    pub fn confine(mut self, confine: &'a dyn Fn(&mut Command)) -> Self {
        self.confine = Some(confine);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput {
    /// `None` when the timeout fired and the tree was killed.
    pub status: Option<ExitStatus>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

impl RunOutput {
    pub fn success(&self) -> bool {
        matches!(self.status, Some(s) if s.success())
    }
    pub fn code(&self) -> Option<i32> {
        self.status.and_then(|s| s.code())
    }
    pub fn describe_exit(&self) -> String {
        match self.status {
            Some(s) => match s.code() {
                Some(code) => format!("exit {code}"),
                None => "killed by a signal".to_string(),
            },
            None => "timed out".to_string(),
        }
    }
}

#[derive(Debug)]
pub struct ProcError(pub String);

impl std::fmt::Display for ProcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ProcError {}

pub fn run(spec: &RunSpec<'_>) -> Result<RunOutput, ProcError> {
    if let Some(bytes) = spec.stdin {
        if bytes.len() > STDIN_LIMIT {
            return Err(ProcError(format!(
                "stdin payload is {} bytes, over the {} byte limit",
                bytes.len(),
                STDIN_LIMIT
            )));
        }
    }
    let scratch = Scratch::new()?;
    let stdout_file = scratch.file("stdout")?;
    let stderr_file = scratch.file("stderr")?;

    let mut cmd = Command::new(spec.program);
    cmd.args(spec.args)
        .current_dir(spec.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file));
    prepare_process_group(&mut cmd);
    if let Some(confine) = spec.confine {
        confine(&mut cmd);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| ProcError(format!("spawning {}: {e}", spec.program)))?;
    if let Some(bytes) = spec.stdin {
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(bytes).map_err(|e| ProcError(format!("writing stdin: {e}")))?;
            stdin.flush().ok();
            // Dropped here: the child sees EOF.
        }
    }

    let (status, timed_out) = match wait_with_timeout(&mut child, spec.timeout) {
        Ok(status) => (Some(status), false),
        Err(TimedOut) => {
            kill_process_group(&mut child);
            (None, true)
        }
    };

    let (stdout, stdout_truncated) = read_bounded(&scratch.path("stdout"), spec.output_limit)?;
    let (stderr, stderr_truncated) = read_bounded(&scratch.path("stderr"), spec.output_limit)?;
    Ok(RunOutput { status, timed_out, stdout, stderr, stdout_truncated, stderr_truncated })
}

/// Two scratch files plus their directory, removed on drop.
struct Scratch {
    dir: PathBuf,
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

impl Scratch {
    fn new() -> Result<Self, ProcError> {
        let dir = std::env::temp_dir().join(format!(
            "loong-proc-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir)
            .map_err(|e| ProcError(format!("{}: {e}", dir.display())))?;
        Ok(Self { dir })
    }
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
    fn file(&self, name: &str) -> Result<std::fs::File, ProcError> {
        std::fs::File::create(self.path(name))
            .map_err(|e| ProcError(format!("creating scratch output: {e}")))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct TimedOut;

fn wait_with_timeout(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<ExitStatus, TimedOut> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(_) => {}
        }
        if Instant::now() >= deadline {
            return Err(TimedOut);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn read_bounded(path: &Path, limit: usize) -> Result<(String, bool), ProcError> {
    let file = std::fs::File::open(path)
        .map_err(|e| ProcError(format!("{}: {e}", path.display())))?;
    let mut reader = std::io::BufReader::new(file);
    let mut collected = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if collected.len() < limit {
                    let room = limit - collected.len();
                    collected.extend_from_slice(&buf[..n.min(room)]);
                    if n > room {
                        truncated = true;
                    }
                } else {
                    truncated = true;
                }
            }
            Err(_) => break,
        }
    }
    if !truncated {
        truncated = std::fs::metadata(path).map(|m| m.len() as usize > limit).unwrap_or(false);
    }
    Ok((String::from_utf8_lossy(&collected).to_string(), truncated))
}

#[cfg(unix)]
fn prepare_process_group(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
}

#[cfg(not(unix))]
fn prepare_process_group(_cmd: &mut Command) {}

/// Kill the child **and everything it spawned**.
#[cfg(unix)]
fn kill_process_group(child: &mut std::process::Child) {
    extern "C" {
        fn syscall(num: std::ffi::c_long, ...) -> std::ffi::c_long;
    }
    const SYS_KILL: std::ffi::c_long = 62; // x86_64 and aarch64
    const SIGKILL: i32 = 9;
    let pid = child.id() as i64;
    unsafe {
        syscall(SYS_KILL, -pid, SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
fn kill_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);
    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "loong-proc-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn sh(script: &str) -> Vec<String> {
        vec!["-c".to_string(), script.to_string()]
    }

    #[test]
    fn output_and_exit_status_come_back() {
        let cwd = tempdir();
        let args = sh("echo out; echo err >&2; exit 4");
        let out = run(&RunSpec::new("sh", &args, &cwd, Duration::from_secs(10))).unwrap();
        assert_eq!(out.stdout.trim(), "out");
        assert_eq!(out.stderr.trim(), "err");
        assert_eq!(out.code(), Some(4));
        assert!(!out.success());
        assert!(!out.timed_out);
    }

    #[test]
    fn stdin_reaches_the_child() {
        let cwd = tempdir();
        let args = sh("cat");
        let spec = RunSpec::new("sh", &args, &cwd, Duration::from_secs(10))
            .stdin(b"{\"event\":\"pre_tool\"}");
        let out = run(&spec).unwrap();
        assert_eq!(out.stdout, "{\"event\":\"pre_tool\"}");
    }

    #[test]
    fn output_is_bounded_per_stream() {
        let cwd = tempdir();
        let args = sh("yes aaaaaaaaaaaaaaaaaaaa | head -n 2000");
        let spec = RunSpec::new("sh", &args, &cwd, Duration::from_secs(20)).output_limit(1024);
        let out = run(&spec).unwrap();
        assert!(out.stdout_truncated);
        assert!(out.stdout.len() <= 1024);
    }

    #[test]
    fn a_timeout_kills_the_whole_tree() {
        let cwd = tempdir();
        // The `sleep` is a grandchild: killing only `sh` would leave it running,
        // and this pipe marker proves whether the tree really died.
        let started = Instant::now();
        let args = sh("sleep 5 & echo $! > grandchild.pid; wait");
        let out = run(&RunSpec::new("sh", &args, &cwd, Duration::from_millis(250))).unwrap();
        assert!(out.timed_out, "the timeout fires");
        assert!(started.elapsed() < Duration::from_secs(3), "and it fires quickly");

        let pid_file = cwd.join("grandchild.pid");
        if let Ok(pid) = std::fs::read_to_string(&pid_file).unwrap_or_default().trim().parse::<i32>() {
            std::thread::sleep(Duration::from_millis(200));
            let alive = Path::new(&format!("/proc/{pid}")).exists();
            assert!(!alive, "the grandchild must be dead too");
        }
    }

    #[test]
    fn an_oversized_stdin_payload_is_refused_before_spawning() {
        let cwd = tempdir();
        let big = vec![b'x'; STDIN_LIMIT + 1];
        let args = sh("cat");
        let spec = RunSpec::new("sh", &args, &cwd, Duration::from_secs(5)).stdin(&big);
        assert!(run(&spec).unwrap_err().0.contains("over the"));
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let cwd = tempdir();
        let spec = RunSpec::new("definitely-not-a-program", &[], &cwd, Duration::from_secs(5));
        assert!(run(&spec).unwrap_err().0.contains("spawning"));
    }
}
