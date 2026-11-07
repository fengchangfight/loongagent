//! Purpose: The full-screen event loop: replay the past, take the terminal, and turn keys and worker messages into frames.
//!
//! # Contract
//! - **Serves:** `crates/cli/src/tui.rs`, the only production caller.
//! - **Design:** `docs/design/surfaces.md` §3 and §6; `docs/design/sessions-lineage.md` §1 and §5.
//! - **Upstream:** `crates/cli/src/tui.rs` calls `run` with a connected `Client`.
//! - **Downstream:** `App` for state and drawing, `spawn_worker` for the client thread, `copy_everywhere` for `/copy`, and `loong_tui::Client` for replay and models.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
use super::*;

/// Purpose: Take the terminal for the full-screen view with the mouse captured, and quiet the wheel-to-arrow translation for the state where `/mouse` hands it back.
/// Serves: `run` and `the_mouse_can_be_handed_back_to_the_terminal`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `run`.
/// Down: crossterm's `execute!` for the alternate screen, mouse capture and bracketed paste, and the raw DECSET 1007 reset.
/// Verify: `cargo test -p loong-tui-app`.
/// Take the alternate screen, bracketed paste and the mouse: the screen needs wheel events
/// to scroll, and because capture disables the terminal's own selection it draws and copies
/// the selection itself (`tui-selection.md`). The escape that follows is xterm's
/// alternate-scroll (DECSET 1007): it would turn a wheel notch into ↑/↓ when `/mouse` hands
/// the mouse back, and those keys are prompt history here, so it is turned off for the rest
/// of the run. Shift still bypasses capture and selects in the terminal.
pub(crate) fn take_screen(writer: &mut impl std::io::Write) -> std::io::Result<()> {
    use ratatui::crossterm::event::{EnableBracketedPaste, EnableMouseCapture};
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::EnterAlternateScreen;
    execute!(
        writer,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
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

/// Purpose: What a closed screen reports back: which log was on screen at the end, and how the run ended.
/// Serves: `crates/cli/src/tui.rs`, which prints the way back after the terminal is restored.
/// Design: `docs/design/sessions-lineage.md` §5 (the way back into a log); `docs/design/surfaces.md` §3.
/// Up: returned by `run`; no other producer.
/// Down: `App::session`, which `/new` moves when the worker answers with `session.started`.
/// Verify: `cargo test -p loongagent --test tui_surface`.
///
/// `/new` writes a second log, so the id the process started with stops being the live
/// one. The exit hint resumes a session, and naming the first one would reopen a log the
/// person had already left — hence the field, rather than the caller's own copy.
pub struct RunOutcome {
    /// Purpose: The id of the log on screen when the screen closed; `/new` moves it.
    /// Serves: `crates/cli/src/tui.rs`'s `session` and `reopen` lines.
    /// Design: `docs/design/sessions-lineage.md` §5.
    /// Up: `run`'s return value.
    /// Down: nothing.
    /// Verify: `cargo test -p loongagent --test tui_surface`.
    pub session: String,
    /// Purpose: `Ok` when someone left on purpose, `Err` when the screen itself failed.
    /// Serves: `crates/cli/src/tui.rs`, which hands it back to `main`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `run`'s return value.
    /// Down: nothing.
    /// Verify: `cargo test -p loongagent --test tui_surface`.
    pub outcome: Result<(), String>,
}

/// Purpose: Replay the session, spawn the worker, own the terminal and loop: drain, draw, read a key, act, until quit.
/// Serves: `crates/cli/src/tui.rs`.
/// Design: `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §1 (replay reads the log) and §5.
/// Up: `crates/cli/src/tui.rs`.
/// Down: `Client::{replay, folded_results, models}`, `spawn_worker`, `App`, `copy_everywhere` and `RunOutcome`.
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
) -> RunOutcome {
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

    // Taking the terminal can fail before the first frame. Nothing has moved the session
    // yet, so even that report names the one this call started with.
    let failed = |e: std::io::Error| RunOutcome {
        session: app.session.clone(),
        outcome: Err(e.to_string()),
    };
    if let Err(e) = enable_raw_mode() {
        return failed(e);
    }
    let mut stdout = std::io::stdout();
    // Capture the mouse: the wheel scrolls the page, and the screen turns a drag into its
    // own selection and copy because capture is what takes that away from the terminal.
    // `/mouse` still hands the mouse back for a terminal-native selection.
    if let Err(e) = take_screen(&mut stdout) {
        return failed(e);
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(terminal) => terminal,
        Err(e) => return failed(e),
    };

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
                    // Wrap to the page that is actually on screen; before the first frame,
                    // ask the terminal. A startup note is the one line that most needs it.
                    let width = app
                        .page
                        .as_ref()
                        .map(|page| page.area.width)
                        .unwrap_or_else(|| terminal.size().map(|size| size.width).unwrap_or(80));
                    app.push_note(note, width);
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
                            // A key press never selects with the mouse; the arm exists so
                            // this match stays exhaustive.
                            Action::CopyText(_) => {}
                            Action::Quit => return Ok(()),
                            Action::None => {}
                        }
                    }
                    event::Event::Mouse(mouse) => {
                        // A selection copy is silent on purpose: the highlight is the
                        // feedback, and a transcript line about it is one more row the
                        // reader has to scroll past. The file is still written.
                        if let Action::CopyText(text) = app.handle_mouse(mouse) {
                            let _ = copy_everywhere(&text, &app.session);
                        }
                    }
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
    // The session that was on screen at the end, which `/new` may have moved: the caller
    // prints the way back to this log, not to the one the process started with.
    RunOutcome {
        session: app.session.clone(),
        outcome,
    }
}
