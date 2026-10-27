//! Purpose: The control-plane first-run form and the terminal guard every full-screen surface asks before drawing.
//!
//! # Contract
//! - **Serves:** `crates/cli/src/tui.rs` (the `tui` command's setup) and `crates/tui-app/src/wizard.rs`.
//! - **Design:** `docs/design/surfaces.md` §3; `docs/design/control-plane.md` §2 and §4 for what the address, pinned key and token configure.
//! - **Upstream:** `crates/cli/src/tui.rs` calls `setup_form` and `full_screen_needs_a_terminal`; `wizard::local_wizard` calls the guard.
//! - **Downstream:** `ratatui` for the form; nothing of its own beyond the form state.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
use super::*;

/// Purpose: The first-run control-plane form: address, pinned public key, optional token reference and the caret's field.
/// Serves: `setup_form`, `draw_setup_form` and the form tests.
/// Design: `docs/design/surfaces.md` §3; `docs/design/control-plane.md` §2.
/// Up: `SetupForm::new`, `setup_form` and the tests assign fields.
/// Down: no call; `answer` and `handle_key` read it.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: What the setup form's keyboard asks the event loop to do.
/// Serves: `SetupForm::handle_key`, `setup_form` and the tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `SetupForm::handle_key` returns it.
/// Down: `setup_form` matches it.
/// Verify: `cargo test -p loong-tui-app`.
#[derive(Debug, PartialEq, Eq)]
pub enum SetupAction {
    None,
    Submit,
    Cancel,
}

impl SetupForm {
    /// Purpose: An empty form with the caret on the address.
    /// Serves: `setup_form` and the form tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `setup_form` and the tests.
    /// Down: no call; it initialises the fields.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn new() -> Self {
        SetupForm {
            url: String::new(),
            public_key: String::new(),
            token: String::new(),
            field: 0,
            error: None,
        }
    }

    /// Purpose: The field the caret is in, mutably, so one edit path serves all three.
    /// Contract: called by `handle_key`'s Backspace and character arms.
    /// The field the caret is in, mutably. Taking the index first keeps the borrow
    /// checker happy and the call sites readable.
    fn active(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.url,
            1 => &mut self.public_key,
            _ => &mut self.token,
        }
    }

    /// Purpose: The label for one field, so the form and its test name the same thing.
    /// Serves: `draw_setup_form` and the field-layout test.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `draw_setup_form` and the tests.
    /// Down: no call; it maps an index to text.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn label(&self, index: usize) -> &'static str {
        match index {
            0 => "控制面地址",
            1 => "公钥（管理员给你的那个值）",
            _ => "令牌引用（可选：file:… 或 env:…）",
        }
    }

    /// Purpose: The three values when the form is complete, or `None` when a required field is empty.
    /// Serves: `SetupForm::handle_key`'s Enter arm and `setup_form`.
    /// Design: `docs/design/surfaces.md` §3; `docs/design/control-plane.md` §2.
    /// Up: `handle_key` and `setup_form`.
    /// Down: no call; it reads the fields.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: The form's keyboard: edit a field, move between them, submit a complete form, or leave.
    /// Serves: `setup_form` and the form tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `setup_form` and the tests.
    /// Down: `answer` and `active`.
    /// Verify: `cargo test -p loong-tui-app`.
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
                        self.error = Some("地址和公钥都必须填".to_string());
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

/// Purpose: Refuse a full-screen surface when stdout is not a terminal, before asking for anything.
/// Serves: `setup_form`, `wizard::local_wizard`, `crates/cli/src/tui.rs` and `crates/cli/src/local.rs`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: those callers.
/// Down: `std::io::IsTerminal`.
/// Verify: `cargo test -p loong-tui-app`.
/// A full-screen surface needs a terminal, and must say so before asking for anything:
/// discovering the problem after typing three fields would be rude, and in a pipe the
/// form cannot be drawn at all.
pub fn full_screen_needs_a_terminal() -> Result<(), String> {
    use std::io::IsTerminal;
    if std::io::stdout().is_terminal() {
        return Ok(());
    }
    Err("stdout 不是终端：用 `loongagent repl` 走行模式，或者在真正的终端里运行".to_string())
}

/// Purpose: Draw the three labelled fields and the hint line, so the layout can be asserted without a terminal.
/// Serves: `setup_form` and the form-layout test.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `setup_form` and the tests.
/// Down: `SetupForm::{label, field}`.
/// Verify: `cargo test -p loong-tui-app`.
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
            "loongagent — 连接你所在公司的控制面\n\
             配置从这里下发，模型不用你自己配。",
        )
        .block(ratatui::widgets::Block::bordered().title(" 首次运行 ")),
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
        None => "tab 下一个字段 · 回车连接 · esc 跳过（不用控制面）".to_string(),
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

/// Purpose: Run the full-screen first-run form and return the three values, or `None` when the person skipped it.
/// Serves: `crates/cli/src/tui.rs`.
/// Design: `docs/design/surfaces.md` §3; `docs/design/control-plane.md` §2.
/// Up: `crates/cli/src/tui.rs`.
/// Down: `full_screen_needs_a_terminal`, `SetupForm` and `draw_setup_form`.
/// Verify: `cargo test -p loong-tui-app`.
/// Ask for the control-plane configuration before the session starts, in a small
/// full-screen form. `Ok(None)` means the user declined — a session without a control
/// plane is a legitimate state (a local profile, or a provider double).
pub fn setup_form() -> Result<Option<(String, String, String)>, String> {
    full_screen_needs_a_terminal()?;
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
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
        // Crossterm's Windows backend reports a Release alongside every Press;
        // acting on both makes one keystroke do the thing twice.
        if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
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
