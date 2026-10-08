//! The full-screen terminal surface (ADR-0009, ADR-0013): a ratatui view over a
//! [`loong_tui::Client`].
//!
//! The split matters. `loong-tui` holds the protocol conversation and no terminal
//! dependency at all, so it stays testable headless; this crate holds the screen.
//! And the screen is testable too, because the state machine and the drawing are
//! separate from the parts that need a TTY: [`App::handle_key`] and [`App::draw`]
//! are driven from ratatui's `TestBackend` in the tests below, and only [`run`]
//! touches the terminal.
//!
//! A turn is blocking I/O, so it runs on a worker thread and the UI stays
//! responsive: the worker owns the client, the UI owns the screen, and they talk
//! over two channels.

use loong_tui::{Client, Transport};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

/// What the screen wants the worker to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToWorker {
    Submit(String),
    Respond { request_id: String, granted: bool },
}

/// What the worker reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromWorker {
    Lines(Vec<String>),
    /// A turn finished (or stopped for approval), so the UI can update.
    State { pending: Option<String> },
    Failed(String),
}

/// What a key press means (returned so the caller — or a test — decides).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Send(ToWorker),
    Quit,
}

pub struct App {
    /// The rendered conversation, oldest first.
    pub lines: Vec<String>,
    pub input: String,
    /// The request awaiting an answer, if the turn stopped for one.
    pub pending: Option<String>,
    /// Diagnostics from the server process (its stderr): startup notes, warnings.
    pub notes: Vec<String>,
    pub session: String,
    pub schema: String,
    pub busy: bool,
    pub error: Option<String>,
    /// Lines scrolled up from the bottom.
    pub scroll: usize,
    pub quit: bool,
}

impl App {
    pub fn new(session: impl Into<String>, schema: impl Into<String>) -> Self {
        Self {
            lines: Vec::new(),
            input: String::new(),
            pending: None,
            notes: Vec::new(),
            session: session.into(),
            schema: schema.into(),
            busy: false,
            error: None,
            scroll: 0,
            quit: false,
        }
    }

    pub fn push_lines(&mut self, lines: impl IntoIterator<Item = String>) {
        for line in lines {
            for piece in line.split('\n') {
                self.lines.push(piece.to_string());
            }
        }
        // New output pulls the view back to the bottom, where a reader expects to be.
        self.scroll = 0;
    }

    pub fn push_note(&mut self, note: String) {
        if self.notes.last() != Some(&note) {
            self.notes.push(note);
        }
        if self.notes.len() > 100 {
            self.notes.remove(0);
        }
    }

    pub fn drain(&mut self, worker: &Receiver<FromWorker>) {
        loop {
            match worker.try_recv() {
                Ok(FromWorker::Lines(lines)) => self.push_lines(lines),
                Ok(FromWorker::State { pending }) => {
                    self.pending = pending;
                    self.busy = false;
                }
                Ok(FromWorker::Failed(message)) => {
                    self.busy = false;
                    self.error = Some(message.clone());
                    self.push_lines(vec![format!("! {message}")]);
                }
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
    }

    /// The keys that mean something. While an approval is pending the input line is
    /// repurposed: typing a prompt then would be ignored anyway, and saying so is
    /// kinder than silently swallowing it.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                self.quit = true;
                Action::Quit
            }
            KeyCode::Char('d') if ctrl => {
                self.quit = true;
                Action::Quit
            }
            KeyCode::Esc if self.pending.is_none() => {
                self.quit = true;
                Action::Quit
            }
            KeyCode::Char('a') | KeyCode::Char('y') if self.pending.is_some() => self.answer(true),
            KeyCode::Char('d') | KeyCode::Char('n') if self.pending.is_some() => self.answer(false),
            KeyCode::Esc if self.pending.is_some() => self.answer(false),
            KeyCode::PageUp => {
                self.scroll = (self.scroll + 5).min(self.lines.len());
                Action::None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(5);
                Action::None
            }
            KeyCode::Up => {
                self.scroll = (self.scroll + 1).min(self.lines.len());
                Action::None
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_sub(1);
                Action::None
            }
            KeyCode::Home => {
                self.scroll = self.lines.len();
                Action::None
            }
            KeyCode::End => {
                self.scroll = 0;
                Action::None
            }
            // While a question is open the input line is not an input line.
            KeyCode::Backspace if self.pending.is_none() => {
                self.input.pop();
                Action::None
            }
            KeyCode::Enter if self.pending.is_none() => {
                let text = self.input.trim().to_string();
                if text.is_empty() {
                    return Action::None;
                }
                self.input.clear();
                self.push_lines(vec![format!("you: {text}")]);
                self.busy = true;
                self.error = None;
                Action::Send(ToWorker::Submit(text))
            }
            KeyCode::Char(c) if !ctrl && self.pending.is_none() => {
                self.input.push(c);
                Action::None
            }
            _ => Action::None,
        }
    }

    fn answer(&mut self, granted: bool) -> Action {
        let Some(request_id) = self.pending.take() else {
            return Action::None;
        };
        self.push_lines(vec![format!(
            "{} {request_id}",
            if granted { "approved" } else { "denied" }
        )]);
        self.busy = true;
        self.error = None;
        Action::Send(ToWorker::Respond { request_id, granted })
    }

    /// The scrollback the reader should see at this height.
    pub fn visible_lines(&self, height: usize) -> Vec<Line<'static>> {
        let total = self.lines.len();
        let end = total.saturating_sub(self.scroll);
        let start = end.saturating_sub(height.max(1));
        self.lines[start..end].iter().map(|l| Line::from(l.clone())).collect()
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(if self.notes.is_empty() { 0 } else { 2 }),
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(frame.area());

        if !self.notes.is_empty() {
            let latest = self.notes.last().cloned().unwrap_or_default();
            let widget = Paragraph::new(latest)
                .style(Style::default().fg(Color::DarkGray))
                .wrap(Wrap { trim: true });
            frame.render_widget(widget, areas[0]);
        }

        let height = areas[1].height.saturating_sub(2) as usize;
        let body = Paragraph::new(self.visible_lines(height))
            .block(Block::default().borders(Borders::ALL).title(format!(
                "loongagent — session {} (schema {})",
                self.session, self.schema
            )))
            .wrap(Wrap { trim: false });
        frame.render_widget(body, areas[1]);

        let (title, style) = match &self.pending {
            Some(request_id) => (
                format!("approval required: {request_id} — [a]pprove / [d]eny"),
                Style::default().fg(Color::Yellow),
            ),
            None if self.busy => ("working…".to_string(), Style::default().fg(Color::Cyan)),
            // A leading `/` is a command: say where the list of them is.
            None if self.input.starts_with('/') => (
                "command — /help lists the skills available here".to_string(),
                Style::default().fg(Color::Green),
            ),
            None => ("prompt".to_string(), Style::default()),
        };
        let input = Paragraph::new(self.input.clone())
            .block(Block::default().borders(Borders::ALL).title(title))
            .style(style);
        frame.render_widget(input, areas[2]);

        let mut hints = vec![
            Span::styled("enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" send  "),
            Span::styled("pgup/pgdn", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" scroll  "),
            Span::styled("ctrl-c", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" quit"),
        ];
        if self.scroll > 0 {
            hints.push(Span::raw(format!(
                "  [{}/{}]",
                self.lines.len() - self.scroll,
                self.lines.len()
            )));
        }
        if let Some(error) = &self.error {
            hints.push(Span::styled(format!("  ! {error}"), Style::default().fg(Color::Red)));
        }
        frame.render_widget(Paragraph::new(Line::from(hints)), areas[3]);
    }
}

/// Own the client on a worker thread so a blocking turn never blocks the screen.
pub fn spawn_worker<T: Transport + Send + 'static>(
    mut client: Client<T>,
) -> (Sender<ToWorker>, Receiver<FromWorker>) {
    let (to_worker, commands) = channel::<ToWorker>();
    let (events, from_worker) = channel::<FromWorker>();
    std::thread::spawn(move || {
        while let Ok(command) = commands.recv() {
            let result = match command {
                ToWorker::Submit(text) => client.submit(&text),
                ToWorker::Respond { request_id, granted } => client.respond(
                    &request_id,
                    granted,
                    loong_schema::generated::ApprovalScope::Once,
                    None,
                ),
            };
            match result {
                Ok(lines) => {
                    let _ = events.send(FromWorker::Lines(lines));
                }
                Err(e) => {
                    let _ = events.send(FromWorker::Failed(e));
                }
            }
            let _ = events.send(FromWorker::State {
                pending: client.pending_approval().map(|s| s.to_string()),
            });
        }
    });
    (to_worker, from_worker)
}

/// The TTY-bound part: raw mode, the alternate screen, the event loop.
///
/// `notes` carries the server process's stderr. It must not be printed while a
/// full-screen UI owns the terminal, but the startup lines (sandbox, hooks, MCP)
/// are diagnostics the operator wants, so they are drawn instead of dumped.
pub fn run<T: Transport + Send + 'static>(
    client: Client<T>,
    session: String,
    schema: String,
    notes: Option<Receiver<String>>,
) -> Result<(), String> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event;
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::Terminal;
    use std::io::IsTerminal;

    if !std::io::stdout().is_terminal() {
        return Err(
            "no terminal on stdout: use `loongagent repl` for a line-mode session, or run in a TTY"
                .to_string(),
        );
    }

    let (to_worker, from_worker) = spawn_worker(client);
    let mut app = App::new(session, schema);

    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let outcome = (|| -> Result<(), String> {
        loop {
            app.drain(&from_worker);
            if let Some(notes) = &notes {
                while let Ok(note) = notes.try_recv() {
                    app.push_note(note);
                }
            }
            terminal.draw(|frame| app.draw(frame)).map_err(|e| e.to_string())?;

            if event::poll(std::time::Duration::from_millis(80)).map_err(|e| e.to_string())? {
                if let event::Event::Key(key) = event::read().map_err(|e| e.to_string())? {
                    match app.handle_key(key) {
                        Action::Send(command) => {
                            let _ = to_worker.send(command);
                        }
                        Action::Quit => return Ok(()),
                        Action::None => {}
                    }
                }
            }
            if app.quit {
                return Ok(());
            }
        }
    })();

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn draw(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        (0..area.height)
            .map(|y| {
                (0..area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn the_screen_shows_the_conversation_the_input_and_the_session() {
        let mut app = App::new("s-1", "0.1.0");
        app.push_lines(vec!["you: hello".to_string(), "hi there".to_string()]);
        type_text(&mut app, "next question");
        let screen = draw(&mut app, 60, 16);
        assert!(screen.contains("session s-1"), "{screen}");
        assert!(screen.contains("schema 0.1.0"), "{screen}");
        assert!(screen.contains("you: hello"), "{screen}");
        assert!(screen.contains("hi there"), "{screen}");
        assert!(screen.contains("next question"), "{screen}");
        assert!(screen.contains("prompt"), "{screen}");
    }

    #[test]
    fn enter_submits_a_non_empty_prompt_and_clears_the_input() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "do it");
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::Submit("do it".to_string()))
        );
        assert!(app.input.is_empty());
        assert!(app.busy);
        assert!(app.lines.iter().any(|l| l == "you: do it"), "{:?}", app.lines);
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None, "empty is not a prompt");
    }

    #[test]
    fn backspace_edits_and_ctrl_c_quits() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "ab");
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "a");
        assert_eq!(app.handle_key(key(KeyCode::Char('d'))), Action::None, "plain 'd' types");
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        assert!(app.quit);
    }

    #[test]
    fn an_approval_repurposes_the_input_line_and_answers_it() {
        let mut app = App::new("s", "v");
        app.pending = Some("appr-1".to_string());
        let screen = draw(&mut app, 70, 16);
        assert!(screen.contains("approval required: appr-1"), "{screen}");
        assert!(screen.contains("[a]pprove / [d]eny"), "{screen}");

        app.handle_key(key(KeyCode::Char('z')));
        assert!(app.input.is_empty(), "a prompt cannot be typed while the question is open");

        assert_eq!(
            app.handle_key(key(KeyCode::Char('a'))),
            Action::Send(ToWorker::Respond { request_id: "appr-1".to_string(), granted: true })
        );
        assert!(app.pending.is_none());
        assert!(app.lines.iter().any(|l| l.contains("approved appr-1")), "{:?}", app.lines);
    }

    #[test]
    fn typing_a_slash_says_where_the_commands_are_listed() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "/rev");
        let screen = draw(&mut app, 70, 16);
        assert!(screen.contains("command — /help lists the skills available here"), "{screen}");
        // And it is only a hint: the text is still a normal prompt buffer.
        assert_eq!(app.input, "/rev");
    }

    #[test]
    fn a_denial_is_an_answer_too() {
        let mut app = App::new("s", "v");
        app.pending = Some("appr-2".to_string());
        assert_eq!(
            app.handle_key(key(KeyCode::Char('d'))),
            Action::Send(ToWorker::Respond { request_id: "appr-2".to_string(), granted: false })
        );
        assert!(app.lines.iter().any(|l| l.contains("denied appr-2")));
    }

    #[test]
    fn scrolling_moves_the_window_and_new_output_returns_to_the_bottom() {
        let mut app = App::new("s", "v");
        app.push_lines((1..=50).map(|i| format!("line {i}")));
        let bottom = draw(&mut app, 40, 20);
        assert!(bottom.contains("line 50"), "{bottom}");

        app.handle_key(key(KeyCode::PageUp));
        let scrolled = draw(&mut app, 40, 20);
        assert!(!scrolled.contains("line 50"), "{scrolled}");
        assert!(scrolled.contains("line"), "{scrolled}");

        app.handle_key(key(KeyCode::End));
        assert_eq!(app.scroll, 0, "End returns to the newest output");

        app.handle_key(key(KeyCode::PageUp));
        app.push_lines(vec!["a new answer".to_string()]);
        assert_eq!(app.scroll, 0, "new output pulls the view back");
        assert!(draw(&mut app, 40, 20).contains("a new answer"));
    }

    #[test]
    fn server_notes_are_drawn_instead_of_corrupting_the_screen() {
        let mut app = App::new("s", "v");
        app.push_note("[sandbox] filesystem boundary enforced by landlock".to_string());
        app.push_note("[sandbox] filesystem boundary enforced by landlock".to_string());
        assert_eq!(app.notes.len(), 1, "repeats are collapsed");
        app.push_note("[hooks] no hooks".to_string());
        let screen = draw(&mut app, 70, 16);
        assert!(screen.contains("[hooks] no hooks"), "{screen}");
    }

    #[test]
    fn a_failure_is_visible_and_does_not_wedge_the_screen() {
        let (to_app, from_worker) = channel::<FromWorker>();
        let mut app = App::new("s", "v");
        app.busy = true;
        to_app.send(FromWorker::Failed("the server closed the connection".to_string())).unwrap();
        to_app.send(FromWorker::State { pending: None }).unwrap();
        app.drain(&from_worker);
        assert!(!app.busy);
        assert_eq!(app.error.as_deref(), Some("the server closed the connection"));
        assert!(draw(&mut app, 80, 16).contains("the server closed the connection"));
    }

    #[test]
    fn a_line_with_embedded_newlines_keeps_its_shape() {
        let mut app = App::new("s", "v");
        app.push_lines(vec!["header\nbody".to_string()]);
        assert_eq!(app.lines, vec!["header".to_string(), "body".to_string()]);
    }

    #[test]
    fn the_worker_turns_commands_into_screen_lines() {
        // A transport that answers everything with one canned line: the worker's
        // contract is that a command produces lines plus a state update.
        struct Echo;
        impl loong_tui::Transport for Echo {
            fn send(&mut self, _line: &str) -> Result<(), String> {
                Ok(())
            }
            fn recv_line(&mut self) -> Result<Option<String>, String> {
                Ok(Some(
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": {"turn_id": 1, "accepted": true}
                    })
                    .to_string(),
                ))
            }
        }
        let client = Client::new(Echo, "s-worker");
        let (to_worker, from_worker) = spawn_worker(client);
        to_worker.send(ToWorker::Submit("hello".to_string())).unwrap();
        let mut app = App::new("s-worker", "v");
        app.busy = true;
        // Wait for the worker to report back.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && app.busy {
            app.drain(&from_worker);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!app.busy, "the worker reported a state change");
    }

}
