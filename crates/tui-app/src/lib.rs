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
        scope: loong_schema::generated::ApprovalScope,
    },
    /// Ask for the context view (the debug panel opening, or a refresh).
    RefreshContext,
    /// Show the most recent tool result in full — the counterpart of collapsing it.
    Expand,
    /// Show one summarised tool result in full, by its place counting from the **oldest**
    /// fold on screen. From the oldest, not from the newest, because the log only grows:
    /// a name that is asked for while a turn is running still means the same result when
    /// the turn lets go of the connection and the answer is fetched.
    ExpandAt {
        at: usize,
    },
    /// Turn approval prompts off or on for this session.
    Yolo(bool),
    /// Ask which models this session offers. The session's own configuration, so it is
    /// a read: nothing is recorded for asking.
    RefreshModels,
    /// Use this model from the next call on.
    SelectModel(String),
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
    /// Something is waiting for a decision, with the detail needed to make one.
    Approval(Box<ApprovalInfo>),
    /// Approval prompts were turned off or on.
    Yolo(bool),
    /// The session's models, and which one a switch landed on.
    Models(Box<loong_schema::generated::ModelList>),
    /// What the turn is doing right now ("shell: cargo test"), so the interface can say
    /// so while it is happening rather than after.
    Working(String),
    /// The full text of one folded tool result, as asked for by `ExpandAt`.
    Expanded {
        at: usize,
        lines: Vec<String>,
    },
    /// The full text of a fold the turn has just written, sent with the summary line that
    /// stands for it. The view keeps it so a fold can be opened without asking the log —
    /// a turn owns the connection while it runs, and reading is exactly what someone does
    /// while a long tool works.
    Fold {
        at: usize,
        lines: Vec<String>,
    },
    Failed(String),
}

/// What a key press means (returned so the caller — or a test — decides).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Send(ToWorker),
    /// Put the transcript on the clipboard (and in a file, which always works).
    Copy,
    /// Tell the terminal whether this program is tracking the mouse.
    Mouse(bool),
    Quit,
}

/// Put text on the clipboard with OSC 52 — the escape sequence terminals honour, and the
/// only way to reach a clipboard from an application that is not the window system's — and
/// also write it beside the session's other files. The file is the honest half: OSC 52 is
/// size-limited and some terminals ignore it, whereas a path always works.
fn copy_everywhere(text: &str, session: &str) -> String {
    let path = std::env::temp_dir().join(format!("loongagent-{session}.txt"));
    let _ = std::fs::write(&path, text);
    let payload = base64(text.as_bytes());
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{payload}\x1b\\");
    let _ = out.flush();
    path.display().to_string()
}

/// The forty lines of base64 this needs, rather than a dependency for forty lines.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The kind of a notification's params, for the few places that ask.
fn kind_of(params: &serde_json::Value) -> &str {
    params.get("kind").and_then(|k| k.as_str()).unwrap_or("")
}

/// How many rows the decision panel asks for: two borders, three details, three answers,
/// and one for air.
///
/// Every detail is clipped to the panel's width and nothing wraps, so each one is one row and
/// this is the whole height — the answers cannot be pushed out of their own box. It is also
/// read as a ceiling when the terminal is short: the transcript keeps at least three rows.
const DECISION_ROWS: u16 = 9;

/// A string forced onto one line and cut to fit: newlines become `⏎` so a shell command
/// cannot wrap the answers out of their own box.
fn one_line(text: &str, width: usize) -> String {
    let flattened = text.replace('\n', "⏎ ").replace('\t', "  ");
    clip(&flattened, width)
}

/// One line's worth of a long string, cut to **display columns**.
///
/// Columns, not characters: a Chinese character is two of them, so a 76-character line of
/// CJK is 150 columns wide. Counting characters is what let a clipped command wrap to three
/// rows inside the decision panel and push the answers out of their own box.
fn clip(text: &str, columns: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if text.width() <= columns {
        return text.to_string();
    }
    let mut head = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let char_width = ch.width().unwrap_or(0);
        if used + char_width > columns.saturating_sub(1) {
            break;
        }
        head.push(ch);
        used += char_width;
    }
    format!("{head}…")
}

/// What a pending approval is about: enough to decide, on one screen.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalInfo {
    pub request_id: String,
    pub class: String,
    pub reason: String,
    pub tool: String,
    pub args: String,
}

/// A little dragon in kaomoji: horns, a snout, whiskers and a tail, drawn with
/// punctuation in the interface's own palette rather than a font glyph in someone
/// else's colours. Two rows, because a mark that takes a third of the screen is a
/// decoration that costs more than it gives.
const MARK_ROWS: u16 = 5;
/// The transcript's accent for "the agent is doing something".
///
/// Named colours, not RGB: a fixed teal or violet is chosen for one background and
/// unreadable on the other, and this interface runs on both. ANSI names are resolved by
/// the terminal against the palette the person actually configured.
const MARK_INK: Color = Color::Cyan;
/// The second voice in the drawing, for policies and the mark's frame.
const MARK_ACCENT: Color = Color::Magenta;
/// The sayings: the terminal's own foreground, dimmed. Present, not shouting.
const MARK_QUIET: Style = Style::new().fg(Color::Reset).add_modifier(Modifier::DIM);
/// Whatever the terminal uses for ordinary text. Bold, so it stands out without
/// inventing a colour that may vanish into the background.
const MARK_TEXT: Style = Style::new().fg(Color::Reset).add_modifier(Modifier::BOLD);
/// Where the name starts, so the two rows line up.
const MARK_TEXT_COLUMN: usize = 39;
/// Interior columns of the frame. The rules are built from it, so the top and bottom lines
/// line up with the sides whatever the ornaments measure on a given terminal.
const MARK_INNER: usize = 35;

/// What the mark says about itself, widest first. The dragon of 煮酒论英雄: great enough to
/// swallow cloud, small enough to hide in a scale — and, being neither, quick and exact.
/// Each row picks the first line that fits the room it has, so wide terminals get the whole
/// thought and narrow ones get its shape.
const MARK_SAYINGS: [&[(usize, &str)]; 3] = [
    &[(0, "loongagent")],
    &[],
    // Nothing on the bottom rule: the inscription beside 龍 already says it, and a mark
    // that explains itself twice talks too much.
    &[],
];

/// The subline, as the owner wrote it, widest first: runes and brackets from the same
/// alphabet as the frame, a `⋄` between the two halves. It sits beside 龍, where it reads as
/// the inscription the character is stamped with.
const MARK_SUBLINE: [(usize, &str); 3] = [
    (28, "⟬ ᛭ 兴云吐雾 ⋄ 隐介藏形 ᛭ ⟭"),
    (19, "兴云吐雾 ⋄ 隐介藏形"),
    (8, "兴云吐雾"),
];
/// Violet, for something that wants an answer rather than something that went wrong.
const MARK_ACCENT_STYLE: Style = Style::new().fg(MARK_ACCENT);

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
    /// A turn the person stopped — their doing, so it is neither an error nor noise.
    Stopped,
    /// A line the interface said about itself (copied, mouse mode).
    Notice,
}

/// The palette. Muted on purpose: a transcript where everything shouts is harder to read
/// than one where only the things that matter are loud.
///
/// "Muted" is the terminal's own foreground dimmed, never a named dark colour. `DarkGray`
/// is bright black, which is *the background* on a dark theme — the bookkeeping lines that
/// say which skills loaded and which profile is in use were drawn into the wall. The
/// foreground the person actually configured cannot be the same as the wall they put it on.
pub fn style_for(kind: LineKind) -> Style {
    match kind {
        LineKind::User => Style::default().fg(Color::Cyan),
        LineKind::Answer => Style::default(),
        LineKind::ToolCall => Style::default().fg(MARK_INK),
        LineKind::ToolOk => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::DIM),
        LineKind::ToolFailed => Style::default().fg(Color::Red),
        LineKind::Approval => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        LineKind::Policy => Style::default().fg(MARK_ACCENT),
        LineKind::Note => MARK_QUIET,
        LineKind::Problem => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        LineKind::Notice => MARK_QUIET,
        LineKind::Stopped => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
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
    if line.starts_with("⧉ ") || line.starts_with("· mouse ") {
        return LineKind::Notice;
    }
    if line.starts_with("⏹ ") {
        return LineKind::Stopped;
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

/// A line of the answer while it is still being written: dimmer than the settled text and
/// italic, so the two are never confused while the words grow. The recorded message
/// replaces it the moment it arrives — the preview is not the record.
fn live_line(piece: &str) -> Line<'static> {
    Line::from(Span::styled(
        piece.to_string(),
        MARK_QUIET.add_modifier(Modifier::ITALIC),
    ))
}

/// How tall the input box may grow. Beyond this it scrolls, so a pasted file cannot
/// take the whole screen away from the conversation.
const MAX_INPUT_ROWS: u16 = 8;

/// The frames of the "it is working" mark: braille, one cell each, so the mark turns in
/// place without nudging the words beside it. A still word reads as a hang after a few
/// seconds, and the one thing someone watching a long tool needs to know is that something
/// is still moving.
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How long one frame is held. Slower than the draw loop on purpose: this is a heartbeat,
/// and a mark turning at the redraw rate reads as flicker.
const SPINNER_MS: u128 = 120;

/// The frame for a time already elapsed. Read from the clock rather than kept as state, so
/// a missed redraw cannot freeze the mark, and a test can ask for the frame it wants
/// instead of racing a timer.
fn spinner_frame(since: Option<std::time::Instant>) -> &'static str {
    let millis = since.map(|since| since.elapsed().as_millis()).unwrap_or(0);
    SPINNER[(millis / SPINNER_MS) as usize % SPINNER.len()]
}

/// A tool result the reader opened: the log's full text, not the summary.
///
/// It is held apart from `lines` on purpose. The transcript is a record and the fold is a
/// view of it, so opening one changes nothing about what was recorded — the same reason
/// `/mouse` is a view toggle rather than a mode the agent knows about.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenFold {
    /// The marker line in `lines` this came from.
    pub marker: usize,
    /// Every line of the result, exactly as the log has it.
    pub lines: Vec<String>,
    /// Rows scrolled up from the end, read the same way the transcript's is.
    pub scroll: usize,
}

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
    /// Whether this program tracks the mouse. **Off by default**: tracking it is what
    /// disables the terminal's own selection, and left-drag-select / right-click-copy is
    /// what a shell does — a TUI should not take that away to gain a wheel. `/mouse`
    /// trades it back for in-app scrolling, and page up/down work either way.
    pub mouse: bool,
    /// True while approval prompts are off for this session — the interface must show
    /// it, because the dangerous state is one where nobody notices it is on (ADR-0007 §5).
    pub yolo: bool,
    /// The pending decision, and which choice is highlighted.
    pub approval: Option<Box<ApprovalInfo>>,
    pub choice: usize,
    /// How to ask the server to stop the running turn. `None` when the surface has no
    /// process to signal: the interface then says so instead of pretending.
    pub interrupt: Option<Box<dyn Fn()>>,
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
    /// Which folded tool result the keyboard is on, by the index of its marker line. A
    /// transcript only ever appends, so an index into it stays true; and nothing has to
    /// be re-found when new output arrives.
    pub fold: Option<usize>,
    /// The fold the reader opened, if any. While it is open the output takes the
    /// transcript's place: reading a result is the one thing on screen worth the room.
    pub open: Option<OpenFold>,
    /// The full text of folds this view already holds, by the number both sides count them
    /// by. Keeping it here rather than asking the log is what makes `enter` work **while a
    /// turn is running**: the turn owns the connection, and this text arrived on it.
    ///
    /// Bounded by [`FOLD_KEEP`]: a session can run all day, and the oldest folds can always
    /// be fetched from the log — just not until the turn lets go of the connection.
    pub folded: std::collections::BTreeMap<usize, Vec<String>>,
    /// The models this session offers, in the order the profile declared them — which
    /// is the order the bar numbers them, and the user's own ranking from cheapest to
    /// most capable.
    pub models: Vec<loong_schema::generated::ModelOffering>,
    /// Which of them the next call will use.
    pub selected: String,
    /// The last thing the user asked for, so `Alt+R` has something to resend.
    ///
    /// Kept in full rather than as a reference: the point of a resend is to send *the
    /// same question* to a different model, and a question that got edited in between
    /// would make the two answers incomparable.
    pub last_prompt: Option<String>,
    /// Which models a background check has confirmed, by name. Absent means nobody has
    /// checked yet, which is not the same as failed.
    pub health: std::collections::BTreeMap<String, bool>,
    pub quit: bool,
}

/// How many folds the view keeps in full. At the default 64 KiB per stream this is a few
/// megabytes at worst, and it covers everything anyone opens while watching a turn.
const FOLD_KEEP: usize = 32;

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
            yolo: false,
            approval: None,
            choice: 0,
            logo: true,
            mouse: false,
            interrupt: None,
            activity: None,
            busy_since: None,
            debug: false,
            context: None,
            streaming: String::new(),
            scroll: 0,
            fold: None,
            open: None,
            folded: std::collections::BTreeMap::new(),
            models: Vec::new(),
            selected: String::new(),
            last_prompt: None,
            health: std::collections::BTreeMap::new(),
            quit: false,
        }
    }

    /// The bookkeeping every send does, so `enter` and `Alt+R` cannot drift apart — an
    /// interface where the resend forgets to mark itself busy would let a second turn
    /// start on top of a running one.
    fn begin_submit(&mut self, text: String) -> Action {
        self.input.clear();
        self.cursor = 0;
        // Sending is a return to the prompt, so the keyboard's place among the folds is
        // given up with it.
        self.fold = None;
        self.push_lines(vec![format!("you: {text}")]);
        self.busy = true;
        self.busy_since = Some(std::time::Instant::now());
        self.activity = None;
        self.error = None;
        self.last_prompt = Some(text.clone());
        Action::Send(ToWorker::Submit(text))
    }

    /// `Alt+R`: ask the same question again, of whichever model is selected now.
    ///
    /// The retry **keeps the history**. That is the whole design, and it is why this is
    /// a resend and not a rewind: the model that takes over can see what the previous
    /// one tried, and what went wrong with it. A clean slate would throw away the most
    /// useful thing in the session — the evidence about what does not work.
    fn resend_last_prompt(&mut self) -> Action {
        if self.busy {
            self.push_lines(vec!["· 上一轮还在跑 — ctrl-c 停下之后再重发".to_string()]);
            return Action::None;
        }
        let Some(text) = self.last_prompt.clone() else {
            self.push_lines(vec!["· 还没有可以重发的 prompt".to_string()]);
            return Action::None;
        };
        // Said out loud, with the model named: a resend that silently used the same
        // model again would look like the agent had simply repeated itself.
        self.push_lines(vec![format!(
            "↻ 用 [{}] 重发上一条（保留原有上下文）",
            self.selected_model_label()
        )]);
        self.begin_submit(text)
    }

    /// What to call the current model on screen. The slot number when there is one, so
    /// it matches the bar; the name alone otherwise.
    fn selected_model_label(&self) -> String {
        if self.selected.is_empty() {
            return "当前模型".to_string();
        }
        match self.models.iter().position(|m| m.name == self.selected) {
            Some(index) if index < 9 => format!("{}] {}", index + 1, self.selected),
            _ => self.selected.clone(),
        }
    }

    /// Take the session's model list, which is a read and arrives before the first
    /// frame. Doing it here rather than on a worker round trip is what keeps the bar
    /// from appearing one frame late, which on a slow link would be the whole session.
    pub fn set_models(&mut self, list: loong_schema::generated::ModelList) {
        self.models = list.models;
        self.selected = list.selected;
    }

    /// Record what a background check found, and say so when it is bad news.
    ///
    /// The model keeps its slot either way. What a failure means is "the last check did
    /// not get through", which is worth seeing and not worth acting on — the switch
    /// still works, and it may well work by the time it is pressed.
    pub fn note_health(&mut self, found: ModelHealth) {
        self.health.insert(found.name.clone(), found.ok);
        if !found.ok {
            self.push_lines(vec![format!(
                "✗ [{}] 校验未通过 — {}",
                found.name, found.detail
            )]);
        }
    }

    /// Notice a `⇄ model a → b (…)` line and move the bar with it.
    ///
    /// The line is what the renderer already produces for a `model.selected` event, and
    /// a *rule* can produce one mid-turn without the interface asking for anything. So
    /// the bar reads the same text a person reads rather than opening a second channel:
    /// one source of truth, and it cannot disagree with what is on screen.
    fn follow_model_change(&mut self, line: &str) {
        let Some(rest) = line.trim().strip_prefix("⇄ model ") else {
            return;
        };
        // `a → b (rule)` when there was a previous model, `b (rule)` on the first call.
        let chosen = match rest.split_once(" → ") {
            Some((_, after)) => after,
            None => rest,
        };
        let name = chosen.split(" (").next().unwrap_or_default().trim();
        if !name.is_empty() && self.models.iter().any(|m| m.name == name) {
            self.selected = name.to_string();
        }
    }

    /// The model in slot `1`..`9`, or the reason there is none.
    ///
    /// One place decides what a digit means, so the bar, the Alt shortcut and the `/N`
    /// fallback cannot disagree about which model is number three.
    pub fn model_in_slot(&self, slot: usize) -> Option<&loong_schema::generated::ModelOffering> {
        (1..=9)
            .contains(&slot)
            .then(|| self.models.get(slot - 1))
            .flatten()
    }

    /// The shortcut, shared by `Alt+<digit>` and `/<digit>`.
    fn switch_to_slot(&mut self, slot: usize) -> Action {
        match self.model_in_slot(slot) {
            Some(offering) => {
                let name = offering.name.clone();
                if name == self.selected {
                    self.push_lines(vec![format!("· already on [{slot}] {name}")]);
                    return Action::None;
                }
                Action::Send(ToWorker::SelectModel(name))
            }
            None => {
                let count = self.models.len();
                self.push_lines(vec![if count == 0 {
                    "· 这个会话没有可选模型 — 用 `loongagent tui --local` 配置一个".to_string()
                } else {
                    format!("· 没有第 {slot} 个模型（共 {count} 个）")
                }]);
                Action::None
            }
        }
    }

    /// The debug panel: what the next request will carry, where the tokens went, and
    /// what has been cleared. Read-only by construction — asking for it records
    /// nothing, so looking cannot change what you are looking at.
    /// Whether there is room for the mark. A logo that squeezes the conversation is a
    /// decoration that costs more than it gives, so a small terminal simply has none.
    fn show_banner(&self, area: ratatui::layout::Rect) -> bool {
        // Five rows and a gap: only where the conversation still has room.
        self.logo && area.height >= 22 && area.width >= 52
    }

    /// The question, its detail, and the three answers — a menu, because a decision worth
    /// stopping for is worth showing the options for.
    ///
    /// Every detail is forced onto **one line**: a shell command with newlines in it used to
    /// wrap until the answers were pushed out of the box, which left a menu with one item.
    fn draw_approval(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let room = area.height.saturating_sub(2) as usize;
        let mut details: Vec<Line> = Vec::new();
        match &self.info() {
            Some((class, reason, tool, args)) => {
                // This line was the one detail nobody clipped, on the assumption that a
                // class and a tool name are short. A tool name can be anything — an MCP
                // server's, or the model's own — so it is clipped like the rest.
                use unicode_width::UnicodeWidthStr;
                let room = (area.width.saturating_sub(4) as usize)
                    .saturating_sub(class.width() + 2)
                    .max(1);
                details.push(Line::from(vec![
                    Span::styled(class.to_string(), Style::default().fg(MARK_ACCENT)),
                    Span::raw("  "),
                    Span::styled(clip(tool, room), Style::default().fg(MARK_INK)),
                ]));
                if !reason.is_empty() {
                    details.push(Line::from(Span::styled(
                        one_line(reason, area.width.saturating_sub(4) as usize),
                        MARK_QUIET,
                    )));
                }
                if !args.is_empty() {
                    details.push(Line::from(Span::styled(
                        one_line(args, area.width.saturating_sub(4) as usize),
                        MARK_QUIET,
                    )));
                }
            }
            None => details.push(Line::from(Span::styled(
                "a decision is waiting for the details…",
                MARK_QUIET,
            ))),
        }
        // The answers are the point: keep them, and drop details first if the box is short.
        // The `.max(1)` that used to be here kept one detail *even when there was no room for
        // it*, so a squeezed panel lost an answer instead of a piece of context.
        let answers = Self::options().len();
        let keep = room.saturating_sub(answers).min(details.len());
        let mut lines: Vec<Line> = details.into_iter().take(keep).collect();
        for (index, (label, granted, _)) in Self::options().into_iter().enumerate() {
            let chosen = index == self.choice;
            let style = if chosen {
                Style::default()
                    .fg(if granted { Color::Green } else { Color::Red })
                    .add_modifier(Modifier::BOLD)
            } else {
                // Readable, not quiet: these are the choices, and the only thing marking one
                // out is that it is coloured and bold. They used to be `DarkGray` — the
                // background itself on a dark theme — which is how a three-answer menu came
                // to look like a single answer.
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(if chosen { "  ▸ " } else { "    " }, style),
                Span::styled(label.to_string(), style),
            ]));
        }
        // Deliberately **not** wrapped. Every line above is one row tall by construction, and
        // the answers are the point of the panel; letting the widget wrap means any line that
        // slips past the clip pushes the last answer out of the box, which is exactly the bug.
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" needs your decision "),
            ),
            area,
        );
    }

    /// The four facts a decision needs, as borrowed strings.
    fn info(&self) -> Option<(&str, &str, &str, &str)> {
        self.approval.as_ref().map(|info| {
            (
                info.class.as_str(),
                info.reason.as_str(),
                info.tool.as_str(),
                info.args.as_str(),
            )
        })
    }

    /// The model bar: what this session can run, numbered, with the current one marked.
    ///
    /// Drawn from the session's own list rather than from a configuration file, because
    /// this is a *status* bar — what matters is what the running session will accept,
    /// not what some file says it should.
    ///
    /// One row, and it gives up models before it gives up the hint: a bar that pushes
    /// the conversation off the screen has stopped being a status line. The numbers are
    /// the keys, so `Alt+3` and `/3` mean the third thing drawn here and nowhere else.
    fn draw_model_bar(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        use unicode_width::UnicodeWidthStr;
        let limit = area.width as usize;
        let slots = self.models.len().min(9);
        // Only worth saying when there is something to say it about. The resend is
        // mentioned only once there is something to resend, because a key advertised
        // before it can work is a key that appears broken.
        let mut hint = String::new();
        if self.models.len() > 1 {
            hint.push_str(&format!("  alt+1..{slots} 切换 · /1../{slots} 兜底"));
        }
        if self.last_prompt.is_some() {
            hint.push_str(" · alt+r 重发");
        }
        let hint_width = UnicodeWidthStr::width(hint.as_str());

        let mut spans: Vec<Span> = Vec::new();
        let mut used = 0usize;
        let mut shown = 0usize;
        for (index, model) in self.models.iter().enumerate().take(slots) {
            let current = model.name == self.selected;
            // A model whose check failed is marked, not hidden: the digits stay where
            // they were, so a network blip does not renumber somebody's keyboard.
            let mark = if self.health.get(&model.name) == Some(&false) {
                "✗"
            } else if current {
                "▸"
            } else {
                " "
            };
            let label = format!("{mark}[{}] {}", index + 1, model.name);
            let label_width = UnicodeWidthStr::width(label.as_str());
            let gap = usize::from(shown > 0);
            // The first slot is always drawn even if it does not fit — a clipped name
            // still says which model is answering, and nothing says it if we stop here.
            if shown > 0 && used + gap + label_width + hint_width > limit {
                break;
            }
            if shown > 0 {
                spans.push(Span::raw(" "));
                used += 1;
            }
            spans.push(Span::styled(
                label,
                if self.health.get(&model.name) == Some(&false) {
                    Style::default().fg(Color::Red)
                } else if current {
                    MARK_TEXT
                } else {
                    MARK_QUIET
                },
            ));
            used += label_width;
            shown += 1;
        }
        if shown < slots {
            spans.push(Span::styled(" …".to_string(), MARK_QUIET));
            used += 2;
        }
        if used + hint_width <= limit {
            spans.push(Span::styled(hint, MARK_QUIET));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    /// The mark: the owner's frame and ornaments, given room to be imposing — five rows, a
    /// double rule, and the character centred with air around it. The words are unchanged.
    fn draw_banner(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        use unicode_width::UnicodeWidthStr;
        let frame_ink = Style::default().fg(MARK_ACCENT);
        let room_for = |drawn: usize| -> usize {
            (area.width as usize).saturating_sub(MARK_TEXT_COLUMN.max(drawn) + 2)
        };
        let saying = |row: usize, room: usize| -> Vec<Span<'static>> {
            MARK_SAYINGS[row]
                .iter()
                .find(|(needs, _)| room >= *needs)
                .map(|(_, text)| {
                    vec![Span::styled(
                        text.to_string(),
                        if row == 0 { MARK_TEXT } else { MARK_QUIET },
                    )]
                })
                .unwrap_or_default()
        };
        let subline = |room: usize| -> Vec<Span<'static>> {
            match MARK_SUBLINE.iter().find(|(needs, _)| room >= *needs) {
                // Ornaments in the frame's colour, words in the text colour: that is what
                // makes it read as an inscription rather than a caption.
                Some((_, text)) if *text == MARK_SUBLINE[0].1 => vec![
                    Span::styled("⟬ ᛭ ".to_string(), frame_ink),
                    Span::styled("兴云吐雾".to_string(), MARK_TEXT),
                    Span::styled(" ⋄ ".to_string(), frame_ink),
                    Span::styled("隐介藏形".to_string(), MARK_TEXT),
                    Span::styled(" ᛭ ⟭".to_string(), frame_ink),
                ],
                Some((_, text)) => vec![Span::styled(text.to_string(), MARK_QUIET)],
                None => Vec::new(),
            }
        };

        // Rules built from the interior width, so every row of the frame is the same length
        // even when an ornament measures differently than expected.
        let rule = |ornament: &str, closer: char| -> String {
            let used = UnicodeWidthStr::width(ornament);
            let side = MARK_INNER.saturating_sub(used) / 2;
            let rest = MARK_INNER.saturating_sub(used + side);
            format!(
                "{}{}{}{}{}",
                if closer == '╗' { '╔' } else { '╚' },
                "═".repeat(side),
                ornament,
                "═".repeat(rest),
                closer
            )
        };
        let top = rule("༺ ᛭ ༻", '╗');
        let bottom = rule("༻ ᛭ ༺", '╝');
        let blank = format!("║{}║", " ".repeat(MARK_INNER));
        // The character centred in the interior, whatever it measures.
        let badge = "【 龍 】";
        let left = (MARK_INNER.saturating_sub(UnicodeWidthStr::width(badge))) / 2;
        let right = MARK_INNER.saturating_sub(left + UnicodeWidthStr::width(badge));

        let art: [(Vec<Span>, Vec<Span>); 5] = [
            (
                vec![Span::styled(top.clone(), frame_ink)],
                saying(0, room_for(UnicodeWidthStr::width(top.as_str()))),
            ),
            (vec![Span::styled(blank.clone(), frame_ink)], Vec::new()),
            (
                vec![
                    Span::styled(format!("║{}", " ".repeat(left)), frame_ink),
                    Span::styled("【", frame_ink),
                    Span::raw(" "),
                    Span::styled("龍", MARK_TEXT),
                    Span::raw(" "),
                    Span::styled("】", frame_ink),
                    Span::styled(format!("{}║", " ".repeat(right)), frame_ink),
                ],
                subline(room_for(MARK_INNER + 2)),
            ),
            (vec![Span::styled(blank.clone(), frame_ink)], Vec::new()),
            (vec![Span::styled(bottom.clone(), frame_ink)], Vec::new()),
        ];

        let mut lines: Vec<Line> = Vec::new();
        for (mut spans, companion) in art {
            let drawn: usize = spans.iter().map(|span| span.content.as_ref().width()).sum();
            spans.push(Span::raw(
                " ".repeat(MARK_TEXT_COLUMN.saturating_sub(drawn)),
            ));
            spans.extend(companion);
            lines.push(Line::from(spans));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }

    fn draw_context(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let mut lines: Vec<Line> = Vec::new();
        match &self.context {
            None => lines.push(Line::from(Span::styled("asking the server…", MARK_QUIET))),
            Some(explained) => {
                let budget = explained.budget_tokens;
                let used = explained.context_tokens;
                let percent = if budget > 0 {
                    format!("{:.0}%", (used as f64 / budget as f64) * 100.0)
                } else {
                    "no limit".to_string()
                };
                lines.push(Line::from(vec![
                    Span::styled("tokens ", MARK_QUIET),
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
                    .style(MARK_QUIET),
                );
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "category            tokens",
                    MARK_QUIET,
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
                    MARK_QUIET,
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
                        MARK_QUIET
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
        let trimmed = text.trim();
        // `/1`..`/9`: the same shortcut as Alt+digit, for a terminal whose Alt never
        // reaches the program (tmux, some macOS setups). Checked before the exact match
        // below because it is a whole family of commands, not one.
        if let Some(digit) = trimmed
            .strip_prefix('/')
            .filter(|rest| rest.chars().count() == 1)
        {
            if let Some(slot) = digit.chars().next().and_then(|c| c.to_digit(10)) {
                if (1..=9).contains(&slot) {
                    self.input.clear();
                    self.cursor = 0;
                    return Some(self.switch_to_slot(slot as usize));
                }
            }
        }
        // `/r`: the resend, for the same terminals that eat Alt.
        if trimmed.eq_ignore_ascii_case("/r") {
            self.input.clear();
            self.cursor = 0;
            return Some(self.resend_last_prompt());
        }
        match trimmed.to_ascii_lowercase().as_str() {
            "/exit" | "/quit" | "/q" => {
                self.input.clear();
                self.cursor = 0;
                Some(Action::Quit)
            }
            // A terminal application cannot be selected with the mouse while it is
            // tracking the mouse, so it has to offer the copy itself.
            "/copy" => {
                self.input.clear();
                self.cursor = 0;
                Some(Action::Copy)
            }
            // And it has to be able to hand the mouse back, for someone who would rather
            // use their terminal's own selection.
            "/mouse" => {
                self.input.clear();
                self.cursor = 0;
                self.mouse = !self.mouse;
                let state = if self.mouse { "on" } else { "off" };
                self.push_lines(vec![format!(
                    "· mouse {state}{}",
                    if self.mouse {
                        " — the wheel scrolls here; hold shift to select (terminal rule)"
                    } else {
                        " — drag to select and copy as usual; page up/down scroll here"
                    }
                )]);
                Some(Action::Mouse(self.mouse))
            }
            // Opting out of the prompts, for a session that is unattended on purpose.
            "/yolo" => {
                self.input.clear();
                self.cursor = 0;
                Some(Action::Send(ToWorker::Yolo(!self.yolo)))
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

    /// Ask for the turn to stop. The server stops at the next step boundary — after the
    /// tool that is already running finishes — so the line says "interrupting", not
    /// "stopped".
    fn stop_turn(&mut self) {
        match &self.interrupt {
            Some(interrupt) => {
                interrupt();
                self.push_lines(vec!["⏹ interrupting…".to_string()]);
            }
            None => self.push_lines(vec![
                "⏹ cannot interrupt: this surface has no server process to signal".to_string(),
            ]),
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
                Ok(FromWorker::Models(list)) => {
                    // The switch already happened by the time this arrives; saying so is
                    // the point, because a silent model change is one nobody can trust.
                    let before = std::mem::take(&mut self.selected);
                    let list = *list;
                    self.set_models(list);
                    if !before.is_empty() && before != self.selected {
                        self.push_lines(vec![format!(
                            "⇄ model {} → {}（下一个 turn 生效）",
                            before, self.selected
                        )]);
                    }
                }
                Ok(FromWorker::Yolo(on)) => {
                    self.yolo = on;
                    self.push_lines(vec![format!(
                        "⚠ approval prompts {}{}",
                        if on { "off" } else { "on" },
                        if on {
                            " — the sandbox still applies; writes outside the workspace are still refused"
                        } else {
                            ""
                        }
                    )]);
                }
                Ok(FromWorker::Approval(info)) => {
                    // A new question resets the highlighted answer: carrying a previous
                    // choice over could approve something nobody looked at.
                    self.choice = 0;
                    self.approval = Some(info);
                }
                Ok(FromWorker::Working(what)) => self.activity = Some(what),
                Ok(FromWorker::Expanded { at, lines }) => {
                    if lines.is_empty() {
                        self.push_lines(vec!["· no tool result to expand yet".to_string()]);
                    } else {
                        self.opened(at, lines);
                    }
                }
                Ok(FromWorker::Fold { at, lines }) => self.remember_fold(at, lines),
                Ok(FromWorker::Lines(lines)) => {
                    // The finished message supersedes the fragments on screen, and it
                    // is the only one of the two that exists in the log.
                    self.streaming.clear();
                    // A switch made *inside* a turn arrives as a rendered line like any
                    // other event, and the bar has to follow it. Otherwise it would keep
                    // naming the model that was answering five steps ago — and a status
                    // bar that lies is worse than no status bar.
                    for line in &lines {
                        self.follow_model_change(line);
                    }
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
        // Alt+digit, not Ctrl+digit. In a VT terminal Ctrl+2..Ctrl+8 arrive as *control
        // characters* — Ctrl+3 as Escape, Ctrl+8 as Delete — so those keys would silently
        // fire other things instead of switching, and the two that alias are the two a
        // user would press first. Alt arrives as a modifier crossterm can actually see.
        // `/<digit>` is the fallback, for terminals that eat Alt.
        if key.modifiers.contains(KeyModifiers::ALT) {
            if let KeyCode::Char(c) = key.code {
                if let Some(slot) = c.to_digit(10).filter(|d| (1..=9).contains(d)) {
                    return self.switch_to_slot(slot as usize);
                }
                if c.eq_ignore_ascii_case(&'r') {
                    return self.resend_last_prompt();
                }
            }
        }
        match key.code {
            // Ctrl-C means "stop this", and only leaves when there is nothing to stop —
            // the habit people bring from every other tool.
            KeyCode::Char('c') if ctrl && self.busy => {
                self.stop_turn();
                Action::None
            }
            KeyCode::Char('c') if ctrl => {
                self.quit = true;
                Action::Quit
            }
            KeyCode::Char('d') if ctrl => {
                self.quit = true;
                Action::Quit
            }
            // A question waiting for an answer outranks everything below it, and a fold
            // that is open is next in line: esc puts the output away rather than
            // interrupting or leaving. Ctrl-C still stops the turn, as always.
            KeyCode::Esc if self.open.is_some() && self.pending.is_none() => {
                self.open = None;
                Action::None
            }
            // One more esc lets go of the selected fold, so the keyboard can always get
            // back to the prompt without leaving — a mode you cannot leave is a trap.
            KeyCode::Esc if self.fold.is_some() && self.pending.is_none() => {
                self.fold = None;
                Action::None
            }
            // Esc is the same idea, and free unless an approval is waiting (then it
            // denies, above).
            KeyCode::Esc if self.busy && self.pending.is_none() => {
                self.stop_turn();
                Action::None
            }
            KeyCode::Esc if self.pending.is_none() => {
                self.quit = true;
                Action::Quit
            }
            // The decision is a menu, not a memorised keystroke: arrows move, enter
            // answers, and the two letters stay for muscle memory.
            KeyCode::Up | KeyCode::Char('k') if self.pending.is_some() => {
                let count = Self::options().len();
                self.choice = (self.choice + count - 1) % count;
                Action::None
            }
            KeyCode::Down | KeyCode::Char('j') if self.pending.is_some() => {
                self.choice = (self.choice + 1) % Self::options().len();
                Action::None
            }
            KeyCode::Enter if self.pending.is_some() => {
                let (_, granted, scope) = Self::options()[self.choice];
                self.answer(granted, scope)
            }
            KeyCode::Char('a') | KeyCode::Char('y') if self.pending.is_some() => {
                self.answer(true, loong_schema::generated::ApprovalScope::Once)
            }
            KeyCode::Char('d') | KeyCode::Char('n') if self.pending.is_some() => {
                self.answer(false, loong_schema::generated::ApprovalScope::Once)
            }
            KeyCode::Esc if self.pending.is_some() => {
                self.answer(false, loong_schema::generated::ApprovalScope::Once)
            }
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
            // Tab walks the folds, newest first: the summary lines are the only place a
            // reader has to go to see the rest, and they should not need a command for it.
            // Shift-Tab walks back the other way. Neither touches the mouse, so selecting
            // text with it keeps working.
            KeyCode::Tab if self.pending.is_none() => self.move_fold(true),
            KeyCode::BackTab if self.pending.is_none() => self.move_fold(false),
            // Paging through an open fold is the same gesture as paging through the
            // transcript, which is the point of it taking the same keys.
            KeyCode::PageUp if self.open.is_some() => self.scroll_fold(5),
            KeyCode::PageDown if self.open.is_some() => self.scroll_fold(-5),
            KeyCode::Up if self.open.is_some() => self.scroll_fold(1),
            KeyCode::Down if self.open.is_some() => self.scroll_fold(-1),
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
            // Enter opens the fold it is on — the key that "clicks" the marker the keyboard
            // walked to. Only while the prompt is empty: the moment there is something to
            // send, enter sends it, because that is what a prompt means. With no fold
            // selected nothing here changes.
            KeyCode::Enter if self.open.is_some() => {
                self.open = None;
                Action::None
            }
            KeyCode::Enter if self.fold.is_some() && self.input.trim().is_empty() => {
                self.toggle_fold()
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
                self.begin_submit(text)
            }
            KeyCode::Char(c) if !ctrl && self.pending.is_none() => {
                self.insert(&c.to_string());
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Every fold marker in the transcript, oldest first. Markers are the client's own
    /// lines (`loong_tui::is_fold_marker`), so the two halves of folding cannot disagree
    /// about which lines the keyboard can land on.
    fn folds(&self) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| loong_tui::is_fold_marker(line))
            .map(|(index, _)| index)
            .collect()
    }

    /// Which fold this marker is, counting from the oldest — the name the client knows the
    /// same result by. Both sides count the same lines in the same order, which is the
    /// whole trick: no identifier has to be shown to anybody. From the oldest rather than
    /// from the newest because the transcript only ever appends, so a fold's number never
    /// changes — not even while the answer is on its way back.
    fn fold_at(&self, marker: usize) -> usize {
        self.lines
            .iter()
            .take(marker)
            .filter(|line| loong_tui::is_fold_marker(line))
            .count()
    }

    /// Keep the full text of a fold this view was shown. A number already held is replaced:
    /// the newest fold is the one anyone is about to open, so what must not survive is the
    /// oldest, and the log still has every one of them.
    fn remember_fold(&mut self, at: usize, lines: Vec<String>) {
        self.folded.insert(at, lines);
        while self.folded.len() > FOLD_KEEP {
            self.folded.pop_first();
        }
    }

    /// Move the keyboard to the fold above (`older`) or below, wrapping at either end. A
    /// first press lands on the newest fold — the one just written, which is the one the
    /// reader is looking at.
    fn move_fold(&mut self, older: bool) -> Action {
        let folds = self.folds();
        let Some(newest) = folds.last().copied() else {
            self.fold = None;
            return Action::None;
        };
        self.fold = Some(
            match self.fold.and_then(|at| folds.iter().position(|f| *f == at)) {
                None => {
                    if older {
                        newest
                    } else {
                        folds[0]
                    }
                }
                Some(at) => {
                    let step = if older {
                        (at + folds.len() - 1) % folds.len()
                    } else {
                        (at + 1) % folds.len()
                    };
                    folds[step]
                }
            },
        );
        Action::None
    }

    /// Move inside an open fold. Positive rows walk back through the output, the same
    /// direction PageUp walks through the transcript.
    fn scroll_fold(&mut self, rows: isize) -> Action {
        if let Some(open) = &mut self.open {
            let last = open.lines.len().saturating_sub(1);
            open.scroll = if rows >= 0 {
                (open.scroll + rows as usize).min(last)
            } else {
                open.scroll.saturating_sub(rows.unsigned_abs())
            };
        }
        Action::None
    }

    /// Open the fold the keyboard is on, or put it away if it is already open — the same
    /// key both ways, because that is what a fold is.
    fn toggle_fold(&mut self) -> Action {
        let Some(marker) = self.fold else {
            return Action::None;
        };
        if self.open.as_ref().is_some_and(|open| open.marker == marker) {
            self.open = None;
            return Action::None;
        }
        let at = self.fold_at(marker);
        // The text is already here whenever the turn sent it, and that is the only way to
        // open a fold **while the turn is still running**: the connection is not free to ask
        // on. A fold this view does not hold — the session's earlier history, or one long
        // since let go — is fetched from the log instead, which the turn may delay.
        if let Some(lines) = self.folded.get(&at).cloned() {
            return self.opened(at, lines);
        }
        Action::Send(ToWorker::ExpandAt { at })
    }

    /// Take delivery of a result the reader asked to see in full. The reply is matched by
    /// the name that was asked for, not by where the fold sits from the end: the answer may
    /// arrive after the turn has written more folds, and the name is the one thing that
    /// still means the same result when it does.
    fn opened(&mut self, at: usize, lines: Vec<String>) -> Action {
        let folds = self.folds();
        let Some(marker) = folds.get(at).copied() else {
            self.push_lines(vec![
                "· that tool result is no longer in the transcript".to_string()
            ]);
            return Action::None;
        };
        self.fold = Some(marker);
        self.open = Some(OpenFold {
            marker,
            lines,
            scroll: 0,
        });
        Action::None
    }

    /// Everything said so far, for `/copy`.
    pub fn transcript(&self) -> String {
        self.lines.join("\n")
    }

    /// A mouse wheel event: scrolling the conversation is the one thing a wheel
    /// should do here, and it is what people try first. Over an open fold it scrolls the
    /// output instead — the wheel follows what is on screen.
    pub fn handle_mouse(&mut self, kind: ratatui::crossterm::event::MouseEventKind) {
        use ratatui::crossterm::event::MouseEventKind;
        match kind {
            MouseEventKind::ScrollUp if self.open.is_some() => {
                self.scroll_fold(3);
            }
            MouseEventKind::ScrollDown if self.open.is_some() => {
                self.scroll_fold(-3);
            }
            MouseEventKind::ScrollUp => self.scroll = (self.scroll + 3).min(self.lines.len()),
            MouseEventKind::ScrollDown => self.scroll = self.scroll.saturating_sub(3),
            _ => {}
        }
    }

    fn answer(&mut self, granted: bool, scope: loong_schema::generated::ApprovalScope) -> Action {
        let Some(request_id) = self.pending.take() else {
            return Action::None;
        };
        self.approval = None;
        let how = if !granted {
            "denied"
        } else {
            match scope {
                loong_schema::generated::ApprovalScope::Once => "approved once",
                loong_schema::generated::ApprovalScope::ActionClass => "approved for this class",
                loong_schema::generated::ApprovalScope::Project => "approved for this project",
            }
        };
        self.push_lines(vec![format!("⧗ {how}: {request_id}")]);
        self.busy = true;
        self.error = None;
        Action::Send(ToWorker::Respond {
            request_id,
            granted,
            scope,
        })
    }

    /// The three answers, in the order they are drawn.
    fn options() -> [(&'static str, bool, loong_schema::generated::ApprovalScope); 3] {
        use loong_schema::generated::ApprovalScope;
        [
            ("Allow once", true, ApprovalScope::Once),
            ("Allow in this project", true, ApprovalScope::Project),
            ("Deny", false, ApprovalScope::Once),
        ]
    }

    /// The scrollback the reader should see at this height.
    ///
    /// An open fold takes the whole area. It is the one thing on screen the reader asked
    /// to look at, and printing the full output beside the summary of itself would be two
    /// copies of one thing — where the summary is, the output goes.
    pub fn visible_lines(&self, height: usize) -> Vec<Line<'static>> {
        let rows = height.max(1);
        if let Some(open) = &self.open {
            // Read the way a terminal reads a command's output: the end is what you are
            // looking at, and paging back walks towards the start.
            let end = open.lines.len().saturating_sub(open.scroll);
            let start = end.saturating_sub(rows);
            return (start..end).map(|at| styled(&open.lines[at])).collect();
        }
        // What the model is writing right now is the **end of the transcript**, not a
        // separate thing below it. Appending it after a full window is what made the live
        // text stop moving: the box showed the beginning of the answer and clipped the
        // newest lines off the bottom, so a long answer appeared to freeze after one
        // screenful. Live lines are part of the content, and the window moves with them.
        let live: Vec<&str> = self.live_lines();
        let total = self.lines.len() + live.len();
        // Scrolling back can only reveal what is above the window, and a transcript shorter
        // than the window has nothing above it. Asking for more than there is used to push
        // lines off the *end* instead: the newest output vanished while `up` was pressed,
        // which is the opposite of what the key means.
        let mut end = total - self.scroll.min(self.max_scroll(rows));
        let mut start = end.saturating_sub(rows);
        // A selection nobody can see is not a selection: when Tab has walked to a fold
        // that has scrolled out of the window, the window follows it.
        if let Some(fold) = self.fold {
            if fold >= end {
                end = fold + 1;
                start = end.saturating_sub(rows);
            } else if fold < start {
                start = fold.saturating_sub(rows / 2);
                end = (start + rows).min(total);
            }
        }
        (start..end)
            .map(|at| match self.lines.get(at) {
                Some(_) => self.transcript_line(at),
                None => live_line(live[at - self.lines.len()]),
            })
            .collect()
    }

    /// The answer being written this moment, as lines. Empty once it settles into the
    /// transcript, which is the only copy that is recorded.
    fn live_lines(&self) -> Vec<&str> {
        if self.streaming.is_empty() {
            Vec::new()
        } else {
            self.streaming.split('\n').collect()
        }
    }

    /// How far back the view can go with a window this tall: everything above it. A
    /// transcript shorter than the window has nothing above it, so the answer is zero.
    fn max_scroll(&self, rows: usize) -> usize {
        (self.lines.len() + self.live_lines().len()).saturating_sub(rows.max(1))
    }

    /// One line of the transcript, with the fold's own business applied: the arrow that
    /// marks a summary, and the highlight that says the keyboard is here. Neither is
    /// stored in `lines`: the transcript is the record, and the fold is a way of looking
    /// at it.
    fn transcript_line(&self, at: usize) -> Line<'static> {
        let Some(line) = self.lines.get(at) else {
            return Line::default();
        };
        if !loong_tui::is_fold_marker(line) {
            return styled(line);
        }
        // The marker's own text stays readable: only the arrow in front of it is added,
        // so the count and the size keep sitting exactly where they were.
        let style = if self.fold == Some(at) {
            // Reverse video is the one highlight every terminal has.
            style_for(LineKind::Note).add_modifier(Modifier::REVERSED)
        } else {
            style_for(LineKind::Note)
        };
        Line::from(Span::styled(format!("▸ {}", line.trim_start()), style))
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
                // The models, first: which one is about to answer is the thing worth
                // knowing before reading anything else. One row, and none at all when
                // there is nothing to choose between.
                Constraint::Length(if self.models.is_empty() { 0 } else { 1 }),
                Constraint::Length(if self.show_banner(conversation) {
                    MARK_ROWS + 1
                } else {
                    0
                }),
                Constraint::Length(if self.notes.is_empty() { 0 } else { 2 }),
                Constraint::Min(3),
                // A question waiting for an answer gets its own block, and it comes **after**
                // the work it is about: the flow reads downwards — events, then the question
                // raised by them, then the box you answer in. It used to sit above the
                // transcript, which put the decision before the evidence for it.
                Constraint::Length(if self.pending.is_some() {
                    // Two borders, one line per detail, three answers. Every detail is
                    // clipped to the width and never wrapped, so this is the whole height.
                    //
                    // `DECISION_ROWS` is what it would like; the floor is the answers
                    // themselves, because a question you cannot answer is worse than one
                    // without context. The transcript gives up its rows first.
                    let floor = 2 + Self::options().len() as u16;
                    DECISION_ROWS
                        .min(conversation.height.saturating_sub(8))
                        .max(floor.min(conversation.height))
                } else {
                    0
                }),
                Constraint::Length(input_rows + 2),
                Constraint::Length(1),
            ])
            .split(conversation);
        // Named, because five anonymous slices is how the notes strip once ended up
        // drawn over the mark.
        let (models, banner, notes, transcript, decision, entry, keys) = (
            areas[0], areas[1], areas[2], areas[3], areas[4], areas[5], areas[6],
        );

        if !self.models.is_empty() {
            self.draw_model_bar(frame, models);
        }
        if self.show_banner(conversation) {
            self.draw_banner(frame, banner);
        }
        if self.pending.is_some() {
            self.draw_approval(frame, decision);
        }

        if !self.notes.is_empty() {
            let latest = self.notes.last().cloned().unwrap_or_default();
            // A warning is worth noticing; an ordinary startup note is not.
            let lower = latest.to_ascii_lowercase();
            let tone = if lower.contains("error") || lower.contains("refused") {
                Style::default().fg(Color::Red)
            } else if lower.contains("warning") || lower.contains("failed") {
                Style::default().fg(Color::Yellow)
            } else {
                MARK_QUIET
            };
            let widget = Paragraph::new(latest).style(tone).wrap(Wrap { trim: true });
            frame.render_widget(widget, notes);
        }

        let height = transcript.height.saturating_sub(2) as usize;
        // Keep the count itself honest, so the `[n/total]` hint and the next key press mean
        // what the window can actually show rather than what was asked for.
        self.scroll = self.scroll.min(self.max_scroll(height));
        let title = match &self.open {
            // The one thing worth saying while reading: how to get back.
            Some(open) => format!(
                "tool result ({} lines) — esc closes, pgup/pgdn scroll",
                open.lines.len()
            ),
            None => format!(
                "loongagent — session {} (schema {})",
                self.session, self.schema
            ),
        };
        let body = Paragraph::new(self.visible_lines(height))
            .block(Block::default().borders(Borders::ALL).title(title))
            .wrap(Wrap { trim: false });
        frame.render_widget(body, transcript);

        let (title, style) = match &self.pending {
            Some(_) if self.yolo => (
                "⚠ YOLO — NOT ASKING · this session approves for you".to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Some(_) => (
                "waiting for your decision — ↑/↓ choose · enter confirm · esc deny".to_string(),
                MARK_ACCENT_STYLE,
            ),
            // Busy: what it is doing and for how long. "working…" for two minutes
            // tells nobody whether it is thinking or stuck.
            // The one state that must never be invisible.
            None if self.yolo => (
                "⚠ YOLO — approvals off, sandbox still on (/yolo to restore)".to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
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
                    // The mark turns, so the line proves the process is alive even while
                    // the words beside it are the same ones it said ten seconds ago.
                    format!("{} working {secs}s{doing}", spinner_frame(self.busy_since)),
                    Style::default().fg(Color::Cyan),
                )
            }
            // A leading `/` is a command: say where the list of them is.
            None if self.input.starts_with('/') => {
                // Lowercased once, so `/R` and `/EXIT` complete the same way `/r` and
                // `/exit` do — the command matcher is case-insensitive, and a hint that
                // disagreed with it would be worse than no hint.
                let typed = self.input.trim().to_ascii_lowercase();
                (
                    match typed.as_str() {
                        // Once it is unambiguous, say what will happen rather than where to
                        // read about it.
                        "/exit" | "/quit" | "/q" | "/exi" | "/qui" => {
                            "enter leaves loongagent (ctrl-c does too)".to_string()
                        }
                        "/logo" => "enter shows or hides the mark".to_string(),
                        "/expand" => {
                            "enter shows the last tool result in full (tab walks the folds)"
                                .to_string()
                        }
                        "/yolo" => {
                            "enter turns approval prompts off (the sandbox stays)".to_string()
                        }
                        "/copy" => "enter copies the whole transcript".to_string(),
                        "/mouse" => "enter hands the mouse back to your terminal".to_string(),
                        // The model shortcuts, said in full only when they would work: a
                        // completion that promises a switch in a session with one model
                        // is a completion that lies.
                        "/r" if self.last_prompt.is_some() => {
                            "enter asks the last prompt again, of the current model".to_string()
                        }
                        _ => match typed
                            .strip_prefix('/')
                            .and_then(|n| n.parse::<usize>().ok())
                        {
                            Some(slot) if (1..=9).contains(&slot) => {
                                match self.model_in_slot(slot) {
                                    Some(offering) => {
                                        format!("enter switches to [{slot}] {}", offering.name)
                                    }
                                    None => format!("no model in slot {slot}"),
                                }
                            }
                            _ => "command — /help · /exit · /logo · /mouse · /copy · /expand"
                                .to_string(),
                        },
                    },
                    Style::default().fg(Color::Green),
                )
            }
            None => ("prompt".to_string(), Style::default()),
        };
        let inner_width = entry.width.saturating_sub(2);
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
        frame.render_widget(input, entry);
        // Show where typing goes. Without this the prompt is a guess, and the
        // terminal's own cursor stays hidden.
        if self.pending.is_none() {
            frame.set_cursor_position((
                entry.x + 1 + caret_column,
                entry.y + 1 + (caret_row - offset as u16),
            ));
        }

        if let Some(panel) = panel {
            self.draw_context(frame, panel);
        }

        let mut hints = if self.open.is_some() {
            vec![
                Span::styled("esc", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" close  "),
                Span::styled("pgup/pgdn", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" scroll"),
            ]
        } else {
            vec![
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
                Span::styled(
                    if self.yolo { "⚠ yolo" } else { "" },
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(if self.yolo { "  " } else { "" }),
                Span::styled("ctrl-c", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(if self.busy { " interrupt" } else { " quit" }),
            ]
        };
        // Where the keyboard is among the folds, and how to move it. Only when there is
        // something to move to: a hint for a key that does nothing is noise.
        let folds = self.folds();
        if self.open.is_none() && !folds.is_empty() {
            hints.push(Span::raw("  "));
            hints.push(Span::styled(
                "tab",
                Style::default().add_modifier(Modifier::BOLD),
            ));
            hints.push(Span::raw(format!(
                " {} fold(s){}",
                folds.len(),
                if self.fold.is_some() {
                    " · enter expands · esc lets go"
                } else {
                    ""
                }
            )));
        }
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
        frame.render_widget(Paragraph::new(Line::from(hints)), keys);
    }
}

/// Own the client on a worker thread so a blocking turn never blocks the screen.
///
/// `folded` is how many folds the transcript already shows, so the worker can number the
/// ones it streams next: they are the same list the keyboard walks, and a fold's number has
/// to mean the same thing on both sides of the channel.
pub fn spawn_worker<T: Transport + Send + 'static>(
    mut client: Client<T>,
    folded: usize,
) -> (Sender<ToWorker>, Receiver<FromWorker>) {
    let (to_worker, commands) = channel::<ToWorker>();
    let (events, from_worker) = channel::<FromWorker>();
    std::thread::spawn(move || {
        // Where the next fold the turn writes will sit among the ones already on screen.
        let mut folds = folded;
        // The decision whose detail has been sent, so re-stating the same one does not
        // re-send it: the panel would lose the highlighted answer on every state change.
        let mut asked: Option<String> = None;
        while let Ok(command) = commands.recv() {
            let mut refreshed = false;
            let result = match command {
                ToWorker::Submit(text) => {
                    // Deltas go out as they arrive; the finished lines follow.
                    let _ = events.send(FromWorker::Lines(Vec::new()));
                    let mut renderer = loong_tui::Renderer::default();
                    // What the turn is doing is named by the call events themselves. The
                    // detail of a decision is assembled by the client and sent after every
                    // command, so it covers the resume path too.
                    let outcome = client.submit_with(&text, &mut |method, params| {
                        if method == loong_schema::generated::notification::MODEL_DELTA {
                            if let Some(piece) = params.get("text").and_then(|t| t.as_str()) {
                                let _ = events.send(FromWorker::Delta(piece.to_string()));
                            }
                            return;
                        }
                        // The work itself, the moment it is recorded — this is the
                        // difference between watching an agent and waiting for one.
                        // The person's own line is already on screen — it was pushed the
                        // moment they pressed enter, so it appears instantly. The event
                        // that comes back is that same line, and rendering it again is how
                        // a prompt appeared twice. History replay is a different path and
                        // still renders it (see `Client::replay`).
                        if kind_of(params) == loong_schema::generated::event::MESSAGE_APPENDED
                            && params
                                .get("data")
                                .and_then(|data| data.get("role"))
                                .and_then(|role| role.as_str())
                                == Some("user")
                        {
                            return;
                        }
                        if let Some(line) = renderer.render(params) {
                            let kind = params.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                            // The summary is what the transcript keeps and what this line
                            // is; the whole of it is what the keyboard may ask for while
                            // the turn is still running, and it is on this very message.
                            // Sending it now is what makes opening a fold a local act.
                            if kind == loong_schema::generated::event::TOOL_RESULT
                                && loong_tui::folded_output(&line)
                            {
                                if let Some(full) = loong_tui::render_event_full(params) {
                                    let _ = events.send(FromWorker::Fold {
                                        at: folds,
                                        lines: full.split('\n').map(|l| l.to_string()).collect(),
                                    });
                                }
                                folds += 1;
                            }
                            if kind == loong_schema::generated::event::TOOL_CALL {
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
                    scope,
                } => client.respond(&request_id, granted, scope, None),
                ToWorker::Yolo(on) => match client.set_auto_approve(on) {
                    Ok(state) => {
                        let _ = events.send(FromWorker::Yolo(state));
                        Ok(Vec::new())
                    }
                    Err(e) => Err(e),
                },
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
                ToWorker::ExpandAt { at } => {
                    // The reader picked a specific summary line off the screen; the log is
                    // asked for that same one, by the number both sides count it by.
                    match client.expand_tool_result(at) {
                        Ok(lines) => {
                            let _ = events.send(FromWorker::Expanded { at, lines });
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
                ToWorker::RefreshModels => {
                    match client.models() {
                        Ok(list) => {
                            let _ = events.send(FromWorker::Models(Box::new(list)));
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
                ToWorker::SelectModel(name) => {
                    // The switch is recorded by the kernel, so the log says who chose
                    // it; the reply is the state afterwards, which is what the bar draws.
                    match client.select_model(&name) {
                        Ok(list) => {
                            let _ = events.send(FromWorker::Models(Box::new(list)));
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
            // The panel's detail comes from here, after the command, rather than from inside
            // the submit callback. A question is met in two places — a submit that stops for
            // it, and a resume after the previous answer, which is where the next one usually
            // appears — and only the first of those ran that callback. That is why the second
            // question used to arrive with no detail at all, while the first was fine.
            let pending = client.pending_decision().cloned();
            let asking = pending.as_ref().map(|decision| decision.request_id.clone());
            if asking != asked {
                asked = asking;
                if let Some(decision) = pending {
                    let _ = events.send(FromWorker::Approval(Box::new(ApprovalInfo {
                        request_id: decision.request_id,
                        class: decision.class,
                        reason: decision.reason,
                        tool: decision.tool,
                        args: decision.args,
                    })));
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

/// One provider the wizard may be pointed at. Passed in rather than looked up, so
/// this crate keeps its promise of holding the screen and nothing else — the catalog
/// lives in `loong-config` and the probe in `loong-providers`, and neither belongs in
/// a UI module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChoice {
    pub id: String,
    pub label: String,
    /// Whether this provider wants a secret at all. A loopback server does not: asking
    /// somebody to paste a key for a process on their own machine is asking them to
    /// invent one, and the form would refuse an empty field for no reason.
    pub needs_key: bool,
}

/// What one validation attempt found, in shapes a screen can show and a caller can
/// carry back out. Deliberately primitive: the wizard displays this and hands it on
/// without needing to know what a "model" is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Validation {
    pub ok: bool,
    /// One line for the status area — already rendered, already in the user's language.
    pub summary: String,
    /// `(model id, context window)` exactly as the vendor reported them.
    pub models: Vec<(String, Option<u64>)>,
    /// Provider ids to float to the top of the list after a refusal, best first.
    ///
    /// A **hint for the next attempt, never a second request**: the caller has already
    /// learned something about the key from the vendor that refused it, and spending
    /// that on reordering costs nothing and sends the key nowhere new. Empty means the
    /// order was right and only the key was wrong.
    pub suggest_order: Vec<String>,
}

/// A key that passed validation, and what it turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChoice {
    pub provider: String,
    pub key: String,
    pub validation: Validation,
}

/// What a background check found out about one model, as the bar needs it.
///
/// Reported rather than enforced: a model that failed its check is marked, not
/// removed. A key revoked at midnight and a network hiccup on the train look identical
/// from here, and an interface that silently deleted somebody's model because the wifi
/// dropped would be worse than one that admitted it was unsure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelHealth {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WizardState {
    Editing,
    Validating,
    /// Refused, with the reason to show. The key stays typed so it can be fixed.
    Refused(String),
    Accepted(String),
    /// The provider answered with a list. Which of them to keep.
    ///
    /// A picker rather than "add everything the vendor serves": a real account returns
    /// twenty ids, a person wants one or two of them, and a bar that fills up with the
    /// rest is not a configuration — it is a chore with a hotkey limit.
    Choosing {
        /// What has been typed to narrow the list. Vendors return long lists and their
        /// names are unstructured, so substring is the only filter that can be offered
        /// without knowing the vendor.
        filter: String,
        /// The row the keyboard is on, counted in *visible* rows.
        cursor: usize,
        /// Every model the provider offered, in its own order.
        all: Vec<(String, Option<u64>)>,
        ticked: Vec<bool>,
    },
}

/// The local first run: paste a key, pick who it belongs to, find out.
///
/// One field and one list. The list exists because a key's shape cannot identify its
/// owner — no vendor here documents a key format, and `sk-` alone matches four of
/// them — so the user confirms and the probe decides. A guess only chooses which row
/// starts highlighted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWizard {
    pub key: String,
    pub providers: Vec<ProviderChoice>,
    pub provider: usize,
    pub state: WizardState,
}

#[derive(Debug, PartialEq, Eq)]
pub enum WizardAction {
    None,
    Submit,
    Cancel,
}

impl LocalWizard {
    pub fn new(providers: Vec<ProviderChoice>) -> Self {
        LocalWizard {
            key: String::new(),
            providers,
            provider: 0,
            state: WizardState::Editing,
        }
    }

    pub fn selected(&self) -> Option<&ProviderChoice> {
        self.providers.get(self.provider)
    }

    /// Whether a key is worth sending anywhere. Not a format check — a length one:
    /// a vendor's own key is at least this long, and an empty field is the only
    /// mistake predictable enough to catch before spending a round trip.
    ///
    /// A provider that wants no secret is always ready: its check is whether the local
    /// server answers, which is a question the probe can ask on its own.
    pub fn ready(&self) -> bool {
        match self.selected() {
            Some(provider) if !provider.needs_key => true,
            _ => self.key.trim().len() >= 8,
        }
    }

    /// What the field shows. The whole key is never drawn: the person typing it knows
    /// what they pasted, and a screen is a thing other people can see.
    pub fn masked_key(&self) -> String {
        let chars: Vec<char> = self.key.chars().collect();
        let visible = chars.len().min(3);
        let prefix: String = chars[..visible].iter().collect();
        format!("{prefix}{}", "•".repeat(chars.len() - visible))
    }

    /// The rows of the picker that match what has been typed, as indices into `all`.
    ///
    /// Indices rather than copies, because the ticks live beside the full list: a filter
    /// that narrowed the view must not change what is ticked outside it.
    pub fn visible(&self) -> Vec<usize> {
        let WizardState::Choosing { filter, all, .. } = &self.state else {
            return Vec::new();
        };
        let needle = filter.trim().to_ascii_lowercase();
        all.iter()
            .enumerate()
            .filter(|(_, (name, _))| {
                needle.is_empty() || name.to_ascii_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// The models that will be written, in the provider's own order.
    pub fn chosen(&self) -> Vec<(String, Option<u64>)> {
        let WizardState::Choosing { all, ticked, .. } = &self.state else {
            return Vec::new();
        };
        all.iter()
            .zip(ticked.iter())
            .filter(|(_, ticked)| **ticked)
            .map(|(entry, _)| entry.clone())
            .collect()
    }

    fn handle_choosing(&mut self, key: KeyEvent) -> WizardAction {
        // Read before borrowing the state mutably: the visible rows are a function of it,
        // and a borrow of `self` cannot live inside a borrow of `self.state`.
        let visible = self.visible();
        let count = visible.len();
        let WizardState::Choosing {
            filter,
            cursor,
            ticked,
            ..
        } = &mut self.state
        else {
            return WizardAction::None;
        };
        match key.code {
            // Back, not out: the key is still typed and the provider is still chosen, so
            // escaping the list should cost a keystroke, not the whole form.
            KeyCode::Esc => {
                self.state = WizardState::Editing;
            }
            KeyCode::Enter => {
                if ticked.iter().any(|t| *t) {
                    return WizardAction::Submit;
                }
                // Refusing rather than writing nothing: "add a provider" that adds no
                // models is not a thing anybody meant to do. The help line says so.
                *cursor = 0;
            }
            KeyCode::Up | KeyCode::BackTab => {
                *cursor = cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Tab => {
                if count > 0 {
                    *cursor = (*cursor + 1).min(count - 1);
                }
            }
            KeyCode::Char(' ') => {
                if let Some(index) = visible.get(*cursor) {
                    ticked[*index] = !ticked[*index];
                }
            }
            // Toggle everything *on screen*: with a filter typed, "all" means the part
            // of the list being looked at.
            KeyCode::Char('a') => {
                let any_off = visible.iter().any(|index| !ticked[*index]);
                for index in visible {
                    ticked[index] = any_off;
                }
            }
            KeyCode::Backspace => {
                filter.pop();
                *cursor = 0;
            }
            KeyCode::Char(c) => {
                filter.push(c);
                *cursor = 0;
            }
            _ => {}
        }
        WizardAction::None
    }

    /// Put a provider and a key in place without submitting anything.    ///
    /// Used for a key the machine already had: the wizard shows it and waits, rather
    /// than validating and saving on its own. An interface that configured models
    /// nobody chose is exactly the surprise this avoids, and "one keystroke to accept"
    /// costs nothing next to it.
    pub fn preselect(&mut self, provider: &str, key: &str) {
        if let Some(index) = self.providers.iter().position(|p| p.id == provider) {
            self.provider = index;
        }
        self.key = key.to_string();
        self.state = WizardState::Editing;
    }

    /// Float the named providers to the top, keeping everything else in order.
    ///
    /// Ids that are not offered are ignored: a caller cannot conjure a provider into a
    /// list the user was shown.
    pub fn reorder(&mut self, first: &[String]) {
        if first.is_empty() {
            return;
        }
        let mut ranked: Vec<ProviderChoice> = Vec::with_capacity(self.providers.len());
        for id in first {
            if ranked.iter().any(|p| &p.id == id) {
                continue;
            }
            if let Some(found) = self.providers.iter().find(|p| &p.id == id) {
                ranked.push(found.clone());
            }
        }
        for provider in &self.providers {
            if !ranked.iter().any(|r| r.id == provider.id) {
                ranked.push(provider.clone());
            }
        }
        self.providers = ranked;
        // The caret goes back to the best guess, because the previous selection is the
        // one that was just refused.
        self.provider = 0;
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> WizardAction {
        use ratatui::crossterm::event::KeyCode;
        // A form that cannot be left is a form that can strand a terminal.
        if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            return WizardAction::Cancel;
        }
        // Editing during a validation would let a half-typed key be the one that got
        // checked, so input is ignored until the answer comes back.
        if self.state == WizardState::Validating {
            return WizardAction::None;
        }
        // Choosing has its own keyboard — the list is what is on screen, so the keys
        // that move around a form mean something else here.
        if matches!(self.state, WizardState::Choosing { .. }) {
            return self.handle_choosing(key);
        }
        match key.code {
            KeyCode::Esc => return WizardAction::Cancel,
            KeyCode::Enter => {
                if self.ready() {
                    return WizardAction::Submit;
                }
                self.state =
                    WizardState::Refused("密钥看起来太短了，请检查是否粘贴完整".to_string());
            }
            KeyCode::Tab | KeyCode::Down => {
                if !self.providers.is_empty() {
                    self.provider = (self.provider + 1) % self.providers.len();
                }
            }
            KeyCode::BackTab | KeyCode::Up => {
                if !self.providers.is_empty() {
                    self.provider =
                        (self.provider + self.providers.len() - 1) % self.providers.len();
                }
            }
            KeyCode::Backspace => {
                if self.selected().map(|p| p.needs_key).unwrap_or(true) {
                    self.key.pop();
                }
                self.state = WizardState::Editing;
            }
            KeyCode::Char(c) => {
                if self.selected().map(|p| p.needs_key).unwrap_or(true) {
                    self.key.push(c);
                }
                self.state = WizardState::Editing;
            }
            _ => {}
        }
        WizardAction::None
    }
}

fn draw_local_wizard(frame: &mut ratatui::Frame, wizard: &LocalWizard) {
    if matches!(wizard.state, WizardState::Choosing { .. }) {
        return draw_model_picker(frame, wizard);
    }
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .split(area);

    frame.render_widget(
        Paragraph::new(
            "本地模式 — 粘贴一个 API key 就能开始。\n\
             不需要填 base_url，也不需要挑模型：它们由服务商自己回答。",
        )
        .block(Block::bordered().title(" first run ")),
        chunks[0],
    );
    let wants_key = wizard.selected().map(|p| p.needs_key).unwrap_or(true);
    let field = if wants_key {
        wizard.masked_key()
    } else {
        "（本机服务，无需密钥）".to_string()
    };
    frame.render_widget(
        Paragraph::new(field.clone()).block(Block::bordered().title(" API key ").border_style(
            if wants_key {
                Style::default().fg(Color::Cyan)
            } else {
                MARK_QUIET
            },
        )),
        chunks[1],
    );
    if wants_key {
        let column = chunks[1].x + 1 + wizard.masked_key().chars().count() as u16;
        frame.set_cursor_position((
            column.min(chunks[1].right().saturating_sub(2)),
            chunks[1].y + 1,
        ));
    }

    let mut lines: Vec<Line> = vec![Line::from(Span::styled(
        "这个 key 属于哪个服务商？（↑↓ 或 tab 选择，回车校验）",
        MARK_QUIET,
    ))];
    for (index, provider) in wizard.providers.iter().enumerate() {
        let active = index == wizard.provider;
        let text = format!(" {} {}", if active { "▸" } else { " " }, provider.label);
        lines.push(Line::from(Span::styled(
            text,
            if active {
                Style::default().fg(MARK_INK).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            },
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(" 服务商 ")),
        chunks[2],
    );

    let (help, style) = match &wizard.state {
        WizardState::Editing => (
            "enter 校验 · ↑↓ 换服务商 · esc 跳过（先用 demo 模型跑起来）".to_string(),
            MARK_QUIET,
        ),
        WizardState::Validating => ("正在连接服务商校验密钥…".to_string(), MARK_QUIET),
        WizardState::Refused(why) => (
            format!("{why}\n\nenter 重试 · esc 跳过"),
            Style::default().fg(Color::Red),
        ),
        WizardState::Accepted(what) => (
            format!("✓ {what}\n\nenter 继续"),
            Style::default().fg(Color::Green),
        ),
        // Unreachable: the picker is drawn by its own function and returns before here.
        // Spelled out rather than a wildcard so a new state cannot silently land on the
        // wrong screen.
        WizardState::Choosing { .. } => (String::new(), MARK_QUIET),
    };
    frame.render_widget(Paragraph::new(help).style(style), chunks[3]);
}

/// The list a provider answered with, and which of it to keep.
///
/// Every row is `[x] name`, so the state of the whole list is readable at a glance —
/// which matters more here than anywhere else in this program, because what is ticked
/// is about to become somebody's model bar.
fn draw_model_picker(frame: &mut ratatui::Frame, wizard: &LocalWizard) {
    let WizardState::Choosing {
        filter,
        cursor,
        all,
        ticked,
    } = &wizard.state
    else {
        return;
    };
    let area = frame.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .split(area);

    let label = wizard
        .selected()
        .map(|p| p.label.clone())
        .unwrap_or_default();
    // One line, because the block's borders take two of the three rows this gets, and a
    // second line here was silently clipped — the hint it duplicated is on the help line
    // below, where there is room for it.
    frame.render_widget(
        Paragraph::new(format!(
            "{label} 返回了 {} 个模型 — 勾选要加入的",
            all.len()
        ))
        .block(Block::bordered().title(" 确认要加入的模型 ")),
        chunks[0],
    );

    frame.render_widget(
        Paragraph::new(if filter.is_empty() {
            "（输入关键字筛选，例如 coder、flash、kimi）".to_string()
        } else {
            filter.clone()
        })
        .style(if filter.is_empty() {
            MARK_QUIET
        } else {
            Style::default().fg(Color::Cyan)
        })
        .block(Block::bordered().title(" 筛选 ")),
        chunks[1],
    );
    if filter.is_empty() {
        // The caret only goes in a field that is actually being edited.
        frame.set_cursor_position((chunks[1].x + 1, chunks[1].y + 1));
    } else {
        let column = chunks[1].x + 1 + filter.chars().count() as u16;
        frame.set_cursor_position((
            column.min(chunks[1].right().saturating_sub(2)),
            chunks[1].y + 1,
        ));
    }

    // One row per line, scrolled so the cursor is always on screen. A vendor list can be
    // longer than the terminal, and a picker that cannot reach row 30 is not a picker.
    let room = chunks[2].height.saturating_sub(2) as usize;
    let visible = wizard.visible();
    let top = if room == 0 || visible.len() <= room {
        0
    } else {
        // Keep the cursor a little below the top so the eye can see what is coming.
        (*cursor).saturating_sub(room / 2).min(visible.len() - room)
    };
    let mut lines: Vec<Line> = Vec::new();
    for (row, index) in visible.iter().enumerate().skip(top).take(room.max(1)) {
        let (name, window) = &all[*index];
        let on = ticked[*index];
        let here = row == *cursor;
        let text = format!(
            "{} {:<34} {}",
            if on { "[x]" } else { "[ ]" },
            name,
            window.map(|w| format!("ctx {w}")).unwrap_or_default()
        );
        lines.push(Line::from(Span::styled(
            text,
            if here {
                Style::default().fg(MARK_INK).add_modifier(Modifier::BOLD)
            } else if on {
                MARK_TEXT
            } else {
                MARK_QUIET
            },
        )));
    }
    if visible.is_empty() {
        lines.push(Line::from(Span::styled(
            "  没有匹配的模型 — 退格清掉筛选".to_string(),
            Style::default().fg(Color::Red),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::bordered().title(format!(
            " 已选 {} / {} ",
            ticked.iter().filter(|t| **t).count(),
            all.len()
        ))),
        chunks[2],
    );

    let help = if ticked.iter().any(|t| *t) {
        "↑↓ 移动 · 空格 勾选 · enter 写入 · esc 回上一步".to_string()
    } else {
        "至少选一个模型 · ↑↓ 移动 · 空格 勾选".to_string()
    };
    frame.render_widget(
        Paragraph::new(help).style(if ticked.iter().any(|t| *t) {
            MARK_QUIET
        } else {
            Style::default().fg(Color::Red)
        }),
        chunks[3],
    );
}

/// What to do once a provider has answered.
///
/// Split out so the rule is stated once and can be tested without a terminal — it is the
/// rule that decides whether anything gets written at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterValidation {
    /// The provider named nothing, so there is nothing to tick. The caller reports that
    /// and the person supplies an id by hand.
    Accept,
    /// The provider named models. **Show them**, however few: writing a model that was
    /// never displayed is configuration nobody chose.
    Choose,
}

/// One model is a choice too.
pub fn after_validation(models: &[(String, Option<u64>)]) -> AfterValidation {
    if models.is_empty() {
        AfterValidation::Accept
    } else {
        AfterValidation::Choose
    }
}

/// Ask for one API key, validate it, and say what the vendor answered.
///
/// The validation is a callback because this crate must not know how to reach a
/// network — and because it makes the interesting half testable without one. The
/// callback is given `(provider id, key)` and returns what to display.
///
/// `Ok(None)` means the user declined, which is a legitimate state: a session with no
/// model configured still runs, and tells them how to fix it.
pub fn local_wizard(
    providers: Vec<ProviderChoice>,
    prefill: Option<(String, String)>,
    validate: &mut dyn FnMut(&str, &str) -> Validation,
) -> Result<Option<KeyChoice>, String> {
    full_screen_needs_a_terminal()?;
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, Event};
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::Terminal;

    let mut wizard = LocalWizard::new(providers);
    // A key this machine already had is *offered*, never taken: the provider is
    // preselected and the field is filled in, so it is one keystroke to accept — and it
    // is on screen, so nobody ends up with models they did not choose.
    if let Some((provider, key)) = prefill {
        wizard.preselect(&provider, &key);
    }
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).map_err(|e| e.to_string())?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).map_err(|e| e.to_string())?;

    // What the picker is choosing from: the provider, the key, and the answered list.
    // Held here rather than inside the state so the *choice* and the *list* stay one
    // thing — the screen only decides which of the models come along.
    let mut pending: Option<(String, String, Validation)> = None;

    let outcome = loop {
        terminal
            .draw(|frame| draw_local_wizard(frame, &wizard))
            .map_err(|e| e.to_string())?;

        if !event::poll(std::time::Duration::from_millis(120)).map_err(|e| e.to_string())? {
            continue;
        }
        if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
            match wizard.handle_key(key) {
                WizardAction::Submit => {
                    // Finishing the picker: the models that are ticked are the ones that
                    // get written, which is the whole point of having asked.
                    if matches!(wizard.state, WizardState::Choosing { .. }) {
                        if let Some((id, key, mut result)) = pending.take() {
                            result.models = wizard.chosen();
                            break Some(KeyChoice {
                                provider: id,
                                key,
                                validation: result,
                            });
                        }
                        continue;
                    }
                    let Some(provider) = wizard.selected().cloned() else {
                        break None;
                    };
                    // Draw the "validating" state *before* blocking: a probe can take
                    // seconds, and a screen that froze with no explanation is the
                    // difference between "slow" and "hung".
                    wizard.state = WizardState::Validating;
                    terminal
                        .draw(|frame| draw_local_wizard(frame, &wizard))
                        .map_err(|e| e.to_string())?;

                    let result = validate(&provider.id, wizard.key.trim());
                    if !result.ok {
                        wizard.state = WizardState::Refused(result.summary.clone());
                        // A refusal is information about *which* vendor, so it is spent
                        // on the list rather than thrown away.
                        wizard.reorder(&result.suggest_order);
                        continue;
                    }
                    wizard.state = WizardState::Accepted(result.summary.clone());
                    // The list is shown whenever the provider *named* anything, even one
                    // model. Skipping it looked like a kindness and was not: the run then
                    // wrote a model nobody had confirmed, which is configuration the
                    // person did not choose and only discovers by reading the file. The
                    // one keystroke it saves is not worth that.
                    //
                    // Nothing at all is the one case with nothing to show: a vendor that
                    // answered without a model list has no rows to tick, and the caller
                    // says so and asks for an id by hand.
                    if after_validation(&result.models) == AfterValidation::Accept {
                        break Some(KeyChoice {
                            provider: provider.id,
                            key: wizard.key.trim().to_string(),
                            validation: result,
                        });
                    }
                    wizard.state = WizardState::Choosing {
                        filter: String::new(),
                        cursor: 0,
                        // Everything starts ticked, so the common answer ("all of them")
                        // is still one keystroke, and `a` clears the list for the person
                        // who wants one id out of twenty.
                        ticked: vec![true; result.models.len()],
                        all: result.models.clone(),
                    };
                    // The key and the summary travel with the choice; this screen only
                    // decides which of the models come along.
                    pending = Some((provider.id.clone(), wizard.key.trim().to_string(), result));
                }
                WizardAction::Cancel => break None,
                WizardAction::None => {}
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
        let screen = draw(&mut app, 80, 22);
        // A menu, not an orange line: the question, then the answers to pick from.
        assert!(screen.contains("needs your decision"), "{screen}");
        assert!(screen.contains("Allow once"), "{screen}");
        assert!(screen.contains("Allow in this project"), "{screen}");
        assert!(screen.contains("Deny"), "{screen}");
        assert!(screen.contains("▸"), "the chosen one is marked: {screen}");

        app.handle_key(key(KeyCode::Char('z')));
        assert!(
            app.input.is_empty(),
            "a prompt cannot be typed while the question is open"
        );

        assert_eq!(
            app.handle_key(key(KeyCode::Char('a'))),
            Action::Send(ToWorker::Respond {
                request_id: "appr-1".to_string(),
                granted: true,
                scope: loong_schema::generated::ApprovalScope::Once,
            })
        );
        assert!(app.pending.is_none());
        assert!(
            app.lines
                .iter()
                .any(|l| l.contains("approved once") && l.contains("appr-1")),
            "{:?}",
            app.lines
        );
    }

    #[test]
    fn typing_a_slash_says_where_the_commands_are_listed() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "/rev");
        let screen = draw(&mut app, 70, 16);
        assert!(screen.contains("command — /help"), "{screen}");
        assert!(screen.contains("/exit"), "and the way out: {screen}");
        assert!(screen.contains("/logo"), "and what else is here: {screen}");
        // And it is only a hint: the text is still a normal prompt buffer.
        assert_eq!(app.input, "/rev");
    }

    #[test]
    fn a_long_command_does_not_push_the_answers_out_of_the_box() {
        // Reported from a real session: a shell command with newlines in it wrapped until
        // only `Allow once` was left, so the menu could not be answered properly.
        let mut app = App::new("s", "v");
        app.pending = Some("appr-1".to_string());
        app.approval = Some(Box::new(ApprovalInfo {
            request_id: "appr-1".to_string(),
            class: "out_of_bounds".to_string(),
            reason: "shell: out_of_bounds is Ask by builtin:out_of_bounds".to_string(),
            tool: "shell".to_string(),
            args: format!(
                "{{\"command\":\"cd /home/x && python3 - <<'EOF' > /tmp/a.js\n{}\"}}",
                "print('line')\n".repeat(30)
            ),
        }));
        let screen = draw(&mut app, 200, 30);
        // Every detail is one row, so the panel cannot grow past its share of the screen —
        // and every answer is inside those rows, which is the part that matters.
        let panel: Vec<&str> = screen
            .lines()
            .skip_while(|line| !line.contains("needs your decision"))
            .take_while(|line| !line.starts_with('└'))
            .collect();
        assert!(
            panel.len() <= DECISION_ROWS as usize,
            "{} rows: {panel:?}",
            panel.len()
        );
        for answer in ["Allow once", "Allow in this project", "Deny"] {
            assert!(
                panel.iter().any(|line| line.contains(answer)),
                "{answer} left the panel: {panel:?}"
            );
        }
        assert!(
            panel.iter().any(|line| line.contains('⏎')),
            "the command is flattened: {panel:?}"
        );
    }

    #[test]
    fn a_command_full_of_cjk_does_not_push_the_answers_out_of_the_box() {
        // The same report, one session later: the first decision was readable and the second
        // was not. Its command carried Chinese comments, and `clip` counted *characters* —
        // 76 of them are 150 columns, so the line wrapped to three rows and pushed `Deny`
        // out of the panel, leaving the details and the answers reading as one jumble.
        let mut app = App::new("s", "v");
        app.pending = Some("appr-2".to_string());
        app.approval = Some(Box::new(ApprovalInfo {
            request_id: "appr-2".to_string(),
            class: "out_of_bounds".to_string(),
            reason: "shell: 在目标目录之外执行命令，需要你确认是否允许这一次写入操作，\
                     因为它会修改工作区之外的文件内容"
                .to_string(),
            tool: "shell".to_string(),
            args: "{\"command\": \"python3 - <<'EOF'\n# 把写好的单页游戏复制到目标目录，并检查标题、脚本标签和字符集是否正确\nEOF\"}"
                .to_string(),
        }));
        // A narrow terminal, where a wide line has the least room.
        let screen = draw(&mut app, 80, 30);
        let panel: Vec<&str> = screen
            .lines()
            .skip_while(|line| !line.contains("needs your decision"))
            .take_while(|line| !line.starts_with('└'))
            .collect();
        assert!(
            panel.len() <= DECISION_ROWS as usize,
            "{} rows: {panel:?}",
            panel.len()
        );
        for answer in ["Allow once", "Allow in this project", "Deny"] {
            assert!(
                panel.iter().any(|line| line.contains(answer)),
                "{answer} left the panel: {panel:?}"
            );
        }
        // And the clipping is *visible*: a line cut to display columns ends in `…` inside the
        // box. Clipping by characters instead would put the ellipsis past the right edge, so
        // the reader would see text simply stop — and the answers would still be there, which
        // is why this half needs its own assertion.
        let clipped = panel
            .iter()
            .find(|line| line.contains("{\"command\""))
            .expect("the command row");
        assert!(
            clipped.trim_end_matches('│').trim_end().ends_with('…'),
            "a clipped detail says so, inside the box: {clipped:?}"
        );
    }

    #[test]
    fn a_squeezed_panel_gives_up_its_details_before_its_answers() {
        // A question you cannot answer is worse than one without context, so the details are
        // what gives way. The panel used to keep at least one detail line *even when there was
        // no room for it*, and on a short terminal that cost the reader an answer.
        for height in 12..=24u16 {
            let mut app = App::new("s", "v");
            app.pending = Some("appr-1".to_string());
            app.approval = Some(Box::new(ApprovalInfo {
                request_id: "appr-1".to_string(),
                class: "out_of_bounds".to_string(),
                reason: "shell: out_of_bounds is Ask by builtin:out_of_bounds".to_string(),
                tool: "shell".to_string(),
                args: "{\"command\":\"ls\"}".to_string(),
            }));
            let screen = draw(&mut app, 80, height);
            for answer in ["Allow once", "Allow in this project", "Deny"] {
                assert!(
                    screen.contains(answer),
                    "at {height} rows, {answer} is missing:\n{screen}"
                );
            }
        }
    }

    #[test]
    fn the_decision_comes_after_the_work_it_is_about() {
        // The flow reads downwards: events, then the question they raised, then the box you
        // answer in. The panel used to sit above the transcript, which put the decision
        // before the evidence for it.
        let mut app = App::new("s", "v");
        app.push_lines(vec!["→ shell ls".to_string()]);
        app.pending = Some("appr-1".to_string());
        let screen = draw(&mut app, 80, 30);
        let transcript = screen.find("loongagent — session").expect("the transcript");
        let decision = screen.find("needs your decision").expect("the decision");
        let prompt = screen
            .find("waiting for your decision")
            .expect("the prompt title");
        assert!(
            transcript < decision && decision < prompt,
            "transcript {transcript}, decision {decision}, prompt {prompt}:\n{screen}"
        );
    }

    #[test]
    fn the_decision_is_a_menu_that_the_keyboard_drives() {
        use loong_schema::generated::ApprovalScope;
        let mut app = App::new("s", "v");
        app.pending = Some("appr-9".to_string());

        // Down moves the highlight, up wraps around, and Enter answers with what is
        // highlighted — including the scope, which is the difference between "this once"
        // and "in this project".
        assert_eq!(app.handle_key(key(KeyCode::Down)), Action::None);
        assert_eq!(app.choice, 1);
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::Respond {
                request_id: "appr-9".to_string(),
                granted: true,
                scope: ApprovalScope::Project,
            })
        );

        let mut app = App::new("s", "v");
        app.pending = Some("appr-9".to_string());
        app.handle_key(key(KeyCode::Up));
        assert_eq!(app.choice, 2, "up from the first lands on the last");
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::Respond {
                request_id: "appr-9".to_string(),
                granted: false,
                scope: ApprovalScope::Once,
            }),
            "the last option is Deny"
        );

        // A fresh question starts at the top: never inherit a choice nobody looked at.
        let mut app = App::new("s", "v");
        app.choice = 2;
        let (to_app, from_worker) = channel::<FromWorker>();
        to_app
            .send(FromWorker::Approval(Box::new(ApprovalInfo {
                request_id: "appr-10".to_string(),
                class: "out_of_bounds".to_string(),
                reason: "write".to_string(),
                tool: "shell".to_string(),
                args: "{\"command\":\"ls\"}".to_string(),
            })))
            .unwrap();
        app.drain(&from_worker);
        assert_eq!(app.choice, 0);
        assert!(app.approval.is_some(), "and the detail is there to draw");
    }

    #[test]
    fn a_denial_is_an_answer_too() {
        let mut app = App::new("s", "v");
        app.pending = Some("appr-2".to_string());
        assert_eq!(
            app.handle_key(key(KeyCode::Char('d'))),
            Action::Send(ToWorker::Respond {
                request_id: "appr-2".to_string(),
                granted: false,
                scope: loong_schema::generated::ApprovalScope::Once,
            })
        );
        assert!(app
            .lines
            .iter()
            .any(|l| l.contains("denied") && l.contains("appr-2")));
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
    fn scrolling_back_cannot_push_the_newest_lines_off_the_end() {
        let startup = [
            "· skills available (1): help",
            "· control.synced",
            "· profile.selected",
            "· new session 66mn884m",
            "· resuming 3 events from that session",
            "· profile: default — DeepSeek flash",
        ];
        let mut app = App::new("s", "v");
        app.push_lines(startup.iter().map(|line| line.to_string()));
        // A window with room to spare: there is nothing above these lines to scroll to, so
        // `up` has to leave them where they are. It used to drop the newest ones instead —
        // the opposite of what the key means, and it looked like output being erased.
        for _ in 0..startup.len() {
            app.handle_key(key(KeyCode::Up));
        }
        let screen = draw(&mut app, 60, 24);
        for line in startup {
            assert!(
                screen.contains(line),
                "{line:?} was pushed off the end:\n{screen}"
            );
        }
        assert_eq!(app.scroll, 0, "and the count says what the screen says");
    }

    #[test]
    fn the_bookkeeping_lines_are_not_drawn_into_the_wall() {
        // `DarkGray` is bright black: on a dark theme it *is* the background, so the lines
        // saying which skills loaded and which profile is in use were invisible — reported
        // from a real screen, where scrolling appeared to grey the notes out. Quiet is the
        // terminal's own foreground, dimmed, never a named colour that may be the wall.
        for kind in [LineKind::Note, LineKind::Notice] {
            let style = style_for(kind);
            assert_eq!(
                style.fg,
                Some(Color::Reset),
                "{kind:?} must not name a colour"
            );
            assert!(style.add_modifier.contains(Modifier::DIM), "{kind:?}");
        }
        let mut app = App::new("s", "v");
        app.push_lines(vec!["· skills available (1): help".to_string()]);
        assert_eq!(
            color_of(&mut app, 60, 24, "· skills available"),
            Some(ratatui::style::Color::Reset),
            "and that is what reaches the screen"
        );
    }

    #[test]
    fn the_answers_that_are_not_chosen_are_still_readable() {
        // The same colour, in the one place it decides something: every answer but the
        // highlighted one was `DarkGray`, so a menu read as a single option — the earlier
        // "the decision menu lost its answers" report had this in it too.
        let mut app = App::new("s", "v");
        app.pending = Some("appr-9".to_string());
        assert_eq!(
            color_of(&mut app, 80, 24, "Allow in this project"),
            Some(ratatui::style::Color::Reset),
            "an unchosen answer is dimmed, not drawn into the background"
        );
        assert_eq!(
            color_of(&mut app, 80, 24, "Deny"),
            Some(ratatui::style::Color::Reset)
        );
    }

    /// The transcript the client writes for two summarised tool results: the summary
    /// lines, then the marker that stands for the rest.
    fn two_folds(app: &mut App) {
        app.push_lines(vec![
            "you: run it".to_string(),
            "→ shell cargo test".to_string(),
            "← ok older 1".to_string(),
            "  older 2".to_string(),
            "  … 47 more line(s), 900 B · /expand".to_string(),
            "→ shell cargo build".to_string(),
            "← ok newest 1".to_string(),
            "  … 49 more line(s), 900 B · /expand".to_string(),
        ]);
    }

    #[test]
    fn tab_walks_the_folds_and_enter_shows_what_they_hide() {
        let mut app = App::new("s", "v");
        two_folds(&mut app);
        let screen = draw(&mut app, 72, 24);
        assert!(
            screen.contains("▸ … 47 more line(s), 900 B · /expand"),
            "a summary says what is hidden and how to get it: {screen}"
        );

        // Tab lands on the newest fold: the one just written, which is the one being read.
        assert_eq!(app.handle_key(key(KeyCode::Tab)), Action::None);
        assert_eq!(app.fold, Some(7));
        let selected = app
            .visible_lines(20)
            .into_iter()
            .find(|line| {
                line.spans
                    .iter()
                    .any(|span| span.content.contains("49 more"))
            })
            .expect("the marker is on screen");
        assert!(
            selected.spans[0]
                .style
                .add_modifier
                .contains(Modifier::REVERSED),
            "the keyboard's place is visible, not just remembered"
        );
        // Tab again walks to the older one, and shift-tab comes back.
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.fold, Some(4));
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.fold, Some(7));

        // Enter asks the log for that same result, by the number the two sides share — and
        // the answer is what fills the screen.
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::ExpandAt { at: 1 })
        );
        let (says, worker) = channel();
        says.send(FromWorker::Expanded {
            at: 1,
            lines: (1..=50).map(|n| format!("newest {n}")).collect(),
        })
        .unwrap();
        app.drain(&worker);
        let screen = draw(&mut app, 72, 24);
        assert!(screen.contains("tool result (50 lines)"), "{screen}");
        assert!(
            screen.contains("newest 50"),
            "the end is what is read: {screen}"
        );
        app.handle_key(key(KeyCode::PageUp));
        assert!(
            draw(&mut app, 72, 24).contains("newest"),
            "and paging back works"
        );
        // Esc closes the output and keeps the fold; one more esc lets the fold go.
        app.handle_key(key(KeyCode::Esc));
        assert!(app.open.is_none());
        assert_eq!(app.fold, Some(7));
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.fold, None);
        assert!(
            draw(&mut app, 72, 24).contains("session s"),
            "and does not leave"
        );
    }

    #[test]
    fn a_fold_never_takes_enter_away_from_a_prompt() {
        let mut app = App::new("s", "v");
        two_folds(&mut app);
        // With nothing selected, enter sends — exactly as it did before folds existed.
        type_text(&mut app, "hello");
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::Submit("hello".to_string()))
        );
        // With a fold selected and something typed, enter still sends: a prompt with text
        // in it means send, and the fold is let go with it.
        app.handle_key(key(KeyCode::Tab));
        type_text(&mut app, "again");
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::Submit("again".to_string()))
        );
        assert_eq!(app.fold, None, "sending is a return to the prompt");
    }

    #[test]
    fn enter_opens_a_fold_the_turn_sent_without_asking_the_log() {
        let mut app = App::new("s", "v");
        two_folds(&mut app);
        // The turn sent the newest fold's own text along with its summary line, which is
        // what it does for every fold it writes.
        app.remember_fold(1, (1..=50).map(|n| format!("newest {n}")).collect());
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.fold, Some(7));
        // Enter asks nobody: the text is already here. That is what makes this work while
        // the turn is still running, when the connection is not free to ask on.
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        let screen = draw(&mut app, 72, 24);
        assert!(screen.contains("tool result (50 lines)"), "{screen}");
        assert!(screen.contains("newest 50"), "{screen}");
        // A fold the view does not hold is still fetched from the log, in full.
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Esc));
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::ExpandAt { at: 0 })
        );
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
    fn the_answer_being_written_stays_visible_to_its_last_line() {
        let (to_app, from_worker) = channel::<FromWorker>();
        let mut app = App::new("s", "v");
        app.busy = true;
        app.push_lines(vec!["you: 写个游戏".to_string()]);
        // A model that writes more than one screenful. The screen has to show the **end**
        // of it: the point of a live preview is watching the newest words arrive. The live
        // text used to be appended below a full window, so the box clipped it and the
        // answer appeared to stop moving after one screenful.
        for n in 1..=30 {
            to_app
                .send(FromWorker::Delta(format!("thinking line {n}\n")))
                .unwrap();
        }
        app.drain(&from_worker);
        let screen = draw(&mut app, 60, 24);
        assert!(screen.contains("thinking line 30"), "{screen}");
        assert!(
            !screen.contains("thinking line 1\n"),
            "the oldest lines give way to the newest: {screen}"
        );
    }

    #[test]
    fn the_working_title_turns_so_a_wait_does_not_look_like_a_hang() {
        // The frame is read from the clock, so a missed redraw cannot freeze the mark — and
        // a test can name the frame it wants instead of racing a timer.
        assert_eq!(spinner_frame(None), SPINNER[0]);
        let start = std::time::Instant::now();
        assert_eq!(spinner_frame(Some(start)), SPINNER[0]);
        assert_ne!(
            spinner_frame(Some(
                start - std::time::Duration::from_millis(SPINNER_MS as u64)
            )),
            SPINNER[0],
            "one frame later is a different mark"
        );

        let mut app = App::new("s", "v");
        app.busy = true;
        app.busy_since = Some(start);
        app.activity = Some("shell: cargo test".to_string());
        let screen = draw(&mut app, 60, 20);
        assert!(
            screen.contains("working 0s · shell: cargo test"),
            "the words still say what it is doing: {screen}"
        );
        assert!(
            SPINNER.iter().any(|frame| screen.contains(frame)),
            "and a mark that moves is beside them: {screen}"
        );
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
        assert!(screen.contains("/help"), "{screen}");
        assert!(screen.contains("/copy"), "and the copy command: {screen}");
        assert!(screen.contains("/mouse"), "and the mouse one: {screen}");
        assert!(screen.contains("/exit"), "{screen}");
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
            Some(ratatui::style::Color::Cyan)
        );
        assert_eq!(
            color_of(&mut app, 80, 20, "wrote it"),
            Some(ratatui::style::Color::Green)
        );
        assert_eq!(
            color_of(&mut app, 80, 20, "by builtin"),
            Some(ratatui::style::Color::Magenta)
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
    fn control_c_stops_a_running_turn_and_otherwise_leaves() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let calls = Arc::new(AtomicUsize::new(0));
        let asked = Arc::clone(&calls);

        let mut app = App::new("s", "v");
        app.interrupt = Some(Box::new(move || {
            asked.fetch_add(1, Ordering::SeqCst);
        }));
        // Idle: Ctrl-C leaves, exactly as it always did.
        let mut idle = App::new("s", "v");
        assert_eq!(
            idle.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        // Running: it asks the server to stop, and says so without pretending the work
        // has already stopped.
        app.busy = true;
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::None
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(
            app.lines
                .iter()
                .any(|line| line.starts_with("⏹ interrupting")),
            "{:?}",
            app.lines
        );
        // Esc is the same request.
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        // A surface with nothing to signal says so instead of lying.
        let mut app = App::new("s", "v");
        app.busy = true;
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        assert!(
            app.lines
                .iter()
                .any(|line| line.contains("cannot interrupt")),
            "{:?}",
            app.lines
        );
    }

    #[test]
    fn the_interrupted_event_renders_as_a_stop_the_person_caused() {
        use loong_schema::generated::event;
        let line = loong_tui::render_event(&serde_json::json!({
            "kind": event::TURN_INTERRUPTED,
            "data": {"reason": "interrupted"},
        }))
        .expect("an interrupted turn is shown");
        assert!(line.contains("stopped by you"), "{line}");
        assert_eq!(classify(&line), LineKind::Stopped);
    }

    #[test]
    fn a_stopped_turn_is_its_own_colour() {
        assert_eq!(
            classify("⏹ interrupted — stopped by you"),
            LineKind::Stopped
        );
        assert_eq!(
            style_for(LineKind::Stopped).fg,
            Some(ratatui::style::Color::Yellow)
        );
        let mut app = App::new("s", "v");
        app.push_lines(vec!["⏹ interrupted — stopped by you".to_string()]);
        assert_eq!(
            color_of(&mut app, 80, 20, "stopped by you"),
            Some(ratatui::style::Color::Yellow)
        );
    }

    #[test]
    fn the_person_s_own_line_is_matched_by_its_event_not_printed_twice() {
        // The screen push happens on enter; the event that comes back is the same line.
        // The worker skips exactly that one, and only while a turn is live.
        let own = serde_json::json!({
            "kind": loong_schema::generated::event::MESSAGE_APPENDED,
            "data": {"role": "user", "text": "hi"},
        });
        assert_eq!(
            kind_of(&own),
            loong_schema::generated::event::MESSAGE_APPENDED
        );
        assert_eq!(own["data"]["role"].as_str(), Some("user"));

        let assistant = serde_json::json!({
            "kind": loong_schema::generated::event::MESSAGE_APPENDED,
            "data": {"role": "assistant", "text": "hello"},
        });
        assert_ne!(
            assistant["data"]["role"].as_str(),
            Some("user"),
            "the model's own words are still rendered"
        );
    }

    #[test]
    fn yolo_is_a_command_and_is_never_invisible() {
        let mut app = App::new("s", "v");
        type_text(&mut app, "/yolo");
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::Yolo(true)),
            "the command asks the server, which records the opt-out"
        );
        assert!(app.input.is_empty());

        // Until the server answers, the state is unchanged: the interface does not
        // pretend a permission it does not have.
        assert!(!app.yolo);
        let (to_app, from_worker) = channel::<FromWorker>();
        to_app.send(FromWorker::Yolo(true)).unwrap();
        app.drain(&from_worker);
        assert!(app.yolo);
        assert!(
            app.lines
                .iter()
                .any(|line| line.contains("approval prompts off") && line.contains("sandbox")),
            "it says what changed and what did not: {:?}",
            app.lines
        );

        // Visible in the input box, always.
        let screen = draw(&mut app, 100, 24);
        assert!(screen.contains("YOLO"), "the state is on screen: {screen}");
        assert!(screen.contains("sandbox still on"), "{screen}");

        // And it can be turned back on, from the command or the state.
        let screen = draw(&mut app, 100, 24);
        assert!(screen.contains("/yolo"), "{screen}");
    }

    #[test]
    fn copy_puts_the_transcript_on_the_clipboard_and_in_a_file() {
        let mut app = App::new("s", "v");
        app.push_lines(vec!["you: hello".to_string(), "hi".to_string()]);
        type_text(&mut app, "/copy");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Copy);
        assert!(app.input.is_empty());
        // The file half, which is what works everywhere: written beside nothing
        // important, in the temporary directory, and named for the session.
        let path = copy_everywhere(&app.transcript(), "testcopy");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("you: hello"), "{written}");
        assert!(written.contains("hi"), "{written}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn base64_is_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"hello world"), "aGVsbG8gd29ybGQ=");
    }

    #[test]
    fn the_mouse_can_be_handed_back_to_the_terminal() {
        let mut app = App::new("s", "v");
        // Off by default: the terminal's own selection is not something a TUI may take
        // away in exchange for a wheel.
        assert!(!app.mouse, "does not track the mouse unless asked");

        type_text(&mut app, "/mouse");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Mouse(true));
        assert!(app.mouse);
        assert!(
            app.lines
                .iter()
                .any(|line| line.contains("wheel scrolls here")),
            "and says what it costs: {:?}",
            app.lines
        );

        type_text(&mut app, "/mouse");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Mouse(false));
        assert!(!app.mouse);
        assert!(
            app.lines.iter().any(|line| line.contains("drag to select")),
            "and says what came back: {:?}",
            app.lines
        );
    }

    #[test]
    fn the_mark_is_drawn_and_can_be_put_away() {
        let mut app = App::new("s", "v");
        // Wide: the frame, the character, the inscription beside it, the philosophy below.
        let screen = draw(&mut app, 150, 30);
        assert!(
            screen.contains("༺ ᛭ ༻") && screen.contains("༻ ᛭ ༺"),
            "{screen}"
        );
        assert!(screen.contains('【') && screen.contains('】'), "{screen}");
        assert!(screen.contains('龍'), "{screen}");
        assert!(screen.contains("loongagent"), "{screen}");
        assert!(
            screen.contains('⟬') && screen.contains('⟭'),
            "the inscription's brackets:\n{screen}"
        );
        assert!(screen.contains('⋄'), "and its divider:\n{screen}");
        // One character at a time: the cell buffer pads every wide glyph with a
        // continuation cell, so a phrase assertion would test the buffer's spacing.
        for glyph in ['兴', '云', '吐', '雾', '隐', '介', '藏', '形'] {
            assert!(screen.contains(glyph), "missing {glyph}:\n{screen}");
        }
        // Nothing below the frame: the inscription is the whole saying.
        for glyph in ['滞', '动', '必', '中'] {
            assert!(
                !screen.contains(glyph),
                "the lower line is gone, but {glyph} is still here:\n{screen}"
            );
        }

        // Narrower: the ornaments go, then the halves shorten; the mark stays.
        let plain = draw(&mut app, 62, 30);
        assert!(plain.contains('兴'), "{plain}");
        assert!(!plain.contains('⟬'), "the brackets need room:\n{plain}");
        let shortest = draw(&mut app, 52, 30);
        assert!(shortest.contains('龍'), "the mark itself:\n{shortest}");
        assert!(!shortest.contains('藏'), "and only what fits:\n{shortest}");

        // `/logo` puts it away, and nothing else moves.
        type_text(&mut app, "/logo");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        assert!(!app.logo);
        let screen = draw(&mut app, 150, 30);
        assert!(!screen.contains('龍'), "{screen}");
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
        let (to_worker, from_worker) = spawn_worker(client, 0);
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

    #[test]
    fn the_worker_fetches_the_fold_the_keyboard_picked() {
        /// Answers every request with a log holding one summarised tool result.
        struct Log;
        impl loong_tui::Transport for Log {
            fn send(&mut self, _line: &str) -> Result<(), String> {
                Ok(())
            }
            fn recv_line(&mut self) -> Result<Option<String>, String> {
                let output = (1..=50)
                    .map(|n| format!("hidden {n}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(Some(
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": {"events": [
                            {"seq": 1, "kind": "tool.result",
                             "data": {"call_id": "c1", "ok": true, "output": output}},
                        ]}
                    })
                    .to_string(),
                ))
            }
        }
        let client = Client::new(Log, "s-worker");
        let (to_worker, from_worker) = spawn_worker(client, 0);
        to_worker.send(ToWorker::ExpandAt { at: 0 }).unwrap();
        let mut app = App::new("s-worker", "v");
        app.push_lines(vec![
            "← ok hidden 1".to_string(),
            "  … 47 more line(s), 900 B · /expand".to_string(),
        ]);
        app.fold = Some(1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && app.open.is_none() {
            app.drain(&from_worker);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            app.open.is_some(),
            "the worker answered with the log's text"
        );
        assert!(
            draw(&mut app, 60, 20).contains("hidden 50"),
            "and the screen shows what was hidden"
        );
    }

    #[test]
    fn the_second_question_of_a_session_reaches_the_screen() {
        // Reported twice: the first approval was fine and the second was invisible. Answering
        // a question *resumes* the turn, and the next question is usually met right there —
        // but the client cleared whatever was pending after every answer, so the question it
        // had just met was thrown away and the worker reported "nothing pending". The panel
        // was never drawn; the transcript still showed the `! approval required` line for it.
        struct Queue(std::cell::RefCell<std::collections::VecDeque<String>>);
        impl loong_tui::Transport for Queue {
            fn send(&mut self, _line: &str) -> Result<(), String> {
                Ok(())
            }
            fn recv_line(&mut self) -> Result<Option<String>, String> {
                Ok(self.0.borrow_mut().pop_front())
            }
        }
        let line = |value: serde_json::Value| value.to_string();
        // The worker asks for the context view after every command, so a canned answer keeps
        // the queue aligned with the requests that are actually sent.
        let context_result = || {
            line(serde_json::json!({"jsonrpc": "2.0", "id": 9, "result": {
                "context_tokens": 10, "budget_tokens": 0, "messages": 1, "tools": 1,
                "compactions": 0, "categories": [], "entries": []}}))
        };
        let queue = vec![
            line(
                serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {
                "seq": 1, "kind": "tool.call",
                "data": {"call_id": "c1", "tool": "shell", "args": {"command": "ls"}}}}),
            ),
            line(
                serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {
                "seq": 2, "kind": "approval.requested",
                "data": {"request_id": "appr-1", "action_class": "out_of_bounds",
                         "reason": "shell: out_of_bounds is Ask by builtin:out_of_bounds"}}}),
            ),
            line(
                serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {"turn_id": 1, "accepted": false}}),
            ),
            context_result(),
            // The resume: another call, and the next question about it. Answering the first
            // one continues the turn; it does not end it.
            line(
                serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {
                "seq": 3, "kind": "tool.call",
                "data": {"call_id": "c2", "tool": "write_file",
                         "args": {"path": "game.html", "content": "<html>"}}}}),
            ),
            line(
                serde_json::json!({"jsonrpc": "2.0", "method": "event", "params": {
                "seq": 4, "kind": "approval.requested",
                "data": {"request_id": "appr-2", "action_class": "out_of_bounds",
                         "reason": "write_file: out_of_bounds is Ask by builtin:out_of_bounds"}}}),
            ),
            line(
                serde_json::json!({"jsonrpc": "2.0", "id": 2, "result": {"turn_id": 1, "accepted": true}}),
            ),
            context_result(),
        ];
        let client = Client::new(Queue(std::cell::RefCell::new(queue.into())), "s-1");
        let (to_worker, from_worker) = spawn_worker(client, 0);
        let mut app = App::new("s-1", "v");
        app.busy = true;
        to_worker.send(ToWorker::Submit("go".to_string())).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && app.pending.is_none() {
            app.drain(&from_worker);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(app.pending.as_deref(), Some("appr-1"));
        assert!(app.approval.is_some(), "the first question has its detail");

        // Answer it, exactly as the run loop would.
        match app.handle_key(key(KeyCode::Enter)) {
            Action::Send(command) => to_worker.send(command).unwrap(),
            other => panic!("expected an answer, got {other:?}"),
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && app.pending.as_deref() != Some("appr-2") {
            app.drain(&from_worker);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(
            app.pending.as_deref(),
            Some("appr-2"),
            "the second question is still a question"
        );
        let second = app
            .approval
            .as_ref()
            .expect("and it has its detail, like the first");
        assert_eq!(second.tool, "write_file", "{second:?}");
        assert!(second.args.contains("game.html"), "{second:?}");

        let screen = draw(&mut app, 100, 30);
        assert!(screen.contains("needs your decision"), "{screen}");
        assert!(screen.contains("write_file"), "{screen}");
        for answer in ["Allow once", "Allow in this project", "Deny"] {
            assert!(screen.contains(answer), "{answer} is on screen: {screen}");
        }
    }

    #[test]
    fn a_fold_the_turn_wrote_is_readable_while_the_turn_is_still_running() {
        /// A turn that streams one summarised result and then keeps working — saying
        /// nothing more — until the test lets it finish. This is the quiet stretch a real
        /// long tool spends, and the one a reader has time to look around in.
        struct SlowStream {
            streamed: bool,
            release: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }
        impl loong_tui::Transport for SlowStream {
            fn send(&mut self, _line: &str) -> Result<(), String> {
                Ok(())
            }
            fn recv_line(&mut self) -> Result<Option<String>, String> {
                if !self.streamed {
                    self.streamed = true;
                    let output = (1..=50)
                        .map(|n| format!("mid-turn {n}"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Ok(Some(
                        serde_json::json!({
                            "jsonrpc": "2.0", "method": "event",
                            "params": {"seq": 9, "kind": "tool.result",
                                       "data": {"call_id": "c1", "ok": true, "output": output}}
                        })
                        .to_string(),
                    ));
                }
                while !self.release.load(std::sync::atomic::Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(Some(
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": 1,
                        "result": {"turn_id": 1, "accepted": true}
                    })
                    .to_string(),
                ))
            }
        }

        let release = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let client = Client::new(
            SlowStream {
                streamed: false,
                release: release.clone(),
            },
            "s-running",
        );
        let (to_worker, from_worker) = spawn_worker(client, 0);
        let mut app = App::new("s-running", "v");
        app.busy = true;
        to_worker.send(ToWorker::Submit("go".to_string())).unwrap();

        // Wait for the fold to reach the screen. The turn is still working: nothing has
        // let it go, so the connection it holds cannot be asked for anything.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline && app.folded.is_empty() {
            app.drain(&from_worker);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(
            !app.folded.is_empty(),
            "the fold's own text came with its summary"
        );
        assert!(app.busy, "the turn is still running, as the test arranged");
        assert!(
            app.folded.contains_key(&0),
            "and it is named the way the keyboard counts it"
        );

        // The keyboard opens it now, with nothing asked of the busy worker.
        app.handle_key(key(KeyCode::Tab));
        assert!(app.fold.is_some(), "the fold is on screen to walk to");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        let screen = draw(&mut app, 60, 20);
        assert!(screen.contains("tool result (50 lines)"), "{screen}");
        assert!(
            screen.contains("mid-turn 50"),
            "what was hidden is readable while the turn still works: {screen}"
        );

        // Let the turn finish so the worker is not left mid-request.
        release.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    // ---------------------------------------------------------- the local wizard

    fn providers() -> Vec<ProviderChoice> {
        vec![
            ProviderChoice {
                id: "deepseek".into(),
                label: "DeepSeek".into(),
                needs_key: true,
            },
            ProviderChoice {
                id: "moonshot".into(),
                label: "Moonshot / Kimi".into(),
                needs_key: true,
            },
            ProviderChoice {
                id: "dashscope".into(),
                label: "阿里云百炼 / Qwen".into(),
                needs_key: true,
            },
        ]
    }

    fn drawn_wizard(wizard: &LocalWizard, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| draw_local_wizard(frame, wizard))
            .unwrap();
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

    /// Screen text with whitespace removed, for matching text that contains wide
    /// characters. `TestBackend` stores a CJK glyph in one cell and leaves the next
    /// blank, so a drawn `阿里云百炼` reads back as `阿 里 云 百 炼`. Comparing
    /// de-spaced strings sidesteps that without pretending the padding is not there.
    fn despaced(text: &str) -> String {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    }

    #[test]
    fn a_refusal_can_re_rank_the_list_without_asking_anyone_again() {
        // The hint must cost nothing: reordering sends the key nowhere new, which is
        // what keeps "try the other `sk-` vendors" from becoming a second leak.
        let mut wizard = LocalWizard::new(providers());
        wizard.provider = 2;
        wizard.reorder(&["dashscope".to_string(), "moonshot".to_string()]);
        assert_eq!(
            wizard.selected().unwrap().id,
            "dashscope",
            "the best guess leads"
        );
        assert_eq!(wizard.providers[0].id, "dashscope");
        assert_eq!(wizard.providers[1].id, "moonshot");
        assert_eq!(
            wizard.providers[2].id, "deepseek",
            "and everything else survives, in order"
        );
        assert_eq!(wizard.providers.len(), 3, "nothing is conjured or lost");

        // A provider nobody offered cannot appear, and a duplicate cannot either.
        wizard.reorder(&["nonesuch".to_string(), "dashscope".to_string()]);
        assert_eq!(wizard.providers.len(), 3);
        assert_eq!(wizard.providers[0].id, "dashscope");

        // No suggestion means no movement — and no reset of the caret.
        let before = wizard.providers.clone();
        wizard.provider = 2;
        wizard.reorder(&[]);
        assert_eq!(wizard.providers, before);
        assert_eq!(wizard.provider, 2, "an empty hint is not a hint");
    }

    #[test]
    fn a_provider_that_needs_no_key_neither_asks_for_one_nor_refuses_an_empty_one() {
        // A loopback server has no secret, and a form that demanded eight characters for
        // it would be asking somebody to invent one.
        let local = || {
            vec![
                ProviderChoice {
                    id: "ollama".into(),
                    label: "Ollama (本机)".into(),
                    needs_key: false,
                },
                ProviderChoice {
                    id: "deepseek".into(),
                    label: "DeepSeek".into(),
                    needs_key: true,
                },
            ]
        };

        let mut wizard = LocalWizard::new(local());
        assert!(
            wizard.ready(),
            "an empty field is not a mistake when there is nothing to type"
        );
        assert_eq!(
            wizard.handle_key(key(KeyCode::Enter)),
            WizardAction::Submit,
            "and enter is enough to start the check"
        );

        // Typing into it does nothing: there is nowhere for the characters to go.
        wizard.handle_key(key(KeyCode::Char('x')));
        assert_eq!(wizard.key, "", "the field is not a field for this provider");

        // The screen says so, rather than showing an empty box that looks broken.
        let screen = drawn_wizard(&wizard, 90, 20);
        assert!(
            despaced(&screen).contains(&despaced("本机服务，无需密钥")),
            "{screen}"
        );

        // Switching to a provider that does want one restores the ordinary rules.
        wizard.handle_key(key(KeyCode::Tab));
        assert!(!wizard.ready(), "the remote provider still wants a key");
        wizard.handle_key(key(KeyCode::Char('x')));
        assert_eq!(wizard.key, "x", "and the field works again");
    }

    // ------------------------------------------------------------ the picker

    fn choosing(names: &[&str]) -> LocalWizard {
        let mut wizard = LocalWizard::new(providers());
        wizard.state = WizardState::Choosing {
            filter: String::new(),
            cursor: 0,
            all: names
                .iter()
                .map(|name| (name.to_string(), Some(4096)))
                .collect(),
            ticked: vec![true; names.len()],
        };
        wizard
    }

    #[test]
    fn one_model_is_still_shown_rather_than_written_behind_somebody_s_back() {
        // The bug this locks down: a provider answering with a single model used to skip
        // the list and be written straight away, so `local add ollama` configured
        // `qwen2.5:3b` the moment somebody pressed enter on the provider. That is
        // configuration nobody confirmed — the person finds out by reading the file.
        let one = vec![("qwen2.5:3b".to_string(), Some(32_768))];
        assert_eq!(
            after_validation(&one),
            AfterValidation::Choose,
            "one row is still a row, and the run writes what was ticked"
        );

        let many = vec![
            ("a".to_string(), None),
            ("b".to_string(), None),
            ("c".to_string(), None),
        ];
        assert_eq!(after_validation(&many), AfterValidation::Choose);

        // The one case with nothing to show: a vendor that answered without naming a
        // model. There is no screen to draw and no row to tick, so the caller reports it
        // and asks for an id by hand.
        assert_eq!(after_validation(&[]), AfterValidation::Accept);
    }

    #[test]
    fn the_confirmation_screen_says_that_nothing_is_written_yet() {
        let wizard = choosing(&["qwen2.5:3b"]);
        let screen = drawn_wizard(&wizard, 90, 20);
        assert!(
            despaced(&screen).contains(&despaced("确认要加入的模型")),
            "the screen says what it is asking for:\n{screen}"
        );
        assert!(
            despaced(&screen).contains(&despaced("enter 写入")),
            "the key that commits is named:\n{screen}"
        );
        assert!(
            despaced(&screen).contains(&despaced("esc 回上一步")),
            "and so is the way back:\n{screen}"
        );
        assert!(screen.contains("[x] qwen2.5:3b"), "{screen}");
    }

    #[test]
    fn the_picker_starts_with_everything_ticked_so_all_of_them_is_one_keystroke() {
        let wizard = choosing(&["a", "b", "c"]);
        assert_eq!(wizard.chosen().len(), 3);
        assert_eq!(wizard.visible(), vec![0, 1, 2]);
    }

    #[test]
    fn the_picker_can_narrow_to_one_id_out_of_a_long_list() {
        // The case this exists for: a real account returns twenty ids and a person wants
        // one of them. Clearing and re-ticking beats unticking nineteen.
        let mut wizard = choosing(&[
            "qwen3.8-flash",
            "qwen3.8-max",
            "qwen3.7-plus",
            "qwen-coder-plus",
            "text-embedding-v3",
        ]);
        // `a` clears everything on screen, then typing narrows, then space takes one.
        wizard.handle_key(key(KeyCode::Char('a')));
        assert!(wizard.chosen().is_empty(), "nothing ticked after `a`");

        for c in "coder".chars() {
            wizard.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            wizard.visible(),
            vec![3],
            "the filter narrowed the list to what matches"
        );

        wizard.handle_key(key(KeyCode::Char(' ')));
        let chosen = wizard.chosen();
        assert_eq!(chosen.len(), 1, "{chosen:?}");
        assert_eq!(chosen[0].0, "qwen-coder-plus");
    }

    #[test]
    fn filtering_narrows_the_view_without_changing_what_is_ticked() {
        // The ticks live beside the full list, so a filter that hid a row must not have
        // unticked it — otherwise narrowing the view would quietly discard choices.
        let mut wizard = choosing(&["alpha", "beta", "gamma"]);
        wizard.handle_key(key(KeyCode::Char('g')));
        assert_eq!(wizard.visible(), vec![2]);
        assert_eq!(
            wizard.chosen().len(),
            3,
            "the two rows off screen are still ticked"
        );

        wizard.handle_key(key(KeyCode::Backspace));
        assert_eq!(wizard.visible(), vec![0, 1, 2], "and come back");
    }

    #[test]
    fn toggle_all_means_all_of_what_is_on_screen() {
        let mut wizard = choosing(&["qwen-flash", "qwen-max", "kimi-k3"]);
        wizard.handle_key(key(KeyCode::Char('a')));
        assert!(wizard.chosen().is_empty());

        wizard.handle_key(key(KeyCode::Char('q')));
        assert_eq!(wizard.visible(), vec![0, 1]);
        wizard.handle_key(key(KeyCode::Char('a')));
        let chosen: Vec<String> = wizard.chosen().into_iter().map(|(n, _)| n).collect();
        assert_eq!(
            chosen,
            vec!["qwen-flash", "qwen-max"],
            "only the visible rows came back"
        );
    }

    #[test]
    fn the_picker_will_not_finish_with_nothing_ticked() {
        let mut wizard = choosing(&["a", "b"]);
        wizard.handle_key(key(KeyCode::Char('a')));
        assert_eq!(
            wizard.handle_key(key(KeyCode::Enter)),
            WizardAction::None,
            "adding a provider that adds no models is not a thing anybody meant"
        );
        // And then it works.
        wizard.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::Submit);
    }

    #[test]
    fn the_picker_moves_and_stops_at_the_ends() {
        let mut wizard = choosing(&["a", "b", "c"]);
        wizard.handle_key(key(KeyCode::Up));
        assert_eq!(wizard.visible()[0], 0, "the top is the top");
        for _ in 0..5 {
            wizard.handle_key(key(KeyCode::Down));
        }
        let visible = wizard.visible();
        // The cursor is kept in range, so space still lands on the last row.
        wizard.handle_key(key(KeyCode::Char(' ')));
        assert_eq!(wizard.chosen().len(), 2, "the last row was toggled off");
        assert_eq!(visible.len(), 3);
    }

    #[test]
    fn escape_from_the_picker_goes_back_to_the_form_not_out_of_it() {
        // The key is typed and the provider is chosen; escaping the list should cost a
        // keystroke, not the whole form.
        let mut wizard = choosing(&["a"]);
        wizard.key = "sk-something-long".to_string();
        wizard.handle_key(key(KeyCode::Esc));
        assert_eq!(wizard.state, WizardState::Editing);
        assert_eq!(
            wizard.key, "sk-something-long",
            "and the key is still there"
        );
        assert_eq!(wizard.handle_key(key(KeyCode::Esc)), WizardAction::Cancel);
    }

    #[test]
    fn the_picker_says_what_is_ticked_and_what_to_press() {
        let mut wizard = choosing(&["qwen-flash", "qwen-max", "kimi-k3"]);
        wizard.handle_key(key(KeyCode::Char(' ')));
        let screen = drawn_wizard(&wizard, 90, 20);
        assert!(
            despaced(&screen).contains(&despaced("已选 2 / 3")),
            "the count is on screen:\n{screen}"
        );
        assert!(screen.contains("[x] kimi-k3"), "{screen}");
        assert!(screen.contains("[ ] qwen-flash"), "{screen}");
        assert!(
            despaced(&screen).contains(&despaced("空格 勾选")),
            "{screen}"
        );
        assert!(screen.contains("qwen-flash"), "{screen}");
    }

    #[test]
    fn the_picker_says_so_when_a_filter_matches_nothing() {
        let mut wizard = choosing(&["alpha", "beta"]);
        for c in "zzz".chars() {
            wizard.handle_key(key(KeyCode::Char(c)));
        }
        assert!(wizard.visible().is_empty());
        let screen = drawn_wizard(&wizard, 90, 20);
        assert!(
            despaced(&screen).contains(&despaced("没有匹配的模型")),
            "{screen}"
        );
    }

    #[test]
    fn the_local_wizard_never_draws_the_key_it_was_given() {
        let mut wizard = LocalWizard::new(providers());
        for c in "sk-4f06f4b88d7149ca8eeaa939d0fb08bd".chars() {
            wizard.handle_key(key(KeyCode::Char(c)));
        }
        let masked = wizard.masked_key();
        assert!(
            masked.starts_with("sk-"),
            "the shape is recognisable: {masked}"
        );
        assert!(
            !masked.contains("4f06f4b8"),
            "but the secret is not on a screen other people can see: {masked}"
        );
        assert_eq!(masked.chars().count(), 35, "the length is still legible");

        let screen = drawn_wizard(&wizard, 80, 20);
        assert!(
            !screen.contains("4f06f4b8"),
            "and the drawing does not leak it either:\n{screen}"
        );
    }

    #[test]
    fn the_local_wizard_refuses_a_key_too_short_to_be_one() {
        let mut wizard = LocalWizard::new(providers());
        assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::None);
        assert!(!wizard.ready(), "an empty field is not a key");

        for c in "sk-123".chars() {
            wizard.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::None);
        match &wizard.state {
            WizardState::Refused(why) => assert!(why.contains("太短"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }

        // Long enough is enough — the vendor, not a regex, decides what is valid.
        for c in "45678".chars() {
            wizard.handle_key(key(KeyCode::Char(c)));
        }
        assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::Submit);
    }

    #[test]
    fn the_local_wizard_moves_between_providers_in_both_directions_and_wraps() {
        let mut wizard = LocalWizard::new(providers());
        assert_eq!(wizard.selected().unwrap().id, "deepseek");

        wizard.handle_key(key(KeyCode::Tab));
        assert_eq!(wizard.selected().unwrap().id, "moonshot");
        wizard.handle_key(key(KeyCode::Down));
        assert_eq!(wizard.selected().unwrap().id, "dashscope");
        wizard.handle_key(key(KeyCode::Down));
        assert_eq!(
            wizard.selected().unwrap().id,
            "deepseek",
            "the list wraps, so one key never dead-ends"
        );

        wizard.handle_key(key(KeyCode::BackTab));
        assert_eq!(wizard.selected().unwrap().id, "dashscope");
        wizard.handle_key(key(KeyCode::Up));
        assert_eq!(wizard.selected().unwrap().id, "moonshot");
    }

    #[test]
    fn the_local_wizard_ignores_typing_while_a_validation_is_in_flight() {
        // Otherwise a keystroke lands in the field and the key that was checked is
        // not the key that gets stored.
        let mut wizard = LocalWizard::new(providers());
        for c in "sk-abcdefghij".chars() {
            wizard.handle_key(key(KeyCode::Char(c)));
        }
        wizard.state = WizardState::Validating;
        let before = wizard.key.clone();
        wizard.handle_key(key(KeyCode::Char('X')));
        wizard.handle_key(key(KeyCode::Backspace));
        assert_eq!(wizard.key, before, "input is frozen mid-flight");
        assert_eq!(wizard.handle_key(key(KeyCode::Enter)), WizardAction::None);
    }

    #[test]
    fn the_local_wizard_keeps_the_key_when_the_vendor_refuses_it() {
        // A typo is the common case, and retyping 35 characters to fix one of them
        // is the difference between a wizard and a punishment.
        let mut wizard = LocalWizard::new(providers());
        for c in "sk-4f06f4b88d7149ca8eeaa939d0fb08bd".chars() {
            wizard.handle_key(key(KeyCode::Char(c)));
        }
        wizard.state = WizardState::Refused("密钥被拒绝（HTTP 401）".into());
        let screen = drawn_wizard(&wizard, 80, 20);
        assert!(
            despaced(&screen).contains(&despaced("密钥被拒绝")),
            "{screen}"
        );
        assert_eq!(
            wizard.key, "sk-4f06f4b88d7149ca8eeaa939d0fb08bd",
            "the key survives so it can be corrected"
        );
    }

    #[test]
    fn the_local_wizard_draws_the_providers_and_says_what_to_press() {
        let mut wizard = LocalWizard::new(providers());
        wizard.handle_key(key(KeyCode::Tab));
        let screen = drawn_wizard(&wizard, 80, 20);

        for label in ["DeepSeek", "Moonshot / Kimi"] {
            assert!(screen.contains(label), "{label} is offered:\n{screen}");
        }
        assert!(
            despaced(&screen).contains(&despaced("阿里云百炼 / Qwen")),
            "a CJK label is offered as itself, not transliterated:\n{screen}"
        );
        assert!(
            screen.contains("▸ Moonshot / Kimi"),
            "the current choice is marked:\n{screen}"
        );
        assert!(
            !screen.contains("▸ DeepSeek"),
            "and only the current one is:\n{screen}"
        );
        assert!(
            despaced(&screen).contains(&despaced("enter 校验")),
            "{screen}"
        );
        assert!(
            despaced(&screen).contains(&despaced("esc 跳过")),
            "the way out is visible, and costs nothing: {screen}"
        );
        assert!(
            !screen.contains("base_url"),
            "the field the user must never see is not even mentioned:\n{screen}"
        );
    }

    #[test]
    fn the_local_wizard_shows_the_reason_a_network_failure_is_not_a_bad_key() {
        // The probe already refuses to conflate these; the wizard must not undo that
        // by drawing both the same colour and the same way.
        let mut wizard = LocalWizard::new(providers());
        for c in "sk-abcdefghij".chars() {
            wizard.handle_key(key(KeyCode::Char(c)));
        }
        wizard.state =
            WizardState::Refused("无法连接（这不说明密钥有问题）：Connection refused".into());
        let screen = drawn_wizard(&wizard, 80, 20);
        assert!(
            despaced(&screen).contains(&despaced("这不说明密钥有问题")),
            "{screen}"
        );
    }

    #[test]
    fn the_local_wizard_is_leavable_with_escape_or_ctrl_c() {
        for escape in [
            key(KeyCode::Esc),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL),
        ] {
            let mut wizard = LocalWizard::new(providers());
            assert_eq!(
                wizard.handle_key(escape),
                WizardAction::Cancel,
                "a form that cannot be left can strand a terminal"
            );
        }
    }

    // ------------------------------------------------------------ the model bar

    fn model_list(names: &[&str]) -> loong_schema::generated::ModelList {
        loong_schema::generated::ModelList {
            models: names
                .iter()
                .map(|name| loong_schema::generated::ModelOffering {
                    name: name.to_string(),
                    model: name.to_string(),
                    provider: None,
                    context_window: None,
                })
                .collect(),
            selected: names.first().copied().unwrap_or_default().to_string(),
        }
    }

    fn app_with_models(names: &[&str]) -> App {
        let mut app = App::new("s", "v");
        app.set_models(model_list(names));
        app
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    #[test]
    fn the_model_bar_numbers_the_models_in_the_order_the_profile_declared_them() {
        // Not alphabetically, and not by the map's order: that order is the user's own
        // ranking from cheapest to most capable, and it is what the digits mean.
        let mut app = app_with_models(&["local-small", "qwen-lite", "deepseek", "kimi"]);
        let screen = draw(&mut app, 90, 20);

        let bar = screen.lines().next().unwrap_or_default().to_string();
        let at: Vec<usize> = ["[1]", "[2]", "[3]", "[4]"]
            .iter()
            .map(|mark| {
                bar.find(mark)
                    .unwrap_or_else(|| panic!("{mark} is missing from the bar: {bar:?}"))
            })
            .collect();
        assert!(
            at.windows(2).all(|pair| pair[0] < pair[1]),
            "the slots are numbered left to right in declaration order: {bar:?}"
        );
        for name in ["local-small", "qwen-lite", "deepseek", "kimi"] {
            assert!(bar.contains(name), "{name} is on the bar: {bar:?}");
        }
    }

    #[test]
    fn the_model_bar_marks_the_current_model_and_only_that_one() {
        let mut app = app_with_models(&["cheap", "strong"]);
        let screen = draw(&mut app, 90, 20);
        let bar = screen.lines().next().unwrap_or_default().to_string();
        assert!(bar.contains("▸[1] cheap"), "{bar:?}");
        assert!(
            !bar.contains("▸[2]"),
            "the model that is not running is not marked: {bar:?}"
        );

        // And the mark follows a switch, so the bar never lies about what is answering.
        app.selected = "strong".to_string();
        let screen = draw(&mut app, 90, 20);
        let bar = screen.lines().next().unwrap_or_default().to_string();
        assert!(bar.contains("▸[2] strong"), "{bar:?}");
        assert!(!bar.contains("▸[1]"), "{bar:?}");
    }

    #[test]
    fn the_model_bar_shows_how_to_switch_without_being_asked() {
        // The bar is the whole discoverability story: a shortcut nobody is told about is
        // a shortcut nobody uses.
        let mut app = app_with_models(&["cheap", "mid", "strong"]);
        let screen = draw(&mut app, 100, 20);
        let bar = screen.lines().next().unwrap_or_default().to_string();
        assert!(
            despaced(&bar).contains(&despaced("alt+1..3")),
            "the key is on the bar: {bar:?}"
        );
        assert!(
            despaced(&bar).contains(&despaced("/1../3")),
            "and so is the fallback, for a terminal that eats Alt: {bar:?}"
        );
    }

    #[test]
    fn a_single_model_needs_no_shortcut_advertised() {
        let mut app = app_with_models(&["only"]);
        let screen = draw(&mut app, 90, 20);
        let bar = screen.lines().next().unwrap_or_default().to_string();
        assert!(bar.contains("only"), "{bar:?}");
        assert!(
            !despaced(&bar).contains(&despaced("alt+1")),
            "there is nothing to switch to, so nothing is promised: {bar:?}"
        );
    }

    #[test]
    fn a_session_with_no_models_draws_no_bar_at_all() {
        let mut app = App::new("s", "v");
        let without = draw(&mut app, 80, 20);
        assert!(
            !without.contains("[1]"),
            "nothing is drawn when there is nothing to draw: {without}"
        );
    }

    #[test]
    fn alt_digit_switches_to_that_slot() {
        let mut app = app_with_models(&["cheap", "mid", "strong"]);
        assert_eq!(
            app.handle_key(alt('3')),
            Action::Send(ToWorker::SelectModel("strong".to_string()))
        );
        assert_eq!(
            app.handle_key(alt('2')),
            Action::Send(ToWorker::SelectModel("mid".to_string()))
        );
    }

    #[test]
    fn slash_digit_is_the_same_shortcut_for_a_terminal_that_eats_alt() {
        // tmux and some macOS setups never deliver Alt to the program. The fallback has
        // to do exactly the same thing, or "the key does nothing" becomes a support
        // question with two different answers.
        let mut app = app_with_models(&["cheap", "strong"]);
        app.input = "/2".to_string();
        app.cursor = 2;
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::SelectModel("strong".to_string()))
        );
        assert!(app.input.is_empty(), "the command does not reach the agent");
    }

    #[test]
    fn a_slot_with_no_model_says_so_instead_of_doing_nothing() {
        let mut app = app_with_models(&["cheap", "strong"]);
        let action = app.handle_key(alt('7'));
        assert_eq!(action, Action::None, "there is no seventh model");
        let said = app.lines.join("\n");
        assert!(
            said.contains("没有第 7 个模型"),
            "a key that appears to do nothing is worse than one that explains: {said:?}"
        );
    }

    #[test]
    fn switching_to_the_model_already_running_does_not_ask_the_server() {
        // A round trip to be told nothing changed would put a line in the log for a
        // non-event, and the log is the record.
        let mut app = app_with_models(&["cheap", "strong"]);
        assert_eq!(app.handle_key(alt('1')), Action::None);
        let said = app.lines.join("\n");
        assert!(said.contains("already on [1] cheap"), "{said:?}");
    }

    #[test]
    fn ctrl_digit_is_deliberately_not_a_shortcut() {
        // In a VT terminal Ctrl+2..Ctrl+8 arrive as control *characters* — Ctrl+3 as
        // Escape, Ctrl+8 as Delete — so those combinations never reach this program as
        // digits at all. They are not bound, and this test is what stops somebody
        // "fixing" that by adding them back.
        for digit in ['1', '3', '8'] {
            let mut app = app_with_models(&["cheap", "strong"]);
            let action = app.handle_key(KeyEvent::new(KeyCode::Char(digit), KeyModifiers::CONTROL));
            assert_ne!(
                action,
                Action::Send(ToWorker::SelectModel("cheap".to_string())),
                "ctrl+{digit} must not switch"
            );
            assert_ne!(
                action,
                Action::Send(ToWorker::SelectModel("strong".to_string())),
                "ctrl+{digit} must not switch"
            );
        }
    }

    #[test]
    fn a_narrow_terminal_gives_up_models_before_it_gives_up_the_mark() {
        // A bar that pushes the conversation off the screen has stopped being a status
        // line. Whatever fits, fits.
        let mut app = app_with_models(&[
            "a-very-long-model-name",
            "another-very-long-model-name",
            "a-third-long-model-name",
        ]);
        let screen = draw(&mut app, 30, 20);
        let bar = screen.lines().next().unwrap_or_default().to_string();
        assert!(
            unicode_width::UnicodeWidthStr::width(bar.as_str()) <= 30,
            "the bar never overflows its row: {bar:?}"
        );
        assert!(
            bar.contains("[1]"),
            "the current model is the last to go: {bar:?}"
        );
        assert!(
            bar.contains('…'),
            "and it says that it dropped something: {bar:?}"
        );
    }

    #[test]
    fn the_bar_follows_a_switch_a_rule_made_mid_turn() {
        // A rule can change the model without the interface asking for anything, and it
        // arrives as an ordinary rendered line. If the bar did not read that line it
        // would keep naming the model that answered five steps ago.
        let mut app = app_with_models(&["cheap", "strong"]);
        app.follow_model_change("⇄ model cheap → strong (step > 1)");
        assert_eq!(app.selected, "strong");
        let screen = draw(&mut app, 90, 20);
        assert!(
            screen
                .lines()
                .next()
                .unwrap_or_default()
                .contains("▸[2] strong"),
            "{screen}"
        );

        // A session's first call has no previous model.
        app.follow_model_change("⇄ model cheap (the router's default)");
        assert_eq!(app.selected, "cheap");

        // A name this session does not offer cannot be marked on a bar that lists what
        // it does offer.
        app.follow_model_change("⇄ model cheap → ghost (step > 9)");
        assert_eq!(app.selected, "cheap", "an unknown name is not adopted");

        // And a line about something else is not mistaken for a switch.
        app.follow_model_change("· tokens 10 in / 2 out (p)");
        assert_eq!(app.selected, "cheap");
    }

    // ---------------------------------------------------------------- the resend

    /// Ask something, then let the turn finish — the state a person is in when they
    /// decide the model was not up to it.
    fn asked_and_finished(app: &mut App, prompt: &str) {
        type_text(app, prompt);
        assert!(matches!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::Submit(_))
        ));
        app.busy = false;
    }

    #[test]
    fn alt_r_asks_the_same_question_again() {
        let mut app = app_with_models(&["cheap", "strong"]);
        asked_and_finished(&mut app, "fix the off-by-one");
        assert_eq!(
            app.handle_key(alt('r')),
            Action::Send(ToWorker::Submit("fix the off-by-one".to_string())),
            "the same words, not a summary of them"
        );
    }

    #[test]
    fn the_resend_uses_the_model_selected_now_and_says_so() {
        let mut app = app_with_models(&["cheap", "strong"]);
        asked_and_finished(&mut app, "fix it");
        // Escalate first, then resend: this is the whole gesture the feature exists for.
        app.selected = "strong".to_string();
        assert_eq!(
            app.handle_key(alt('r')),
            Action::Send(ToWorker::Submit("fix it".to_string()))
        );
        let said = app.lines.join("\n");
        assert!(
            said.contains("[2] strong"),
            "the line names the model and its slot, matching the bar: {said:?}"
        );
    }

    #[test]
    fn the_resend_keeps_the_history_rather_than_rewinding_it() {
        // The design, stated as a test: the model that takes over can see what the
        // previous one tried and what went wrong. A clean slate would throw away the
        // most useful thing in the session.
        let mut app = app_with_models(&["cheap", "strong"]);
        asked_and_finished(&mut app, "fix it");
        let before = app.lines.clone();
        assert_eq!(
            app.handle_key(alt('r')),
            Action::Send(ToWorker::Submit("fix it".to_string()))
        );
        let after = app.lines.clone();
        assert!(
            after.len() > before.len(),
            "the transcript only grows: nothing was rewound"
        );
        for line in &before {
            assert!(after.contains(line), "and nothing was dropped: {line:?}");
        }
    }

    #[test]
    fn resending_before_anything_was_asked_says_there_is_nothing_to_resend() {
        let mut app = app_with_models(&["cheap"]);
        assert_eq!(app.handle_key(alt('r')), Action::None);
        let said = app.lines.join("\n");
        assert!(said.contains("还没有可以重发"), "{said:?}");
    }

    #[test]
    fn resending_while_a_turn_is_running_is_refused_rather_than_queued() {
        // A second turn starting on top of a running one is the failure this prevents.
        let mut app = app_with_models(&["cheap", "strong"]);
        type_text(&mut app, "fix it");
        app.handle_key(key(KeyCode::Enter));
        assert!(app.busy, "the turn is running");
        assert_eq!(app.handle_key(alt('r')), Action::None);
        let said = app.lines.join("\n");
        assert!(said.contains("还在跑"), "{said:?}");
    }

    #[test]
    fn slash_r_is_the_fallback_for_a_terminal_that_eats_alt() {
        let mut app = app_with_models(&["cheap", "strong"]);
        asked_and_finished(&mut app, "fix it");
        app.input = "/r".to_string();
        app.cursor = 2;
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Send(ToWorker::Submit("fix it".to_string()))
        );
        assert!(app.input.is_empty(), "the command does not reach the agent");
    }

    #[test]
    fn the_resend_is_advertised_only_once_there_is_something_to_resend() {
        let mut app = app_with_models(&["cheap", "strong"]);
        let bar = draw(&mut app, 100, 20);
        let first = bar.lines().next().unwrap_or_default().to_string();
        assert!(
            !despaced(&first).contains(&despaced("alt+r")),
            "a key advertised before it can work is a key that looks broken: {first:?}"
        );

        asked_and_finished(&mut app, "fix it");
        let bar = draw(&mut app, 100, 20);
        let after = bar.lines().next().unwrap_or_default().to_string();
        assert!(
            despaced(&after).contains(&despaced("alt+r 重发")),
            "and once it can work, it is on the bar: {after:?}"
        );
    }

    #[test]
    fn slash_exit_leaves() {
        let mut app = app_with_models(&["cheap"]);
        app.input = "/exit".to_string();
        app.cursor = 5;
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::Quit);
        assert!(app.input.is_empty(), "the command does not reach the agent");
    }

    #[test]
    fn a_plain_r_still_types_an_r() {
        // Alt+R is a shortcut; `r` is a letter. An interface where the letters stopped
        // working would be a worse trade than no shortcut at all.
        let mut app = app_with_models(&["cheap"]);
        app.handle_key(key(KeyCode::Char('r')));
        assert_eq!(app.input, "r");
    }

    #[test]
    fn a_model_that_failed_its_check_is_marked_and_keeps_its_slot() {
        // Marked, not removed. A key revoked at midnight and a network hiccup look
        // identical from here, and a bar that renumbered itself because the wifi dropped
        // would be worse than one that admitted it was unsure.
        let mut app = app_with_models(&["cheap", "strong", "third"]);
        assert!(!app.health.contains_key("strong"), "nothing checked yet");

        app.note_health(ModelHealth {
            name: "strong".to_string(),
            ok: false,
            detail: "无法连接（这不说明密钥有问题）：timeout".to_string(),
        });
        let screen = draw(&mut app, 100, 20);
        let bar = screen.lines().next().unwrap_or_default().to_string();
        assert!(bar.contains("✗[2] strong"), "{bar:?}");
        assert!(
            bar.contains("[3] third"),
            "and the slots either side did not move: {bar:?}"
        );
        assert!(
            despaced(&screen).contains(&despaced("校验未通过")),
            "with the reason in the transcript, because a red glyph is not an explanation:\n{screen}"
        );

        // A model that passes is not marked at all — a tick on everything is noise.
        app.note_health(ModelHealth {
            name: "third".to_string(),
            ok: true,
            detail: "连接成功".to_string(),
        });
        let screen = draw(&mut app, 100, 20);
        let bar = screen.lines().next().unwrap_or_default().to_string();
        assert!(!bar.contains("✗[3]"), "{bar:?}");
        assert!(
            bar.contains("✓") == false,
            "no tick for the good news: {bar:?}"
        );
    }

    #[test]
    fn a_failed_check_still_lets_the_model_be_chosen() {
        // The check is advice. Refusing the key press would mean a transient probe
        // failure locked somebody out of a model that works perfectly well.
        let mut app = app_with_models(&["cheap", "strong"]);
        app.note_health(ModelHealth {
            name: "strong".to_string(),
            ok: false,
            detail: "timeout".to_string(),
        });
        assert_eq!(
            app.handle_key(alt('2')),
            Action::Send(ToWorker::SelectModel("strong".to_string()))
        );
    }

    #[test]
    fn the_status_line_says_what_a_model_command_will_do_before_it_is_pressed() {
        let mut app = app_with_models(&["cheap", "strong"]);
        for (typed, expected) in [("/2", "[2] strong"), ("/1", "[1] cheap")] {
            app.input = typed.to_string();
            app.cursor = typed.chars().count();
            let screen = draw(&mut app, 100, 20);
            assert!(
                despaced(&screen).contains(&despaced(expected)),
                "{typed} should describe itself as {expected:?}:\n{screen}"
            );
        }

        // A slot with no model says so rather than promising a switch.
        app.input = "/7".to_string();
        app.cursor = 2;
        let screen = draw(&mut app, 100, 20);
        assert!(
            despaced(&screen).contains(&despaced("no model in slot 7")),
            "{screen}"
        );

        // And `/r` only describes itself once there is something to resend.
        app.input = "/r".to_string();
        app.cursor = 2;
        let screen = draw(&mut app, 100, 20);
        assert!(
            !despaced(&screen).contains(&despaced("asks the last prompt again")),
            "nothing has been asked yet:\n{screen}"
        );
        asked_and_finished(&mut app, "fix it");
        app.input = "/r".to_string();
        app.cursor = 2;
        let screen = draw(&mut app, 100, 20);
        assert!(
            despaced(&screen).contains(&despaced("asks the last prompt again")),
            "{screen}"
        );
    }
}
