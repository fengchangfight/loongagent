//! Purpose: The full-screen event loop: replay the past, take the terminal, and turn keys and worker messages into frames.
//!
//! # Contract
//! - **Serves:** `crates/cli/src/tui.rs`, the only production caller.
//! - **Design:** `docs/design/surfaces.md` §3 and §6; `docs/design/sessions-lineage.md` §1 and §5.
//! - **Upstream:** `crates/cli/src/tui.rs` calls `run` with a connected `Client`.
//! - **Downstream:** `App` for state and drawing, `spawn_worker` for the client thread, `copy_everywhere` for `/copy`, and `loong_tui::Client` for replay and models.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
use super::*;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Purpose: Replay the session, spawn the worker, own the terminal and loop: drain, draw, read a key, act, until quit.
/// Serves: `crates/cli/src/tui.rs`.
/// Design: `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §1 (replay reads the log) and §5.
/// Up: `crates/cli/src/tui.rs`.
/// Down: `Client::{replay, folded_results, models}`, `spawn_worker`, `App` and `copy_everywhere`.
/// Verify: `cargo test -p loong-tui-app`.
pub fn run<T: Transport + Send + 'static>(
    client: Client<T>,
    session: String,
    schema: String,
    history_path: std::path::PathBuf,
    notes: Option<Receiver<String>>,
    intro: Vec<String>,
    interrupt: Option<Box<dyn Fn()>>,
    permissions: Option<Box<dyn Fn(bool)>>,
    health: Option<Receiver<ModelHealth>>,
) -> Result<(), String> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, KeyEventKind,
    };
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::Terminal;

    // A session that already has a past: show it before taking over the terminal, so a
    // reattached conversation looks reattached rather than new. Reading it records
    // nothing (ADR-0009: the log is the record, and this is a read of it).
    let mut client = client;
    let history = client.replay().unwrap_or_else(|_| Vec::new());
    // The bill the session already ran up. It came with the same replay read, so a
    // resumed session does not start at zero and then pretend its past was free.
    let costs = client.costs();
    // The full text of the folds that history shows, so opening one is a local act from the
    // first key press: a turn that is already running owns the connection, and the view
    // should not have to wait for it to read its own transcript.
    let folded = client.folded_results().unwrap_or_default();
    // Which models this session offers, read before the worker takes the connection —
    // one more read on a client that is already reading twice, and it means the bar is
    // there on the first frame instead of appearing after it.
    let models = client.models().ok();
    let shown = history
        .iter()
        .filter(|line| loong_tui::is_fold_marker(line))
        .count();
    let (to_worker, from_worker) = spawn_worker(client, shown);
    // The usage row needs the effective budget before the first turn ends. The worker
    // refreshes the context after every command, but a fresh session has not run one
    // yet; asking once here is what makes the first turn's percentage appear at all.
    let _ = to_worker.send(ToWorker::RefreshContext);
    let mut app = App::new(session, schema);
    // Read before the first frame: the up-arrow should work on the first key, not after
    // the first prompt of the run. A missing file is an empty history, not an error.
    app.load_history(history_path);
    app.spend = Spend::from_events(&costs);
    app.interrupt = interrupt;
    app.set_permissions = permissions;
    if let Some(list) = models {
        app.set_models(list);
    }
    for (at, text) in folded.into_iter().enumerate().take(shown) {
        app.remember_fold(at, text.split('\n').map(|line| line.to_string()).collect());
    }
    app.push_lines(history);
    // Intro lines belong in the conversation view, not the one-line diagnostics
    // strip: the session path and "no model configured" both need to be readable.
    app.push_lines(
        intro
            .into_iter()
            .map(|line| format!("· {line}"))
            .collect::<Vec<_>>(),
    );

    // Windows: the whole interface is UTF-8 — box drawing, the spinner, and Chinese in
    // both the messages and the model names — but a console starts on a legacy code page
    // (437, 936, …) and renders those bytes as mojibake. Rust writes UTF-8 to the console
    // handle, so telling the console that its code page *is* UTF-8 is the whole fix.
    // Best-effort: a console-less stdout simply fails here, and the "no terminal on
    // stdout" guard is what reports that.
    #[cfg(windows)]
    {
        // `kernel32` — the console input/output code pages.
        extern "system" {
            /// Purpose: The Windows console output code page setter, declared from `kernel32` rather than owned here.
            /// Contract: declaration only; `run` calls it under `#[cfg(windows)]` to switch the console to UTF-8.
            fn SetConsoleOutputCP(code_page: u32) -> i32;
            /// Purpose: The Windows console input code page setter, declared from `kernel32` rather than owned here.
            /// Contract: declaration only; `run` calls it under `#[cfg(windows)]` beside the output setter.
            fn SetConsoleCP(code_page: u32) -> i32;
        }
        const CP_UTF8: u32 = 65_001;
        // Safety: two integer arguments, no memory contract.
        unsafe {
            SetConsoleOutputCP(CP_UTF8);
            SetConsoleCP(CP_UTF8);
        }
    }

    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = std::io::stdout();
    // Mouse capture so the wheel scrolls the conversation. Text selection then needs
    // the terminal's modifier (Shift or Option) — the usual trade for this.
    // Deliberately **not** `EnableMouseCapture`: see `App::mouse`.
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste).map_err(|e| e.to_string())?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let outcome = (|| -> Result<(), String> {
        // A terminal that spells Meta as an ESC prefix — xterm's `metaSendsEscape`,
        // and the readline convention — sends Alt+Left as ESC ESC [ D. Crossterm
        // reads the first two bytes as one bare Esc and then hands over `[` and `D`
        // as characters, so the key would be typed into the prompt rather than
        // reorder anything. `MetaPrefix` holds that start for one short poll.
        let mut prefix = MetaPrefix::default();
        loop {
            app.drain(&from_worker);
            // A background check reports when it reports; the bar is redrawn either way,
            // so a slow provider never holds up the first frame.
            if let Some(health) = &health {
                while let Ok(found) = health.try_recv() {
                    app.note_health(found);
                }
            }
            if let Some(notes) = &notes {
                while let Ok(note) = notes.try_recv() {
                    app.push_note(note);
                }
            }
            terminal
                .draw(|frame| app.draw(frame))
                .map_err(|e| e.to_string())?;

            let wait = if prefix.is_holding() {
                std::time::Duration::from_millis(30)
            } else {
                std::time::Duration::from_millis(80)
            };
            let mut keys: Vec<KeyEvent> = Vec::new();
            let mut mouse = None;
            let mut pasted = None;
            if event::poll(wait).map_err(|e| e.to_string())? {
                match event::read().map_err(|e| e.to_string())? {
                    // Crossterm's Windows backend reports a Release alongside
                    // every Press; only Press may act.
                    event::Event::Key(key) if key.kind == KeyEventKind::Press => {
                        keys = prefix.feed(key);
                    }
                    // A held prefix is older than the mouse or the paste: settle it
                    // first, so an Esc still closes what it was closing.
                    event::Event::Mouse(found) => {
                        keys = prefix.flush();
                        mouse = Some(found);
                    }
                    // A bracketed paste arrives as one block, including its newlines: it
                    // goes into the input, and enter later sends the whole thing.
                    event::Event::Paste(text) => {
                        keys = prefix.flush();
                        pasted = Some(text);
                    }
                    _ => {}
                }
            } else {
                keys = prefix.flush();
            }
            for key in keys {
                if act(&mut app, &mut terminal, &to_worker, key)? {
                    return Ok(());
                }
            }
            if let Some(mouse) = mouse {
                app.handle_mouse(mouse.kind);
            }
            if let Some(text) = pasted {
                app.paste(&text);
            }
            if app.quit {
                return Ok(());
            }
        }
    })();

    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )
    .ok();
    terminal.show_cursor().ok();
    outcome
}

/// Purpose: Hold the start of a Meta-prefix arrow and rejoin it as the one Alt+arrow it means.
/// Serves: `run`'s key arm.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `run`.
/// Down: no call; `feed` and `flush` return the keys that `act` handles.
/// Verify: `cargo test -p loong-tui-app` (the `MetaPrefix` tests).
/// A terminal that encodes Meta as an ESC prefix sends Alt+Left as ESC ESC [ D.
/// Crossterm consumes the first two bytes as one bare `Esc`, then reports `[` and
/// `D` as ordinary characters — so the whole thing would be typed into the prompt
/// instead of reordering anything. Holding those first keys for one short poll is
/// what puts the key back together.
///
/// A real `Esc` followed by a key is not swallowed: `feed` gives it back as soon
/// as the next key is not the opener of an arrow sequence. `Esc` then `Left` or
/// `Right` as two separate events is also rejoined, because that is the same
/// split seen from the other side of a slow read; `Esc` then `Up`/`Down` is left
/// alone, since closing a menu and walking the history is a useful pair.
#[derive(Default)]
pub(crate) struct MetaPrefix {
    /// The keys held so far: nothing, `Esc`, or `Esc` and the `[`/`O` that starts
    /// the arrow sequence. Never longer than two.
    held: Vec<KeyEvent>,
}

impl MetaPrefix {
    /// Purpose: Take one key and return the keys that are now settled, in order.
    /// Serves: `run`'s key arm.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `run`.
    /// Down: no call; the result goes to `act`.
    /// Verify: `cargo test -p loong-tui-app` (the `MetaPrefix` test).
    /// An empty return means a prefix is being held and the next key decides.
    pub(crate) fn feed(&mut self, key: KeyEvent) -> Vec<KeyEvent> {
        let bare = key.kind == KeyEventKind::Press && key.modifiers.is_empty();
        // Crossterm marks an uppercase character with SHIFT even when the user
        // never pressed it: `ESC [ C` arrives as `Char('C')` + SHIFT. The letters
        // that end the sequence are uppercase by definition, so they are accepted
        // with that synthesized modifier and no other.
        let letter = key.kind == KeyEventKind::Press
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT);
        if self.held.is_empty() {
            if bare && key.code == KeyCode::Esc {
                self.held.push(key);
                return Vec::new();
            }
            return vec![key];
        }
        // Two Esc bytes can arrive in different reads; the later one is the prefix,
        // so the earlier one is dropped rather than acted on.
        if bare && key.code == KeyCode::Esc {
            if self.held.len() == 1 {
                self.held[0] = key;
                return Vec::new();
            }
            let mut settled = std::mem::take(&mut self.held);
            settled.push(key);
            return settled;
        }
        if self.held.len() == 1 {
            // The arrow sequence's own opener, or an arrow crossterm parsed directly.
            if letter && matches!(key.code, KeyCode::Char('[') | KeyCode::Char('O')) {
                self.held.push(key);
                return Vec::new();
            }
            if bare && matches!(key.code, KeyCode::Left | KeyCode::Right) {
                self.held.clear();
                return vec![KeyEvent::new(key.code, KeyModifiers::ALT)];
            }
            let mut settled = std::mem::take(&mut self.held);
            settled.push(key);
            return settled;
        }
        // `Esc` and the opener are held: the final byte names the arrow. Up and
        // Down are settled as Alt+Up/Alt+Down (which this screen ignores) rather
        // than left to be typed as `[A`/`[B`.
        if letter {
            let arrow = match key.code {
                KeyCode::Char('C') => Some(KeyCode::Right),
                KeyCode::Char('D') => Some(KeyCode::Left),
                KeyCode::Char('A') => Some(KeyCode::Up),
                KeyCode::Char('B') => Some(KeyCode::Down),
                _ => None,
            };
            if let Some(code) = arrow {
                self.held.clear();
                return vec![KeyEvent::new(code, KeyModifiers::ALT)];
            }
        }
        let mut settled = std::mem::take(&mut self.held);
        settled.push(key);
        settled
    }

    /// Purpose: Hand back whatever a timeout left held, in the order it arrived.
    /// Serves: `run`'s key arm, on the poll that times out.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `run`.
    /// Down: no call; the result goes to `act`.
    /// Verify: `cargo test -p loong-tui-app` (the `MetaPrefix` test).
    pub(crate) fn flush(&mut self) -> Vec<KeyEvent> {
        std::mem::take(&mut self.held)
    }

    /// Purpose: Whether a prefix is being held, so the loop can poll briefly for its rest.
    /// Serves: `run`'s key arm.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `run`.
    /// Down: no call; it reads `MetaPrefix::held`.
    /// Verify: `cargo test -p loong-tui-app` (the `MetaPrefix` test).
    pub(crate) fn is_holding(&self) -> bool {
        !self.held.is_empty()
    }
}

/// Purpose: Carry out one key's intent — send it to the worker, copy, flip the mouse, or quit.
/// Serves: `run`'s key arm, including the Alt+arrow rejoin.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `run`.
/// Down: `App::handle_key`, `to_worker`, `copy_everywhere` and the backend's mouse capture.
/// Verify: `cargo test -p loong-tui-app` · `./scripts/check.sh`.
/// Returns `true` when the run should end.
fn act<B: ratatui::backend::Backend + std::io::Write>(
    app: &mut App,
    terminal: &mut ratatui::Terminal<B>,
    to_worker: &Sender<ToWorker>,
    key: KeyEvent,
) -> Result<bool, String> {
    use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use ratatui::crossterm::execute;
    match app.handle_key(key) {
        Action::Send(command) => {
            let _ = to_worker.send(command);
        }
        // The interface cannot reach the terminal or the clipboard from
        // `handle_key`; the loop that owns them does it here.
        Action::Copy => {
            let text = app.transcript();
            let path = copy_everywhere(&text, &app.session);
            app.push_lines(vec![format!(
                "⧉ 已复制 {} 行 → {path}",
                text.lines().count()
            )]);
        }
        Action::Mouse(on) => {
            let backend = terminal.backend_mut();
            if on {
                let _ = execute!(backend, EnableMouseCapture);
            } else {
                let _ = execute!(backend, DisableMouseCapture);
            }
            // The screen is redrawn next pass; the terminal is what changed.
        }
        Action::Quit => return Ok(true),
        Action::None => {}
    }
    Ok(false)
}
