//! Purpose: The local agent server: one session, JSON-RPC over stdio, every event streamed before the response.
//!
//! # Contract
//! - **Serves:** every surface that attaches to a session, and `crates/cli/src/server.rs`, which runs it.
//! - **Design:** `docs/design/surfaces.md` §1 and §7; `docs/design/sessions-lineage.md` §1 and §5.
//! - **Upstream:** `crates/cli/src/server.rs` builds a `Server` and calls `run_stdio`; `crates/cli/src/main.rs` imports it.
//! - **Downstream:** `loong_kernel` for the turn and the log, `loong_protocol` for envelopes, `loong_eventlog` for replay and `loong_schema` for method and event names.
//! - **Verify:** `cargo test -p loong-server` · `./scripts/check.sh`
//!
//! The local agent server (ADR-0001): sessions live here, clients attach.
//!
//! Transport is newline-delimited JSON-RPC over stdio (a unix socket is the same
//! loop over a different stream). The server owns the session: it asks the
//! kernel to run a turn, then **streams every event the turn produced** as an
//! `event` notification before answering the request. A thin client therefore
//! needs no agent logic to render what happened (ADR-0009) — and no client can
//! see a decision that was not also written to the log.

use loong_kernel::{Kernel, KernelError};
use loong_protocol::{codes, Notification, Request, Response};
use loong_schema::generated as gen;
use serde_json::Value;
use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::rc::Rc;

/// Purpose: The outbound side of one connection, shareable so work in progress can write to it.
/// Serves: `handle_line_streaming`, `run_stdio`, the event sink and the delta callback.
/// Design: `docs/design/surfaces.md` §1 and §7.
/// Up: `run_stdio` and the tests.
/// Down: `stdout`, `memory` and `line`, and the boxed writer they wrap.
/// Verify: `cargo test -p loong-server`.
/// The outbound side of one connection, shareable so that work **in progress** can write
/// to it. A turn runs inside a single call, so a sink borrowed for that call cannot reach
/// the wire; this can, because it is a handle rather than a borrow.
#[derive(Clone)]
pub struct Out(Rc<RefCell<Box<dyn Write>>>);

impl Out {
    /// Purpose: The process's real stdout as a connection.
    /// Serves: `run_stdio`.
    /// Design: `docs/design/surfaces.md` §1.
    /// Up: `run_stdio`.
    /// Down: `std::io::stdout`; no further call.
    /// Verify: `cargo test -p loong-server`.
    pub fn stdout() -> Self {
        Self(Rc::new(RefCell::new(Box::new(std::io::stdout()))))
    }

    /// Purpose: A connection that keeps complete lines, for tests and collecting callers.
    /// Serves: `handle_line` and the tests.
    /// Design: `docs/design/surfaces.md` §7.
    /// Up: `handle_line` and the tests.
    /// Down: the nested `Lines` writer it boxed.
    /// Verify: `cargo test -p loong-server`.
    /// A connection that keeps what was written, for tests and for callers that would
    /// rather collect than stream.
    pub fn memory() -> (Self, Rc<RefCell<Vec<String>>>) {
        let collected: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        /// Purpose: The writer behind `Out::memory`: bytes in, complete lines collected.
        /// Serves: `Out::memory`.
        /// Design: `docs/design/surfaces.md` §7.
        /// Up: `Out::memory` constructs it.
        /// Down: its `write` and `flush` impls, and the shared line buffer.
        /// Verify: `cargo test -p loong-server`.
        struct Lines(Rc<RefCell<Vec<String>>>, String);
        impl Write for Lines {
            /// Purpose: Buffer bytes and push each complete line into the collector.
            /// Contract: the `Write` impl of `Lines`, driven by `writeln!` in `Out::line`.
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.1.push_str(&String::from_utf8_lossy(buf));
                while let Some(index) = self.1.find('\n') {
                    let line = self.1[..index].to_string();
                    self.1.drain(..=index);
                    self.0.borrow_mut().push(line);
                }
                Ok(buf.len())
            }
            /// Purpose: The no-op flush: lines are already pushed as they complete.
            /// Contract: the `Write` impl of `Lines`.
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let out = Self(Rc::new(RefCell::new(Box::new(Lines(
            Rc::clone(&collected),
            String::new(),
        )))));
        (out, collected)
    }

    /// Purpose: Write one line and flush it, so a watching person sees it before the turn ends.
    /// Serves: the kernel event sink, the delta callback and `stream_drained`.
    /// Design: `docs/design/surfaces.md` §1 and §7.
    /// Up: `dispatch`'s event sink, its delta callback and `stream_drained`.
    /// Down: the boxed writer; no further call.
    /// Verify: `cargo test -p loong-server`.
    /// One line, on the wire now. Flushing matters more than batching here: the point of
    /// streaming is that a person watching sees it before the turn ends.
    pub fn line(&self, line: &str) {
        let mut writer = self.0.borrow_mut();
        let _ = writeln!(writer, "{line}");
        let _ = writer.flush();
    }
}

/// Purpose: Why the server could not start: an I/O failure or an interrupt handler that would not install.
/// Serves: `run_stdio`, `catch_interrupts` and `crates/cli/src/server.rs`.
/// Design: `docs/design/surfaces.md` §1.
/// Up: `run_stdio` and `catch_interrupts` return it; `crates/cli/src/server.rs` maps it to a string.
/// Down: its `Display` impl and `From<std::io::Error>`.
/// Verify: `cargo test -p loong-server`.
#[derive(Debug)]
pub enum ServerError {
    Io(std::io::Error),
    /// The platform's interrupt handler could not be installed.
    Signal(String),
}

/// The turn to stop when the platform says the person asked. A handler cannot hold a
/// `Kernel`, so it holds the one flag the kernel reads.
///
/// The flag is created **once per process** and every kernel this server builds is
/// pointed at it (`use_interruptor`), including the one `/new` swaps in. A second
/// `OnceLock::set` would fail and a fresh kernel would otherwise get an orphaned flag:
/// the interrupt would look like it worked and silently stop nothing.
static INTERRUPT: std::sync::OnceLock<std::sync::Arc<std::sync::atomic::AtomicBool>> =
    std::sync::OnceLock::new();

/// Purpose: Set the process-wide interrupt flag from a handler, allocating and locking nothing.
/// Contract: called by both platform `handle_interrupt` bodies.
/// The one thing a handler is allowed to do: store into an atomic that lives for the
/// process. Called from a signal handler on Unix and from a console control thread on
/// Windows, so it allocates nothing and locks nothing.
fn raise_interrupt() {
    if let Some(flag) = INTERRUPT.get() {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Purpose: The Unix `SIGINT` handler: record the interrupt and return.
/// Contract: installed by `install_interrupt_handler_once` on unix.
#[cfg(unix)]
extern "C" fn handle_interrupt(_signal: libc::c_int) {
    raise_interrupt();
}

/// Purpose: The Windows console handler for `CTRL_BREAK`: record the interrupt and claim the event.
/// Contract: registered by `install_interrupt_handler_once` on Windows.
/// Windows: the console control handler. The launcher sends `CTRL_BREAK_EVENT` to the
/// server's process group (`crates/tui/src/lib.rs::interrupt`), not `CTRL_C_EVENT` —
/// Microsoft documents that a `CTRL_C_EVENT` cannot be aimed at one process group, so it
/// would also hit the TUI. `CTRL+BREAK` is always treated as a signal (it cannot be
/// turned into keyboard input by the console mode), which is what makes this reliable.
///
/// Returning `1` (TRUE) claims the event: without that, the default handler would run and
/// `ExitProcess` would take the server and its session down.
#[cfg(windows)]
unsafe extern "system" fn handle_interrupt(event: u32) -> i32 {
    // `CTRL_BREAK_EVENT`, wincon.h.
    const CTRL_BREAK_EVENT: u32 = 1;
    if event == CTRL_BREAK_EVENT {
        raise_interrupt();
    }
    1
}

#[cfg(windows)]
extern "system" {
    /// Purpose: The kernel32 routine that registers a console control handler.
    /// `kernel32` — register the console control routine above.
    fn SetConsoleCtrlHandler(
        handler: Option<unsafe extern "system" fn(u32) -> i32>,
        add: i32,
    ) -> i32;
    /// Purpose: The kernel32 routine that returns the calling thread's last error code.
    fn GetLastError() -> u32;
}

/// Purpose: Install the platform handler once per process and remember the first result.
/// Contract: called by `catch_interrupts`; the `OnceLock` makes later calls cheap.
/// Install the handler exactly once per process. Registering it twice would be harmless
/// on Unix (the second call replaces the first) but on Windows it would run the routine
/// twice, so the result of the first attempt is what every later caller sees.
fn install_interrupt_handler() -> Result<(), ServerError> {
    static INSTALLED: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    INSTALLED
        .get_or_init(install_interrupt_handler_once)
        .clone()
        .map_err(ServerError::Signal)
}

/// Purpose: Point `SIGINT` at the process-wide flag on Unix.
/// Contract: the unix arm of `install_interrupt_handler`.
#[cfg(unix)]
fn install_interrupt_handler_once() -> Result<(), String> {
    // Safety: the handler only stores into an atomic that lives for the process.
    unsafe {
        libc::signal(
            libc::SIGINT,
            handle_interrupt as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
    }
    Ok(())
}

/// Purpose: Register the `CTRL_BREAK` console handler on Windows.
/// Contract: the windows arm of `install_interrupt_handler`.
#[cfg(windows)]
fn install_interrupt_handler_once() -> Result<(), String> {
    // Safety: a pointer to a routine whose whole body is one atomic store.
    let added = unsafe { SetConsoleCtrlHandler(Some(handle_interrupt), 1) };
    if added == 0 {
        // Safety: reading the thread's last error, immediately after the call.
        let code = unsafe { GetLastError() };
        return Err(format!("SetConsoleCtrlHandler failed (win32 error {code})"));
    }
    Ok(())
}

/// Purpose: Report that this platform has no interrupt mechanism.
/// Contract: the fallback arm of `install_interrupt_handler`.
#[cfg(not(any(unix, windows)))]
fn install_interrupt_handler_once() -> Result<(), String> {
    Err("this platform has no interrupt mechanism wired up".to_string())
}

impl std::fmt::Display for ServerError {
    /// Purpose: Render a server failure for the CLI's error output.
    /// Contract: the `Display` half of `ServerError`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServerError::Io(e) => write!(f, "io: {e}"),
            ServerError::Signal(m) => write!(f, "signal: {m}"),
        }
    }
}

impl std::error::Error for ServerError {}

impl From<std::io::Error> for ServerError {
    /// Purpose: Convert an I/O failure into `ServerError::Io`.
    /// Contract: lets `?` in `run_stdio` produce a `ServerError`.
    fn from(e: std::io::Error) -> Self {
        ServerError::Io(e)
    }
}

/// Purpose: A closure that builds a fresh session id and kernel for `session.new`.
/// Serves: `Server::with_factory` and `crates/cli/src/server.rs`.
/// Design: `docs/design/surfaces.md` §1; `docs/design/sessions-lineage.md` §1.
/// Up: `crates/cli/src/server.rs` and the session-new test.
/// Down: the `(id, kernel)` pair consumed by `dispatch`.
/// Verify: `cargo test -p loong-server`.
/// Builds a kernel for a session this server has not been given one for.
///
/// A closure rather than a config struct because building a kernel is the caller's
/// business: it knows about layers, hooks, models and where the log goes. The server only
/// needs to be able to ask for another one.
pub type KernelFactory = Box<dyn FnMut() -> Result<(String, Kernel), String>>;

/// Purpose: One process serving one session: the kernel, the id it owns and the stream bookkeeping.
/// Serves: `run_stdio` and `crates/cli/src/server.rs`.
/// Design: `docs/design/surfaces.md` §1; `docs/design/sessions-lineage.md` §1.
/// Up: `crates/cli/src/server.rs` builds it; the tests use it directly.
/// Down: `handle_line`, `handle_line_streaming`, `session_id`, `catch_interrupts` and `run_stdio`.
/// Verify: `cargo test -p loong-server`.
pub struct Server {
    kernel: Kernel,
    session_id: String,
    /// How to get a kernel for a session that does not exist yet. Absent when the server
    /// was handed one kernel and nothing else — it then simply cannot start new sessions,
    /// and says so rather than pretending.
    factory: Option<KernelFactory>,
    /// Highest sequence already streamed to the client.
    last_sent: u64,
    /// The connection this call is writing to, so a response can stream the events it
    /// drained instead of returning them.
    connection: Option<Out>,
    /// Lines with nowhere to go yet (no connection, which only tests and programs using
    /// `handle_line` hit).
    pending_lines: Vec<String>,
    turns: u64,
}

impl Server {
    /// Purpose: A server around one kernel and the session id it already owns.
    /// Serves: `crates/cli/src/server.rs` and the tests.
    /// Design: `docs/design/surfaces.md` §1.
    /// Up: `crates/cli/src/server.rs` and the tests.
    /// Down: no call; the fields it sets are read by the methods.
    /// Verify: `cargo test -p loong-server`.
    pub fn new(kernel: Kernel, session_id: impl Into<String>) -> Self {
        Self {
            kernel,
            session_id: session_id.into(),
            factory: None,
            last_sent: 0,
            connection: None,
            pending_lines: Vec::new(),
            turns: 0,
        }
    }

    /// Purpose: Give the server a way to start a session it was not handed.
    /// Serves: `crates/cli/src/server.rs` and the session-new test.
    /// Design: `docs/design/sessions-lineage.md` §1.
    /// Up: `crates/cli/src/server.rs` and the tests.
    /// Down: `dispatch`'s `session.new` arm.
    /// Verify: `cargo test -p loong-server`.
    /// Teach this server how to start a session of its own.
    pub fn with_factory(mut self, factory: KernelFactory) -> Self {
        self.factory = Some(factory);
        self
    }

    /// Purpose: The session this server currently owns.
    /// Serves: `dispatch` and the tests.
    /// Design: `docs/design/sessions-lineage.md` §1.
    /// Up: no production caller; the tests assert on it.
    /// Down: no call; it borrows the stored id.
    /// Verify: `cargo test -p loong-server`.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Purpose: Handle one inbound line and return every outbound line, the response last.
    /// Serves: tests and callers that collect instead of streaming.
    /// Design: `docs/design/surfaces.md` §7.
    /// Up: no production caller outside the tests; `run_stdio` streams instead.
    /// Down: `handle_line_streaming` through an in-memory `Out`.
    /// Verify: `cargo test -p loong-server`.
    /// Handle one inbound line and return the outbound lines, in order:
    /// event notifications first, then exactly one response for a request.
    /// Pure enough to test without any transport at all.
    pub fn handle_line(&mut self, line: &str) -> Vec<String> {
        // Collect rather than stream, then hand back everything in order: the response
        // last, as this API has always promised. Streaming callers use the method below.
        let (out, collected) = Out::memory();
        let mut responses = self.handle_line_streaming(line, &out);
        let mut lines = collected.borrow().clone();
        lines.append(&mut responses);
        lines
    }

    /// Purpose: Handle one line, streaming events and deltas to the connection as they happen.
    /// Serves: `run_stdio` and the tests.
    /// Design: `docs/design/surfaces.md` §1 and §7.
    /// Up: `run_stdio` and the tests.
    /// Down: `handle_request`, which produces the response lines.
    /// Verify: `cargo test -p loong-server`.
    /// As [`Server::handle_line`], but writing to the connection **while the turn runs**
    /// and returning only the response.
    ///
    /// Two kinds of thing go out this way. A **delta** is a `model.delta` notification:
    /// written immediately and nowhere else — the log carries only the finished message,
    /// and a client that misses a delta has missed nothing. An **event** is different:
    /// it was written to the log first and then announced, which is what lets a surface
    /// show the work as it happens without ever seeing a decision the log does not
    /// have (ADR-0009).
    pub fn handle_line_streaming(&mut self, line: &str, out: &Out) -> Vec<String> {
        // So `respond` and `fail` can put what they drained on the same wire.
        self.connection = Some(out.clone());
        let result = self.handle_request(line);
        self.connection = None;
        result
    }

    /// Purpose: Parse one line and dispatch it, or answer the parse error.
    /// Contract: called by `handle_line_streaming`.
    fn handle_request(&mut self, line: &str) -> Vec<String> {
        match Request::parse(line) {
            Ok(request) => self.dispatch(request),
            Err(response) => vec![response.to_line()],
        }
    }

    /// Purpose: Route one request method to the kernel, stream what it caused and build the response.
    /// Serves: `handle_request`.
    /// Design: `docs/design/surfaces.md` §7; `docs/design/sessions-lineage.md` §1 and §5.
    /// Up: `handle_request`.
    /// Down: `respond`, `refuse`, `fail`, `wrong_session`, `model_list`, `stream_drained` and the kernel methods.
    /// Verify: `cargo test -p loong-server`.
    fn dispatch(&mut self, request: Request) -> Vec<String> {
        let id = request.id.clone().unwrap_or(Value::Null);
        match request.method.as_str() {
            gen::method::SESSION_NEW => {
                let params: gen::SessionOnlyParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                // The client has to name the session it thinks it is in. Starting a new
                // one is not destructive — the old log is kept — but a client that has
                // lost track of where it is should find that out here rather than later.
                if let Some(bad) = self.wrong_session(&id, &params.session_id) {
                    return vec![bad];
                }
                let Some(factory) = self.factory.as_mut() else {
                    return self.refuse(
                        id,
                        "this server was started on one session and cannot begin another",
                    );
                };
                let (session_id, kernel) = match factory() {
                    Ok(made) => made,
                    Err(e) => return self.refuse(id, &e),
                };
                self.kernel = kernel;
                // The interrupt flag belongs to the process, not to a log. Without this
                // the kernel `/new` installs would hold a fresh, unreachable flag and the
                // next Ctrl-C would stop nothing while still saying "interrupting…".
                if let Some(flag) = INTERRUPT.get() {
                    self.kernel.use_interruptor(std::sync::Arc::clone(flag));
                }
                self.session_id = session_id.clone();
                // The new session has its own log, so every counter about the old one is
                // now wrong. Forgetting to reset `last_sent` would stream the new
                // session's first events as if they were the old session's past.
                self.last_sent = 0;
                self.turns = 0;
                self.respond(id, gen::SessionNewResult { session_id }, "session.new")
            }
            gen::method::SESSION_ATTACH => {
                let params: gen::SessionAttachParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                let _ = &params.client;
                let result = gen::SessionAttachResult {
                    session_id: self.session_id.clone(),
                    schema_version: gen::SCHEMA_VERSION.to_string(),
                    existing_events: self.kernel.last_seq(),
                };
                self.respond(id, result, "session.attach")
            }
            gen::method::CONTEXT_EXPLAIN => {
                let params: gen::SessionOnlyParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                if let Some(bad) = self.wrong_session(&id, &params.session_id) {
                    return vec![bad];
                }
                // A read of the kernel's own state: nothing is recorded, because
                // asking "what is in the context?" is not a fact about the session.
                let explained = self.kernel.context_explain();
                self.respond(
                    id,
                    serde_json::to_value(explained).unwrap_or(Value::Null),
                    "context.explain",
                )
            }
            gen::method::PERMISSIONS_SET => {
                let params: gen::PermissionsSetParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                if let Some(bad) = self.wrong_session(&id, &params.session_id) {
                    return vec![bad];
                }
                match self.kernel.set_auto_approve(params.auto_approve) {
                    Ok(()) => {
                        // Said out loud, because a session that stops asking is a session
                        // whose log has to say why (ADR-0007 §5).
                        eprintln!(
                            "approval prompts {}",
                            if params.auto_approve {
                                "off (YOLO)"
                            } else {
                                "on"
                            }
                        );
                        self.respond(
                            id,
                            gen::PermissionsState {
                                auto_approve: params.auto_approve,
                            },
                            "permissions.set",
                        )
                    }
                    Err(e) => self.fail(id, e),
                }
            }
            gen::method::MODEL_LIST => {
                let params: gen::SessionOnlyParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                if let Some(bad) = self.wrong_session(&id, &params.session_id) {
                    return vec![bad];
                }
                // A read of the session's own configuration: asking which models exist
                // is not a fact about the session, so nothing is recorded.
                self.respond(id, self.model_list(), "model.list")
            }
            gen::method::MODEL_SELECT => {
                let params: gen::ModelSelectParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                if let Some(bad) = self.wrong_session(&id, &params.session_id) {
                    return vec![bad];
                }
                // The switch records its own event (a person's choice is as
                // explicable as a rule's), so the reply is just the new state.
                match self.kernel.select_model(&params.name) {
                    Ok(()) => self.respond(id, self.model_list(), "model.select"),
                    Err(e) => self.fail(id, e),
                }
            }
            gen::method::SESSION_REPLAY => {
                let params: gen::SessionOnlyParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                if let Some(bad) = self.wrong_session(&id, &params.session_id) {
                    return vec![bad];
                }
                // The log is the record, so replaying it is a read: nothing is written,
                // and a client that attaches to an old session can show its past instead
                // of an empty screen.
                let events = loong_eventlog::replay_lenient(self.kernel.log_path())
                    .map(|replay| {
                        replay
                            .events
                            .iter()
                            .filter_map(|event| serde_json::to_value(event).ok())
                            .collect()
                    })
                    .unwrap_or_default();
                self.respond(id, gen::SessionReplayResult { events }, "session.replay")
            }
            gen::method::PROMPT_SUBMIT => {
                let params: gen::PromptSubmitParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                if let Some(bad) = self.wrong_session(&id, &params.session_id) {
                    return vec![bad];
                }
                self.turns += 1;
                let turn_id = self.turns;
                // While this turn runs, its events go out as they happen and its deltas
                // go out as they arrive. `last_sent` travels with the sink so the final
                // response does not repeat what was already streamed.
                let sent = Rc::new(std::cell::Cell::new(self.last_sent));
                {
                    let out = self.connection.clone().unwrap_or_else(|| Out::memory().0);
                    let sent = Rc::clone(&sent);
                    self.kernel.set_event_sink(Some(Box::new(
                        move |event: &gen::EventEnvelope| {
                            match serde_json::to_value(event) {
                                Ok(value) => out.line(&Notification::event(&value).to_line()),
                                Err(_) => return,
                            }
                            sent.set(sent.get().max(event.seq));
                        },
                    )));
                }
                let deltas = self.connection.clone().unwrap_or_else(|| Out::memory().0);
                let outcome = self.kernel.submit_with(&params.text, &mut |piece| {
                    let notification = Notification::new(
                        gen::notification::MODEL_DELTA,
                        serde_json::json!({"turn_id": turn_id, "text": piece}),
                    );
                    deltas.line(&notification.to_line());
                });
                self.kernel.set_event_sink(None);
                self.last_sent = self.last_sent.max(sent.get());
                match outcome {
                    Ok(outcome) => {
                        // A `user_prompt_submit` hook can refuse the turn before
                        // the model runs; the reason travels as hook.fired events
                        // in the same stream, so the client can show why.
                        let result = gen::PromptSubmitResult {
                            turn_id: self.turns,
                            accepted: outcome.refused.is_none(),
                        };
                        self.respond(id, result, "prompt.submit")
                    }
                    Err(e) => self.fail(id, e),
                }
            }
            gen::method::APPROVAL_RESPOND => {
                let params: gen::ApprovalRespondParams = match request.params_as() {
                    Ok(p) => p,
                    Err(e) => return vec![e.to_line()],
                };
                if let Some(bad) = self.wrong_session(&id, &params.session_id) {
                    return vec![bad];
                }
                let result = gen::PromptSubmitResult {
                    turn_id: self.turns,
                    accepted: params.granted,
                };
                match self.kernel.respond_to_approval(
                    &params.request_id,
                    params.granted,
                    params.scope,
                    params.reason.as_deref(),
                ) {
                    Ok(_) => self.respond(id, result, "approval.respond"),
                    Err(e) => self.fail(id, e),
                }
            }
            other => vec![Response::err(
                id,
                codes::METHOD_NOT_FOUND,
                format!("unknown method {other:?}"),
            )
            .to_line()],
        }
    }

    /// Purpose: Stream what this call drained, then answer with one result.
    /// Contract: called by every `dispatch` arm that succeeds.
    /// `<T>`: stream the events this call produced, then the response.
    fn respond<T: serde::Serialize>(&mut self, id: Value, result: T, _method: &str) -> Vec<String> {
        self.stream_drained();
        match serde_json::to_value(&result) {
            Ok(value) => vec![Response::ok(id, value).to_line()],
            Err(e) => vec![Response::err(id, codes::INTERNAL_ERROR, e.to_string()).to_line()],
        }
    }

    /// Purpose: Send the events recorded since the last one sent, to the connection or the pending list.
    /// Contract: called by `respond` and `refuse`.
    /// Events recorded since the last one sent go to the connection now: they are news,
    /// and news arrives when it happens (ADR-0009). A caller with no connection to write
    /// to — `handle_line` collects through an in-memory one — gets them in its list.
    fn stream_drained(&mut self) {
        for line in self.drain_events() {
            match &self.connection {
                Some(out) => out.line(&line),
                None => self.pending_lines.push(line),
            }
        }
    }

    /// Purpose: Answer in words for a failure that is not the kernel's.
    /// Contract: `dispatch`'s `session.new` arm uses it when no factory exists.
    /// Say no in words, for the failures that are not the kernel's.
    ///
    /// A kernel error has a shape and a code; "this server was built with one kernel and
    /// nothing to build another from" does not, and inventing a `KernelError` for it would
    /// put a lie in the type that the rest of the program reasons about.
    fn refuse(&mut self, id: Value, message: &str) -> Vec<String> {
        self.stream_drained();
        let mut lines: Vec<String> = std::mem::take(&mut self.pending_lines);
        lines.push(Response::err(id, codes::INTERNAL_ERROR, message.to_string()).to_line());
        lines
    }

    /// Purpose: Answer a kernel failure with the JSON-RPC code its kind deserves.
    /// Contract: every `dispatch` arm that calls a kernel method.
    fn fail(&mut self, id: Value, error: KernelError) -> Vec<String> {
        self.stream_drained();
        let mut lines: Vec<String> = std::mem::take(&mut self.pending_lines);
        let code = match error {
            KernelError::OutOfOrder(_) => codes::INVALID_PARAMS,
            _ => codes::INTERNAL_ERROR,
        };
        lines.push(Response::err(id, code, error.to_string()).to_line());
        lines
    }

    /// Purpose: The models this session offers and the one selected now.
    /// Contract: `dispatch`'s `model.list` and `model.select` arms.
    /// The models this session offers and which one is current — the one shape both
    /// `model.list` and `model.select` answer with, so a client that switches does not
    /// have to ask again to find out what happened.
    fn model_list(&self) -> gen::ModelList {
        gen::ModelList {
            models: self.kernel.model_list(),
            selected: self.kernel.selected_model().to_string(),
        }
    }

    /// Purpose: A refusal carrying the request's id when the client names another session.
    /// Serves: every `dispatch` arm that takes a session id.
    /// Design: `docs/design/surfaces.md` §7; `docs/design/sessions-lineage.md` §1.
    /// Up: `dispatch`.
    /// Down: `Response::err`; no further call.
    /// Verify: `cargo test -p loong-server`.
    /// A refusal that is still *this request's* answer.
    ///
    /// It used to be built with a null id, which a JSON-RPC client cannot match to
    /// anything — the caller waited for a reply that had already been sent, and the
    /// connection simply went quiet. A refusal nobody can attribute is worse than a slow
    /// one, because it looks like a hang.
    fn wrong_session(&self, id: &Value, session_id: &str) -> Option<String> {
        (session_id != self.session_id).then(|| {
            Response::err(
                id.clone(),
                codes::INVALID_PARAMS,
                format!(
                    "unknown session {session_id:?} (this server owns {:?})",
                    self.session_id
                ),
            )
            .to_line()
        })
    }

    /// Purpose: Every event appended since the last one sent, as notification lines.
    /// Serves: `stream_drained`.
    /// Design: `docs/design/surfaces.md` §7.
    /// Up: `stream_drained`.
    /// Down: `Kernel::events_since` and `Notification::event`.
    /// Verify: `cargo test -p loong-server`.
    /// Every event appended since the client last heard from us.
    fn drain_events(&mut self) -> Vec<String> {
        let Ok(events) = self.kernel.events_since(self.last_sent) else {
            return Vec::new();
        };
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            self.last_sent = self.last_sent.max(event.seq);
            match serde_json::to_value(&event) {
                Ok(value) => out.push(Notification::event(&value).to_line()),
                Err(_) => continue,
            }
        }
        out
    }

    /// Purpose: Point the kernel at the process-wide interrupt flag and install the handler.
    /// Serves: `run_stdio`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `run_stdio`.
    /// Down: `Kernel::interruptor`, `Kernel::use_interruptor` and `install_interrupt_handler`.
    /// Verify: `cargo test -p loong-server`.
    /// Let the platform's interrupt stop the running turn instead of killing the server.
    ///
    /// The TUI spawns this process and signals it when someone presses Ctrl-C, which is
    /// why the flag is handed out rather than the message being sent over the protocol:
    /// the client is blocked reading the turn's own stream, and a process it owns already
    /// has a way to say "stop".
    ///
    /// The kernel is (re-)pointed at the process-wide flag every time, so the kernel a
    /// later `/new` installs is interruptible too.
    pub fn catch_interrupts(&mut self) -> Result<(), ServerError> {
        let flag = match INTERRUPT.get() {
            Some(flag) => std::sync::Arc::clone(flag),
            None => {
                let flag = self.kernel.interruptor();
                let _ = INTERRUPT.set(std::sync::Arc::clone(&flag));
                flag
            }
        };
        self.kernel.use_interruptor(flag);
        install_interrupt_handler()
    }

    /// Purpose: Serve stdin lines until EOF, writing each response and its stream in order.
    /// Serves: `crates/cli/src/server.rs`.
    /// Design: `docs/design/surfaces.md` §1.
    /// Up: `crates/cli/src/server.rs`.
    /// Down: `catch_interrupts`, `Out::stdout` and `handle_line_streaming`.
    /// Verify: `cargo test -p loong-server`.
    /// Serve stdio until EOF (the transport the TUI spawns).
    pub fn run_stdio(mut self) -> Result<(), ServerError> {
        // Ctrl-C stops the turn; the process stays for the next one.
        self.catch_interrupts()?;
        let stdin = std::io::stdin();
        let mut stdout = std::io::stdout();
        // One handle for the whole connection: progress is written and flushed the moment
        // it happens, and the final response goes through the same door.
        let connection = Out::stdout();
        for line in stdin.lock().lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            for response in self.handle_line_streaming(&line, &connection) {
                writeln!(stdout, "{response}")?;
            }
            stdout.flush()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loong_kernel::KernelConfig;
    use loong_providers::{
        CompletionRequest, CompletionResponse, Model, ModelError, ScriptedModel, ToolCallRequest,
        Usage,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// Purpose: A temporary directory that removes itself when the test ends.
    /// Serves: every test in this module.
    /// Design: `docs/design/surfaces.md` §1.
    /// Up: the tests.
    /// Down: its `Drop` impl and the filesystem.
    /// Verify: `cargo test -p loong-server`.
    struct Temp(PathBuf);
    impl Temp {
        /// Purpose: Create a unique temporary directory for one test.
        /// Contract: test helper; paired with `Temp`'s `Drop`.
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "loong-server-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::SeqCst)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Temp(dir)
        }
    }
    impl Drop for Temp {
        /// Purpose: Remove the temporary directory when the test ends.
        /// Contract: the `Drop` impl for `Temp`.
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Purpose: A scripted model that makes one tool call and then answers.
    /// Serves: the dispatch tests.
    /// Design: `docs/design/surfaces.md` §7.
    /// Up: the dispatch tests.
    /// Down: `ScriptedModel::new`.
    /// Verify: `cargo test -p loong-server`.
    fn scripted(calls: Vec<ToolCallRequest>, text: Option<&str>) -> ScriptedModel {
        ScriptedModel::new(vec![
            CompletionResponse {
                text: None,
                tool_calls: calls,
                usage: Usage {
                    tokens_in: 11,
                    tokens_out: 4,
                },
            },
            CompletionResponse {
                text: text.map(|t| t.to_string()),
                tool_calls: vec![],
                usage: Usage {
                    tokens_in: 5,
                    tokens_out: 2,
                },
            },
        ])
    }

    /// Purpose: A well-formed `session.attach` request line.
    /// Contract: the tests' opening call.
    fn attach_line() -> String {
        Request::new(
            1,
            gen::method::SESSION_ATTACH,
            json!({"cwd": "/tmp", "client": "tui"}),
        )
        .to_line()
    }

    /// Purpose: Handle one line and parse every outbound line as JSON.
    /// Contract: test helper for the dispatch tests.
    fn lines_of(server: &mut Server, line: &str) -> Vec<Value> {
        server
            .handle_line(line)
            .into_iter()
            .map(|l| serde_json::from_str(&l).expect("every outbound line is JSON"))
            .collect()
    }

    /// Purpose: Pin that every session-naming method refuses with the request's own id.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn a_refusal_carries_the_id_of_the_request_it_refuses() {
        // Found while wiring `session.new`: `wrong_session` built its error with a null
        // id, so a JSON-RPC client could not match it to anything. The reply had been
        // sent and the caller was still waiting — a hang, from the only side that can
        // see it. Every method that names a session goes through here.
        let t = Temp::new();
        let kernel = Kernel::create(
            t.0.join("s.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(scripted(vec![], Some("hi"))),
        )
        .unwrap();
        let mut server = Server::new(kernel, "mine");
        for method in [
            gen::method::CONTEXT_EXPLAIN,
            gen::method::MODEL_LIST,
            gen::method::SESSION_REPLAY,
            gen::method::SESSION_NEW,
        ] {
            let out = lines_of(
                &mut server,
                &Request::new(7, method, json!({"session_id": "not-mine"})).to_line(),
            );
            let refusal = out
                .iter()
                .find(|v| v.get("error").is_some())
                .unwrap_or_else(|| panic!("{method} refused silently: {out:?}"));
            assert_eq!(
                refusal["id"], 7,
                "{method}: a refusal nobody can attribute looks like a hang"
            );
        }
    }

    /// Purpose: Pin `/new`'s kernel swap, the kept old log and the refused old id.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn a_new_session_moves_the_server_onto_a_fresh_log_and_keeps_the_old_one() {
        let t = Temp::new();
        let kernel = Kernel::create(
            t.0.join("first.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(scripted(vec![], Some("hi"))),
        )
        .unwrap();
        // The factory is what `serve` hands over: same settings, a fresh id and a fresh
        // log beside the current one.
        let root = t.0.clone();
        let builds = Rc::new(RefCell::new(0usize));
        let counted = Rc::clone(&builds);
        let factory: KernelFactory = Box::new(move || {
            *counted.borrow_mut() += 1;
            let id = format!("second{}", *counted.borrow());
            let kernel = Kernel::create(
                root.join(format!("{id}.jsonl")),
                KernelConfig::new(&root),
                Box::new(scripted(vec![], Some("hi"))),
            )
            .map_err(|e| e.to_string())?;
            Ok((id, kernel))
        });
        let mut server = Server::new(kernel, "first").with_factory(factory);

        let out = lines_of(
            &mut server,
            &Request::new(2, gen::method::SESSION_NEW, json!({"session_id": "first"})).to_line(),
        );
        let responses: Vec<&Value> = out.iter().filter(|v| v.get("result").is_some()).collect();
        assert_eq!(responses.len(), 1, "{out:?}");
        assert_eq!(responses[0]["result"]["session_id"], "second1");
        assert_eq!(server.session_id(), "second1");

        // The old log is still there. A new session is not a way to lose an old one.
        assert!(t.0.join("first.jsonl").exists());
        assert!(t.0.join("second1.jsonl").exists());

        // And the server is really on the new one: a request naming the old id is refused.
        let out = lines_of(
            &mut server,
            &Request::new(
                3,
                gen::method::CONTEXT_EXPLAIN,
                json!({"session_id": "first"}),
            )
            .to_line(),
        );
        assert!(
            out.iter().any(|v| v.get("error").is_some()),
            "the closed session is not the current one: {out:?}"
        );
    }

    /// Purpose: Pin the honest refusal when a server has no factory for a second session.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn a_server_given_one_kernel_says_it_cannot_start_another() {
        // Not pretending: a server built around a single kernel has no way to make a
        // second, and saying so is better than half a session.
        let t = Temp::new();
        let kernel = Kernel::create(
            t.0.join("only.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(scripted(vec![], Some("hi"))),
        )
        .unwrap();
        let mut server = Server::new(kernel, "only");
        let out = lines_of(
            &mut server,
            &Request::new(1, gen::method::SESSION_NEW, json!({"session_id": "only"})).to_line(),
        );
        let error = out
            .iter()
            .find_map(|v| v.get("error"))
            .expect("an error, not a silent success");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("cannot begin another"),
            "{error:?}"
        );
        assert_eq!(server.session_id(), "only", "and nothing changed");
    }

    /// Purpose: Pin attach's result fields and that its opening event precedes the response.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn attach_answers_with_the_session_and_schema_version() {
        let t = Temp::new();
        let model = scripted(vec![], Some("hi"));
        let kernel = Kernel::create(
            t.0.join("s.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(model),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-local");
        let out = lines_of(&mut server, &attach_line());
        // Attaching streams the session's opening events (the skill catalogue),
        // then answers.
        let response = out
            .iter()
            .find(|line| line.get("result").is_some())
            .expect("a response");
        assert_eq!(response["result"]["session_id"], "s-local");
        assert_eq!(response["result"]["schema_version"], gen::SCHEMA_VERSION);
        assert_eq!(out.last().unwrap(), response, "the response comes last");
        let events: Vec<&str> = out
            .iter()
            .filter(|line| line["method"] == "event")
            .map(|line| line["params"]["kind"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(
            events,
            vec!["skills.loaded"],
            "the opening event, and only it"
        );
    }

    /// Purpose: Pin the turn's event order and the single trailing response.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn a_prompt_streams_events_then_answers() {
        let t = Temp::new();
        std::fs::write(t.0.join("a.txt"), "hello").unwrap();
        let model = scripted(
            vec![ToolCallRequest {
                call_id: "c1".into(),
                tool: "read_file".into(),
                args: json!({"path": "a.txt"}),
            }],
            Some("it says hello"),
        );
        let kernel = Kernel::create(
            t.0.join("s.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(model),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-local");
        let out = lines_of(
            &mut server,
            &Request::new(
                2,
                gen::method::PROMPT_SUBMIT,
                json!({"session_id": "s-local", "text": "read it"}),
            )
            .to_line(),
        );
        let events: Vec<&Value> = out.iter().filter(|v| v["method"] == "event").collect();
        let responses: Vec<&Value> = out.iter().filter(|v| v.get("result").is_some()).collect();
        assert_eq!(responses.len(), 1, "exactly one response");
        assert_eq!(responses[0]["result"]["accepted"], true);
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| e["params"]["kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                // The turn's own events, in order. The session's *opening* events
                // (`skills.loaded` and friends) belong to `session.attach`, which drains
                // them when it answers — each call streams what it caused.
                "message.appended",
                "cost.recorded",
                "message.appended",
                "tool.call",
                "policy.decided",
                "tool.result",
                "cost.recorded",
                "message.appended",
            ]
        );
        assert_eq!(
            out.last().unwrap()["result"]["turn_id"],
            1,
            "responses come last"
        );
    }

    /// Purpose: Pin the approval round trip over the wire and that nothing happens before the answer.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn an_out_of_bounds_write_asks_over_the_wire_and_resumes() {
        let t = Temp::new();
        let escape = t.0.parent().unwrap().join("loong-server-escape.txt");
        let _ = std::fs::remove_file(&escape);
        let model = scripted(
            vec![ToolCallRequest {
                call_id: "c1".into(),
                tool: "write_file".into(),
                args: json!({"path": escape.to_string_lossy(), "content": "x"}),
            }],
            Some("understood"),
        );
        let kernel = Kernel::create(
            t.0.join("s.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(model),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-local");
        let out = lines_of(
            &mut server,
            &Request::new(
                2,
                gen::method::PROMPT_SUBMIT,
                json!({"session_id": "s-local", "text": "escape"}),
            )
            .to_line(),
        );
        let asked = out
            .iter()
            .find(|v| v["method"] == "event" && v["params"]["kind"] == "approval.requested")
            .expect("the client is told an approval is pending");
        let request_id = asked["params"]["data"]["request_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(asked["params"]["data"]["action_class"], "out_of_bounds");
        assert!(!escape.exists(), "nothing happens before the answer");

        let out = lines_of(
            &mut server,
            &Request::new(
                3,
                gen::method::APPROVAL_RESPOND,
                json!({
                    "session_id": "s-local",
                    "request_id": request_id,
                    "granted": false,
                    "scope": "once",
                    "reason": "no"
                }),
            )
            .to_line(),
        );
        let kinds: Vec<&str> = out
            .iter()
            .filter(|v| v["method"] == "event")
            .map(|e| e["params"]["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds[0], "approval.decided");
        assert!(kinds.contains(&"tool.result"));
        assert_eq!(out.last().unwrap()["result"]["accepted"], false);
        assert!(!escape.exists());
    }

    /// Purpose: A model that writes its answer in pieces, for the delta path.
    /// Serves: `deltas_are_written_as_they_arrive_and_never_enter_the_log`.
    /// Design: `docs/design/surfaces.md` §7.
    /// Up: that test, through the kernel.
    /// Down: `Model::complete` and `Model::complete_streaming`.
    /// Verify: `cargo test -p loong-server`.
    /// A model that writes its answer in pieces, the way a real one does.
    struct StreamingModel;

    impl Model for StreamingModel {
        /// Purpose: The non-streaming path, which this test double never uses.
        /// Contract: the `Model` impl for `StreamingModel`.
        fn complete(
            &mut self,
            _request: &CompletionRequest,
        ) -> Result<CompletionResponse, ModelError> {
            unreachable!("used only through its streaming path")
        }
        /// Purpose: Emit the answer in three pieces and return the finished text.
        /// Serves: `deltas_are_written_as_they_arrive_and_never_enter_the_log`.
        /// Design: `docs/design/surfaces.md` §7.
        /// Up: the kernel's turn, through that test.
        /// Down: the `on_delta` callback.
        /// Verify: `cargo test -p loong-server`.
        fn complete_streaming(
            &mut self,
            _request: &CompletionRequest,
            on_delta: &mut dyn FnMut(&str),
        ) -> Result<CompletionResponse, ModelError> {
            for piece in ["the ", "answer ", "arrives"] {
                on_delta(piece);
            }
            Ok(CompletionResponse {
                text: Some("the answer arrives".to_string()),
                tool_calls: Vec::new(),
                usage: Usage {
                    tokens_in: 5,
                    tokens_out: 4,
                },
            })
        }
    }

    /// Purpose: Pin that events stream during the turn, in order, and none twice.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn a_turn_reports_its_work_before_it_is_over() {
        // The difference between watching an agent and waiting for one: the events reach
        // the connection while the turn is still running, in the order they happened,
        // and the response comes last.
        let t = Temp::new();
        std::fs::write(t.0.join("a.txt"), "hello").unwrap();
        let model = scripted(
            vec![ToolCallRequest {
                call_id: "c1".to_string(),
                tool: "read_file".to_string(),
                args: json!({"path": "a.txt"}),
            }],
            Some("done"),
        );
        let kernel = Kernel::create(
            t.0.join("c.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(model),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-live");

        // The attach's stream is not what this test is about; the prompt's is.
        let (out, _written) = Out::memory();
        let responses = server.handle_line_streaming(&attach_line(), &out);
        assert_eq!(
            responses.len(),
            1,
            "just the response: the events were streamed"
        );

        let (out, written) = Out::memory();
        let responses = server.handle_line_streaming(
            &Request::new(
                2,
                gen::method::PROMPT_SUBMIT,
                json!({"session_id": "s-live", "text": "read it"}),
            )
            .to_line(),
            &out,
        );
        let lines = written.borrow().clone();
        let kinds: Vec<String> = lines
            .iter()
            .filter(|line| line.contains("\"method\":\"event\""))
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|value| {
                value["params"]["kind"]
                    .as_str()
                    .map(|kind| kind.to_string())
            })
            .collect();
        assert!(
            kinds.contains(&gen::event::TOOL_CALL.to_string()),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&gen::event::TOOL_RESULT.to_string()),
            "{kinds:?}"
        );
        assert!(
            kinds.contains(&gen::event::MESSAGE_APPENDED.to_string()),
            "the answer is announced too: {kinds:?}"
        );
        let tool_call = kinds
            .iter()
            .position(|k| k == gen::event::TOOL_CALL)
            .unwrap();
        let tool_result = kinds
            .iter()
            .position(|k| k == gen::event::TOOL_RESULT)
            .unwrap();
        assert!(tool_call < tool_result, "in the order they happened");
        assert_eq!(responses.len(), 1);
        assert!(
            responses[0].contains("\"result\""),
            "and the response is the only thing left over"
        );
        // Nothing was streamed twice.
        assert_eq!(
            kinds.iter().filter(|k| *k == gen::event::TOOL_CALL).count(),
            1,
            "{kinds:?}"
        );
    }

    /// Purpose: Pin replay as a read that returns the log without moving it.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn a_client_that_attaches_can_replay_what_happened_before_it_arrived() {
        let t = Temp::new();
        let model = scripted(vec![], Some("noted"));
        let kernel = Kernel::create(
            t.0.join("c.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(model),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-replay");
        let _ = lines_of(&mut server, &attach_line());
        let _ = lines_of(
            &mut server,
            &Request::new(
                2,
                gen::method::PROMPT_SUBMIT,
                json!({"session_id": "s-replay", "text": "remember this"}),
            )
            .to_line(),
        );
        let before = loong_eventlog::replay(t.0.join("c.jsonl"))
            .unwrap()
            .events
            .len() as u64;

        let replayed = lines_of(
            &mut server,
            &Request::new(
                3,
                gen::method::SESSION_REPLAY,
                json!({"session_id": "s-replay"}),
            )
            .to_line(),
        );
        let events = replayed[0]["result"]["events"].as_array().unwrap();
        assert_eq!(events.len() as u64, before, "everything the log holds");
        assert!(
            events
                .iter()
                .any(|e| e["kind"] == "message.appended" && e["data"]["text"] == "remember this"),
            "{events:?}"
        );
        assert_eq!(events[0]["seq"], 1, "oldest first");
        assert!(
            replayed.iter().all(|line| line["method"] != "event"),
            "replaying is a read, not four new events: {replayed:?}"
        );
        assert_eq!(
            loong_eventlog::replay(t.0.join("c.jsonl"))
                .unwrap()
                .events
                .len() as u64,
            before,
            "and the log did not move"
        );
    }

    /// Purpose: Pin `context.explain` as a read, its shape and the wrong-session refusal.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn a_client_can_ask_what_is_in_the_context_without_changing_it() {
        let t = Temp::new();
        let model = scripted(vec![], Some("noted"));
        let kernel = Kernel::create(
            t.0.join("c.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(model),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-context");
        let attached = lines_of(&mut server, &attach_line());
        assert_eq!(attached[0]["method"], "event");

        let submitted = lines_of(
            &mut server,
            &Request::new(
                2,
                gen::method::PROMPT_SUBMIT,
                json!({"session_id": "s-context", "text": "remember this"}),
            )
            .to_line(),
        );
        let events_after_turn = submitted
            .iter()
            .filter(|line| line["method"] == "event")
            .count();
        assert!(events_after_turn > 0);

        let explained = lines_of(
            &mut server,
            &Request::new(
                3,
                gen::method::CONTEXT_EXPLAIN,
                json!({"session_id": "s-context"}),
            )
            .to_line(),
        );
        let result = &explained[0]["result"];
        assert!(result["context_tokens"].as_u64().unwrap() > 0, "{result}");
        assert!(result["messages"].as_u64().unwrap() >= 2, "{result}");
        assert!(result["tools"].as_u64().unwrap() > 0, "{result}");
        assert!(
            !result["entries"].as_array().unwrap().is_empty(),
            "{result}"
        );
        assert!(
            !result["categories"].as_array().unwrap().is_empty(),
            "{result}"
        );
        assert_eq!(result["compactions"], 0);
        assert!(
            explained.iter().all(|line| line["method"] != "event"),
            "asking is not an event: {explained:?}"
        );

        // And a session that is not this one is refused rather than answered.
        let wrong = lines_of(
            &mut server,
            &Request::new(
                4,
                gen::method::CONTEXT_EXPLAIN,
                json!({"session_id": "someone-else"}),
            )
            .to_line(),
        );
        assert!(wrong[0]["error"].is_object(), "{wrong:?}");
    }

    /// Purpose: Pin `model.delta` streaming and that the log keeps only the finished message.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn deltas_are_written_as_they_arrive_and_never_enter_the_log() {
        let t = Temp::new();
        let kernel = Kernel::create(
            t.0.join("s.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(StreamingModel),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-stream");
        assert_eq!(lines_of(&mut server, &attach_line())[0]["method"], "event");

        let (live, streamed) = Out::memory();
        let responses = server.handle_line_streaming(
            &Request::new(
                2,
                gen::method::PROMPT_SUBMIT,
                json!({"session_id": "s-stream", "text": "say something"}),
            )
            .to_line(),
            &live,
        );

        // The deltas reached the connection, in order, as `model.delta` notifications.
        let streamed = streamed.borrow().clone();
        let deltas: Vec<Value> = streamed
            .iter()
            .map(|line| serde_json::from_str(line).expect("JSON"))
            .filter(|value: &Value| value["method"] == "model.delta")
            .collect();
        assert_eq!(deltas.len(), 3, "{deltas:?}");
        assert!(deltas.iter().all(|d| d["method"] == "model.delta"));
        let pieces: Vec<&str> = deltas
            .iter()
            .map(|d| d["params"]["text"].as_str().unwrap())
            .collect();
        assert_eq!(pieces, vec!["the ", "answer ", "arrives"]);
        assert_eq!(deltas[0]["params"]["turn_id"], 1);

        // The connection (and the log) hold the finished message, not fragments.
        let out: Vec<Value> = streamed
            .iter()
            .map(|line| serde_json::from_str(line).expect("JSON"))
            .collect();
        assert!(
            out.iter()
                .all(|line| line["method"] != "event" || line["params"]["kind"] != "model.delta"),
            "a delta is never an event: {out:?}"
        );
        assert_eq!(responses.len(), 1, "only the response is returned now");
        let answer = out
            .iter()
            .filter(|line| line["method"] == "event")
            .filter(|line| line["params"]["kind"] == "message.appended")
            .map(|line| line["params"]["data"]["text"].as_str().unwrap_or(""))
            .next_back()
            .unwrap();
        assert_eq!(answer, "the answer arrives");

        let logged = std::fs::read_to_string(t.0.join("s.jsonl")).unwrap();
        assert!(
            !logged.contains("model.delta"),
            "the log never holds a delta"
        );
        assert!(logged.contains("the answer arrives"));
    }

    /// Purpose: Pin the `hook.fired` event and `accepted: false` on a hook refusal.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn a_hook_refusal_is_reported_as_not_accepted() {
        let t = Temp::new();
        let hooks = t.0.join(".loongagent");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(
            hooks.join("gate.sh"),
            "cat > /dev/null\necho 'not today' >&2\nexit 2\n",
        )
        .unwrap();
        std::fs::write(
            hooks.join("hooks.toml"),
            "[[hook]]\nid = \"gate\"\nevent = \"user_prompt_submit\"\nkind = \"shell\"\nscript = \"gate.sh\"\n",
        )
        .unwrap();
        loong_hooks::HookSet::load(&t.0)
            .unwrap()
            .trust_now()
            .unwrap();

        // An empty script: if the model were called the turn would fail instead.
        let kernel = Kernel::create(
            t.0.join("s.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(ScriptedModel::new(vec![])),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-hooks");
        let attached = lines_of(&mut server, &attach_line());
        let response = attached
            .iter()
            .find(|line| line.get("result").is_some())
            .unwrap();
        assert_eq!(response["result"]["session_id"], "s-hooks");

        let out = lines_of(
            &mut server,
            &Request::new(
                2,
                gen::method::PROMPT_SUBMIT,
                json!({"session_id": "s-hooks", "text": "refactor everything"}),
            )
            .to_line(),
        );
        let kinds: Vec<&str> = out
            .iter()
            .filter(|line| line["method"] == "event")
            .map(|line| line["params"]["kind"].as_str().unwrap_or(""))
            .collect();
        assert!(
            kinds.contains(&"hook.fired"),
            "the reason travels in the stream: {kinds:?}"
        );
        assert_eq!(out.last().unwrap()["result"]["accepted"], false, "{out:?}");
        assert_eq!(out.last().unwrap()["result"]["turn_id"], 1);
    }

    /// Purpose: Pin that parse, unknown-method and wrong-session errors are answered, not ignored.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn protocol_errors_are_answered_not_ignored() {
        let t = Temp::new();
        let kernel = Kernel::create(
            t.0.join("s.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(ScriptedModel::saying("hi")),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-local");
        assert_eq!(
            lines_of(&mut server, "{oops")[0]["error"]["code"],
            codes::PARSE_ERROR as i64
        );
        assert_eq!(
            lines_of(
                &mut server,
                &Request::new(9, "nope.method", json!({})).to_line()
            )[0]["error"]["code"],
            codes::METHOD_NOT_FOUND as i64
        );
        let wrong = lines_of(
            &mut server,
            &Request::new(
                10,
                gen::method::PROMPT_SUBMIT,
                json!({"session_id": "other", "text": "x"}),
            )
            .to_line(),
        );
        assert_eq!(wrong[0]["error"]["code"], codes::INVALID_PARAMS as i64);
    }

    /// Purpose: The one response line in a batch that may also carry streamed events.
    /// Contract: test helper for the model-surface tests.
    /// The one response line in a batch that may also carry streamed events.
    fn reply(lines: &[String]) -> Value {
        lines
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| value.get("result").is_some() || value.get("error").is_some())
            .unwrap_or(Value::Null)
    }

    /// Purpose: Pin model list/select results and the recorded `model.selected` event.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn the_model_surface_lists_what_a_session_has_and_switches_on_request() {
        let t = Temp::new();
        let kernel = Kernel::create_with_models(
            t.0.join("models.jsonl"),
            KernelConfig::new(&t.0),
            vec![
                (
                    "cheap".to_string(),
                    Box::new(ScriptedModel::saying("cheap")),
                ),
                (
                    "strong".to_string(),
                    Box::new(ScriptedModel::saying("strong")),
                ),
            ],
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-models");
        let (out, _written) = Out::memory();

        // Listing is a read of the session's own configuration, so only the response
        // comes back — nothing is recorded by asking.
        let listed = server.handle_line_streaming(
            &Request::new(
                1,
                gen::method::MODEL_LIST,
                json!({"session_id": "s-models"}),
            )
            .to_line(),
            &out,
        );
        let value = reply(&listed);
        assert_eq!(value["result"]["selected"], "cheap");
        let names: Vec<&str> = value["result"]["models"]
            .as_array()
            .expect("a model list")
            .iter()
            .filter_map(|model| model["name"].as_str())
            .collect();
        assert_eq!(names, vec!["cheap", "strong"]);

        // A switch answers with the state *afterwards*, so a client never has to ask
        // again to find out whether it worked.
        let switched = server.handle_line_streaming(
            &Request::new(
                2,
                gen::method::MODEL_SELECT,
                json!({"session_id": "s-models", "name": "strong"}),
            )
            .to_line(),
            &out,
        );
        let value = reply(&switched);
        assert_eq!(value["result"]["selected"], "strong");
        // The choice is an event like any other model change. It is not in *these* lines
        // because the live sink is installed while a turn runs, and a switch between
        // turns is a read-then-write rather than something happening under a reader — so
        // the record is what this checks. The live case is the kernel's own test.
        let logged = loong_eventlog::replay(&t.0.join("models.jsonl"))
            .unwrap()
            .events;
        let recorded = logged
            .iter()
            .find(|event| event.kind == gen::event::MODEL_SELECTED)
            .expect("the switch is in the log");
        assert_eq!(recorded.data["model"], "strong");
        assert_eq!(recorded.data["previous"], "cheap");

        // A name nobody offers is refused rather than stored.
        let refused = server.handle_line_streaming(
            &Request::new(
                3,
                gen::method::MODEL_SELECT,
                json!({"session_id": "s-models", "name": "nope"}),
            )
            .to_line(),
            &out,
        );
        let value = reply(&refused);
        assert!(
            value["error"].is_object(),
            "a bad name is an error: {value}"
        );
        assert_eq!(value["error"]["code"], codes::INVALID_PARAMS as i64);

        // And the session still runs the model it actually has.
        let after = server.handle_line_streaming(
            &Request::new(
                4,
                gen::method::MODEL_LIST,
                json!({"session_id": "s-models"}),
            )
            .to_line(),
            &out,
        );
        assert_eq!(reply(&after)["result"]["selected"], "strong");
    }

    /// Purpose: Pin the wrong-session refusal for both model methods.
    /// Verify: `cargo test -p loong-server`.
    #[test]
    fn the_model_surface_refuses_a_session_that_is_not_this_one() {
        let t = Temp::new();
        let kernel = Kernel::create(
            t.0.join("models.jsonl"),
            KernelConfig::new(&t.0),
            Box::new(ScriptedModel::saying("hi")),
        )
        .unwrap();
        let mut server = Server::new(kernel, "s-models");
        let (out, _written) = Out::memory();

        for method in [gen::method::MODEL_LIST, gen::method::MODEL_SELECT] {
            let lines = server.handle_line_streaming(
                &Request::new(
                    1,
                    method,
                    json!({"session_id": "someone-else", "name": "default"}),
                )
                .to_line(),
                &out,
            );
            let value = reply(&lines);
            assert_eq!(
                value["error"]["code"],
                codes::INVALID_PARAMS as i64,
                "{method} must not answer for another session: {value}"
            );
        }
    }
}
