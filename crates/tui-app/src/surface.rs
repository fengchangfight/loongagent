//! Purpose: The full-screen event loop: replay the past, take the terminal, and turn keys and worker messages into frames.
//!
//! # Contract
//! - **Serves:** `crates/cli/src/tui.rs`, the only production caller.
//! - **Design:** `docs/design/surfaces.md` §3 and §6; `docs/design/sessions-lineage.md` §1 and §5.
//! - **Upstream:** `crates/cli/src/tui.rs` calls `run` with a connected `Client`.
//! - **Downstream:** `App` for state and drawing, `spawn_worker` for the client thread, `copy_everywhere` for `/copy`, and `loong_tui::Client` for replay and models.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
use super::*;

/// Purpose: Take the terminal for the full-screen view without capturing the mouse, and quiet the wheel-to-arrow translation so a wheel notch cannot walk prompt history.
/// Serves: `run` and `the_mouse_can_be_handed_back_to_the_terminal`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `run`.
/// Down: crossterm's `execute!` for the alternate screen and bracketed paste, and the raw DECSET 1007 reset.
/// Verify: `cargo test -p loong-tui-app`.
/// Take the alternate screen and bracketed paste, but **not** the mouse: capture disables
/// the terminal's own selection, and selection is the thing to keep. The escape that
/// follows is xterm's alternate-scroll (DECSET 1007): with it on, the terminal turns a
/// wheel notch into ↑/↓, and those keys are prompt history here — so a reader's wheel
/// would rewrite the prompt. Off, the wheel is inert, PageUp/PageDown scroll, and
/// `/mouse` opts into the wheel at the cost of Shift-to-select.
pub(crate) fn take_screen(writer: &mut impl std::io::Write) -> std::io::Result<()> {
    use ratatui::crossterm::event::EnableBracketedPaste;
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::EnterAlternateScreen;
    execute!(writer, EnterAlternateScreen, EnableBracketedPaste)?;
    write!(writer, "\x1b[?1007l")?;
    writer.flush()
}

/// Purpose: Give the terminal back on exit: close the mouse if `/mouse` opened it, drop bracketed paste, and restore the wheel translation.
/// Serves: `run`'s cleanup and `the_mouse_can_be_handed_back_to_the_terminal`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `run`.
/// Down: crossterm's `execute!` for leaving the screen, and the raw DECSET 1007 restore.
/// Verify: `cargo test -p loong-tui-app`.
pub(crate) fn leave_screen(writer: &mut impl std::io::Write) -> std::io::Result<()> {
    use ratatui::crossterm::event::{DisableBracketedPaste, DisableMouseCapture};
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::LeaveAlternateScreen;
    // The xterm default is on, so put it back rather than leaving the user's terminal
    // with wheel-to-arrow quietly disabled in every other application.
    write!(writer, "\x1b[?1007h")?;
    execute!(
        writer,
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    )
}

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
    save_order: Option<Box<dyn Fn(&[String]) -> Result<(), String>>>,
    notes: Option<Receiver<String>>,
    intro: Vec<String>,
    interrupt: Option<Box<dyn Fn()>>,
    permissions: Option<Box<dyn Fn(bool)>>,
    health: Option<Receiver<ModelHealth>>,
) -> Result<(), String> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, KeyEventKind};
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{disable_raw_mode, enable_raw_mode};
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
    if let Some(save) = save_order {
        app.set_order_saver(save);
    }
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
    // The terminal keeps the mouse: drag-select and copy stay the shell's own, which is
    // what someone reading a transcript expects. `/mouse` trades that for the wheel;
    // take_screen also turns off the wheel-as-arrow rule that would otherwise walk the
    // prompt's history while the mouse is not captured.
    take_screen(&mut stdout).map_err(|e| e.to_string())?;
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
                    // Crossterm's Windows backend reports a Release alongside
                    // every Press; only Press may act.
                    event::Event::Key(key) if key.kind == KeyEventKind::Press => {
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
                            Action::Quit => return Ok(()),
                            Action::None => {}
                        }
                    }
                    event::Event::Mouse(mouse) => app.handle_mouse(mouse.kind),
                    // A bracketed paste arrives as one block, including its newlines: it
                    // goes into the input, and enter later sends the whole thing.
                    event::Event::Paste(text) => {
                        app.paste(&text);
                    }
                    _ => {}
                }
            }
            if app.quit {
                return Ok(());
            }
        }
    })();

    disable_raw_mode().ok();
    leave_screen(terminal.backend_mut()).ok();
    terminal.show_cursor().ok();
    outcome
}
