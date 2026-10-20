//! Purpose: Fire hooks and aggregate their verdicts monotonically, so hooks can only tighten a turn.
//!
//! # Contract
//! - **Serves:** the kernel's turn through `Kernel::fire_hooks` and the tool-decision path.
//! - **Design:** `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §1–§3.
//! - **Upstream:** `crates/kernel/src/turn.rs` and `turn/hooks.rs`; `KernelMcp` implements `McpCaller`.
//! - **Downstream:** `loong_proc` for shell hooks, `loong_http` for HTTP hooks, `loong_schema::Verdict`.
//! - **Verify:** `cargo test -p loong-hooks` · `./scripts/check.sh`
//!
//! Firing hooks and aggregating what they say.
//!
//! Three properties are load-bearing:
//!
//! * **Monotonic aggregation** — deny > ask > allow, so adding a hook can only
//!   tighten what the session may do and never widen it (ADR-0008).
//! * **Fail closed by default** — a hook that cannot answer is a deny, unless its
//!   own config says otherwise. A guard that silently evaporates is worse than one
//!   that stops work, and the failure is recorded either way.
//! * **One contract across kinds** — a shell hook answers through its exit code or
//!   a JSON verdict on stdout; an HTTP hook answers with the same JSON. Both are
//!   ordinary code deciding, so a malformed answer falls back to `on_error` and is
//!   never treated as consent.

use crate::config::{HookEvent, HookKind, HookSpec, OnError};
use loong_proc::RunSpec;
use loong_schema::generated::Verdict;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// Calling a tool on an MCP server, without the hook engine knowing anything about
/// MCP: the kernel owns the servers and supplies this.
/// Purpose: Calling an MCP tool without the hook engine knowing anything about MCP.
/// Serves: `HookRunner::run_mcp`.
/// Design: `docs/design/extension-model.md` §1.
/// Up: the kernel's build step supplies a `KernelMcp`.
/// Down: `run_mcp` calls `call`.
/// Verify: `cargo test -p loong-hooks`.
pub trait McpCaller: Send + Sync {
    /// Purpose: Call a named tool on a named server and return its raw body.
    fn call(&self, server: &str, tool: &str, args: &Value) -> Result<String, String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: What one hook answered, and whether it answered at all.
/// Serves: `HookReport` and the kernel's `hook.fired` event.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `HookRunner` constructs it.
/// Down: the kernel's `hook.fired` recording.
/// Verify: `cargo test -p loong-hooks`.
pub struct HookOutcome {
    pub id: String,
    pub event: HookEvent,
    pub kind: &'static str,
    pub verdict: Verdict,
    pub reason: String,
    pub duration_ms: u64,
    /// The hook could not answer; the verdict came from `on_error`.
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: The aggregated verdict plus every hook's outcome.
/// Serves: the kernel's `turn/hooks.rs` and the event log.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `HookRunner::fire`.
/// Down: `crates/kernel/src/turn/hooks.rs`.
/// Verify: `cargo test -p loong-hooks`.
pub struct HookReport {
    pub verdict: Verdict,
    pub outcomes: Vec<HookOutcome>,
}

impl HookReport {
    /// Purpose: The empty report: allow, with no hooks run.
    /// Serves: the kernel's fast path when no hook is registered for an event.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `crates/kernel/src/turn.rs`.
    /// Down: the `HookReport` fields.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn allow() -> Self {
        Self {
            verdict: Verdict::Allow,
            outcomes: Vec::new(),
        }
    }

    /// Did any hook hold the action up (ask or deny)?
    /// Purpose: Whether any hook held the action up.
    /// Serves: `crates/kernel/src/turn/hooks.rs`'s `tighten`.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `crates/kernel/src/turn/hooks.rs`.
    /// Down: the aggregated `verdict`.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn tightening(&self) -> bool {
        !matches!(self.verdict, Verdict::Allow)
    }

    /// The reasons of the hooks that tightened, for the human and the log.
    /// Purpose: The reasons of the hooks that tightened, for the human and the log.
    /// Serves: the kernel's `hook.fired` reason and the denial text.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `crates/kernel/src/turn/hooks.rs`.
    /// Down: the non-allow outcomes.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn reason(&self) -> String {
        self.outcomes
            .iter()
            .filter(|o| !matches!(o.verdict, Verdict::Allow))
            .map(|o| {
                let why = if o.reason.is_empty() {
                    "no reason given"
                } else {
                    o.reason.as_str()
                };
                format!("{} ({why})", o.id)
            })
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// Hooks that could not answer — visible, never silent.
    /// Purpose: The hooks that could not answer.
    /// Serves: the kernel's `hook.fired` recording and this module's tests.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: this module's tests; no in-tree production caller.
    /// Down: the `failed` field.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn failures(&self) -> Vec<&HookOutcome> {
        self.outcomes.iter().filter(|o| o.failed).collect()
    }
}

/// Purpose: The active hooks and the ability to fire them.
/// Serves: the kernel's turn.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `crates/kernel/src/turn/build.rs`.
/// Down: `fire`, `run_one` and the three transports.
/// Verify: `cargo test -p loong-hooks`.
pub struct HookRunner {
    workspace: PathBuf,
    specs: Vec<HookSpec>,
    /// `None` when the session has no MCP servers: an `mcp` hook then fails
    /// closed with a reason that says so, rather than silently passing.
    mcp: Option<Arc<dyn McpCaller>>,
}

impl HookRunner {
    /// Purpose: Build a runner from the workspace and its trusted specs.
    /// Serves: `crates/kernel/src/turn/build.rs`.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: the kernel's build step.
    /// Down: the `workspace` and `specs` fields.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn new(workspace: impl Into<PathBuf>, specs: Vec<HookSpec>) -> Self {
        Self {
            workspace: workspace.into(),
            specs,
            mcp: None,
        }
    }

    /// Purpose: Supply the MCP caller an `mcp` hook needs, when a session has servers.
    /// Serves: `crates/kernel/src/turn/build.rs`.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: the kernel's build step.
    /// Down: the `mcp` field.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn with_mcp(mut self, caller: Option<Arc<dyn McpCaller>>) -> Self {
        self.mcp = caller;
        self
    }

    /// Purpose: Whether this runner has no hooks at all.
    /// Serves: no in-tree caller; the kernel uses `registered_for` to skip an event instead.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: none in the tree.
    /// Down: the `specs` field.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    /// Purpose: The specs this runner holds.
    /// Serves: no in-tree caller at all.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: none in the tree.
    /// Down: the `specs` field.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn specs(&self) -> &[HookSpec] {
        &self.specs
    }

    /// Purpose: How many hooks are registered for one event.
    /// Serves: the kernel's fast path that skips an event with no hooks.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `crates/kernel/src/turn.rs`.
    /// Down: the `specs` field.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn registered_for(&self, event: HookEvent) -> usize {
        self.specs.iter().filter(|s| s.event == event).count()
    }

    /// Run every hook registered for `event` and aggregate. All of them run (no
    /// short circuit) so the recorded outcome is the whole picture, not the first
    /// one that objected.
    /// Purpose: Run every hook for an event and aggregate without short-circuiting.
    /// Serves: the kernel's hook calls.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `crates/kernel/src/turn.rs`.
    /// Down: `run_one` and `strictest`.
    /// Verify: `cargo test -p loong-hooks`.
    pub fn fire(&self, event: HookEvent, payload: &Value) -> HookReport {
        let mut outcomes = Vec::new();
        let mut verdict = Verdict::Allow;
        for spec in self.specs.iter().filter(|s| s.event == event) {
            let outcome = self.run_one(spec, payload);
            verdict = strictest(&verdict, &outcome.verdict);
            outcomes.push(outcome);
        }
        HookReport { verdict, outcomes }
    }

    /// Purpose: Dispatch one spec to its transport.
    /// Contract: `crates/hooks/src/run.rs`.
    fn run_one(&self, spec: &HookSpec, payload: &Value) -> HookOutcome {
        match &spec.kind {
            HookKind::Shell { script } => self.run_shell(spec, script, payload),
            HookKind::Http { url } => self.run_http(spec, url, payload),
            HookKind::Mcp { server, tool } => self.run_mcp(spec, server, tool, payload),
        }
    }

    /// Purpose: Run a shell hook with the payload on stdin and a hard deadline.
    /// Serves: `run_one`.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `run_one`.
    /// Down: `loong_proc::Shell::detect`, `RunSpec` and `parse_shell_answer`.
    /// Verify: `cargo test -p loong-hooks`.
    fn run_shell(&self, spec: &HookSpec, script: &Path, payload: &Value) -> HookOutcome {
        let started = Instant::now();
        let body = payload.to_string();
        // A hook script is a file, so the shell *reads* it (`sh hook.sh`), which is why
        // it need not be executable. A POSIX shell is used wherever one exists — on
        // Windows that means Git for Windows/MSYS/WSL on PATH; without one the fallback
        // is `cmd.exe /C`, and a `.sh` hook then fails closed into `on_error`.
        let (program, args) = loong_proc::Shell::detect().script(script);
        let run = RunSpec::new(&program, &args, &self.workspace, spec.timeout)
            .stdin(body.as_bytes())
            .output_limit(8 * 1024);

        match loong_proc::run(&run) {
            Err(e) => outcome(
                spec,
                self.fallback(spec, format!("could not run the hook: {e}")),
                started,
            ),
            Ok(out) if out.timed_out => outcome(
                spec,
                self.fallback(spec, format!("timed out after {:?}", spec.timeout)),
                started,
            ),
            Ok(out) => {
                let answer =
                    parse_shell_answer(&out.stdout, &out.stderr, out.code(), spec.on_error);
                outcome(spec, answer, started)
            }
        }
    }

    /// Purpose: Post the payload to an HTTP hook and read the same verdict JSON.
    /// Serves: `run_one`.
    /// Design: `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §3.
    /// Up: `run_one`.
    /// Down: `loong_http::post_json` and `parse_verdict_body`.
    /// Verify: `cargo test -p loong-hooks`.
    fn run_http(&self, spec: &HookSpec, url: &str, payload: &Value) -> HookOutcome {
        let started = Instant::now();
        let body = payload.to_string();
        match loong_http::post_json(url, &body, None, spec.timeout) {
            Err(e) => outcome(
                spec,
                self.fallback(spec, format!("request failed: {e}")),
                started,
            ),
            Ok(response) if !(200..300).contains(&response.status) => outcome(
                spec,
                self.fallback(spec, format!("HTTP {} from {url}", response.status)),
                started,
            ),
            Ok(response) => {
                let answer = parse_verdict_body(&response.body, spec.on_error);
                outcome(spec, answer, started)
            }
        }
    }

    /// An `mcp` hook: call the tool, and expect the same verdict JSON an HTTP hook
    /// returns. One contract across all three kinds, so a policy service can move
    /// from a script to an endpoint to an MCP tool without a rewrite.
    /// Purpose: Call an MCP tool and expect the same verdict JSON an HTTP hook returns.
    /// Serves: `run_one`.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `run_one`.
    /// Down: the supplied `McpCaller` and `parse_verdict_body`.
    /// Verify: `cargo test -p loong-hooks`.
    fn run_mcp(&self, spec: &HookSpec, server: &str, tool: &str, payload: &Value) -> HookOutcome {
        let started = Instant::now();
        let Some(caller) = &self.mcp else {
            return outcome(
                spec,
                self.fallback(
                    spec,
                    format!("no MCP servers are running, so {server}/{tool} could not be asked"),
                ),
                started,
            );
        };
        match caller.call(server, tool, payload) {
            Err(e) => outcome(
                spec,
                self.fallback(spec, format!("calling {server}/{tool}: {e}")),
                started,
            ),
            Ok(body) => outcome(spec, parse_verdict_body(&body, spec.on_error), started),
        }
    }

    /// What a hook that could not answer means, according to its own config.
    /// Purpose: What a hook that could not answer means, according to its own config.
    /// Contract: `crates/hooks/src/run.rs`.
    fn fallback(&self, spec: &HookSpec, why: String) -> Answer {
        Answer {
            verdict: match spec.on_error {
                OnError::Deny => Verdict::Deny,
                OnError::Allow => Verdict::Allow,
            },
            reason: format!("{why} (on_error = {})", on_error_label(spec.on_error)),
            answered: false,
        }
    }
}

/// Purpose: The internal result of one hook: verdict, reason and whether it answered.
/// Serves: the parsers and `outcome`.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `parse_shell_answer`, `parse_verdict_body` and `fallback`.
/// Down: `outcome`.
/// Verify: `cargo test -p loong-hooks`.
struct Answer {
    verdict: Verdict,
    reason: String,
    answered: bool,
}

/// Purpose: Turn an internal answer into the recorded outcome.
/// Contract: `crates/hooks/src/run.rs`.
fn outcome(spec: &HookSpec, answer: Answer, started: Instant) -> HookOutcome {
    HookOutcome {
        id: spec.id.clone(),
        event: spec.event,
        kind: spec.kind.label(),
        verdict: answer.verdict,
        reason: answer.reason,
        duration_ms: started.elapsed().as_millis() as u64,
        failed: !answer.answered,
    }
}

/// The shell hook contract: a JSON verdict on stdout, else the exit code
/// (0 = allow, 2 = deny with stderr as the reason), else `on_error`.
/// Purpose: The shell contract: JSON on stdout, else the exit code, else `on_error`.
/// Serves: `run_shell`.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `run_shell`.
/// Down: `json_verdict` and `fallback_verdict`.
/// Verify: `cargo test -p loong-hooks`.
fn parse_shell_answer(stdout: &str, stderr: &str, code: Option<i32>, on_error: OnError) -> Answer {
    if let Some(answer) = json_verdict(stdout) {
        return answer;
    }
    match code {
        Some(0) => Answer {
            verdict: Verdict::Allow,
            reason: String::new(),
            answered: true,
        },
        Some(2) => {
            let reason = stderr.trim();
            Answer {
                verdict: Verdict::Deny,
                reason: if reason.is_empty() {
                    "denied by the hook".to_string()
                } else {
                    reason.to_string()
                },
                answered: true,
            }
        }
        other => Answer {
            verdict: fallback_verdict(on_error),
            reason: format!(
                "the hook exited with {}: {} (on_error = {})",
                match other {
                    Some(code) => code.to_string(),
                    None => "a signal".to_string(),
                },
                first_line(stderr),
                on_error_label(on_error),
            ),
            answered: false,
        },
    }
}

/// The contract for hook kinds that answer over a wire (HTTP, MCP): the body must
/// carry a verdict.
/// Purpose: The wire contract for HTTP and MCP hooks; a body without a verdict falls back.
/// Contract: `crates/hooks/src/run.rs`.
fn parse_verdict_body(body: &str, on_error: OnError) -> Answer {
    json_verdict(body).unwrap_or_else(|| Answer {
        verdict: fallback_verdict(on_error),
        reason: format!(
            "the hook's response carried no verdict: {} (on_error = {})",
            first_line(body),
            on_error_label(on_error)
        ),
        answered: false,
    })
}

/// Purpose: Parse a JSON verdict object, or `None` when it carries no usable verdict.
/// Serves: `parse_shell_answer` and `parse_verdict_body`.
/// Design: `docs/design/extension-model.md` §1.
/// Up: both parsers.
/// Down: `loong_schema::Verdict`.
/// Verify: `cargo test -p loong-hooks`.
fn json_verdict(stdout: &str) -> Option<Answer> {
    let value: Value = serde_json::from_str(stdout.trim()).ok()?;
    let text = value.get("verdict")?.as_str()?;
    let verdict: Verdict = serde_json::from_value(json!(text)).ok()?;
    let reason = value
        .get("reason")
        .and_then(|r| r.as_str())
        .unwrap_or("")
        .to_string();
    Some(Answer {
        verdict,
        reason,
        answered: true,
    })
}

/// Purpose: The verdict a hook's `on_error` policy names.
/// Contract: `crates/hooks/src/run.rs`.
fn fallback_verdict(on_error: OnError) -> Verdict {
    match on_error {
        OnError::Deny => Verdict::Deny,
        OnError::Allow => Verdict::Allow,
    }
}

/// Purpose: The label `on_error` uses in a failure reason.
/// Contract: `crates/hooks/src/run.rs`.
fn on_error_label(on_error: OnError) -> &'static str {
    match on_error {
        OnError::Deny => "deny",
        OnError::Allow => "allow",
    }
}

/// Purpose: The first line of a body, bounded for a reason string.
/// Contract: `crates/hooks/src/run.rs`.
fn first_line(text: &str) -> String {
    let line = text.trim().lines().next().unwrap_or("").trim();
    if line.chars().count() > 200 {
        format!("{}…", line.chars().take(200).collect::<String>())
    } else {
        line.to_string()
    }
}

/// deny > ask > allow: the only way to combine verdicts that keeps hooks
/// tighten-only.
/// Purpose: deny > ask > allow: the only combination that keeps hooks tighten-only.
/// Serves: `HookRunner::fire`.
/// Design: `docs/design/extension-model.md` §1 and `docs/design/security-trust.md` §2.
/// Up: `HookRunner::fire`.
/// Down: the `Verdict` ranking.
/// Verify: `cargo test -p loong-hooks`.
pub fn strictest(a: &Verdict, b: &Verdict) -> Verdict {
    /// Purpose: The numeric rank behind `strictest`.
    /// Contract: `crates/hooks/src/run.rs`.
    fn rank(v: &Verdict) -> u8 {
        match v {
            Verdict::Allow => 0,
            Verdict::Ask => 1,
            Verdict::Deny => 2,
        }
    }
    let (a, b) = (rank(a), rank(b));
    if a >= b {
        // Re-derive from the rank so the enum is not compared on internals.
        match a {
            0 => Verdict::Allow,
            1 => Verdict::Ask,
            _ => Verdict::Deny,
        }
    } else {
        match b {
            0 => Verdict::Allow,
            1 => Verdict::Ask,
            _ => Verdict::Deny,
        }
    }
}

/// The payload every hook receives on stdin: the event name, a timestamp, and
/// whatever fields the moment carries.
/// Purpose: The payload every hook receives: the event, a timestamp and the moment's fields.
/// Serves: the kernel's hook calls.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `crates/kernel/src/turn.rs` and `turn/loop.rs`.
/// Down: `HookEvent::wire` and `now_ms`; the hook's stdin or request body.
/// Verify: `cargo test -p loong-hooks`.
pub fn payload(event: HookEvent, fields: Value) -> Value {
    let mut map = match fields {
        Value::Object(map) => map,
        other => {
            let mut map = serde_json::Map::new();
            map.insert("fields".to_string(), other);
            map
        }
    };
    map.insert("event".to_string(), json!(event.wire()));
    map.insert("time_ms".to_string(), json!(now_ms()));
    Value::Object(map)
}

/// Purpose: The current time in milliseconds for the payload.
/// Serves: `payload`; no other in-tree caller.
/// Design: `docs/design/extension-model.md` §1.
/// Up: `payload`.
/// Down: `SystemTime::now`.
/// Verify: `cargo test -p loong-hooks`.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HookKind, HookSet};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: A fresh temporary directory for a test.
    /// Contract: `crates/hooks/src/run.rs`.
    fn workspace() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-runner-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Purpose: Write a hook script into a test directory and return its path.
    /// Contract: `crates/hooks/src/run.rs`.
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// Purpose: A shell hook spec for the runner tests.
    /// Serves: every test in this module.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: the runner tests.
    /// Down: `HookSpec` and `HookKind::Shell`.
    /// Verify: `cargo test -p loong-hooks`.
    fn spec(
        id: &str,
        event: HookEvent,
        script_path: PathBuf,
        on_error: OnError,
        ms: u64,
    ) -> HookSpec {
        HookSpec {
            id: id.to_string(),
            event,
            kind: HookKind::Shell {
                script: script_path,
            },
            on_error,
            timeout: Duration::from_millis(ms),
        }
    }

    #[test]
    /// Purpose: Pin the exit-code contract: `0` allows, `2` denies with stderr as the reason, every hook runs.
    fn exit_zero_allows_and_exit_two_denies_with_stderr_as_the_reason() {
        let ws = workspace();
        let allow = spec(
            "a",
            HookEvent::PreTool,
            script(&ws, "a.sh", "cat > /dev/null\nexit 0\n"),
            OnError::Deny,
            5_000,
        );
        let deny = spec(
            "d",
            HookEvent::PreTool,
            script(
                &ws,
                "d.sh",
                "cat > /dev/null\necho 'no secrets' >&2\nexit 2\n",
            ),
            OnError::Deny,
            5_000,
        );
        let runner = HookRunner::new(&ws, vec![allow, deny]);

        let report = runner.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(report.verdict, Verdict::Deny);
        assert!(report.tightening());
        assert!(
            report.reason().contains("no secrets"),
            "{}",
            report.reason()
        );
        assert_eq!(
            report.outcomes.len(),
            2,
            "every hook runs, so the record is complete"
        );
        assert!(report.outcomes.iter().all(|o| !o.failed));
    }

    #[test]
    /// Purpose: Pin that a JSON verdict on stdout reaches `ask` and carries its reason.
    fn a_json_verdict_on_stdout_reaches_the_full_range() {
        let ws = workspace();
        let ask = spec(
            "ask",
            HookEvent::PreTool,
            script(&ws, "ask.sh", "cat > /dev/null\nprintf '{\"verdict\":\"ask\",\"reason\":\"looks risky\"}'\nexit 0\n"),
            OnError::Deny,
            5_000,
        );
        let runner = HookRunner::new(&ws, vec![ask]);
        let report = runner.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(report.verdict, Verdict::Ask);
        assert!(
            report.reason().contains("looks risky"),
            "{}",
            report.reason()
        );
    }

    #[test]
    /// Purpose: Pin that a broken hook fails closed by default and records the failure.
    fn a_broken_hook_fails_closed_by_default_and_says_so() {
        let ws = workspace();
        let broken = spec(
            "broken",
            HookEvent::PreTool,
            script(&ws, "b.sh", "cat > /dev/null\nexit 1\n"),
            OnError::Deny,
            5_000,
        );
        let runner = HookRunner::new(&ws, vec![broken]);
        let report = runner.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(
            report.verdict,
            Verdict::Deny,
            "a guard that cannot answer must not evaporate"
        );
        assert_eq!(report.failures().len(), 1);
        assert!(report.failures()[0].reason.contains("on_error = deny"));
    }

    #[test]
    /// Purpose: Pin that `on_error = allow` is possible but still recorded as a failure.
    fn on_error_allow_is_possible_but_explicit() {
        let ws = workspace();
        let flaky = spec(
            "flaky",
            HookEvent::PreTool,
            script(&ws, "f.sh", "cat > /dev/null\nexit 1\n"),
            OnError::Allow,
            5_000,
        );
        let runner = HookRunner::new(&ws, vec![flaky]);
        let report = runner.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(report.verdict, Verdict::Allow);
        assert_eq!(
            report.failures().len(),
            1,
            "allowed, but recorded as a failure"
        );
    }

    #[test]
    /// Purpose: Pin that a slow hook is cut off at its deadline and fails closed.
    fn a_slow_hook_is_cut_off() {
        let ws = workspace();
        let slow = spec(
            "slow",
            HookEvent::PreTool,
            script(&ws, "s.sh", "cat > /dev/null\nsleep 30\n"),
            OnError::Deny,
            250,
        );
        let runner = HookRunner::new(&ws, vec![slow]);
        let started = Instant::now();
        let report = runner.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(report.verdict, Verdict::Deny);
        assert!(report.failures()[0].reason.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    /// Purpose: Pin that the hook receives the payload and runs in the workspace.
    fn the_hook_receives_the_event_and_runs_in_the_workspace() {
        let ws = workspace();
        std::fs::write(ws.join("marker"), "here").unwrap();
        // Deny only when the payload names a tool AND the cwd is the workspace.
        let hook = spec(
            "ctx",
            HookEvent::PreTool,
            script(
                &ws,
                "ctx.sh",
                "payload=$(cat)\ntest -f marker || exit 2\ncase \"$payload\" in *'\"tool\":\"shell\"'*) exit 2;; esac\nexit 0\n",
            ),
            OnError::Deny,
            5_000,
        );
        let runner = HookRunner::new(&ws, vec![hook]);
        let named = runner.fire(
            HookEvent::PreTool,
            &payload(HookEvent::PreTool, json!({"tool": "shell"})),
        );
        assert_eq!(
            named.verdict,
            Verdict::Deny,
            "the payload reached the hook and cwd was the workspace"
        );

        let other = runner.fire(
            HookEvent::PreTool,
            &payload(HookEvent::PreTool, json!({"tool": "read_file"})),
        );
        assert_eq!(other.verdict, Verdict::Allow);
    }

    #[test]
    /// Purpose: Pin that a hook only fires for its own event.
    fn hooks_only_fire_for_their_own_event() {
        let ws = workspace();
        let pre = spec(
            "pre",
            HookEvent::PreTool,
            script(&ws, "p.sh", "exit 2\n"),
            OnError::Deny,
            5_000,
        );
        let runner = HookRunner::new(&ws, vec![pre]);
        assert_eq!(runner.registered_for(HookEvent::PreTool), 1);
        assert_eq!(runner.registered_for(HookEvent::PostTool), 0);
        assert_eq!(
            runner
                .fire(
                    HookEvent::PostTool,
                    &payload(HookEvent::PostTool, json!({}))
                )
                .verdict,
            Verdict::Allow
        );
    }

    #[test]
    /// Purpose: Pin that aggregation is monotonic and independent of hook order.
    fn aggregation_is_monotonic_and_order_independent() {
        let ws = workspace();
        let allow = spec(
            "allow",
            HookEvent::PreTool,
            script(&ws, "a.sh", "exit 0\n"),
            OnError::Deny,
            5_000,
        );
        let ask = spec(
            "ask",
            HookEvent::PreTool,
            script(&ws, "k.sh", "printf '{\"verdict\":\"ask\"}'\n"),
            OnError::Deny,
            5_000,
        );
        let deny = spec(
            "deny",
            HookEvent::PreTool,
            script(&ws, "d.sh", "exit 2\n"),
            OnError::Deny,
            5_000,
        );

        let forward = HookRunner::new(&ws, vec![allow.clone(), ask.clone(), deny.clone()]);
        let reverse = HookRunner::new(&ws, vec![deny, ask.clone(), allow]);
        let a = forward.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        let b = reverse.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(a.verdict, Verdict::Deny);
        assert_eq!(b.verdict, Verdict::Deny);

        let only_ask = HookRunner::new(&ws, vec![ask]);
        assert_eq!(
            only_ask
                .fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})))
                .verdict,
            Verdict::Ask
        );
    }

    #[test]
    /// Purpose: Pin that an HTTP hook's verdict and payload reach the report and the request.
    fn http_hooks_answer_with_a_verdict() {
        let (base, server) = loong_http::testing::fake_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n\
             {\"verdict\":\"deny\",\"reason\":\"outside the sandbox\"}",
        );
        let ws = workspace();
        let hook = HookSpec {
            id: "remote".to_string(),
            event: HookEvent::PreTool,
            kind: HookKind::Http {
                url: format!("{base}/policy"),
            },
            on_error: OnError::Deny,
            timeout: Duration::from_secs(5),
        };
        let runner = HookRunner::new(&ws, vec![hook]);
        let report = runner.fire(
            HookEvent::PreTool,
            &payload(HookEvent::PreTool, json!({"tool": "shell"})),
        );
        assert_eq!(report.verdict, Verdict::Deny);
        assert!(
            report.reason().contains("outside the sandbox"),
            "{}",
            report.reason()
        );

        let sent = server.join().unwrap();
        let (_, body) = sent.split_once("\r\n\r\n").unwrap();
        let parsed: Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["event"], "pre_tool");
        assert_eq!(parsed["tool"], "shell");
        assert!(parsed["time_ms"].is_u64());
    }

    #[test]
    /// Purpose: Pin that a misbehaving HTTP hook falls back to its own `on_error`.
    fn an_http_hook_that_misbehaves_falls_back_to_on_error() {
        let (base, server) = loong_http::testing::fake_server(
            "HTTP/1.1 500 Oops\r\nContent-Type: text/plain\r\n\r\nbroken",
        );
        let ws = workspace();
        let hook = HookSpec {
            id: "remote".to_string(),
            event: HookEvent::PreTool,
            kind: HookKind::Http {
                url: format!("{base}/policy"),
            },
            on_error: OnError::Deny,
            timeout: Duration::from_secs(5),
        };
        let report = HookRunner::new(&ws, vec![hook])
            .fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(report.verdict, Verdict::Deny);
        assert!(report.failures()[0].reason.contains("HTTP 500"));
        server.join().unwrap();

        let (base, server) = loong_http::testing::fake_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"something\":\"else\"}",
        );
        let hook = HookSpec {
            id: "remote".to_string(),
            event: HookEvent::PreTool,
            kind: HookKind::Http {
                url: format!("{base}/policy"),
            },
            on_error: OnError::Allow,
            timeout: Duration::from_secs(5),
        };
        let report = HookRunner::new(&ws, vec![hook])
            .fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(report.verdict, Verdict::Allow);
        assert!(report.failures()[0].reason.contains("no verdict"));
        server.join().unwrap();
    }

    /// Purpose: A test `McpCaller` that returns a fixed answer.
    /// Serves: the MCP hook tests.
    /// Design: `docs/design/extension-model.md` §1.
    /// Up: `an_mcp_hook_uses_the_same_verdict_contract` and its neighbours.
    /// Down: `McpCaller::call`.
    /// Verify: `cargo test -p loong-hooks`.
    struct FakeMcp {
        answer: Result<String, String>,
    }

    impl McpCaller for FakeMcp {
        /// Purpose: Return the fixed answer whatever was asked.
        /// Contract: `crates/hooks/src/run.rs`.
        fn call(&self, _server: &str, _tool: &str, _args: &Value) -> Result<String, String> {
            self.answer.clone()
        }
    }

    #[test]
    /// Purpose: Pin that an MCP hook uses the same verdict contract as a shell or HTTP hook.
    fn an_mcp_hook_uses_the_same_verdict_contract() {
        let ws = workspace();
        let hook = HookSpec {
            id: "policy".to_string(),
            event: HookEvent::PreTool,
            kind: HookKind::Mcp {
                server: "policy".to_string(),
                tool: "decide".to_string(),
            },
            on_error: OnError::Deny,
            timeout: Duration::from_secs(5),
        };
        let deny = HookRunner::new(&ws, vec![hook.clone()]).with_mcp(Some(Arc::new(FakeMcp {
            answer: Ok("{\"verdict\":\"deny\",\"reason\":\"no shell here\"}".to_string()),
        })));
        let report = deny.fire(
            HookEvent::PreTool,
            &payload(HookEvent::PreTool, json!({"tool": "shell"})),
        );
        assert_eq!(report.verdict, Verdict::Deny);
        assert!(
            report.reason().contains("no shell here"),
            "{}",
            report.reason()
        );
        assert!(!report.outcomes[0].failed);

        let ask = HookRunner::new(&ws, vec![hook.clone()]).with_mcp(Some(Arc::new(FakeMcp {
            answer: Ok("{\"verdict\":\"ask\"}".to_string()),
        })));
        assert_eq!(
            ask.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})))
                .verdict,
            Verdict::Ask
        );
    }

    #[test]
    /// Purpose: Pin that an MCP hook fails closed when no server is available or the answer is not a verdict.
    fn an_mcp_hook_fails_closed_when_the_server_is_missing_or_broken() {
        let ws = workspace();
        let hook = HookSpec {
            id: "policy".to_string(),
            event: HookEvent::PreTool,
            kind: HookKind::Mcp {
                server: "policy".to_string(),
                tool: "decide".to_string(),
            },
            on_error: OnError::Deny,
            timeout: Duration::from_secs(5),
        };
        // No MCP servers in this session at all.
        let none = HookRunner::new(&ws, vec![hook.clone()])
            .fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(none.verdict, Verdict::Deny);
        assert!(none.failures()[0]
            .reason
            .contains("no MCP servers are running"));
        assert!(none.failures()[0].reason.contains("on_error = deny"));

        // A server that answers with something that is not a verdict.
        let garbage = HookRunner::new(&ws, vec![hook.clone()]).with_mcp(Some(Arc::new(FakeMcp {
            answer: Ok("I am not JSON".to_string()),
        })));
        let report = garbage.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(report.verdict, Verdict::Deny);
        assert!(report.failures()[0].reason.contains("carried no verdict"));

        // A call that errored.
        let broken = HookRunner::new(&ws, vec![hook]).with_mcp(Some(Arc::new(FakeMcp {
            answer: Err("the tool reported an error".to_string()),
        })));
        let report = broken.fire(HookEvent::PreTool, &payload(HookEvent::PreTool, json!({})));
        assert_eq!(report.verdict, Verdict::Deny);
        assert!(report.failures()[0]
            .reason
            .contains("the tool reported an error"));
    }

    #[test]
    /// Purpose: Pin that the strictest verdict wins for every pair.
    fn the_strictest_verdict_wins() {
        assert_eq!(strictest(&Verdict::Allow, &Verdict::Ask), Verdict::Ask);
        assert_eq!(strictest(&Verdict::Ask, &Verdict::Allow), Verdict::Ask);
        assert_eq!(strictest(&Verdict::Ask, &Verdict::Deny), Verdict::Deny);
        assert_eq!(strictest(&Verdict::Deny, &Verdict::Deny), Verdict::Deny);
        assert_eq!(strictest(&Verdict::Allow, &Verdict::Allow), Verdict::Allow);
    }

    #[test]
    /// Purpose: Pin that a trusted hook set builds a runner that fires.
    fn a_trusted_hook_set_produces_a_runner() {
        let ws = workspace();
        let dir = ws.join(crate::config::HOOKS_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("g.sh"), "cat > /dev/null\nexit 2\n").unwrap();
        std::fs::write(
            dir.join(crate::config::HOOKS_FILE),
            "[[hook]]\nid = \"g\"\nevent = \"user_prompt_submit\"\nkind = \"shell\"\nscript = \"g.sh\"\n",
        )
        .unwrap();
        let set = HookSet::load(&ws).unwrap();
        set.trust_now().unwrap();
        let trusted = HookSet::load(&ws).unwrap();
        assert_eq!(trusted.active().len(), 1);

        let runner = HookRunner::new(&ws, trusted.active().to_vec());
        let report = runner.fire(
            HookEvent::UserPromptSubmit,
            &payload(HookEvent::UserPromptSubmit, json!({"text": "hi"})),
        );
        assert_eq!(report.verdict, Verdict::Deny);
    }
}
