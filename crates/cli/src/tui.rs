//! Purpose: The TUI's composition: the child-process transport and the full-screen surface that owns the terminal.
//!
//! # Contract
//! - **Serves:** `loongagent tui` and `session.rs`'s `resume`, which re-enters through the same surface.
//! - **Design:** `docs/design/surfaces.md` §2 and §3.
//! - **Upstream:** `main.rs`'s `tui` arm and `session.rs`'s `resume` call `full_screen`; `repl`, `headless` and `web` use `ChildTransport`.
//! - **Downstream:** `loong_tui::Client` and `interrupt`, `loong_tui_app::{run, setup_form}`, `control.rs`'s pack cache and `local.rs`'s first-run wizard.
//! - **Verify:** `cargo test -p loongagent --test tui_surface` · `./scripts/check.sh`

use super::*;

use crate::control::{cached_pack_state, connect, load_control};
use crate::local::{configure_local, session_needs_a_model, spawn_model_health};
use crate::session::session_intro;

// ---------------------------------------------------------------- the client

/// Purpose: The pipe transport to one server child, plus the handle that lets the interface stop a turn.
/// Serves: `full_screen`, `repl`, `headless` and `web` — every surface that spawns its own server.
/// Design: `docs/design/surfaces.md` §3 and §7.
/// Up: `full_screen`, `repl`, `headless` and `web` construct one.
/// Down: the `std::process::Child`, its stdin/stdout/stderr and the `loong_tui::Transport` impl.
/// Verify: `cargo test -p loongagent --test tui_surface`.
/// The TUI's transport: it spawns the server as a child process and talks to it
/// over pipes. Same binary, different mode (ADR-0009).
pub(crate) struct ChildTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ChildTransport {
    /// Purpose: The child's process id, so the interface can signal a turn without waiting for its blocked read.
    /// Contract: `tui.rs` transport; `full_screen` is the only caller.
    /// The server process, so the interface can ask it to stop a turn (SIGINT) without the
    /// client's blocked read standing in the way.
    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Purpose: Start the same binary as `server --stdio`, carrying this run's session layer to it, and optionally pipe its diagnostics.
    /// Serves: `full_screen`, `repl`, `headless` and `web`.
    /// Design: `docs/design/surfaces.md` §3 (each surface spawns its own child).
    /// Up: `full_screen`, `repl`, `headless` and `web`; no other caller.
    /// Down: `std::process::Command`, `LOONGAGENT_SERVER_BIN` and a reader thread when notes are captured.
    /// Verify: `cargo test -p loongagent --test tui_surface`.
    /// `capture_notes` pipes the server's stderr instead of inheriting it: a
    /// full-screen UI owns the terminal, so diagnostics must be drawn, not dumped.
    pub(crate) fn spawn(
        args: &Args,
        capture_notes: bool,
    ) -> Result<(Self, Option<Receiver<String>>, Option<Box<dyn Fn(bool)>>), String> {
        // Same binary by default; overridable so a *test* binary can point at the
        // built server (an integration test's current_exe is the test harness).
        let exe = match std::env::var_os("LOONGAGENT_SERVER_BIN") {
            Some(path) => PathBuf::from(path),
            None => std::env::current_exe().map_err(|e| e.to_string())?,
        };
        let mut command = Command::new(exe);
        // Windows: the server leads its own process group, which is how `interrupt()`
        // aims a console control event at it alone (`GenerateConsoleCtrlEvent` takes a
        // group id, and a `CTRL_C_EVENT` could not be limited to one group). Unix needs
        // nothing here — `kill` addresses a single pid directly.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        // The subcommand comes first: flags before it would be read as the command.
        command.arg("server");
        // The session layer travels with the child: a profile file the caller named,
        // and the flags they typed. Everything else the child resolves itself from
        // the same layers, so the two cannot disagree about the configuration.
        if let Some(profile) = &args.profile {
            command.arg("--profile").arg(profile);
        }
        // The child resolves the same layers, so it would usually reach the same
        // answer on its own — but only for the profile's `mode`, not for the flag. A
        // `--local` on the command line exists in no file, so it has to be carried
        // across, or the interface would be local and its own server would not.
        if args.centered {
            command.arg("--centered");
        } else if args.local {
            command.arg("--local");
        }
        if let Some(model) = &args.model {
            command.arg("--model").arg(model);
        }
        if args.debug_model {
            command.arg("--debug-model");
        }
        if let Some(budget) = args.budget {
            command.arg("--budget").arg(budget.to_string());
        }
        if let Some(keep) = args.keep_newest {
            command.arg("--keep-newest").arg(keep.to_string());
        }
        if let Some(steps) = args.max_steps {
            command.arg("--max-steps").arg(steps.to_string());
        }
        // The private control channel: how `/yolo` reaches a turn that owns the
        // connection. A socketpair is made here; the read end is handed to the child as
        // descriptor 3, and the write end never leaves this process — so nothing the
        // agent runs (a shell tool, an MCP server) can reach it and turn the prompts off.
        // Windows has no descriptor inheritance; there the surface keeps the queued
        // behaviour, and `ARGV` below is not passed.
        #[cfg(unix)]
        let (_control_read, control_write) = {
            use std::os::unix::io::AsRawFd;
            use std::os::unix::process::CommandExt;
            let (write, read) = std::os::unix::net::UnixStream::pair()
                .map_err(|e| format!("the control channel: {e}"))?;
            let read_fd = read.as_raw_fd();
            // Safety: this runs after fork and before exec, and `dup2` allocates nothing
            // and takes no lock.
            unsafe {
                command.pre_exec(move || {
                    if read_fd == 3 {
                        // Already the number the child expects. `dup2` onto itself does
                        // nothing on Linux, and the descriptor still carries `CLOEXEC`,
                        // which would close it at exec — so clear that flag instead.
                        let flags = libc::fcntl(3, libc::F_GETFD);
                        if flags < 0 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                    } else if libc::dup2(read_fd, 3) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
            command.arg("--control-fd").arg("3");
            (read, write)
        };
        let mut child = command
            .arg("--stdio")
            .arg("--session")
            .arg(args.session_path())
            .arg("--cwd")
            .arg(&args.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if capture_notes {
                Stdio::piped()
            } else {
                Stdio::inherit()
            })
            .spawn()
            .map_err(|e| format!("spawning the local server: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin on the server child")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout on the server child")?);
        let notes = if capture_notes {
            child.stderr.take().map(|stderr| {
                let (sender, receiver) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        if sender.send(line).is_err() {
                            break;
                        }
                    }
                });
                receiver
            })
        } else {
            None
        };
        // The closure the screen calls when `/yolo` must reach a running turn. Writing
        // to the pipe cannot fail silently in a way that matters: if it does, the turn
        // that owns the connection is already gone. The `permissions.relaxed` event is
        // the confirmation; the write only asks.
        #[cfg(unix)]
        let control: Option<Box<dyn Fn(bool)>> = {
            let write = std::sync::Arc::new(control_write);
            Some(Box::new(move |on: bool| {
                use std::io::Write;
                let line = if on { "yolo on\n" } else { "yolo off\n" };
                let _ = (&*write).write_all(line.as_bytes());
            }))
        };
        #[cfg(not(unix))]
        let control: Option<Box<dyn Fn(bool)>> = None;
        Ok((
            Self {
                child,
                stdin,
                stdout,
            },
            notes,
            control,
        ))
    }
}

impl Transport for ChildTransport {
    /// Purpose: Write one complete JSON-RPC line to the child and flush it.
    /// Contract: `tui.rs` `Transport` impl, driven by `loong_tui::Client`.
    fn send(&mut self, line: &str) -> Result<(), String> {
        writeln!(self.stdin, "{line}").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())
    }

    /// Purpose: Read one line back, or `None` when the child closed the pipe.
    /// Contract: `tui.rs` `Transport` impl, driven by `loong_tui::Client`.
    fn recv_line(&mut self) -> Result<Option<String>, String> {
        let mut buf = String::new();
        match self.stdout.read_line(&mut buf) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(buf.trim_end().to_string())),
            Err(e) => Err(e.to_string()),
        }
    }
}

impl Drop for ChildTransport {
    /// Purpose: Kill and reap the child so no server outlives the surface that spawned it.
    /// Contract: `tui.rs` `Drop` impl for the transport.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Purpose: Resolve settings, make sure a model exists, spawn the child and hand the client to `loong_tui_app::run`.
/// Serves: `loongagent tui` and `loongagent resume`.
/// Design: `docs/design/surfaces.md` §2 and §3.
/// Up: `main.rs`'s `tui` arm and `session.rs`'s `resume`; no other caller.
/// Down: `Args::{is_local, settings_with_pack, session_id}`, `ChildTransport::{spawn, pid}`, `Client::{attach, submit}` and `loong_tui_app::run`.
/// Verify: `cargo test -p loongagent --test tui_surface`.
/// The full-screen surface: the client here, the screen in `loong-tui-app`.
pub(crate) fn full_screen(args: Args) -> Result<(), String> {
    args.ensure_parent()?;
    // Settled first, because it decides whether a control plane is consulted at all.
    let local = args.is_local()?;
    // Resolved once here and reused by the gate below, so the configuration is read and
    // judged the same way twice rather than twice differently.
    let mut resolved_settings: Option<loong_config::Profile> = None;
    // Before the child exists, not after: the server resolves its models once, at
    // startup, so a key written later would not take effect until the next run — and
    // the run it was written for would look broken.
    //
    // Only when there is nothing to run. A configured install starts straight away;
    // adding a *second* provider is `loongagent local add`, so nobody has to sit through
    // a form they did not ask for every time they open a session.
    if local && args.model.is_none() {
        let (resolved, _) = args.settings_with_pack()?;
        configure_local(&resolved, false, None)?;
        resolved_settings = Some(resolved);
    }
    // A session with no model is a session that cannot answer anything, and its first few
    // minutes are spent discovering that. A local install is the one place the person can
    // do something about it, so it is the one place worth refusing — and the message names
    // the command rather than opening a screen that will fail.
    //
    // A *managed* install keeps the old behaviour on purpose: models arrive in a pack the
    // user cannot edit, so refusing would strand them with no interface and no way to fix
    // it. There the server runs an `UnconfiguredModel` that says so inside a working
    // session, which is what that type exists for.
    if local && args.model.is_none() && resolved_settings.is_none() {
        resolved_settings = Some(args.settings_with_pack()?.0);
    }
    if let Some(profile) = &resolved_settings {
        session_needs_a_model(&args, local, profile)?;
    }
    let (transport, notes, control) = ChildTransport::spawn(&args, true)?;
    // Taken before the transport moves into the client: the interface needs the pid to
    // stop a turn, and the client is blocked on the transport while one runs.
    let server_pid = transport.pid();
    let mut client = Client::new(transport, args.session_id());
    let attached = match client.attach(&args.cwd.to_string_lossy(), "tui") {
        Ok(attached) => attached,
        Err(e) => {
            // The child's own explanation is the useful one (a corrupt log, a bad
            // profile); without this the user sees only "the connection closed".
            if let Some(notes) = &notes {
                let mut tail: Vec<String> = Vec::new();
                while let Ok(line) = notes.try_recv() {
                    tail.push(line);
                }
                for line in tail.iter().rev().take(5).rev() {
                    eprintln!("[server] {line}");
                }
            }
            return Err(e);
        }
    };
    // First run: ask here, in the terminal UI, rather than sending the user off to
    // discover a command. Doing it before the session starts is also why nothing has
    // to be reconfigured mid-session.
    //
    // Local mode never asks this. There is no control plane to name, and offering the
    // form anyway would be the one thing `--local` promises not to do.
    if !local && load_control()?.is_none() && args.model.is_none() {
        // In a pipe the form cannot be drawn: refuse first, in the same words the TUI
        // would use, rather than asking for three fields that cannot be seen.
        loong_tui_app::full_screen_needs_a_terminal()?;
        if let Some((url, public_key, token)) = loong_tui_app::setup_form()? {
            let file = ControlFile {
                url,
                public_key,
                token: if token.trim().is_empty() {
                    None
                } else {
                    Some(token)
                },
            };
            match connect(&file, false) {
                Ok((path, synced, pack)) => eprintln!(
                    "wrote {} — verified {} {} ({} profiles)",
                    path.display(),
                    synced.envelope.issuer,
                    synced.envelope.version,
                    pack.profiles.len()
                ),
                Err(e) => return Err(format!("not connected: {e}")),
            }
        }
    }

    // What a first run needs to see, inside the UI rather than on a stderr that the
    // alternate screen would overwrite: where this session lives, and whether a model
    // is configured at all.
    // The client resolves the same layers the server does, so it can tell the user
    // whether a model exists without asking the server about it.
    let (settings, pack) = args.settings_with_pack()?;
    // Started before the interface opens and finishing whenever it finishes: every model
    // is usable from the first frame, and the check marks fill in behind them.
    let health = spawn_model_health(&settings);
    if let Some(state) = &pack {
        eprintln!(
            "[control] {} {} — profile: {}",
            state.provenance.issuer,
            state.provenance.version,
            state.summary()
        );
    }
    let session_id = args.session_id();
    let mut intro = vec![session_intro(
        args.session_source,
        &session_id,
        attached.existing_events,
    )];
    if attached.existing_events > 0 {
        intro.push(format!(
            "resuming {} events from that session",
            attached.existing_events
        ));
    }
    match cached_pack_state(&args) {
        // In local mode there is no pack to consult, and consulting one anyway would
        // be the dependency `--local` exists to remove — so `cached_pack_state` is not
        // called at all, rather than being called and expected to return nothing.
        _ if local => intro.push(match settings.models.len() {
            0 => "本地模式：还没有配置模型 — 粘贴一个 API key 即可开始（loongagent models add）"
                .to_string(),
            n => format!("本地模式：{n} 个模型已配置（不连接控制平面）"),
        }),
        Ok(Some(state)) => intro.push(format!("profile: {}", state.summary())),
        Ok(None) if args.model.is_none() => intro.push(
            "not connected to a control plane — run `loongagent setup` with the address your \
             firm gave you (or pass --model demo-write to try the loop with no model)"
                .to_string(),
        ),
        Ok(None) => {}
        Err(e) => intro.push(format!("control plane: {e}")),
    }
    let outcome = loong_tui_app::run(
        client,
        attached.session_id,
        attached.schema_version,
        notes,
        intro,
        Some(Box::new(move || loong_tui::interrupt(server_pid))),
        control,
        health,
    );
    // Leaving the interface — by `/exit`, ctrl-c, or an error — says where the session
    // went, so the way out is not a dead end.
    println!("session {session_id}");
    println!("reopen:  loongagent resume {session_id}");
    outcome
}
