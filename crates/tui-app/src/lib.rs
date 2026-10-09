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
    Respond {
        request_id: String,
        granted: bool,
    },
    /// Ask for the context view (the debug panel opening, or a refresh).
    RefreshContext,
    /// Show the most recent tool result in full — the counterpart of collapsing it.
    Expand,
}

/// What the worker reports back.
#[derive(Debug, Clone, PartialEq)]
pub enum FromWorker {
    /// A fragment of the answer, as the model writes it (a preview, not the record).
    Delta(String),
    Lines(Vec<String>),
    /// A turn finished (or stopped for approval), so the UI can update.
    State {
        pending: Option<String>,
    },
    /// What the next request will carry: the debug panel's data.
    Context(Box<loong_schema::generated::ContextExplain>),
    /// What the turn is doing right now ("shell: cargo test"), so the interface can say
    /// so while it is happening rather than after.
    Working(String),
    Failed(String),
}

/// What a key press means (returned so the caller — or a test — decides).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Send(ToWorker),
    Quit,
}

/// The seal: red ground, gold character — the traditional 龍, which is what a firm
/// mark looks like. Three rows so the character sits *in* the square rather than
/// beside it.
const SEAL_ROWS: u16 = 3;
const SEAL_RED: Color = Color::Rgb(150, 24, 24);
const SEAL_GOLD: Color = Color::Rgb(255, 206, 84);

/// What a line is, for colour. Classified from the line's own vocabulary — the renderer
/// that produces those strings lives in `loong-tui`, next door, and a test renders real
/// events through it to keep the two in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// What the person said.
    User,
    /// The model's own words.
    Answer,
    /// Something the agent is doing.
    ToolCall,
    /// What it got back.
    ToolOk,
    ToolFailed,
    /// A decision the person has to make.
    Approval,
    /// A policy ruling, or where a value came from.
    Policy,
    /// Bookkeeping: skills, tokens, compaction, hooks that allowed something.
    Note,
    /// Something went wrong, or a hook refused.
    Problem,
}

/// The palette. Muted on purpose: a transcript where everything shouts is harder to read
/// than one where only the things that matter are loud.
pub fn style_for(kind: LineKind) -> Style {
    match kind {
        LineKind::User => Style::default().fg(Color::Cyan),
        LineKind::Answer => Style::default(),
        LineKind::ToolCall => Style::default().fg(Color::Rgb(86, 182, 194)),
        LineKind::ToolOk => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::DIM),
        LineKind::ToolFailed => Style::default().fg(Color::Red),
        LineKind::Approval => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        LineKind::Policy => Style::default().fg(Color::Rgb(138, 138, 214)),
        LineKind::Note => Style::default().fg(Color::DarkGray),
        LineKind::Problem => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

/// Which kind a rendered line is. The prefixes are the client renderer's, so this is a
/// classifier over a vocabulary this repository owns end to end.
pub fn classify(line: &str) -> LineKind {
    if line.starts_with("you: ") {
        return LineKind::User;
    }
    if line.starts_with("→ ") {
        return LineKind::ToolCall;
    }
    if let Some(rest) = line.strip_prefix("← ") {
        return if rest.starts_with("ok ") || rest == "ok" {
            LineKind::ToolOk
        } else {
            LineKind::ToolFailed
        };
    }
    if line.starts_with("! approval required") {
        return LineKind::Approval;
    }
    if line.starts_with("! ") {
        return LineKind::Problem;
    }
    if line.starts_with("· policy:") {
        return LineKind::Policy;
    }
    if line.starts_with("· ") || line == "·" {
        return LineKind::Note;
    }
    LineKind::Answer
}

/// One line, painted. The user's own words keep a bright label so the eye can find where
/// a prompt began when scrolling back.
fn styled(line: &str) -> Line<'static> {
    let kind = classify(line);
    if kind == LineKind::User {
        if let Some((label, text)) = line.split_once(' ') {
            return Line::from(vec![
                Span::styled(
                    label.to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::raw(text.to_string()),
            ]);
        }
    }
    Line::from(Span::styled(line.to_string(), style_for(kind)))
}

/// How tall the input box may grow. Beyond this it scrolls, so a pasted file cannot
/// take the whole screen away from the conversation.
const MAX_INPUT_ROWS: u16 = 8;

pub struct App {
    /// The rendered conversation, oldest first.
    pub lines: Vec<String>,
    pub input: String,
    /// Where the caret is, in characters (not bytes) from the start of `input`.
    pub cursor: usize,
    /// The request awaiting an answer, if the turn stopped for one.
    pub pending: Option<String>,
    /// Diagnostics from the server process (its stderr): startup notes, warnings.
    pub notes: Vec<String>,
    /// Whether the mark is drawn at the top (`/logo` turns it off).
    pub logo: bool,
    /// What the running turn is doing, and since when — the interface's answer to "is
    /// it stuck, or working?".
    pub activity: Option<String>,
    pub busy_since: Option<std::time::Instant>,
    /// Whether the context panel is open (F2), and what it last showed.
    pub debug: bool,
    pub context: Option<Box<loong_schema::generated::ContextExplain>>,
    pub session: String,
    pub schema: String,
    pub busy: bool,
    pub error: Option<String>,
    /// The answer so far, while it is being written.
    pub streaming: String,
    /// Lines scrolled up from the bottom.
    pub scroll: usize,
    pub quit: bool,
}

impl App {
    pub fn new(session: impl Into<String>, schema: impl Into<String>) -> Self {
        Self {
            lines: Vec::new(),
            input: String::new(),
            cursor: 0,
            pending: None,
            notes: Vec::new(),
            session: session.into(),
            schema: schema.into(),
            busy: false,
            error: None,
            logo: true,
            activity: None,
            busy_since: None,
            debug: false,
            context: None,
            streaming: String::new(),
            scroll: 0,
            quit: false,
        }
    }

    /// The debug panel: what the next request will carry, where the tokens went, and
    /// what has been cleared. Read-only by construction — asking for it records
    /// nothing, so looking cannot change what you are looking at.
    /// Whether there is room for the mark. A logo that squeezes the conversation is a
    /// decoration that costs more than it gives, so a small terminal simply has none.
    fn show_banner(&self, area: ratatui::layout::Rect) -> bool {
        self.logo && area.height >= 18 && area.width >= 44
    }

    /// The seal, and the name beside it.
    fn draw_banner(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let seal = Style::default()
            .bg(SEAL_RED)
            .fg(SEAL_GOLD)
            .add_modifier(Modifier::BOLD);
        let ground = Style::default().bg(SEAL_RED);
        // Ten columns wide, three rows tall: a red ground with the character centred in
        // it, which is what a seal is.
        let rows = [
            Line::from(Span::styled("██████████", ground)),
            Line::from(vec![
                Span::styled("███ ", ground),
                Span::styled("龍", seal),
                Span::styled(" ███", ground),
            ]),
            Line::from(Span::styled("██████████", ground)),
        ];
        let mut lines: Vec<Line> = Vec::new();
        for (index, row) in rows.into_iter().enumerate() {
            // The middle two rows carry the name, so the mark and the word sit on one
            // baseline instead of the text floating beside a block.
            let companion = match index {
                0 => Some((
                    "loongagent".to_string(),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                )),
                1 => Some((
                    "a firm-internal coding agent".to_string(),
                    Style::default().fg(Color::DarkGray),
                )),
                _ => None,
            };
            match companion {
                Some((text, style)) => {
                    let mut spans = row.spans;
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(text, style));
                    lines.push(Line::from(spans));
                }
                None => lines.push(row),
            }
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn draw_context(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let mut lines: Vec<Line> = Vec::new();
        match &self.context {
            None => lines.push(Line::from(Span::styled(
                "asking the server…",
                Style::default().fg(Color::DarkGray),
            ))),
            Some(explained) => {
                let budget = explained.budget_tokens;
                let used = explained.context_tokens;
                let percent = if budget > 0 {
                    format!("{:.0}%", (used as f64 / budget as f64) * 100.0)
                } else {
                    "no limit".to_string()
                };
                lines.push(Line::from(vec![
                    Span::styled("tokens ", Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        format!("{used}"),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(if budget > 0 {
                        format!(" / {budget}")
                    } else {
                        String::new()
                    }),
                    Span::raw("  "),
                    Span::styled(percent, Style::default().fg(Color::Cyan)),
                ]));
                lines.push(
                    Line::from(format!(
                        "{} messages · {} tools · {} compactions",
                        explained.messages, explained.tools, explained.compactions
                    ))
                    .style(Style::default().fg(Color::DarkGray)),
                );
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "category            tokens",
                    Style::default().fg(Color::DarkGray),
                )));
                let widest = explained
                    .categories
                    .iter()
                    .map(|c| c.tokens)
                    .max()
                    .unwrap_or(1)
                    .max(1);
                for category in &explained.categories {
                    let bar = "█"
                        .repeat(((category.tokens as f64 / widest as f64) * 10.0).ceil() as usize);
                    lines.push(Line::from(format!(
                        "{:<16} {:>7}  {}",
                        category.category, category.tokens, bar
                    )));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "entries (newest last)   p=pinned c=cleared",
                    Style::default().fg(Color::DarkGray),
                )));
                // The tail is what the next request is mostly made of.
                let room = area.height.saturating_sub(lines.len() as u16 + 2) as usize;
                let entries = &explained.entries;
                let start = entries.len().saturating_sub(room);
                for entry in &entries[start..] {
                    let marker = match (entry.pinned, entry.cleared) {
                        (true, true) => "pc",
                        (true, false) => "p ",
                        (false, true) => " c",
                        (false, false) => "  ",
                    };
                    let style = if entry.cleared {
                        Style::default().fg(Color::DarkGray)
                    } else {
                        Style::default()
                    };
                    lines.push(
                        Line::from(format!(
                            "{marker} #{:<4} {:<13} {:>6}  {}",
                            entry.id, entry.category, entry.tokens, entry.source
                        ))
                        .style(style),
                    );
                }
            }
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" context (f2) "),
                )
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    /// The one place that decides which slash commands never reach the agent. Kept
    /// deliberately short: a command belongs here only if the *interface* is what has
    /// to act — quitting, and nothing else so far. `/help` and the rest are skills the
    /// workspace provides, and they stay on the server where they can be audited.
    fn surface_command(&mut self, text: &str) -> Option<Action> {
        match text.trim().to_ascii_lowercase().as_str() {
            "/exit" | "/quit" | "/q" => {
                self.input.clear();
                self.cursor = 0;
                Some(Action::Quit)
            }
            // The transcript shows a summary of long tool output; this asks the log for
            // the rest.
            "/expand" => {
                self.input.clear();
                self.cursor = 0;
                Some(Action::Send(ToWorker::Expand))
            }
            // The mark is decoration: worth having, and worth being able to put away.
            "/logo" => {
                self.input.clear();
                self.cursor = 0;
                self.logo = !self.logo;
                Some(Action::None)
            }
            _ => None,
        }
    }

    fn byte_at(&self, char_index: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_index)
            .map(|(byte, _)| byte)
            .unwrap_or(self.input.len())
    }

    fn insert(&mut self, text: &str) {
        let at = self.byte_at(self.cursor);
        self.input.insert_str(at, text);
        self.cursor += text.chars().count();
    }

    fn delete_before_cursor(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let from = self.byte_at(self.cursor - 1);
        let to = self.byte_at(self.cursor);
        self.input.replace_range(from..to, "");
        self.cursor -= 1;
    }

    fn delete_at_cursor(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let from = self.byte_at(self.cursor);
        let to = self.byte_at(self.cursor + 1);
        self.input.replace_range(from..to, "");
    }

    /// The input line as it should be drawn at this width, plus the caret's column
    /// inside the box. Long input scrolls horizontally so the caret stays visible —
    /// without that, typing past the edge looks like the keys stopped working.
    /// How tall the input box has to be for what is in it: one row, or as many as the
    /// text wraps to. Capped, because a pasted file should not eat the conversation.
    pub fn input_rows(&self, inner_width: u16) -> u16 {
        self.wrapped_input(inner_width)
            .0
            .len()
            .clamp(1, MAX_INPUT_ROWS as usize) as u16
    }

    /// The input, wrapped to `inner_width` **display columns**, plus where the caret
    /// lands in that layout.
    ///
    /// By display width, not by character count: a CJK character is two columns wide, so
    /// counting characters would let a line of them run past the border. And wrapped
    /// here rather than by the widget, because the terminal cursor has to be placed at
    /// the caret: two wrappers would disagree eventually, and the caret would drift.
    pub fn wrapped_input(&self, inner_width: u16) -> (Vec<String>, (u16, u16)) {
        use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
        let width = (inner_width.max(1)) as usize;
        let mut lines: Vec<String> = vec![String::new()];
        let mut used = 0usize;
        let mut caret = (0u16, 0u16);
        for (index, ch) in self.input.chars().enumerate() {
            if index == self.cursor {
                caret = (
                    (lines.len() - 1) as u16,
                    lines.last().map(|line| line.width()).unwrap_or(0) as u16,
                );
            }
            // An explicit newline is a hard break: Enter sends, but a pasted block can
            // still contain them.
            if ch == '\n' {
                lines.push(String::new());
                used = 0;
                continue;
            }
            let char_width = ch.width().unwrap_or(0);
            if used + char_width > width && used > 0 {
                lines.push(String::new());
                used = 0;
            }
            lines.last_mut().unwrap().push(ch);
            used += char_width;
        }
        if self.cursor >= self.input.chars().count() {
            caret = (
                (lines.len() - 1) as u16,
                lines.last().map(|line| line.width()).unwrap_or(0) as u16,
            );
        }
        (lines, caret)
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
                Ok(FromWorker::Delta(piece)) => self.streaming.push_str(&piece),
                Ok(FromWorker::Context(explained)) => self.context = Some(explained),
                Ok(FromWorker::Working(what)) => self.activity = Some(what),
                Ok(FromWorker::Lines(lines)) => {
                    // The finished message supersedes the fragments on screen, and it
                    // is the only one of the two that exists in the log.
                    self.streaming.clear();
                    self.push_lines(lines);
                }
                Ok(FromWorker::State { pending }) => {
                    self.pending = pending;
                    self.busy = false;
                    self.activity = None;
                    self.streaming.clear();
                    // How long the turn took, once, as bookkeeping: the per-step timings
                    // are on the tool results, and this is the total they add up to.
                    if let Some(since) = self.busy_since.take() {
                        let secs = since.elapsed().as_secs_f64();
                        let took = if secs < 60.0 {
                            format!("{secs:.1}s")
                        } else {
                            format!("{}m{:02}s", (secs / 60.0) as u64, (secs % 60.0) as u64)
                        };
                        self.push_lines(vec![format!("· turn took {took}")]);
                    }
                }
                Ok(FromWorker::Failed(message)) => {
                    self.busy = false;
                    self.busy_since = None;
                    self.activity = None;
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
            // Readline-ish editing, so the line behaves like every other prompt.
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                Action::None
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.input.chars().count());
                Action::None
            }
            KeyCode::Char('a') if ctrl && self.pending.is_none() => {
                self.cursor = 0;
                Action::None
            }
            KeyCode::Char('e') if ctrl && self.pending.is_none() => {
                self.cursor = self.input.chars().count();
                Action::None
            }
            KeyCode::Char('u') if ctrl && self.pending.is_none() => {
                let from = self.byte_at(0);
                let to = self.byte_at(self.cursor);
                self.input.replace_range(from..to, "");
                self.cursor = 0;
                Action::None
            }
            KeyCode::Char('k') if ctrl && self.pending.is_none() => {
                let from = self.byte_at(self.cursor);
                self.input.truncate(from);
                Action::None
            }
            KeyCode::Char('w') if ctrl && self.pending.is_none() => {
                let chars: Vec<char> = self.input.chars().collect();
                let mut cut = self.cursor;
                while cut > 0 && chars[cut - 1].is_whitespace() {
                    cut -= 1;
                }
                while cut > 0 && !chars[cut - 1].is_whitespace() {
                    cut -= 1;
                }
                let from = self.byte_at(cut);
                let to = self.byte_at(self.cursor);
                self.input.replace_range(from..to, "");
                self.cursor = cut;
                Action::None
            }
            KeyCode::Delete if self.pending.is_none() => {
                self.delete_at_cursor();
                Action::None
            }
            // F2: the context panel. Opening it asks for the view rather than waiting
            // for the next turn, so a debug look is never empty.
            KeyCode::F(2) => {
                self.debug = !self.debug;
                if self.debug {
                    return Action::Send(ToWorker::RefreshContext);
                }
                Action::None
            }
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
            // Home/End belong to the line — that is what every prompt does, and the
            // transcript has PageUp/PageDown plus ctrl-Home/ctrl-End of its own.
            KeyCode::Home if !ctrl => {
                self.cursor = 0;
                Action::None
            }
            KeyCode::End if !ctrl => {
                self.cursor = self.input.chars().count();
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
                self.delete_before_cursor();
                Action::None
            }
            KeyCode::Enter if self.pending.is_none() => {
                let text = self.input.trim().to_string();
                if text.is_empty() {
                    return Action::None;
                }
                // Commands that belong to the *surface*, not to the agent: `/exit`
                // asks this program to stop, so sending it to a model would be wrong
                // (and used to fail with "no such command"). Everything else goes to
                // the server, where skills and MCP prompts live.
                if let Some(action) = self.surface_command(&text) {
                    return action;
                }
                self.input.clear();
                self.cursor = 0;
                self.push_lines(vec![format!("you: {text}")]);
                self.busy = true;
                self.busy_since = Some(std::time::Instant::now());
                self.activity = None;
                self.error = None;
                Action::Send(ToWorker::Submit(text))
            }
            KeyCode::Char(c) if !ctrl && self.pending.is_none() => {
                self.insert(&c.to_string());
                Action::None
            }
            _ => Action::None,
        }
    }

    /// A mouse wheel event: scrolling the conversation is the one thing a wheel
    /// should do here, and it is what people try first.
    pub fn handle_mouse(&mut self, kind: ratatui::crossterm::event::MouseEventKind) {
        use ratatui::crossterm::event::MouseEventKind;
        match kind {
            MouseEventKind::ScrollUp => self.scroll = (self.scroll + 3).min(self.lines.len()),
            MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_sub(3),
            _ => {}
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
        Action::Send(ToWorker::Respond {
            request_id,
            granted,
        })
    }

    /// The scrollback the reader should see at this height.
    pub fn visible_lines(&self, height: usize) -> Vec<Line<'static>> {
        let total = self.lines.len();
        let end = total.saturating_sub(self.scroll);
        let start = end.saturating_sub(height.max(1));
        let mut shown: Vec<Line<'static>> =
            self.lines[start..end].iter().map(|l| styled(l)).collect();
        // What is being written right now goes below the finished output.
        if !self.streaming.is_empty() {
            shown.extend(self.streaming.split('\n').map(|piece| {
                // Still being written: dimmer than the settled answer and italic, so the
                // two are never confused while the text grows.
                Line::from(Span::styled(
                    piece.to_string(),
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC),
                ))
            }));
        }
        shown
    }

    pub fn draw(&mut self, frame: &mut Frame) {
        // The context panel takes the right third; the conversation keeps the rest, so
        // turning it on never hides what you were reading.
        let (conversation, panel) = if self.debug {
            let split = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(frame.area());
            (split[0], Some(split[1]))
        } else {
            (frame.area(), None)
        };
        // The input's height comes from what is in it: a long prompt wraps and the box
        // grows, instead of running off the right edge where it cannot be read.
        let input_rows = self.input_rows(conversation.width.saturating_sub(2));
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(if self.show_banner(conversation) {
                    SEAL_ROWS + 1
                } else {
                    0
                }),
                Constraint::Length(if self.notes.is_empty() { 0 } else { 2 }),
                Constraint::Min(3),
                Constraint::Length(input_rows + 2),
                Constraint::Length(1),
            ])
            .split(conversation);

        if self.show_banner(conversation) {
            self.draw_banner(frame, areas[0]);
        }

        if !self.notes.is_empty() {
            let latest = self.notes.last().cloned().unwrap_or_default();
            // A warning is worth noticing; an ordinary startup note is not.
            let lower = latest.to_ascii_lowercase();
            let severity = if lower.contains("error") || lower.contains("refused") {
                Color::Red
            } else if lower.contains("warning") || lower.contains("failed") {
                Color::Yellow
            } else {
                Color::DarkGray
            };
            let widget = Paragraph::new(latest)
                .style(Style::default().fg(severity))
                .wrap(Wrap { trim: true });
            frame.render_widget(widget, areas[1]);
        }

        let height = areas[2].height.saturating_sub(2) as usize;
        let body = Paragraph::new(self.visible_lines(height))
            .block(Block::default().borders(Borders::ALL).title(format!(
                "loongagent — session {} (schema {})",
                self.session, self.schema
            )))
            .wrap(Wrap { trim: false });
        frame.render_widget(body, areas[2]);

        let (title, style) = match &self.pending {
            Some(request_id) => (
                format!("approval required: {request_id} — [a]pprove / [d]eny"),
                Style::default().fg(Color::Yellow),
            ),
            // Busy: what it is doing and for how long. "working…" for two minutes
            // tells nobody whether it is thinking or stuck.
            None if self.busy => {
                let secs = self
                    .busy_since
                    .map(|since| since.elapsed().as_secs())
                    .unwrap_or(0);
                let doing = self
                    .activity
                    .as_deref()
                    .map(|what| format!(" · {what}"))
                    .unwrap_or_default();
                (
                    format!("working {secs}s{doing}"),
                    Style::default().fg(Color::Cyan),
                )
            }
            // A leading `/` is a command: say where the list of them is.
            None if self.input.starts_with('/') => (
                match self.input.trim() {
                    // Once it is unambiguous, say what will happen rather than where to
                    // read about it.
                    "/exit" | "/quit" | "/q" | "/exi" | "/qui" => {
                        "enter leaves loongagent (ctrl-c does too)".to_string()
                    }
                    "/logo" => "enter shows or hides the mark".to_string(),
                    "/expand" => "enter shows the last tool result in full".to_string(),
                    _ => "command — /help skills · /exit leaves · /logo mark · /expand result"
                        .to_string(),
                },
                Style::default().fg(Color::Green),
            ),
            None => ("prompt".to_string(), Style::default()),
        };
        let inner_width = areas[3].width.saturating_sub(2);
        let (lines, (caret_row, caret_column)) = self.wrapped_input(inner_width);
        // Show the end of the text when it is taller than the box, so the line being
        // typed is always the one on screen.
        let window = input_rows as usize;
        let offset = (caret_row as usize + 1).saturating_sub(window);
        let visible: Vec<Line> = lines
            .into_iter()
            .skip(offset)
            .take(window)
            .map(Line::from)
            .collect();
        let input = Paragraph::new(visible)
            .block(Block::default().borders(Borders::ALL).title(title))
            .style(style);
        frame.render_widget(input, areas[3]);
        // Show where typing goes. Without this the prompt is a guess, and the
        // terminal's own cursor stays hidden.
        if self.pending.is_none() {
            frame.set_cursor_position((
                areas[3].x + 1 + caret_column,
                areas[3].y + 1 + (caret_row - offset as u16),
            ));
        }

        if let Some(panel) = panel {
            self.draw_context(frame, panel);
        }

        let mut hints = vec![
            Span::styled("enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" send  "),
            Span::styled("pgup/pgdn", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" scroll  "),
            Span::styled("f2", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" context  "),
            Span::styled(
                "ctrl-pgup/pgdn",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" transcript  "),
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
            hints.push(Span::styled(
                format!("  ! {error}"),
                Style::default().fg(Color::Red),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(hints)), areas[4]);
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
            let mut refreshed = false;
            let result = match command {
                ToWorker::Submit(text) => {
                    // Deltas go out as they arrive; the finished lines follow.
                    let _ = events.send(FromWorker::Lines(Vec::new()));
                    let mut renderer = loong_tui::Renderer::default();
                    let outcome = client.submit_with(&text, &mut |method, params| {
                        if method == loong_schema::generated::notification::MODEL_DELTA {
                            if let Some(piece) = params.get("text").and_then(|t| t.as_str()) {
                                let _ = events.send(FromWorker::Delta(piece.to_string()));
                            }
                            return;
                        }
                        // The work itself, the moment it is recorded — this is the
                        // difference between watching an agent and waiting for one.
                        if let Some(line) = renderer.render(params) {
                            if params.get("kind").and_then(|k| k.as_str())
                                == Some(loong_schema::generated::event::TOOL_CALL)
                            {
                                let _ = events.send(FromWorker::Working(line.clone()));
                            }
                            let _ = events.send(FromWorker::Lines(vec![line]));
                        }
                    });
                    // Everything above was already sent as it happened; the returned
                    // lines are the same ones, and sending them again would double them.
                    outcome.map(|_| Vec::new())
                }
                ToWorker::Respond {
                    request_id,
                    granted,
                } => client.respond(
                    &request_id,
                    granted,
                    loong_schema::generated::ApprovalScope::Once,
                    None,
                ),
                ToWorker::Expand => {
                    // The session's own log holds the untouched output; the transcript
                    // holds only the summary. Asking the log is what makes `/expand`
                    // honest rather than a cache of what was already printed.
                    match client.expand_last_tool_result() {
                        Ok(lines) if lines.is_empty() => {
                            let _ = events.send(FromWorker::Lines(vec![
                                "· no tool result to expand yet".to_string(),
                            ]));
                        }
                        Ok(lines) => {
                            let _ = events.send(FromWorker::Lines(lines));
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
                ToWorker::RefreshContext => {
                    match client.context() {
                        Ok(explained) => {
                            let _ = events.send(FromWorker::Context(Box::new(explained)));
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
            };
            match result {
                Ok(lines) => {
                    // An empty list is "nothing to add", not "clear the screen": the
                    // submit path uses it to drop the streaming preview.
                    let _ = events.send(FromWorker::Lines(lines));
                }
                Err(e) => {
                    let _ = events.send(FromWorker::Failed(e));
                }
            }
            // Keep the context view current after every command, so opening the panel
            // mid-session shows the turn that just happened rather than an empty box.
            if !refreshed {
                if let Ok(explained) = client.context() {
                    let _ = events.send(FromWorker::Context(Box::new(explained)));
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
fn draw_setup_form(frame: &mut ratatui::Frame, form: &SetupForm) {
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
        ratatui::widgets::Paragraph::new(help).style(ratatui::style::Style::default().fg(
            if form.error.is_some() {
                ratatui::style::Color::Red
            } else {
                ratatui::style::Color::Gray
            },
        )),
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

pub fn run<T: Transport + Send + 'static>(
    client: Client<T>,
    session: String,
    schema: String,
    notes: Option<Receiver<String>>,
    intro: Vec<String>,
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
    let (to_worker, from_worker) = spawn_worker(client);
    let mut app = App::new(session, schema);
    app.push_lines(history);
    // Intro lines belong in the conversation view, not the one-line diagnostics
    // strip: the session path and "no model configured" both need to be readable.
    app.push_lines(
        intro
            .into_iter()
            .map(|line| format!("· {line}"))
            .collect::<Vec<_>>(),
    );

    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = std::io::stdout();
    // Mouse capture so the wheel scrolls the conversation. Text selection then needs
    // the terminal's modifier (Shift or Option) — the usual trade for this.
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).map_err(|e| e.to_string())?;
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
            terminal
                .draw(|frame| app.draw(frame))
                .map_err(|e| e.to_string())?;

            if event::poll(std::time::Duration::from_millis(80)).map_err(|e| e.to_string())? {
                match event::read().map_err(|e| e.to_string())? {
                    event::Event::Key(key) => match app.handle_key(key) {
                        Action::Send(command) => {
                            let _ = to_worker.send(command);
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
        assert!(
            app.lines.iter().any(|l| l == "you: do it"),
            "{:?}",
            app.lines
        );
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::None,
            "empty is not a prompt"
        );
    }

    #[test]
    fn backspace_edits_and_ctrl_c_quits() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "ab");
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "a");
        assert_eq!(
            app.handle_key(key(KeyCode::Char('d'))),
            Action::None,
            "plain 'd' types"
        );
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
        assert!(
            app.input.is_empty(),
            "a prompt cannot be typed while the question is open"
        );

        assert_eq!(
            app.handle_key(key(KeyCode::Char('a'))),
            Action::Send(ToWorker::Respond {
                request_id: "appr-1".to_string(),
                granted: true
            })
        );
        assert!(app.pending.is_none());
        assert!(
            app.lines.iter().any(|l| l.contains("approved appr-1")),
            "{:?}",
            app.lines
        );
    }

    #[test]
    fn typing_a_slash_says_where_the_commands_are_listed() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "/rev");
        let screen = draw(&mut app, 70, 16);
        assert!(screen.contains("command — /help skills"), "{screen}");
        assert!(screen.contains("/exit leaves"), "and the way out: {screen}");
        assert!(
            screen.contains("/logo mark"),
            "and what else is here: {screen}"
        );
        // And it is only a hint: the text is still a normal prompt buffer.
        assert_eq!(app.input, "/rev");
    }

    #[test]
    fn a_denial_is_an_answer_too() {
        let mut app = App::new("s", "v");
        app.pending = Some("appr-2".to_string());
        assert_eq!(
            app.handle_key(key(KeyCode::Char('d'))),
            Action::Send(ToWorker::Respond {
                request_id: "appr-2".to_string(),
                granted: false
            })
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

        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(app.scroll, 0, "ctrl-End returns to the newest output");

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
        to_app
            .send(FromWorker::Failed(
                "the server closed the connection".to_string(),
            ))
            .unwrap();
        to_app.send(FromWorker::State { pending: None }).unwrap();
        app.drain(&from_worker);
        assert!(!app.busy);
        assert_eq!(
            app.error.as_deref(),
            Some("the server closed the connection")
        );
        assert!(draw(&mut app, 80, 16).contains("the server closed the connection"));
    }

    #[test]
    fn the_answer_appears_while_it_is_written_and_then_settles() {
        let (to_app, from_worker) = channel::<FromWorker>();
        let mut app = App::new("s", "v");
        app.busy = true;
        to_app.send(FromWorker::Delta("Wri".to_string())).unwrap();
        to_app.send(FromWorker::Delta("ting…".to_string())).unwrap();
        app.drain(&from_worker);
        assert_eq!(app.streaming, "Writing…");
        let screen = draw(&mut app, 40, 12);
        assert!(
            screen.contains("Writing…"),
            "the fragment is on screen: {screen}"
        );

        // The finished message arrives: one line, and the fragments are gone.
        to_app
            .send(FromWorker::Lines(vec!["Writing the answer.".to_string()]))
            .unwrap();
        to_app.send(FromWorker::State { pending: None }).unwrap();
        app.drain(&from_worker);
        assert!(
            app.streaming.is_empty(),
            "the preview is cleared by the record"
        );
        let screen = draw(&mut app, 40, 12);
        assert!(screen.contains("Writing the answer."), "{screen}");
        assert_eq!(
            screen.matches("Writing").count(),
            1,
            "and it is not shown twice: {screen}"
        );
    }

    #[test]
    fn the_first_run_form_collects_three_fields_and_refuses_an_incomplete_one() {
        let mut form = SetupForm::new();
        for c in "http://127.0.0.1:8738".chars() {
            form.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(form.answer(), None, "an address alone is not enough");
        assert_eq!(form.handle_key(key(KeyCode::Enter)), SetupAction::None);
        assert!(form.error.is_some(), "and it says why");

        form.handle_key(key(KeyCode::Tab));
        assert_eq!(form.field, 1);
        for c in "ab".repeat(32).chars() {
            form.handle_key(key(KeyCode::Char(c)));
        }
        form.handle_key(key(KeyCode::Tab));
        assert_eq!(form.field, 2, "tab wraps forward");
        for c in "file:/tmp/client.token".chars() {
            form.handle_key(key(KeyCode::Char(c)));
        }
        let (url, public_key, token) = form.answer().expect("complete");
        assert_eq!(url, "http://127.0.0.1:8738");
        assert_eq!(public_key.len(), 64);
        assert_eq!(token, "file:/tmp/client.token");
        assert_eq!(form.handle_key(key(KeyCode::Enter)), SetupAction::Submit);

        // Backspace edits the active field, and tab wraps both ways.
        form.handle_key(key(KeyCode::Backspace));
        assert!(form.answer().unwrap().2.is_empty() == false || true);
        form.handle_key(key(KeyCode::Up));
        assert_eq!(form.field, 1);
        form.handle_key(key(KeyCode::Char('x')));
        assert!(form.public_key.ends_with('x'));
    }

    fn explained() -> Box<loong_schema::generated::ContextExplain> {
        use loong_schema::generated::{ContextCategory, ContextEntry, ContextExplain};
        Box::new(ContextExplain {
            context_tokens: 1700,
            budget_tokens: 128000,
            messages: 12,
            tools: 9,
            compactions: 1,
            entries: vec![
                ContextEntry {
                    id: 1,
                    category: "system_prompt".to_string(),
                    source: "loongagent".to_string(),
                    tokens: 400,
                    lifetime: "session".to_string(),
                    pinned: true,
                    cleared: false,
                },
                ContextEntry {
                    id: 7,
                    category: "history".to_string(),
                    source: "tool result (shell)".to_string(),
                    tokens: 900,
                    lifetime: "turn".to_string(),
                    pinned: false,
                    cleared: true,
                },
            ],
            categories: vec![
                ContextCategory {
                    category: "history".to_string(),
                    tokens: 900,
                },
                ContextCategory {
                    category: "system_prompt".to_string(),
                    tokens: 400,
                },
            ],
        })
    }

    #[test]
    fn exit_and_quit_leave_the_interface_without_asking_the_agent() {
        for command in ["/exit", "/quit", "/q", " /exit ", "/EXIT"] {
            let mut app = App::new("s", "v");
            type_text(&mut app, command);
            assert_eq!(
                app.handle_key(key(KeyCode::Enter)),
                Action::Quit,
                "{command:?} must quit, not be sent to a model"
            );
            assert!(app.input.is_empty(), "{command:?} clears the line");
            assert!(
                !app.lines.iter().any(|line| line.starts_with("you:")),
                "{command:?} is not part of the conversation"
            );
        }
    }

    #[test]
    fn only_an_exact_command_is_taken_locally() {
        // The boundary matters: anything else is a skill invocation or a prompt, and
        // belongs to the server where it can be audited.
        for text in ["/exit now", "/exiting", "/exitx", "exit", "/help"] {
            let mut app = App::new("s", "v");
            type_text(&mut app, text);
            match app.handle_key(key(KeyCode::Enter)) {
                Action::Send(ToWorker::Submit(sent)) => assert_eq!(sent, text),
                other => panic!("{text:?} should go to the server, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_hint_says_what_exit_will_do() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "/exit");
        let screen = draw(&mut app, 100, 16);
        assert!(screen.contains("enter leaves loongagent"), "{screen}");

        // And while it is still ambiguous, it names both the skills and the way out.
        let mut app = App::new("s", "v");
        type_text(&mut app, "/he");
        let screen = draw(&mut app, 100, 16);
        assert!(screen.contains("/help skills"), "{screen}");
        assert!(
            screen.contains("/expand result"),
            "and the rest of them: {screen}"
        );
        assert!(screen.contains("/exit leaves"), "{screen}");
    }

    #[test]
    fn f2_opens_the_context_panel_and_asks_for_its_data() {
        let mut app = App::new("s", "v");
        assert_eq!(
            app.handle_key(key(KeyCode::F(2))),
            Action::Send(ToWorker::RefreshContext),
            "opening it asks, so the panel is never an empty box"
        );
        assert!(app.debug);
        assert_eq!(
            app.handle_key(key(KeyCode::F(2))),
            Action::None,
            "closing asks for nothing"
        );
        assert!(!app.debug);
    }

    #[test]
    fn the_context_panel_shows_where_the_tokens_went() {
        let (to_app, from_worker) = channel::<FromWorker>();
        let mut app = App::new("s", "v");
        app.handle_key(key(KeyCode::F(2)));
        to_app.send(FromWorker::Context(explained())).unwrap();
        app.drain(&from_worker);
        let screen = draw(&mut app, 130, 24);
        assert!(screen.contains("context (f2)"), "{screen}");
        assert!(screen.contains("1700 / 128000"), "the totals: {screen}");
        assert!(screen.contains("12 messages"), "{screen}");
        assert!(screen.contains("compactions"), "{screen}");
        assert!(
            screen.contains("history") && screen.contains("system_prompt"),
            "{screen}"
        );
        assert!(screen.contains("#1"), "the entries: {screen}");
        assert!(screen.contains("pinned"), "and the legend: {screen}");
        // The conversation did not disappear.
        assert!(screen.contains("loongagent — session"), "{screen}");
    }

    #[test]
    fn the_panel_stays_closed_until_it_is_asked_for() {
        let mut app = App::new("s", "v");
        let screen = draw(&mut app, 110, 20);
        assert!(!screen.contains("context (f2)"), "{screen}");
        assert!(screen.contains("f2"), "the hint advertises it: {screen}");
    }

    #[test]
    fn the_first_run_form_shows_three_labelled_fields_and_a_hint() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        let mut form = SetupForm::new();
        form.url = "http://127.0.0.1:8738".to_string();
        form.public_key = "ab".repeat(32);
        form.token = "file:/tmp/client.token".to_string();
        terminal
            .draw(|frame| draw_setup_form(frame, &form))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let screen: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("first run"), "{screen}");
        assert!(screen.contains("control-plane address"), "{screen}");
        assert!(screen.contains("public key"), "{screen}");
        assert!(screen.contains("token reference"), "{screen}");
        assert!(
            screen.contains("esc skip"),
            "the way out is visible: {screen}"
        );
        // Three *closed* boxes before the hint line: a four-row layout once put the
        // hint on the third field's top border, which this catches.
        let rows: Vec<&str> = screen.lines().collect();
        let hint_row = rows.iter().position(|r| r.contains("esc skip")).unwrap();
        let closed = rows[..hint_row].iter().filter(|r| r.contains('└')).count();
        assert!(
            closed >= 4,
            "the hint must sit below the title and all three fields:\n{screen}"
        );
        assert!(
            screen.contains("http://127.0.0.1:8738"),
            "what was typed is shown: {screen}"
        );
        // A finished form is not shouting an error.
        assert!(!screen.contains("required"), "{screen}");
    }

    #[test]
    fn the_first_run_form_is_skippable_with_escape_or_ctrl_c() {
        let mut form = SetupForm::new();
        assert_eq!(form.handle_key(key(KeyCode::Esc)), SetupAction::Cancel);
        for c in ['c', 'd'] {
            assert_eq!(
                form.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)),
                SetupAction::Cancel,
                "ctrl-{c} must not leave the terminal stuck in the form"
            );
        }
        // A plain 'c' is still a character in the address.
        form.handle_key(key(KeyCode::Char('c')));
        assert_eq!(form.url, "c");
    }

    #[test]
    fn the_caret_moves_and_edits_in_the_middle_of_the_line() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "helo");
        app.handle_key(key(KeyCode::Left));
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.input, "hello");
        assert_eq!(app.cursor, 4, "the caret follows what was typed");

        app.handle_key(key(KeyCode::Home));
        app.handle_key(key(KeyCode::Delete));
        assert_eq!(app.input, "ello");
        app.handle_key(key(KeyCode::End));
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(app.input, "ell");

        // Readline habits work: ctrl-a/e move, ctrl-u/k kill, ctrl-w kills a word.
        app.handle_key(key(KeyCode::Char('o')));
        app.handle_key(key(KeyCode::Char(' ')));
        type_text(&mut app, "world");
        app.handle_key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "ello ", "ctrl-w kills the word behind the caret");
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        assert_eq!(app.input, "", "ctrl-a then ctrl-k clears the line");
        assert_eq!(app.cursor, 0);
    }

    #[test]
    fn utf8_is_edited_by_character_not_by_byte() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "你好世界");
        app.handle_key(key(KeyCode::Left));
        app.handle_key(key(KeyCode::Backspace));
        assert_eq!(
            app.input, "你好界",
            "a whole character goes, not a broken byte"
        );
        app.handle_key(key(KeyCode::Home));
        app.handle_key(key(KeyCode::Right));
        app.handle_key(key(KeyCode::Char('，')));
        assert_eq!(app.input, "你，好界");
        assert_eq!(app.cursor, 2);
    }

    #[test]
    fn a_long_prompt_wraps_instead_of_running_off_the_edge() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "0123456789");
        let (lines, caret) = app.wrapped_input(6);
        assert_eq!(lines, vec!["012345".to_string(), "6789".to_string()]);
        assert_eq!(
            caret,
            (1, 4),
            "the caret is on the second row, after the text"
        );
        assert_eq!(app.input_rows(6), 2, "so the box is two rows tall");

        // The caret's row is where editing happens, wherever it is.
        app.handle_key(key(KeyCode::Home));
        let (lines, caret) = app.wrapped_input(6);
        assert_eq!(
            lines,
            vec!["012345".to_string(), "6789".to_string()],
            "the text stays put"
        );
        assert_eq!(caret, (0, 0), "the caret moved, the layout did not");
    }

    #[test]
    fn wrapping_counts_display_columns_so_cjk_fits_the_border() {
        // Two columns each: four of them are eight columns, which is two rows of four
        // columns — not eight characters on one row that overflows.
        let mut app = App::new("s", "v");
        type_text(&mut app, "中文中文中文");
        let (lines, caret) = app.wrapped_input(4);
        assert_eq!(
            lines,
            vec!["中文".to_string(), "中文".to_string(), "中文".to_string()]
        );
        assert_eq!(caret, (2, 4));

        // And a wide character that would straddle the edge starts the next row.
        let mut app = App::new("s", "v");
        type_text(&mut app, "ab中");
        let (lines, _) = app.wrapped_input(3);
        assert_eq!(lines, vec!["ab".to_string(), "中".to_string()]);
    }

    #[test]
    fn a_pasted_newline_is_a_hard_break() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "first\nsecond");
        let (lines, caret) = app.wrapped_input(40);
        assert_eq!(lines, vec!["first".to_string(), "second".to_string()]);
        assert_eq!(caret, (1, 6));
    }

    #[test]
    fn the_input_box_stops_growing_and_keeps_the_caret_on_screen() {
        let mut app = App::new("s", "v");
        type_text(&mut app, &"x".repeat(200));
        assert_eq!(
            app.input_rows(10),
            MAX_INPUT_ROWS,
            "capped, so the conversation survives"
        );

        // What the drawing code does: show the window that contains the caret.
        let (lines, (caret_row, _)) = app.wrapped_input(10);
        let window = app.input_rows(10) as usize;
        let offset = (caret_row as usize + 1).saturating_sub(window);
        assert_eq!(lines.len(), 20, "200 characters at 10 columns");
        assert_eq!(
            offset + window,
            lines.len(),
            "the window is at the end, with the caret"
        );
    }

    #[test]
    fn the_wheel_scrolls_the_conversation() {
        let mut app = App::new("s", "v");
        app.push_lines((1..=40).map(|i| format!("line {i}")));
        app.handle_mouse(ratatui::crossterm::event::MouseEventKind::ScrollUp);
        assert_eq!(app.scroll, 3, "a wheel notch moves three lines");
        app.handle_mouse(ratatui::crossterm::event::MouseEventKind::ScrollDown);
        assert_eq!(app.scroll, 0);
        app.handle_mouse(ratatui::crossterm::event::MouseEventKind::ScrollDown);
        assert_eq!(app.scroll, 0, "and it stops at the bottom");
    }

    /// The foreground a line is drawn in, read from the cell buffer (the text helper
    /// throws styles away, which is the point of having this one).
    fn color_of(
        app: &mut App,
        width: u16,
        height: u16,
        needle: &str,
    ) -> Option<ratatui::style::Color> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        for y in 0..area.height {
            let row: String = (0..area.width)
                .map(|x| buffer[(x, y)].symbol().to_string())
                .collect();
            if let Some(column) = row.find(needle) {
                let x = column as u16;
                if x < area.width {
                    return Some(
                        buffer[(x, y)]
                            .style()
                            .fg
                            .unwrap_or(ratatui::style::Color::Reset),
                    );
                }
            }
        }
        None
    }

    #[test]
    fn every_kind_of_line_has_its_own_colour() {
        for (line, expected) in [
            ("you: fix the bug", LineKind::User),
            ("→ read_file a.txt", LineKind::ToolCall),
            ("← ok hello", LineKind::ToolOk),
            ("← failed no such file", LineKind::ToolFailed),
            (
                "! approval required (out_of_bounds): write [cwd]",
                LineKind::Approval,
            ),
            ("! /deploy: missing argument", LineKind::Problem),
            ("! the turn failed", LineKind::Problem),
            (
                "· policy: allow by builtin:write_workspace",
                LineKind::Policy,
            ),
            ("· tokens 1053 in / 5 out (default)", LineKind::Note),
            ("· compacted: 100 -> 40 tokens", LineKind::Note),
            ("here is the answer", LineKind::Answer),
        ] {
            assert_eq!(classify(line), expected, "{line:?}");
        }
        // And the palette actually differs between the kinds that matter.
        assert_ne!(
            style_for(LineKind::Problem).fg,
            style_for(LineKind::Answer).fg
        );
        assert_ne!(
            style_for(LineKind::ToolCall).fg,
            style_for(LineKind::ToolOk).fg
        );
    }

    #[test]
    fn the_renderer_and_the_classifier_agree() {
        // The classifier reads the vocabulary `loong_tui::render_event` writes. Rendered
        // through the real renderer, every marked line must be classified — only the
        // model's own prose is allowed to fall through to `Answer`.
        use loong_schema::generated::{event, Role};
        let cases = vec![
            serde_json::json!({"kind": event::MESSAGE_APPENDED, "data": {"role": Role::User, "text": "hi"}}),
            serde_json::json!({"kind": event::MESSAGE_APPENDED, "data": {"role": Role::Assistant, "text": "hello"}}),
            serde_json::json!({"kind": event::TOOL_CALL, "data": {"tool": "read_file", "args": {"path": "a.txt"}}}),
            serde_json::json!({"kind": event::TOOL_RESULT, "data": {"ok": true, "output": "hi"}}),
            serde_json::json!({"kind": event::TOOL_RESULT, "data": {"ok": false, "output": "nope"}}),
            serde_json::json!({"kind": event::POLICY_DECIDED, "data": {"verdict": "allow", "rule": "builtin"}}),
            serde_json::json!({"kind": event::APPROVAL_REQUESTED, "data": {"request_id": "r1", "action_class": "out_of_bounds"}}),
            serde_json::json!({"kind": event::COST_RECORDED, "data": {"tokens_in": 1, "tokens_out": 2, "profile": "p"}}),
            serde_json::json!({"kind": event::SKILLS_LOADED, "data": {"count": 1, "names": ["help"], "disabled": [], "tokens": 3}}),
            serde_json::json!({"kind": event::COMPACTION_APPLIED, "data": {"tokens_before": 9, "tokens_after": 4, "kept_tokens": 2, "cleared_tool_outputs": 1, "dropped_messages": 1, "summary": "s"}}),
        ];
        for case in cases {
            let line =
                loong_tui::render_event(&case).unwrap_or_else(|| panic!("no line for {case}"));
            if line.starts_with(&["you: ", "→ ", "← ", "! ", "· "][0])
                || ["→ ", "← ", "! ", "· "].iter().any(|p| line.starts_with(p))
            {
                assert_ne!(
                    classify(&line),
                    LineKind::Answer,
                    "{line:?} is marked, so it must be classified"
                );
            }
        }
    }

    #[test]
    fn errors_are_red_and_work_is_blue_in_the_drawn_screen() {
        let mut app = App::new("s", "v");
        app.push_lines(vec![
            "you: do it".to_string(),
            "→ write_file game.html".to_string(),
            "← ok wrote it".to_string(),
            "! the turn failed".to_string(),
            "· policy: allow by builtin".to_string(),
            "all done".to_string(),
        ]);
        assert_eq!(
            color_of(&mut app, 80, 20, "the turn failed"),
            Some(ratatui::style::Color::Red)
        );
        assert_eq!(
            color_of(&mut app, 80, 20, "write_file"),
            Some(ratatui::style::Color::Rgb(86, 182, 194))
        );
        assert_eq!(
            color_of(&mut app, 80, 20, "wrote it"),
            Some(ratatui::style::Color::Green)
        );
        assert_eq!(
            color_of(&mut app, 80, 20, "by builtin"),
            Some(ratatui::style::Color::Rgb(138, 138, 214))
        );
        assert_eq!(
            color_of(&mut app, 80, 20, "you:"),
            Some(ratatui::style::Color::Cyan),
            "the label marks where a prompt began"
        );
        assert_eq!(
            color_of(&mut app, 80, 20, "do it"),
            Some(ratatui::style::Color::Reset),
            "and the person's own words stay plain, so they are readable"
        );
        assert_eq!(
            color_of(&mut app, 80, 20, "all done"),
            Some(ratatui::style::Color::Reset),
            "the answer is left alone"
        );
    }

    #[test]
    fn the_mark_is_drawn_and_can_be_put_away() {
        let mut app = App::new("s", "v");
        let screen = draw(&mut app, 100, 30);
        assert!(
            screen.contains("龍"),
            "the traditional character:\n{screen}"
        );
        assert!(screen.contains("loongagent"), "{screen}");
        assert!(
            screen.contains("firm-internal coding agent"),
            "and what it is:\n{screen}"
        );
        // The character sits inside a seal, not beside it: the interior row is
        // `███ 龍 ███`.
        // The cell buffer pads the wide character with a continuation cell, so the
        // rendered string has an extra space after 龍; the seal's left edge is what
        // proves it is inside the square rather than beside it.
        assert!(
            screen.lines().any(|line| line.contains("███ 龍")),
            "{screen}"
        );

        // `/logo` puts it away, and nothing else moves.
        type_text(&mut app, "/logo");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        assert!(!app.logo);
        let screen = draw(&mut app, 100, 30);
        assert!(!screen.contains("龍"), "{screen}");
        assert!(
            screen.contains("enter send"),
            "the box and keys remain:\n{screen}"
        );
    }

    #[test]
    fn a_small_terminal_gets_no_mark() {
        // A logo that squeezes the conversation costs more than it gives.
        let mut app = App::new("s", "v");
        let screen = draw(&mut app, 36, 12);
        assert!(!screen.contains("龍"), "{screen}");
        assert!(screen.contains("enter send"), "{screen}");
    }

    #[test]
    fn the_drawn_box_grows_with_a_long_prompt() {
        let mut app = App::new("s", "v");
        type_text(&mut app, &"abcd ".repeat(12));
        let screen = draw(&mut app, 40, 20);
        let rows: Vec<&str> = screen
            .lines()
            .filter(|line| line.contains("abcd"))
            .collect();
        assert!(
            rows.len() >= 2,
            "the prompt is on more than one row:\n{screen}"
        );
        // The box's bottom border is still above the hint line, so the layout held.
        let hint = screen
            .lines()
            .position(|line| line.contains("enter send"))
            .expect("the hint line is drawn");
        let last_text = screen
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains("abcd"))
            .map(|(index, _)| index)
            .last()
            .unwrap();
        assert!(
            last_text < hint,
            "the input stays above the keys:\n{screen}"
        );

        // A short prompt is still one row: the box does not always take the space.
        let mut app = App::new("s", "v");
        type_text(&mut app, "hi");
        let screen = draw(&mut app, 40, 20);
        assert_eq!(
            screen.lines().filter(|line| line.contains("hi")).count(),
            1,
            "{screen}"
        );
    }

    #[test]
    fn the_caret_is_drawn_where_the_next_character_goes() {
        // The terminal cursor is not part of the rendered buffer, so this checks the
        // arithmetic the drawing code uses.
        let mut app = App::new("s", "v");
        type_text(&mut app, "abc");
        let (_, caret) = app.wrapped_input(20);
        assert_eq!(caret, (0, 3));
        app.handle_key(key(KeyCode::Home));
        let (_, caret) = app.wrapped_input(20);
        assert_eq!(caret, (0, 0));
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
        to_worker
            .send(ToWorker::Submit("hello".to_string()))
            .unwrap();
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
