use super::*;

/// The TTY-bound part: raw mode, the alternate screen, the event loop.
///
/// `notes` carries the server process's stderr. It must not be printed while a
/// full-screen UI owns the terminal, but the startup lines (sandbox, hooks, MCP)
/// are diagnostics the operator wants, so they are drawn instead of dumped.
/// What a first run needs to know, collected inside the terminal UI so nobody has to
/// discover a command first. Three fields, and only the first two are required.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SetupForm {
    pub url: String,
    pub public_key: String,
    pub token: String,
    /// Which field the caret is in: 0 address, 1 public key, 2 token.
    pub field: usize,
    /// Why the last submit was refused, if it was.
    pub error: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SetupAction {
    None,
    Submit,
    Cancel,
}

impl SetupForm {
    pub fn new() -> Self {
        SetupForm {
            url: String::new(),
            public_key: String::new(),
            token: String::new(),
            field: 0,
            error: None,
        }
    }

    /// The field the caret is in, mutably. Taking the index first keeps the borrow
    /// checker happy and the call sites readable.
    fn active(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.url,
            1 => &mut self.public_key,
            _ => &mut self.token,
        }
    }

    pub fn label(&self, index: usize) -> &'static str {
        match index {
            0 => "control-plane address",
            1 => "public key (the value your administrator pinned)",
            _ => "token reference (optional: file:… or env:…)",
        }
    }

    /// `Some((url, public_key, token))` when the form is complete.
    pub fn answer(&self) -> Option<(String, String, String)> {
        if self.url.trim().is_empty() || self.public_key.trim().is_empty() {
            return None;
        }
        Some((
            self.url.trim().to_string(),
            self.public_key.trim().to_string(),
            self.token.trim().to_string(),
        ))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> SetupAction {
        use ratatui::crossterm::event::KeyCode;
        self.error = None;
        // Ctrl-C / Ctrl-D cancel: a form that only knows Esc is a form that can hang
        // somebody's terminal.
        if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return SetupAction::Cancel;
        }
        match key.code {
            KeyCode::Esc => return SetupAction::Cancel,
            KeyCode::Enter => {
                return match self.answer() {
                    Some(_) => SetupAction::Submit,
                    None => {
                        self.error =
                            Some("the address and the public key are both required".to_string());
                        SetupAction::None
                    }
                }
            }
            KeyCode::Tab | KeyCode::Down => self.field = (self.field + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => self.field = (self.field + 2) % 3,
            KeyCode::Backspace => {
                self.active().pop();
            }
            KeyCode::Char(c) => {
                self.active().push(c);
            }
            _ => {}
        }
        SetupAction::None
    }
}

/// A full-screen surface needs a terminal, and must say so before asking for anything:
/// discovering the problem after typing three fields would be rude, and in a pipe the
/// form cannot be drawn at all.
pub fn full_screen_needs_a_terminal() -> Result<(), String> {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        return Ok(());
    }
    Err(
        "no terminal on stdout: use `loongagent repl` for a line-mode session, or run in a TTY"
            .to_string(),
    )
}

/// Draw the first-run form. Separate from the event loop so the layout can be
/// asserted on (three labelled fields and a hint line) without a terminal.
pub(super) fn draw_setup_form(frame: &mut ratatui::Frame, form: &SetupForm) {
    let area = frame.area();
    let chunks = ratatui::layout::Layout::vertical([
        ratatui::layout::Constraint::Length(3),
        ratatui::layout::Constraint::Length(3),
        ratatui::layout::Constraint::Length(3),
        ratatui::layout::Constraint::Length(3),
        ratatui::layout::Constraint::Min(2),
    ])
    .split(area);
    frame.render_widget(
        ratatui::widgets::Paragraph::new(
            "loongagent — connect to your firm's control plane\n\
             Profiles come from there; you do not configure models yourself.",
        )
        .block(ratatui::widgets::Block::bordered().title(" first run ")),
        chunks[0],
    );
    for (index, chunk) in chunks[1..4].iter().enumerate() {
        let active = index == form.field;
        let border = if active {
            ratatui::style::Style::default().fg(ratatui::style::Color::Cyan)
        } else {
            ratatui::style::Style::default()
        };
        let value = match index {
            0 => &form.url,
            1 => &form.public_key,
            _ => &form.token,
        };
        frame.render_widget(
            ratatui::widgets::Paragraph::new(value.as_str()).block(
                ratatui::widgets::Block::bordered()
                    .title(form.label(index))
                    .border_style(border),
            ),
            *chunk,
        );
        if active {
            let column = chunk.x + 1 + value.chars().count() as u16;
            frame.set_cursor_position((column.min(chunk.right().saturating_sub(2)), chunk.y + 1));
        }
    }
    let help = match &form.error {
        Some(error) => {
            format!("{error}\n\ntab next field · enter connect · esc skip (no control plane)")
        }
        None => "tab next field · enter connect · esc skip (no control plane)".to_string(),
    };
    frame.render_widget(
        ratatui::widgets::Paragraph::new(help).style(if form.error.is_some() {
            Style::default().fg(Color::Red)
        } else {
            MARK_QUIET
        }),
        chunks[4],
    );
}

/// Ask for the control-plane configuration before the session starts, in a small
/// full-screen form. `Ok(None)` means the user declined — a session without a control
/// plane is a legitimate state (a local profile, or a provider double).
pub fn setup_form() -> Result<Option<(String, String, String)>, String> {
    full_screen_needs_a_terminal()?;
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, Event};
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::Terminal;

    let mut form = SetupForm::new();
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    let outcome = loop {
        terminal
            .draw(|frame| draw_setup_form(frame, &form))
            .map_err(|e| e.to_string())?;

        if !event::poll(std::time::Duration::from_millis(120)).map_err(|e| e.to_string())? {
            continue;
        }
        if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
            match form.handle_key(key) {
                SetupAction::Submit => break form.answer(),
                SetupAction::Cancel => break None,
                SetupAction::None => {}
            }
        }
    };

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    Ok(outcome)
}
