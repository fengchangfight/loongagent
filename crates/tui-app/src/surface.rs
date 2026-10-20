//! Purpose: The full-screen event loop: replay the past, take the terminal, and turn keys and worker messages into frames.
//!
//! # Contract
//! - **Serves:** `crates/cli/src/tui.rs`, the only production caller.
//! - **Design:** `docs/design/surfaces.md` §3 and §6; `docs/design/sessions-lineage.md` §1 and §5.
//! - **Upstream:** `crates/cli/src/tui.rs` calls `run` with a connected `Client`.
//! - **Downstream:** `App` for state and drawing, `spawn_worker` for the client thread, `copy_everywhere` for `/copy`, and `loong_tui::Client` for replay and models.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
use super::*;

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
    notes: Option<Receiver<String>>,
    intro: Vec<String>,
    interrupt: Option<Box<dyn Fn()>>,
    health: Option<Receiver<ModelHealth>>,
) -> Result<(), String> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
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
    let mut app = App::new(session, schema);
    app.interrupt = interrupt;
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
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let outcome = (|| -> Result<(), String> {
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

            if event::poll(std::time::Duration::from_millis(80)).map_err(|e| e.to_string())? {
                match event::read().map_err(|e| e.to_string())? {
                    event::Event::Key(key) => match app.handle_key(key) {
                        Action::Send(command) => {
                            let _ = to_worker.send(command);
                        }
                        // The interface cannot reach the terminal or the clipboard from
                        // `handle_key`; the loop that owns them does it here.
                        Action::Copy => {
                            let text = app.transcript();
                            let path = copy_everywhere(&text, &app.session);
                            app.push_lines(vec![format!(
                                "⧉ copied {} line(s) → {path}",
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
                        Action::Quit => return Ok(()),
                        Action::None => {}
                    },
                    event::Event::Mouse(mouse) => app.handle_mouse(mouse.kind),
                    _ => {}
                }
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
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();
    outcome
}
