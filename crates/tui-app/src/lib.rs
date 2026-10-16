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

pub mod mention;
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
    /// Close this session and start an empty one.
    NewSession,
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
    /// A new session is running, under this id. The old one is closed and kept.
    SessionStarted(String),
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
    /// How the last turn ended, kept until the next prompt starts.
    ///
    /// The transcript says so as well, in a line that scrolls away — and "did that
    /// finish?" is a question asked while looking at the prompt, not while scrolling.
    pub finished: Option<Finished>,
    /// Set when the person asked for the turn to stop, so its end can be labelled
    /// honestly instead of as a completion.
    stopping: bool,
    pub error: Option<String>,
    /// The answer so far, while it is being written.
    /// The `@` file picker, when the caret is inside a reference.
    picker: Option<mention::Picker>,
    /// The workspace's files, read once on first use. `None` means "not read yet",
    /// which is different from "read and empty" — a picker that read the tree on
    /// every keystroke would stall the interface on a large repository.
    mention_files: Option<mention::FileIndex>,
    /// Set when esc dismissed the picker. The picker is derived from the text, so
    /// without this the next keystroke would derive it straight back and escape
    /// would do nothing — which is what the test caught. Cleared as soon as the text
    /// or the caret changes, so the next `@` opens normally.
    mention_dismissed: bool,
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
            finished: None,
            stopping: false,
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
            picker: None,
            mention_files: None,
            mention_dismissed: false,
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
        // A new task makes the last tick stale, and a tick that outlives its turn is a
        // tick that lies about the work on screen.
        self.finished = None;
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
            "↻ 用 {} 重发上一条（保留原有上下文）",
            self.selected_model_label()
        )]);
        self.begin_submit(text)
    }

    /// What to call the current model on screen — **including** the brackets, so every
    /// caller says the same thing and none of them has to remember to add them.
    fn selected_model_label(&self) -> String {
        if self.selected.is_empty() {
            return "当前模型".to_string();
        }
        match self.models.iter().position(|m| m.name == self.selected) {
            Some(index) if index < 9 => format!("[{}] {}", index + 1, self.selected),
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

    /// The next or previous model, wrapping around the list.
    fn switch_relative(&mut self, delta: isize) -> Action {
        if self.models.is_empty() {
            self.push_lines(vec![
                "· 这个会话没有可选模型 — 配一个：loongagent local add".to_string(),
            ]);
            return Action::None;
        }
        let count = self.models.len();
        let current = self
            .models
            .iter()
            .position(|model| model.name == self.selected)
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(count as isize) as usize;
        self.switch_to_slot(next + 1)
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
            // `ctrl+n`/`ctrl+p` lead, because they are the ones that work everywhere. The
            // numbered forms follow for anybody whose terminal does deliver them.
            hint.push_str(&format!("  ctrl+n/p 切换 · alt+1..{slots} · /1../{slots}"));
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
                    // Reversed as well as bold. `▸` alone was too quiet to answer "did
                    // that switch?", which is the one question the bar exists to answer
                    // — and a whole inverted cell is hard to miss in a row of dim text.
                    MARK_TEXT.add_modifier(Modifier::REVERSED)
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
                lines.extend(memory_lines(explained));
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
            // A fresh session. `/clear` is the same thing and not a screen wipe: the
            // context is what people mean when they ask for a clean start, and clearing
            // only the view would leave the model still reading everything above.
            "/new" | "/clear" | "/reset" => {
                self.input.clear();
                self.cursor = 0;
                Some(Action::Send(ToWorker::NewSession))
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
        self.stopping = true;
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
        self.refresh_picker();
    }

    /// Keep the `@` picker in step with the text and the caret.
    ///
    /// `None` when the caret is not inside a reference, which is also how the picker
    /// closes — there is no separate open/closed flag to fall out of step with what
    /// is actually typed. Called from every path that moves the caret or edits the
    /// line, because a picker that lags one keystroke behind lists the wrong files.
    fn refresh_picker(&mut self) {
        // Any edit or caret move re-arms the picker; the dismissal only covers the
        // text it was given for.
        self.mention_dismissed = false;
        if self.pending.is_some() {
            // A decision is on screen; the prompt underneath is not being typed into.
            self.picker = None;
            return;
        }
        if self.mention_dismissed {
            self.picker = None;
            return;
        }
        if !self.input.contains(mention::TRIGGER) {
            // The common case: no reference anywhere in the line, so the index is not
            // even read. A picker that walked the tree on every keystroke would make
            // typing itself slow.
            self.picker = None;
            return;
        }
        if self.mention_files.is_none() {
            let root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            self.mention_files = Some(mention::FileIndex::read(root));
        }
        let files = self.mention_files.as_ref().expect("just read");
        self.picker =
            mention::Picker::refresh(&self.input, self.cursor, files, self.picker.as_ref());
    }

    /// How many rows the picker needs: its matches, or one line saying there are
    /// none — a picker that vanished when a query matched nothing would look like a
    /// crash rather than an empty answer.
    fn picker_rows(&self) -> u16 {
        match &self.picker {
            None => 0,
            Some(picker) => picker.matches.len().max(1) as u16,
        }
    }

    fn draw_picker(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        let Some(picker) = &self.picker else {
            return;
        };
        let title = if picker.query.is_empty() {
            "files — ↑/↓ choose · enter insert · esc cancel".to_string()
        } else {
            format!(
                "files matching {} — ↑/↓ choose · enter insert · esc cancel",
                picker.query
            )
        };
        let body = if picker.matches.is_empty() {
            vec![ratatui::text::Line::from(
                "  no file matches — keep typing, or esc",
            )]
        } else {
            picker
                .matches
                .iter()
                .enumerate()
                .map(|(index, file)| {
                    let marker = if index == picker.selected {
                        "▸ "
                    } else {
                        "  "
                    };
                    let line = format!("{marker}{file}");
                    if index == picker.selected {
                        ratatui::text::Line::from(line).style(MARK_ACCENT_STYLE)
                    } else {
                        ratatui::text::Line::from(line)
                    }
                })
                .collect()
        };
        let widget = ratatui::widgets::Paragraph::new(body)
            .block(Block::default().borders(Borders::ALL).title(title));
        frame.render_widget(widget, area);
    }

    /// Insert the highlighted path and close the picker.
    fn accept_picker(&mut self) -> Action {
        let Some(picker) = self.picker.as_ref() else {
            return Action::None;
        };
        let Some(path) = picker.selected_path().map(str::to_string) else {
            return Action::None;
        };
        let range = picker.range.clone();
        mention::apply(&mut self.input, range, &path);
        // The caret goes to the end of the line — `apply` adds a trailing space, and
        // leaving the caret inside the token would re-open the picker on it.
        self.cursor = self.input.chars().count();
        self.picker = None;
        Action::None
    }

    fn delete_before_cursor(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let from = self.byte_at(self.cursor - 1);
        let to = self.byte_at(self.cursor);
        self.input.replace_range(from..to, "");
        self.cursor -= 1;
        self.refresh_picker();
    }

    fn delete_at_cursor(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let from = self.byte_at(self.cursor);
        let to = self.byte_at(self.cursor + 1);
        self.input.replace_range(from..to, "");
        self.refresh_picker();
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
                        // Named with its slot, so the line and the bar say the same thing,
                        // and long enough to be read rather than skimmed — this is the
                        // only confirmation that a switch happened.
                        self.push_lines(vec![format!(
                            "⇄ 模型已切到 {}（此前 {}）— 下一次模型调用生效",
                            self.selected_model_label(),
                            before
                        )]);
                    }
                }
                Ok(FromWorker::SessionStarted(id)) => {
                    // A new session means the screen is showing a log that is no longer
                    // the one being written to. Everything tied to the old one goes: the
                    // transcript, the folds, a pending question, the tick.
                    self.lines.clear();
                    self.folded.clear();
                    self.open = None;
                    self.fold = None;
                    self.pending = None;
                    self.approval = None;
                    self.context = None;
                    self.error = None;
                    self.streaming.clear();
                    self.scroll = 0;
                    self.finished = None;
                    self.busy_since = None;
                    self.session = id.clone();
                    self.push_lines(vec![format!(
                        "· 新会话 {id} —— 上一条已存盘，用 `loongagent --session <id>` 可以回去"
                    )]);
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
                    let outcome = if self.stopping {
                        Outcome::Stopped
                    } else {
                        Outcome::Done
                    };
                    self.stopping = false;
                    let seconds = self
                        .busy_since
                        .take()
                        .map(|since| since.elapsed().as_secs_f64());
                    let finished = Finished { seconds, outcome };
                    self.push_lines(vec![finished.line()]);
                    self.finished = Some(finished);
                }
                Ok(FromWorker::Failed(message)) => {
                    let seconds = self
                        .busy_since
                        .take()
                        .map(|since| since.elapsed().as_secs_f64());
                    self.busy = false;
                    self.activity = None;
                    self.stopping = false;
                    self.finished = Some(Finished {
                        seconds,
                        outcome: Outcome::Failed,
                    });
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
        // One refresh per key, before the match: the picker is derived from the text
        // and the caret, so keeping it in step belongs in one place rather than in
        // every branch that happens to move either. Escape re-arms nothing because it
        // sets the dismissal flag *after* this runs, and every other key clears it by
        // running here — which is exactly the intended reading of "not now".
        self.refresh_picker();
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
        // `ctrl+n` / `ctrl+p`: the next or previous model, wrapping.
        //
        // These are here because `Alt+<digit>` is not delivered by every terminal — tmux
        // and some macOS setups swallow the modifier, and `screen` keeps `ctrl+a` for
        // itself — and a shortcut that silently does nothing is worse than no shortcut,
        // because it looks like the *program* is broken. A control character always
        // arrives, and `ctrl+n`/`ctrl+p` are "next"/"previous" everywhere else.
        if ctrl && self.pending.is_none() {
            match key.code {
                KeyCode::Char('n') => return self.switch_relative(1),
                KeyCode::Char('p') => return self.switch_relative(-1),
                _ => {}
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
            // The `@` picker owns these keys while it is open: while a list is on
            // screen, up/down mean "move in the list" and enter means "take this
            // file". It sits above the fold keys for exactly that reason — otherwise
            // tab would walk the transcript while a picker was asking to be finished.
            KeyCode::Esc if self.picker.is_some() => {
                self.picker = None;
                // Remembered, because the picker is derived from the text: clearing it
                // alone would let the next keystroke bring it straight back.
                self.mention_dismissed = true;
                Action::None
            }
            KeyCode::Up if self.picker.is_some() => {
                if let Some(picker) = self.picker.as_mut() {
                    picker.move_by(-1);
                }
                Action::None
            }
            KeyCode::Down if self.picker.is_some() => {
                if let Some(picker) = self.picker.as_mut() {
                    picker.move_by(1);
                }
                Action::None
            }
            KeyCode::Tab if self.picker.is_some() => {
                if let Some(picker) = self.picker.as_mut() {
                    picker.move_by(1);
                }
                Action::None
            }
            KeyCode::BackTab if self.picker.is_some() => {
                if let Some(picker) = self.picker.as_mut() {
                    picker.move_by(-1);
                }
                Action::None
            }
            KeyCode::Enter if self.picker.is_some() => self.accept_picker(),
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
                // The `@` picker sits directly above the input box it is filling in,
                // so the eye does not travel between the list and the token it will
                // replace. Nothing is taken from the transcript when it is closed.
                Constraint::Length(if self.picker_rows() == 0 {
                    0
                } else {
                    self.picker_rows() + 2
                }),
                Constraint::Length(input_rows + 2),
                Constraint::Length(1),
            ])
            .split(conversation);
        // Named, because five anonymous slices is how the notes strip once ended up
        // drawn over the mark.
        let (models, banner, notes, transcript, decision, picker, entry, keys) = (
            areas[0], areas[1], areas[2], areas[3], areas[4], areas[5], areas[6], areas[7],
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
        if self.picker_rows() > 0 {
            self.draw_picker(frame, picker);
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
            // The last turn's ending, held on screen until the next prompt starts.
            //
            // Placed after the command hint so a half-typed `/new` still completes, and
            // before the bare "prompt" label, which says nothing anybody needs.
            //
            // This is the answer to "did that finish?", and it is the one that gets asked
            // while the cursor is sitting in the box — a closing line in the transcript
            // answers it only for somebody who is already scrolling.
            None if self.finished.is_some() && !self.input.starts_with('/') => {
                let finished = self.finished.expect("just checked");
                (finished.line(), finished.style())
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
                        "/new" | "/clear" | "/reset" => {
                            "enter starts a new session — this one is kept, its context is not"
                                .to_string()
                        }
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
                            // `/help` stays in front: it is the answer to "where is the
                            // rest", and a hint that dropped it to make room for a
                            // newcomer would be trading one dead end for another.
                            _ => {
                                "command — /help · /new · /exit · /logo · /mouse · /copy · /expand"
                                    .to_string()
                            }
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
        let input = Paragraph::new(visible).block(
            Block::default()
                .borders(Borders::ALL)
                // The colour belongs to the *status title*, never to the paragraph.
                // `Paragraph::style(style)` painted the text the person was typing
                // too, so the prompt turned green after a successful turn (red under
                // YOLO, cyan while busy) — status read as a drawing leak, and the
                // one place that should be the person's own was the one being
                // recoloured.
                .title(Span::styled(title, style)),
        );
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
        // The memory's state belongs on the bar as well as in the panel: "this
        // session's memory is degraded" is worth seeing without opening a debug
        // view, and a silent fallback is the one thing the design forbids.
        if let Some(context) = &self.context {
            if !context.memory_mode.is_empty() {
                hints.push(Span::raw("  "));
                if context.memory_degraded.is_empty() {
                    hints.push(Span::styled(
                        format!("mem {}", context.memory_mode),
                        MARK_QUIET,
                    ));
                } else {
                    hints.push(Span::styled(
                        "mem ⚠ degraded",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
            }
        }
        frame.render_widget(Paragraph::new(Line::from(hints)), keys);
    }
}

/// The layered memory's own state, as the debug panel draws it (ADR-0017 §10, §11.1).
///
/// The honest part is what it does **not** do: it reads `memory_mode` rather than
/// looking for a `cognitive_map` entry in the ledger. A semantic session's first
/// turns have no map yet, and a panel that read "no map" as "engine off" would be
/// unable to tell a feature that is off from one that has simply not produced
/// anything — which is the dishonest display this design exists to avoid.
///
/// Degradation is always shown: a memory that quietly fell back to its digests is
/// a memory whose answers are thinner than they look.
fn memory_lines(explained: &loong_schema::generated::ContextExplain) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if explained.memory_mode.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("memory  ", MARK_QUIET),
            Span::styled(
                "off",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  no layered memory; compaction drops old exchanges",
                MARK_QUIET,
            ),
        ]));
        return lines;
    }

    let summarizer = if explained.memory_summarizer.is_empty() {
        "none (record mode)".to_string()
    } else {
        explained.memory_summarizer.clone()
    };
    lines.push(Line::from(vec![
        Span::styled("memory  ", MARK_QUIET),
        Span::styled(
            explained.memory_mode.clone(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  summarizer {summarizer}"), MARK_QUIET),
    ]));

    // Counts first, because "how much memory is there" is the question; tokens are
    // the cost of carrying it and ride along where they are not zero.
    let mut layers: Vec<String> = Vec::new();
    for layer in &explained.memory_layers {
        let label = match layer.level {
            1 => "L1",
            2 => "L2",
            3 => "L3",
            4 => "L4",
            _ => "L?",
        };
        if layer.tokens > 0 {
            layers.push(format!("{label} {} ({} tok)", layer.count, layer.tokens));
        } else {
            layers.push(format!("{label} {}", layer.count));
        }
    }
    let map_missing = explained
        .memory_layers
        .iter()
        .any(|layer| layer.level == 4 && layer.count == 0);
    let mut spans = vec![Span::styled("        ", MARK_QUIET)];
    spans.push(Span::raw(layers.join("  ")));
    if map_missing {
        // The distinction the whole field exists for: on, and nothing produced yet.
        spans.push(Span::styled("  · no L4 map yet", MARK_QUIET));
    }
    lines.push(Line::from(spans));

    let worker = &explained.memory_worker;
    lines.push(
        Line::from(format!(
            "        worker {} · {} calls · {} enriched · {} pending · {} deferred",
            worker.state, worker.model_calls, worker.enriched, worker.pending, worker.deferrals
        ))
        .style(MARK_QUIET),
    );

    // Degradation is never optional. The engine-level reason and the worker's last
    // error are both shown when they are different facts.
    if !explained.memory_degraded.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("        degraded: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                explained.memory_degraded.clone(),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }
    if !worker.last_error.is_empty() && !explained.memory_degraded.contains(&worker.last_error) {
        lines.push(Line::from(vec![
            Span::styled("        last error: ", Style::default().fg(Color::Yellow)),
            Span::styled(
                worker.last_error.clone(),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    } else if worker.failed > 0 && explained.memory_degraded.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("        {} enrichment failure(s)", worker.failed),
            Style::default().fg(Color::Yellow),
        )]));
    }
    lines
}

mod setup;
mod surface;
mod wizard;
mod worker;

#[cfg(test)]
pub(crate) use setup::draw_setup_form;
pub use setup::{full_screen_needs_a_terminal, setup_form, SetupAction, SetupForm};
pub use surface::run;
#[cfg(test)]
pub(crate) use wizard::draw_local_wizard;
pub use wizard::{
    after_validation, local_wizard, AfterValidation, Finished, KeyChoice, LocalWizard, ModelHealth,
    OfferedKey, Outcome, ProviderChoice, Validation, WizardAction, WizardState,
};
pub use worker::spawn_worker;

#[cfg(test)]
mod tests;
