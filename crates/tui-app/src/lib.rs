//! Purpose: The full-screen terminal surface: the state machine, layout and palette behind ratatui's screen.
//!
//! # Contract
//! - **Serves:** people running `loongagent tui`; `surface::run` drives it, `worker` feeds it, and the tests in this crate pin what it draws.
//! - **Design:** `docs/design/surfaces.md` §3 and §6; `docs/design/sessions-lineage.md` §1 and §5 for the replayed past and the resend.
//! - **Upstream:** `crates/cli/src/tui.rs` reaches it through `loong_tui_app::run`; the screen's own tests build an `App` directly.
//! - **Downstream:** `loong_tui` for rendered lines, folds and the client; `ratatui` for drawing; `mention`, `wizard` and `setup` for the picker and the first-run forms.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
//!
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

pub mod commands;
pub mod mention;
use loong_tui::{Client, Transport};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

/// Purpose: The intents the screen sends to the worker: a prompt, a decision answer, and the read-only refreshes.
/// Serves: `App::begin_submit`, `App::resend_last_prompt`, `App::switch_to_slot`, `App::answer`, `App::surface_command` and `App::toggle_fold`, plus `worker::spawn_worker`.
/// Design: `docs/design/surfaces.md` §3 and §6.
/// Up: `App::handle_key` and `App::surface_command` build them; `surface::run` forwards them to the worker.
/// Down: `worker::spawn_worker` matches every variant and calls the matching `loong_tui::Client` method.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: The worker's reports back to the screen: rendered lines, streamed deltas, state, folds and failures.
/// Serves: `App::drain`, the only reader, and `worker::spawn_worker` and the tests' `fed` helper, which write them.
/// Design: `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §1 for the replayed past.
/// Up: `worker::spawn_worker` sends every variant; the tests in this crate send them directly through a channel.
/// Down: `App::drain` folds them into `lines`, `pending`, `context`, `models`, `folded` and `finished`.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: What one key press asks the run loop to do, returned rather than done so a caller or a test can assert it.
/// Serves: `surface::run` (the executor) and every keyboard test in this crate.
/// Design: `docs/design/surfaces.md` §3 and §6.
/// Up: `App::handle_key` returns it, as do `App::begin_submit`, `App::resend_last_prompt`, `App::surface_command`, `App::answer` and `App::toggle_fold`.
/// Down: `surface::run` matches it and performs the terminal or channel work.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: Put the transcript on the clipboard with OSC 52 and also write it to a session-named temporary file.
/// Contract: called by `surface::run` for `/copy` and by `copy_puts_the_transcript_on_the_clipboard_and_in_a_file`; calls `base64` for the escape payload and writes the file the caller names on screen.
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

/// Purpose: Encode the OSC 52 clipboard payload, so `/copy` needs no dependency for forty lines.
/// Serves: `copy_everywhere` and the `base64_is_base64` test.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `copy_everywhere`.
/// Down: no call; it is a pure encoder.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: The `kind` field of a notification's params, for the places that need to branch on it.
/// Contract: called by `worker::spawn_worker` and the duplicate-user-line test.
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

/// Purpose: Force a decision detail onto one row and cut it to width, so it cannot push an answer out of the panel.
/// Contract: called by `draw_approval`; calls `clip` after flattening newlines and tabs.
/// A string forced onto one line and cut to fit: newlines become `⏎` so a shell command
/// cannot wrap the answers out of their own box.
fn one_line(text: &str, width: usize) -> String {
    let flattened = text.replace('\n', "⏎ ").replace('\t', "  ");
    clip(&flattened, width)
}

/// Purpose: Cut one string to display columns, counting a CJK character as two so a clipped detail is one row and visibly ends in an ellipsis.
/// Serves: `one_line` and `draw_approval`.
/// Design: `docs/design/surfaces.md` §3 (the decision menu).
/// Up: `one_line` and `draw_approval`.
/// Down: `unicode_width` for the column measurement.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: One pending approval as the screen needs it: the request id, action class, reason, tool and arguments.
/// Serves: `App::approval`, `App::info`, `draw_approval` and the `FromWorker::Approval` path.
/// Design: `docs/design/surfaces.md` §3 and §6; the surface shows the decision but does not make it.
/// Up: `worker::spawn_worker` builds it from `loong_tui::PendingDecision`; `App::drain` stores it.
/// Down: no call; it is data read by `draw_approval` and `info`.
/// Verify: `cargo test -p loong-tui-app`.
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
/// The name, for the frame's right side when the block letters do not fit.
const MARK_WORD: &str = "loongagent";
/// Columns of air between a frame's wall and what it holds, so the contents never touch
/// the rule. It is the same on every side, which is what makes the frame look drawn
/// rather than grown around its contents.
const MARK_AIR: usize = 3;

/// The subline, as the owner wrote it, widest first: runes and brackets from the same
/// alphabet as the frame, a `⋄` between the two halves. It hangs under the frame now,
/// where it reads as the inscription the character is stamped with.
const MARK_SUBLINE: [(usize, &str); 3] = [
    (28, "⟬ ᛭ 兴云吐雾 ⋄ 隐介藏形 ᛭ ⟭"),
    (19, "兴云吐雾 ⋄ 隐介藏形"),
    (8, "兴云吐雾"),
];
/// Violet, for something that wants an answer rather than something that went wrong.
const MARK_ACCENT_STYLE: Style = Style::new().fg(MARK_ACCENT);

/// Purpose: The classes the transcript paints differently, read from the line's own vocabulary.
/// Serves: `classify`, `style_for`, `styled` and `transcript_line`.
/// Design: `docs/design/surfaces.md` §3 ("The TUI's marks and colour").
/// Up: `classify` returns it; `style_for`, `styled` and `transcript_line` match it.
/// Down: no call; it is a tag carried to the palette.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: The palette for one line class: terminal colour names and the configured foreground, never fixed RGB.
/// Serves: `styled`, `transcript_line` and the colour tests.
/// Design: `docs/design/surfaces.md` §3 ("The TUI's marks and colour").
/// Up: `styled` and `transcript_line`, plus the tests that read drawn cell colours.
/// Down: no call; it maps a `LineKind` to a `Style`.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: Decide a rendered line's class from its prefix, so the palette follows the vocabulary `loong_tui` writes.
/// Serves: `styled`, `transcript_line` and the renderer-agreement test.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `styled` and `transcript_line`, plus the classification tests.
/// Down: `LineKind`.
/// Verify: `cargo test -p loong-tui-app`.
/// Which kind a rendered line is. The prefixes are the client renderer's, so this is a
/// classifier over a vocabulary this repository owns end to end.
pub fn classify(line: &str) -> LineKind {
    if line.starts_with("你：") || line.starts_with("you: ") {
        return LineKind::User;
    }
    if line.starts_with("→ ") {
        return LineKind::ToolCall;
    }
    if let Some(rest) = line.strip_prefix("← ") {
        return if rest.starts_with("成功") || rest.starts_with("ok ") || rest == "ok" {
            LineKind::ToolOk
        } else {
            LineKind::ToolFailed
        };
    }
    if line.starts_with("⧉ ") || line.starts_with("· mouse ") || line.starts_with("· 鼠标") {
        return LineKind::Notice;
    }
    if line.starts_with("⏹ ") {
        return LineKind::Stopped;
    }
    if line.starts_with("! 需要确认") || line.starts_with("! approval required") {
        return LineKind::Approval;
    }
    if line.starts_with("! ") {
        return LineKind::Problem;
    }
    if line.starts_with("· 规则：") || line.starts_with("· policy:") {
        return LineKind::Policy;
    }
    if line.starts_with("· ") || line == "·" {
        return LineKind::Note;
    }
    LineKind::Answer
}

/// Purpose: Paint one transcript line, keeping the person's `you:` label bright and their own words plain.
/// Serves: `visible_lines` and `transcript_line`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `visible_lines` and `transcript_line`.
/// Down: `classify` and `style_for`.
/// Verify: `cargo test -p loong-tui-app`.
/// One line, painted. The user's own words keep a bright label so the eye can find where
/// a prompt began when scrolling back.
fn styled(line: &str) -> Line<'static> {
    let kind = classify(line);
    if kind == LineKind::User {
        // Two vocabularies, two separators: the Chinese label ends at its full-width
        // colon and takes no space, the old `you: ` one keeps the space it was written
        // with, so a session written before the change still looks like it did.
        let (label, gap, text) = if let Some((label, text)) = line.split_once('：') {
            (format!("{label}："), "", text)
        } else if let Some((label, text)) = line.split_once(' ') {
            (label.to_string(), " ", text)
        } else {
            (line.to_string(), "", "")
        };
        return Line::from(vec![
            Span::styled(
                label,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(gap),
            Span::raw(text.to_string()),
        ]);
    }
    Line::from(Span::styled(line.to_string(), style_for(kind)))
}

/// Purpose: A line of the answer while it is still being written: dim and italic, so it is never mistaken for the record.
/// Contract: called by `visible_lines` for the streamed tail.
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

/// Purpose: The spinner frame for a time already elapsed, read from the clock so a missed redraw cannot freeze it.
/// Contract: called by `App::draw`'s working title; read by the spinner test.
/// The frame for a time already elapsed. Read from the clock rather than kept as state, so
/// a missed redraw cannot freeze the mark, and a test can ask for the frame it wants
/// instead of racing a timer.
fn spinner_frame(since: Option<std::time::Instant>) -> &'static str {
    let millis = since.map(|since| since.elapsed().as_millis()).unwrap_or(0);
    SPINNER[(millis / SPINNER_MS) as usize % SPINNER.len()]
}

/// Purpose: One tool result the reader opened: the marker it came from, the log's full lines, and its own scroll position.
/// Serves: `App::open`, `visible_lines`, `toggle_fold`, `scroll_fold` and `opened`.
/// Design: `docs/design/surfaces.md` §3 (fold and expand).
/// Up: `App::opened` builds it; `App::drain` and `App::toggle_fold` open it.
/// Down: no call; `visible_lines` and `scroll_fold` read it.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: The whole screen state: transcript, input, folds, models, the pending decision and the last turn's outcome.
/// Serves: `surface::run`, which owns one, and every test in this crate.
/// Design: `docs/design/surfaces.md` §3 and §6; `docs/design/sessions-lineage.md` §1 and §5 for the replayed past.
/// Up: `surface::run` constructs it with `App::new`; the tests construct it directly.
/// Down: no call; its methods below read and change it.
/// Verify: `cargo test -p loong-tui-app`.
pub struct App {
    /// The rendered conversation, oldest first.
    pub lines: Vec<String>,
    pub input: String,
    /// Where the caret is, in characters (not bytes) from the start of `input`.
    pub cursor: usize,
    /// The request awaiting an answer, if the turn stopped for one.
    pub pending: Option<String>,
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
    /// How to change approval prompts while a turn owns the connection. `None` when the
    /// surface has no private control channel (Windows, today): `/yolo` then waits for
    /// the worker like any other command.
    pub set_permissions: Option<Box<dyn Fn(bool)>>,
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
    /// Which command the prompt's menu highlights, by index into its candidates.
    command_choice: usize,
    /// Set when esc put the command menu away for the text it had. Like the `@`
    /// picker's flag, one key later re-arms it: the menu is derived from what is
    /// typed, and this only covers the keystroke that asked for it to go.
    commands_dismissed: bool,
    pub streaming: String,
    /// Lines scrolled up from the bottom.
    pub scroll: usize,
    /// How many rows the page had the last time it was drawn, so content that arrives
    /// while the reader is scrolled up can grow the offset by the same amount and leave
    /// the rows on screen where they are.
    scroll_total: usize,
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
    /// Purpose: A screen for one session and schema version, with nothing on it and no work running.
    /// Serves: `surface::run` and the test helpers that build an `App`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `surface::run` and the tests.
    /// Down: no call; it initialises the `App` fields.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn new(session: impl Into<String>, schema: impl Into<String>) -> Self {
        Self {
            lines: Vec::new(),
            input: String::new(),
            cursor: 0,
            pending: None,
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
            set_permissions: None,
            activity: None,
            busy_since: None,
            debug: false,
            context: None,
            streaming: String::new(),
            picker: None,
            mention_files: None,
            mention_dismissed: false,
            command_choice: 0,
            commands_dismissed: false,
            scroll: 0,
            scroll_total: 0,
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

    /// Purpose: The bookkeeping shared by Enter and Alt+R: clear the prompt, mark the turn busy, keep the text for a resend and return the submit action.
    /// Serves: `App::handle_key`'s Enter arm and `App::resend_last_prompt`.
    /// Design: `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §5 (retry resends, it does not rewind).
    /// Up: `App::handle_key` and `App::resend_last_prompt`.
    /// Down: `App::push_lines`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The bookkeeping every send does, so `enter` and `Alt+R` cannot drift apart — an
    /// interface where the resend forgets to mark itself busy would let a second turn
    /// start on top of a running one.
    fn begin_submit(&mut self, text: String) -> Action {
        // Sending returns the page to the bottom: the prompt is about to be answered, and
        // the reader wants to see it go.
        self.scroll = 0;
        // A new task makes the last tick stale, and a tick that outlives its turn is a
        // tick that lies about the work on screen.
        self.finished = None;
        self.input.clear();
        self.cursor = 0;
        // Sending is a return to the prompt, so the keyboard's place among the folds is
        // given up with it.
        self.fold = None;
        self.push_lines(vec![format!("你：{text}")]);
        self.busy = true;
        self.busy_since = Some(std::time::Instant::now());
        self.activity = None;
        self.error = None;
        self.last_prompt = Some(text.clone());
        Action::Send(ToWorker::Submit(text))
    }

    /// Purpose: `Alt+R`: submit the last prompt again to the model selected now, keeping the history.
    /// Serves: `App::handle_key`'s Alt+R arm and `App::surface_command`'s `/r`.
    /// Design: `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §5.
    /// Up: `App::handle_key` and `App::surface_command`.
    /// Down: `App::begin_submit`, `App::selected_model_label` and `App::push_lines`.
    /// Verify: `cargo test -p loong-tui-app`.
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
            self.push_lines(vec!["· 还没有可以重发的提问".to_string()]);
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

    /// Purpose: The current model as every caller should name it, with its slot when it has one.
    /// Contract: called by `resend_last_prompt` and `drain`'s model-switch line.
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

    /// Purpose: Take the session's model list and current choice, so the bar is right on the first frame.
    /// Serves: `surface::run` before the first draw, `App::drain` after a switch or a new session, and the tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `surface::run`, `App::drain` and the test helpers.
    /// Down: no call; it assigns `models` and `selected`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Take the session's model list, which is a read and arrives before the first
    /// frame. Doing it here rather than on a worker round trip is what keeps the bar
    /// from appearing one frame late, which on a slow link would be the whole session.
    pub fn set_models(&mut self, list: loong_schema::generated::ModelList) {
        self.models = list.models;
        self.selected = list.selected;
    }

    /// Purpose: Record a background model check and say so when it failed, without removing the model.
    /// Serves: `surface::run`'s health loop and the health tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `surface::run` drains the health channel into it; the tests call it directly.
    /// Down: `App::push_lines`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: Read the renderer's `⇄ model` line and move the bar with it, so a rule's mid-turn switch is not missed.
    /// Serves: `App::drain`'s `FromWorker::Lines` arm.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::drain` and `the_bar_follows_a_switch_a_rule_made_mid_turn`.
    /// Down: no call; it assigns `selected`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: The model in slot 1..9, or nothing when the slot is out of range: the one definition the bar and both shortcuts share.
    /// Serves: `App::switch_to_slot`, `App::draw`'s command hint and the model-bar tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::switch_to_slot` and `App::draw`.
    /// Down: no call; it indexes `models`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: The next or previous model, wrapping, so Ctrl-N and Ctrl-P never dead-end.
    /// Serves: `App::handle_key`'s Ctrl-N and Ctrl-P arms.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::handle_key`.
    /// Down: `App::switch_to_slot` and `App::push_lines`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: Turn a slot press into a model switch, saying so when the slot is empty or already current.
    /// Serves: `App::handle_key`'s Alt+digit arm, `App::surface_command`'s `/N` and `App::switch_relative`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::handle_key`, `App::surface_command` and `App::switch_relative`.
    /// Down: `App::model_in_slot` and `App::push_lines`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The shortcut, shared by `Alt+<digit>` and `/<digit>`.
    fn switch_to_slot(&mut self, slot: usize) -> Action {
        match self.model_in_slot(slot) {
            Some(offering) => {
                let name = offering.name.clone();
                if name == self.selected {
                    self.push_lines(vec![format!("· 已经在 [{slot}] {name} 上")]);
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

    /// Purpose: Whether the terminal has room for the mark, so a small screen gets the conversation instead.
    /// Contract: called by `App::draw` for the layout and again before drawing the banner.
    /// The debug panel: what the next request will carry, where the tokens went, and
    /// what has been cleared. Read-only by construction — asking for it records
    /// nothing, so looking cannot change what you are looking at.
    /// Whether there is room for the mark. A logo that squeezes the conversation is a
    /// decoration that costs more than it gives, so a small terminal simply has none.
    fn show_banner(&self, area: ratatui::layout::Rect) -> bool {
        // The framed character is eleven rows and a gap of air; below this the transcript
        // would be smaller than the mark, which is the trade the rule forbids.
        self.logo && area.height >= 26 && area.width >= 52
    }

    /// Purpose: The four decision facts as borrowed strings, for the panel that draws them.
    /// Contract: called by `draw_approval`; reads `App::approval`.
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

    /// Purpose: Draw the numbered model bar, marking the current and failed models, and drop models before it drops the hint.
    /// Serves: `App::draw`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw`.
    /// Down: the model list, `MARK_*` styles and `unicode_width`.
    /// Verify: `cargo test -p loong-tui-app`.
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
            let failed = self.health.get(&model.name) == Some(&false);
            // A failed check is marked, not hidden: the digits stay where they were, so a
            // network blip does not renumber somebody's keyboard.
            let mark = if failed { "✗" } else { " " };
            // The slot number is drawn as a keycap — the key that selects this model. The
            // current one is a filled cap, the rest are outlined, so the bar reads as a
            // row of keys rather than as a sentence with numbers in it.
            // `[n]` for every slot, so the number a person types keeps the same shape
            // whether or not that model is the one running.
            let cap = format!("[{}]", index + 1);
            let label_width = 1
                + UnicodeWidthStr::width(cap.as_str())
                + 1
                + UnicodeWidthStr::width(model.name.as_str());
            let gap = usize::from(shown > 0) * 3;
            // The first slot is always drawn even if it does not fit — a clipped name
            // still says which model is answering, and nothing says it if we stop here.
            // Slots first, hint second: a bar that dropped a model to keep its own
            // instructions has the priority backwards.
            if shown > 0 && used + gap + label_width > limit {
                break;
            }
            if shown > 0 {
                spans.push(Span::raw(" ".repeat(gap)));
                used += gap;
            }
            spans.push(Span::styled(
                mark.to_string(),
                Style::default().fg(Color::Red),
            ));
            spans.push(Span::styled(
                cap,
                if current {
                    // A filled key: the accent behind the digit, the terminal's own
                    // foreground on top, so it reads on either background.
                    Style::default()
                        .fg(Color::Reset)
                        .bg(MARK_ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    MARK_QUIET
                },
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                model.name.clone(),
                if failed {
                    Style::default().fg(Color::Red)
                } else if current {
                    // Bold and underlined as well as capped: the cap says which key it
                    // is, the underline says which one is in hand.
                    MARK_TEXT.add_modifier(Modifier::UNDERLINED)
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

    /// Purpose: The rule under the status bar, so the fixed chrome is not mistaken for the page's first line.
    /// Contract: called by `App::draw`; one dim line across the whole width.
    fn draw_status_rule(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        frame.render_widget(
            Paragraph::new("─".repeat(area.width as usize)).style(MARK_QUIET),
            area,
        );
    }

    /// Purpose: Whether the block wordmark fits inside the frame with room to breathe.
    /// Serves: `draw_banner` and `banner_rows`, so the layout and the paint agree.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `draw_banner`, `banner_rows` and the wordmark test.
    /// Down: `mark::WORDMARK_WIDTH`, `mark::LONG_COLUMNS` and `MARK_AIR`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The framed name is 86 columns wide and wants two of air beside it, so below 90 the
    /// frame closes around the plain word instead. The number is the promise the
    /// documentation makes; the arithmetic lives in `banner_interior`.
    fn wordmark_fits(width: u16) -> bool {
        width >= 90
    }

    /// Purpose: The columns inside the frame: air, the dragon, air, the name, air.
    /// Serves: `draw_banner` and the threshold `wordmark_fits` states.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `draw_banner`.
    /// Down: `MARK_AIR`, `MARK_WORD`, `mark::LONG_COLUMNS` and `mark::WORDMARK_WIDTH`.
    /// Verify: `cargo test -p loong-tui-app`.
    fn banner_interior(blocks: bool) -> usize {
        use unicode_width::UnicodeWidthStr;
        let name = if blocks {
            mark::WORDMARK_WIDTH
        } else {
            UnicodeWidthStr::width(MARK_WORD)
        };
        MARK_AIR + mark::LONG_COLUMNS + MARK_AIR + name + MARK_AIR
    }

    /// Purpose: How many rows the question panel asks for, given the page it sits on.
    /// Contract: called by `App::draw`; the answers are the floor, the details give way first.
    fn decision_rows(height: u16) -> u16 {
        let floor = 2 + Self::options().len() as u16;
        DECISION_ROWS
            .min(height.saturating_sub(8))
            .max(floor.min(height))
    }

    /// Purpose: Which favicon the empty session opens on, if any: the picture and its column count.
    /// Serves: `App::draw`, which hides the header and draws the splash instead.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw`.
    /// Down: `mark::FAVICON_WIDE` and `mark::FAVICON_NARROW`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// A splash needs room the conversation would otherwise take: the picture, a row of
    /// air, the wordmark, a row of air and the inscription. Nothing is shown on an empty
    /// session that has already been sent a line, replayed or not — the first line is what
    /// turns a title screen into a conversation.
    fn splash_art(&self, area: ratatui::layout::Rect) -> Option<(&'static [&'static str], usize)> {
        if !self.logo || !self.lines.is_empty() || self.pending.is_some() {
            return None;
        }
        if area.width >= 112 && area.height >= 41 {
            return Some((&mark::FAVICON_WIDE, mark::FAVICON_WIDE_COLUMNS));
        }
        if area.width >= 92 && area.height >= 35 {
            return Some((&mark::FAVICON_NARROW, mark::FAVICON_NARROW_COLUMNS));
        }
        None
    }

    /// Purpose: Wrap content lines in a one-column border with a title in the top rule.
    /// Serves: `approval_lines`, `command_lines`, `picker_lines` and `input_lines`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: those four builders.
    /// Down: `unicode_width` for the title's measured width and `Line::width` for the padding.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The boxes are lines, not widgets, because the page scrolls as one document: a box
    /// that scrolled half off the top has to lose its top rule, which a widget cannot do.
    fn framed(
        content: Vec<Line<'static>>,
        width: u16,
        title: Span<'static>,
        border: Style,
    ) -> Vec<Line<'static>> {
        use unicode_width::UnicodeWidthStr;
        let inner = width.saturating_sub(2) as usize;
        let used = UnicodeWidthStr::width(title.content.as_ref());
        let fill = inner.saturating_sub(used);
        let mut out = Vec::with_capacity(content.len() + 2);
        out.push(Line::from(vec![
            Span::styled("┌".to_string(), border),
            title,
            Span::styled(format!("{}┐", "─".repeat(fill)), border),
        ]));
        for line in content {
            let drawn = line.width();
            let mut spans = vec![Span::styled("│".to_string(), border)];
            spans.extend(line.spans);
            spans.push(Span::raw(" ".repeat(inner.saturating_sub(drawn))));
            spans.push(Span::styled("│".to_string(), border));
            out.push(Line::from(spans));
        }
        out.push(Line::from(Span::styled(
            format!("└{}┘", "─".repeat(inner)),
            border,
        )));
        out
    }

    /// Purpose: The mark as document lines: the frame around the pixel dragon and the name.
    /// Serves: `App::draw`, as the top of the page.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw`.
    /// Down: `mark::{glyph_lines, LONG, WORDMARK, centered}` and the `MARK_*` constants.
    /// Verify: `cargo test -p loong-tui-app`.
    fn banner_lines(&self, width: u16) -> Vec<Line<'static>> {
        use unicode_width::UnicodeWidthStr;
        let frame_ink = Style::default().fg(MARK_ACCENT);
        let blocks = Self::wordmark_fits(width);
        let glyph = mark::glyph_lines(&mark::LONG, mark::LONG_COLUMNS);
        let name_width = if blocks {
            mark::WORDMARK_WIDTH
        } else {
            UnicodeWidthStr::width(MARK_WORD)
        };
        let name_rows = if blocks { MARK_ROWS as usize } else { 1 };
        let inner = Self::banner_interior(blocks);
        let left = (width as usize).saturating_sub(inner + 2) / 2;
        let lead = || Span::raw(" ".repeat(left));
        let rule = |ornament: &str, closer: char| -> String {
            let used = UnicodeWidthStr::width(ornament);
            let side = inner.saturating_sub(used) / 2;
            let rest = inner.saturating_sub(used + side);
            format!(
                "{}{}{}{}{}",
                if closer == '╗' { '╔' } else { '╚' },
                "═".repeat(side),
                ornament,
                "═".repeat(rest),
                closer
            )
        };
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(vec![
            lead(),
            Span::styled(rule("༺ ᛭ ༻", '╗'), frame_ink),
        ]));
        let first = (glyph.len() - name_rows) / 2;
        for (row, glyph_row) in glyph.into_iter().enumerate() {
            let mut spans = vec![
                lead(),
                Span::styled("║".to_string(), frame_ink),
                Span::raw(" ".repeat(MARK_AIR)),
            ];
            spans.extend(glyph_row.spans);
            spans.push(Span::raw(" ".repeat(MARK_AIR)));
            let name = match row.checked_sub(first).filter(|at| *at < name_rows) {
                Some(at) if blocks => Span::styled(mark::WORDMARK[at].to_string(), MARK_TEXT),
                Some(_) => Span::styled(MARK_WORD.to_string(), MARK_TEXT),
                None => Span::raw(" ".repeat(name_width)),
            };
            spans.push(name);
            spans.push(Span::raw(" ".repeat(MARK_AIR)));
            spans.push(Span::styled("║".to_string(), frame_ink));
            lines.push(Line::from(spans));
        }
        lines.push(Line::from(vec![
            lead(),
            Span::styled(rule("༻ ᛭ ༺", '╝'), frame_ink),
        ]));
        if let Some((_, text)) = MARK_SUBLINE
            .iter()
            .find(|(needs, _)| (width as usize) >= *needs)
        {
            let spans = if *text == MARK_SUBLINE[0].1 {
                vec![
                    Span::styled("⟬ ᛭ ".to_string(), frame_ink),
                    Span::styled("兴云吐雾".to_string(), MARK_TEXT),
                    Span::styled(" ⋄ ".to_string(), frame_ink),
                    Span::styled("隐介藏形".to_string(), MARK_TEXT),
                    Span::styled(" ᛭ ⟭".to_string(), frame_ink),
                ]
            } else {
                vec![Span::styled(text.to_string(), MARK_QUIET)]
            };
            lines.push(mark::centered(Line::from(spans), width));
        }
        lines
    }

    /// Purpose: The opening picture as document lines: the favicon, the wordmark and the inscription.
    /// Serves: `App::draw`, as the top of the page while the session is empty.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw`.
    /// Down: `mark::{favicon_lines, centered}` and `mark::WORDMARK`.
    /// Verify: `cargo test -p loong-tui-app`.
    fn splash_lines(
        &self,
        width: u16,
        art: &'static [&'static str],
        columns: usize,
    ) -> Vec<Line<'static>> {
        let favicon = mark::favicon_lines(art, columns);
        let mut lines: Vec<Line> = Vec::new();
        for line in favicon {
            lines.push(mark::centered(line, width));
        }
        lines.push(Line::from(""));
        for row in mark::WORDMARK {
            lines.push(mark::centered(
                Line::from(Span::styled(row.to_string(), MARK_TEXT)),
                width,
            ));
        }
        lines.push(Line::from(""));
        lines.push(mark::centered(
            Line::from(Span::styled(
                "⟬ ᛭ 兴云吐雾 ⋄ 隐介藏形 ᛭ ⟭".to_string(),
                MARK_QUIET,
            )),
            width,
        ));
        lines
    }

    /// Purpose: The question panel as document lines, so it scrolls with the page it is asked about.
    /// Serves: `App::draw` when a decision is pending.
    /// Design: `docs/design/surfaces.md` §3 and §6.
    /// Up: `App::draw`.
    /// Down: `App::info`, `Self::options`, `one_line` and `clip`.
    /// Verify: `cargo test -p loong-tui-app`.
    fn approval_lines(&self, width: u16, height: u16) -> Vec<Line<'static>> {
        let room = height.saturating_sub(2) as usize;
        let mut details: Vec<Line> = Vec::new();
        match &self.info() {
            Some((class, reason, tool, args)) => {
                use unicode_width::UnicodeWidthStr;
                let room = (width.saturating_sub(4) as usize)
                    .saturating_sub(class.width() + 2)
                    .max(1);
                details.push(Line::from(vec![
                    Span::styled(class.to_string(), Style::default().fg(MARK_ACCENT)),
                    Span::raw("  "),
                    Span::styled(clip(tool, room), Style::default().fg(MARK_INK)),
                ]));
                if !reason.is_empty() {
                    details.push(Line::from(Span::styled(
                        one_line(reason, width.saturating_sub(4) as usize),
                        MARK_QUIET,
                    )));
                }
                if !args.is_empty() {
                    details.push(Line::from(Span::styled(
                        one_line(args, width.saturating_sub(4) as usize),
                        MARK_QUIET,
                    )));
                }
            }
            None => details.push(Line::from(Span::styled("正在取详情…", MARK_QUIET))),
        }
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
                Style::default()
            };
            lines.push(Line::from(vec![
                Span::styled(if chosen { "  ▸ " } else { "    " }, style),
                Span::styled(label.to_string(), style),
            ]));
        }
        Self::framed(
            lines,
            width,
            Span::styled(" 需要你决定 ".to_string(), Style::default()),
            Style::default(),
        )
    }

    /// Purpose: The command menu as document lines.
    /// Serves: `App::draw` while a command is being typed.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw`.
    /// Down: `App::command_candidates`, `commands::{is_exact}` and `MARK_ACCENT_STYLE`.
    /// Verify: `cargo test -p loong-tui-app`.
    fn command_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(found) = self.command_candidates() else {
            return Vec::new();
        };
        let exact = commands::is_exact(&self.input);
        let typed = self.input.trim();
        let title = if exact {
            "命令 — 回车执行 · esc 关闭".to_string()
        } else if typed == "/" {
            "命令 — ↑/↓ 选择 · 回车补全 · esc 关闭".to_string()
        } else {
            format!("匹配 {typed} 的命令 — ↑/↓ 选择 · 回车补全 · esc 关闭")
        };
        let body = if found.is_empty() {
            vec![Line::from(
                "  不是这个界面的命令 — 回车仍然会发出去（/help 这类技能在那边）",
            )]
        } else {
            let chosen = self.command_choice.min(found.len() - 1);
            found
                .iter()
                .enumerate()
                .map(|(index, candidate)| {
                    let marker = if index == chosen { "▸ " } else { "  " };
                    let line = format!("{marker}{} — {}", candidate.insert, candidate.description);
                    if index == chosen {
                        Line::from(line).style(MARK_ACCENT_STYLE)
                    } else {
                        Line::from(line)
                    }
                })
                .collect()
        };
        Self::framed(
            body,
            width,
            Span::styled(title, Style::default()),
            Style::default(),
        )
    }

    /// Purpose: The `@` picker as document lines.
    /// Serves: `App::draw` while a path is being picked.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw`.
    /// Down: `App::picker` and `MARK_ACCENT_STYLE`.
    /// Verify: `cargo test -p loong-tui-app`.
    fn picker_lines(&self, width: u16) -> Vec<Line<'static>> {
        let Some(picker) = &self.picker else {
            return Vec::new();
        };
        let title = if picker.query.is_empty() {
            "文件 — ↑/↓ 选择 · 回车插入 · esc 取消".to_string()
        } else {
            format!(
                "匹配 {} 的文件 — ↑/↓ 选择 · 回车插入 · esc 取消",
                picker.query
            )
        };
        let body = if picker.matches.is_empty() {
            vec![Line::from("  没有匹配的文件 — 继续输入，或按 esc")]
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
                        Line::from(line).style(MARK_ACCENT_STYLE)
                    } else {
                        Line::from(line)
                    }
                })
                .collect()
        };
        Self::framed(
            body,
            width,
            Span::styled(title, Style::default()),
            Style::default(),
        )
    }

    /// Purpose: The input box as document lines, with the caret's place among them.
    /// Serves: `App::draw`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw`.
    /// Down: `wrapped_input`, `title_style` and the status title's own words.
    /// Verify: `cargo test -p loong-tui-app`.
    fn input_lines(&self, width: u16, rows: usize) -> (Vec<Line<'static>>, u16, u16) {
        let (title, style) = self.status();
        let inner_width = width.saturating_sub(2);
        let (lines, (caret_row, caret_column)) = self.wrapped_input(inner_width);
        let offset = (caret_row as usize + 1).saturating_sub(rows);
        let visible: Vec<Line> = lines
            .into_iter()
            .skip(offset)
            .take(rows)
            .map(Line::from)
            .collect();
        let boxed = Self::framed(visible, width, Span::styled(title, style), Style::default());
        (boxed, caret_row - offset as u16, caret_column)
    }

    /// Purpose: The prompt box's status: the sentence in its top rule and the colour it is drawn in.
    /// Serves: `App::draw`'s input lines.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::input_lines`.
    /// Down: `yolo_mark`, `title_style`, `commands::matches` and the finished turn.
    /// Verify: `cargo test -p loong-tui-app`.
    fn status(&self) -> (String, Style) {
        match &self.pending {
            // The question is the state, whatever else is true: a menu that does not
            // say what it is asking is worse than no menu. Under YOLO the mark goes in
            // front rather than replacing the sentence.
            Some(_) => (
                format!(
                    "{}等你决定 — ↑/↓ 选择 · 回车确认 · esc 拒绝",
                    self.yolo_mark()
                ),
                self.title_style(MARK_ACCENT_STYLE),
            ),
            // Busy: what it is doing and for how long. "working…" for two minutes
            // tells nobody whether it is thinking or stuck. Under YOLO this used to be
            // replaced by the warning, so the one state that must never be invisible was
            // the one nobody could see; the mark rides in front of it instead.
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
                    format!(
                        "{}{} 运行 {secs}s{doing}",
                        self.yolo_mark(),
                        spinner_frame(self.busy_since)
                    ),
                    self.title_style(Style::default().fg(Color::Cyan)),
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
                (
                    format!("{}{}", self.yolo_mark(), finished.line()),
                    self.title_style(finished.style()),
                )
            }
            // A leading `/` is a command: say where the list of them is.
            None if self.input.starts_with('/') => {
                // Lowercased once, so `/R` and `/EXIT` complete the same way `/r` and
                // `/exit` do — the command matcher is case-insensitive, and a hint that
                // disagreed with it would be worse than no hint.
                let typed = self.input.trim().to_ascii_lowercase();
                (
                    format!(
                        "{}{}",
                        self.yolo_mark(),
                        match typed.as_str() {
                            // Once it is unambiguous, say what will happen rather than where to
                            // read about it.
                            "/exit" | "/quit" | "/q" => {
                                "回车退出 loongagent（ctrl-c 也可以）".to_string()
                            }
                            "/logo" => "回车显示或隐藏徽标".to_string(),
                            "/expand" => {
                                "回车把上一条工具结果完整展开（tab 在折叠之间走）".to_string()
                            }
                            "/yolo" => "回车关闭确认提示（沙箱仍然生效）".to_string(),
                            "/copy" => "回车复制整段记录".to_string(),
                            "/new" | "/clear" | "/reset" => {
                                "回车开始新会话 — 这一条会存盘，上下文不带走".to_string()
                            }
                            "/mouse" => "回车把鼠标还给终端".to_string(),
                            // The model shortcuts, said in full only when they would work: a
                            // completion that promises a switch in a session with one model
                            // is a completion that lies.
                            "/r" if self.last_prompt.is_some() => {
                                "回车把上一个提问再问一遍，用当前模型".to_string()
                            }
                            "/r" => "还没有可重发的提问 — 先问一句".to_string(),
                            _ => match typed
                                .strip_prefix('/')
                                .and_then(|n| n.parse::<usize>().ok())
                            {
                                Some(slot) if (1..=9).contains(&slot) => {
                                    match self.model_in_slot(slot) {
                                        Some(offering) => {
                                            format!("回车切到 [{slot}] {}", offering.name)
                                        }
                                        None => format!("第 {slot} 个位置没有模型"),
                                    }
                                }
                                // A prefix of exactly one command is worth saying in full: it
                                // is the name the menu is about to complete, and the sentence
                                // is what that command will do. Several, or none, are the
                                // menu's job to explain.
                                _ => {
                                    let found = commands::matches(&self.input);
                                    match found.len() {
                                        1 => format!("回车 · {}", found[0].description),
                                        0 => "不是这个界面的命令 — 回车仍然会发出去".to_string(),
                                        _ => "命令 — ↑/↓ 选择 · 回车补全".to_string(),
                                    }
                                }
                            },
                        }
                    ),
                    self.title_style(Style::default().fg(Color::Green)),
                )
            }
            // Nothing to report but the relaxed session itself. This one keeps the full
            // sentence, because there is room for it; every other state carries the short
            // mark in front, so the warning is never the price of the status.
            None if self.yolo => (
                "⚠ 免确认 — 确认提示已关，沙箱仍然生效（/yolo 恢复）".to_string(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            None => ("输入".to_string(), Style::default()),
        }
    }

    /// Purpose: Draw the F2 context panel: token totals, categories, ledger entries and the memory's state.
    /// Serves: `App::draw` when the debug panel is open.
    /// Design: `docs/design/surfaces.md` §3; `docs/context/status.md` §11.1 for the memory fields.
    /// Up: `App::draw`.
    /// Down: `memory_lines` for the memory block.
    /// Verify: `cargo test -p loong-tui-app`.
    fn draw_context(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let mut lines: Vec<Line> = Vec::new();
        match &self.context {
            None => lines.push(Line::from(Span::styled("正在问服务端…", MARK_QUIET))),
            Some(explained) => {
                let budget = explained.budget_tokens;
                let used = explained.context_tokens;
                let percent = if budget > 0 {
                    format!("{:.0}%", (used as f64 / budget as f64) * 100.0)
                } else {
                    "无上限".to_string()
                };
                lines.push(Line::from(vec![
                    Span::styled("令牌 ", MARK_QUIET),
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
                        "{} 条消息 · {} 个工具 · {} 次压缩",
                        explained.messages, explained.tools, explained.compactions
                    ))
                    .style(MARK_QUIET),
                );
                lines.push(Line::from(""));
                lines.extend(memory_lines(explained));
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "类别                令牌",
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
                    "记录（新的在下）  p=固定 c=清除",
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
                        .title(" 上下文 (f2) "),
                )
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    /// Purpose: The slash commands the screen answers itself, so quitting and the view toggles never reach the agent.
    /// Serves: `App::handle_key`'s Enter arm, and the command hint in `App::draw`.
    /// Design: `docs/design/surfaces.md` §3 and §6.
    /// Up: `App::handle_key`.
    /// Down: `App::switch_to_slot`, `App::resend_last_prompt`, `App::push_lines` and the `Action` variants.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The one place that decides which slash commands never reach the agent. Kept
    /// deliberately short: a command belongs here only if the *interface* is what has
    /// to act — quitting, and nothing else so far. `/help` and the rest are skills the
    /// workspace provides, and they stay on the server where they can be audited.
    fn surface_command(&mut self, text: &str) -> Option<Action> {
        let trimmed = text.trim();
        // `/1`..`/9`: the same shortcut as Alt+digit, for a terminal whose Alt never
        // reaches the program (tmux, some macOS setups). Checked before the table
        // because it is a whole family of commands, not one.
        if let Some(slot) = commands::slot(trimmed) {
            self.input.clear();
            self.cursor = 0;
            return Some(self.switch_to_slot(slot as usize));
        }
        // The table owns the names and the aliases, so a spelling the menu offers is
        // a spelling this dispatcher knows. It does not own what each one *does*:
        // that stays here, next to the action it asks for.
        match commands::primary(trimmed)? {
            "/exit" => {
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
                let state = if self.mouse { "已开启" } else { "已关闭" };
                self.push_lines(vec![format!(
                    "· 鼠标{state}{}",
                    if self.mouse {
                        " — 这里用滚轮滚动；按住 shift 交给终端选择"
                    } else {
                        " — 照常拖动选择、复制；这里用 pgup/pgdn 滚动"
                    }
                )]);
                Some(Action::Mouse(self.mouse))
            }
            // Opting out of the prompts, for a session that is unattended on purpose.
            // While a turn owns the connection a queued request would wait for the turn
            // to end — the very wait `/yolo` exists to prevent. With the private control
            // channel the change goes now: the screen flips at once, the kernel applies
            // it at its next decision boundary, and `permissions.relaxed` confirms it.
            "/yolo" => {
                self.input.clear();
                self.cursor = 0;
                let target = !self.yolo;
                if self.busy && self.set_permissions.is_some() {
                    self.yolo = target;
                    if let Some(tell) = &self.set_permissions {
                        tell(target);
                    }
                    return Some(Action::None);
                }
                Some(Action::Send(ToWorker::Yolo(target)))
            }
            // A fresh session. `/clear` is the same thing and not a screen wipe: the
            // context is what people mean when they ask for a clean start, and clearing
            // only the view would leave the model still reading everything above.
            "/new" => {
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
            // `/r`: the resend, for the same terminals that eat Alt.
            "/r" => {
                self.input.clear();
                self.cursor = 0;
                Some(self.resend_last_prompt())
            }
            _ => None,
        }
    }

    /// Purpose: Ask the server process to stop the running turn, saying "interrupting" rather than "stopped".
    /// Contract: called by `handle_key`'s Ctrl-C and Esc arms and by `a_turn_the_person_stopped_says_so_rather_than_showing_a_tick`; signals `App::interrupt` and calls `push_lines`.
    /// Ask for the turn to stop. The server stops at the next step boundary — after the
    /// tool that is already running finishes — so the line says "interrupting", not
    /// "stopped".
    fn stop_turn(&mut self) {
        self.stopping = true;
        match &self.interrupt {
            Some(interrupt) => {
                interrupt();
                self.push_lines(vec!["⏹ 正在中断…".to_string()]);
            }
            None => self.push_lines(vec![
                "⏹ 无法中断：这个界面没有可通知的服务端进程".to_string()
            ]),
        }
    }

    /// Purpose: The byte offset of a character index in the prompt, for the editing methods.
    /// Contract: called by `insert`, `delete_before_cursor`, `delete_at_cursor` and `handle_key`'s kill chords.
    fn byte_at(&self, char_index: usize) -> usize {
        self.input
            .char_indices()
            .nth(char_index)
            .map(|(byte, _)| byte)
            .unwrap_or(self.input.len())
    }

    /// Purpose: Insert text at the caret, advance it and refresh the picker.
    /// Contract: called by `handle_key`'s character arm; calls `byte_at` and `refresh_picker`.
    fn insert(&mut self, text: &str) {
        let at = self.byte_at(self.cursor);
        self.input.insert_str(at, text);
        self.cursor += text.chars().count();
        self.refresh_picker();
    }

    /// Purpose: Keep the `@` picker in step with the text and the caret, reading the file list only once.
    /// Serves: `App::insert`, `App::delete_before_cursor`, `App::delete_at_cursor` and `App::handle_key`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: every edit and caret move in `App`.
    /// Down: `mention::FileIndex::read` and `mention::Picker::refresh`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: The command candidates the prompt is offering, or `None` when no menu belongs on screen.
    /// Serves: `App::handle_key`, `App::draw`, `App::command_rows`, `App::draw_commands` and the command tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::handle_key` and `App::draw`.
    /// Down: `commands::matches`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// What the command menu should list right now.
    ///
    /// The menu is derived from the text, like the `@` picker: a command is the whole
    /// line while it is being typed, so a space — or a caret moved into the middle of
    /// the word — means the person is no longer naming a command and the menu is not
    /// what their keys mean. `commands_dismissed` only covers the key that asked it to
    /// go; every later key derives it again.
    fn command_candidates(&self) -> Option<Vec<commands::Candidate>> {
        if self.pending.is_some() || self.commands_dismissed {
            return None;
        }
        if !self.input.starts_with('/')
            || self.input.chars().any(char::is_whitespace)
            || self.cursor != self.input.chars().count()
        {
            return None;
        }
        let mut found = commands::matches(&self.input);
        // `/r` is only a command once there is something to resend; offering it
        // before that would promise an action the next enter cannot take. It stays
        // an exact name — enter still answers "nothing to resend yet" — it is just
        // not a menu row until then.
        if self.last_prompt.is_none() {
            found.retain(|candidate| candidate.insert != "/r");
            if found.is_empty() && commands::is_exact(&self.input) {
                return None;
            }
        }
        Some(found)
    }

    /// Purpose: How many commands the prompt is offering.
    /// Contract: called by `handle_key`'s list keys; reads `command_candidates`.
    fn command_menu_len(&self) -> usize {
        self.command_candidates().map_or(0, |found| found.len())
    }

    /// Purpose: Whether Enter should complete the highlighted command rather than run or send what is typed.
    /// Contract: called by `handle_key`'s enter arm; false for nothing to complete and for an exact command.
    fn command_to_complete(&self) -> bool {
        let Some(found) = self.command_candidates() else {
            return false;
        };
        !found.is_empty() && !commands::is_exact(&self.input)
    }

    /// Purpose: Move the command menu's highlight, wrapping at either end.
    /// Contract: called by the arrow, tab and shift-tab arms while the menu has candidates.
    fn move_command(&mut self, delta: isize) {
        if let Some(found) = self.command_candidates() {
            if found.is_empty() {
                return;
            }
            let count = found.len() as isize;
            let current = self.command_choice.min(found.len() - 1) as isize;
            self.command_choice = (current + delta).rem_euclid(count) as usize;
        }
    }

    /// Purpose: Write the highlighted command into the prompt, leaving the running of it to the next enter.
    /// Serves: `App::handle_key`'s enter arm while a prefix is being completed; the command tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::handle_key`.
    /// Down: `commands::Candidate::insert`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Complete, never run: `/ex` prefixes both `/exit` and `/expand`, and an enter
    /// that ran the highlighted one could leave the program on a half-typed name.
    fn accept_command(&mut self) -> Action {
        let Some(found) = self.command_candidates() else {
            return Action::None;
        };
        if found.is_empty() {
            return Action::None;
        }
        let chosen = &found[self.command_choice.min(found.len() - 1)];
        self.input = chosen.insert.clone();
        self.cursor = self.input.chars().count();
        self.command_choice = 0;
        Action::None
    }

    /// Purpose: How many rows the command menu needs: its commands, or one line saying why nothing matched.
    /// Contract: called by `App::draw` and the command menu's height test.
    fn command_rows(&self) -> u16 {
        match self.command_candidates() {
            None => 0,
            Some(found) => found.len().max(1) as u16,
        }
    }

    /// Purpose: How many rows the picker needs: its matches, or one line saying there are none.
    /// Contract: called by `App::draw` and the picker-height test.
    /// How many rows the picker needs: its matches, or one line saying there are
    /// none — a picker that vanished when a query matched nothing would look like a
    /// crash rather than an empty answer.
    fn picker_rows(&self) -> u16 {
        match &self.picker {
            None => 0,
            Some(picker) => picker.matches.len().max(1) as u16,
        }
    }

    /// Purpose: Insert the highlighted path and close the picker, leaving the caret after the token.
    /// Serves: `App::handle_key`'s Enter-while-picker arm.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::handle_key`.
    /// Down: `mention::apply`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: Delete one character before the caret and refresh the picker.
    /// Contract: called by `handle_key`'s Backspace arm; edits through `byte_at` and `refresh_picker`.
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

    /// Purpose: Delete the character under the caret and refresh the picker.
    /// Contract: called by `handle_key`'s Delete arm; edits through `byte_at` and `refresh_picker`.
    fn delete_at_cursor(&mut self) {
        if self.cursor >= self.input.chars().count() {
            return;
        }
        let from = self.byte_at(self.cursor);
        let to = self.byte_at(self.cursor + 1);
        self.input.replace_range(from..to, "");
        self.refresh_picker();
    }

    /// Purpose: How many rows the input box needs for its text, capped so a pasted file cannot eat the conversation.
    /// Serves: `App::draw` and the wrapping tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw` and the tests.
    /// Down: `App::wrapped_input`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: Wrap the input by display columns and say where the caret lands, so the terminal cursor is drawn where typing goes.
    /// Serves: `App::input_rows`, `App::draw` and the wrapping tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::input_rows`, `App::draw` and the tests.
    /// Down: `unicode_width` for column measurement.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: Append rendered lines to the transcript, splitting embedded newlines and pulling the view back to the newest output.
    /// Serves: `surface::run`, `App::begin_submit`, `App::resend_last_prompt`, `App::drain` and the tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: many `App` methods and `surface::run`.
    /// Down: no call; it appends to `lines` and resets `scroll`.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn push_lines(&mut self, lines: impl IntoIterator<Item = String>) {
        for line in lines {
            for piece in line.split('\n') {
                self.lines.push(piece.to_string());
            }
        }
        // Deliberately **not** snapping back to the bottom: a reader who scrolled away is
        // reading, and output that yanks the page from under them is output they cannot
        // select. The draw loop grows the offset by exactly the rows added, so the page
        // stays where it was; only the bottom of the page follows new lines.
    }

    /// Purpose: Put one line of the server's stderr in the transcript, collapsing a repeat of the last line.
    /// Serves: `surface::run`'s notes loop and the notes test.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `surface::run` and the notes test.
    /// Down: `App::push_lines`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The server's stderr is a trail, not a status: the sandbox line, the hooks it found,
    /// an error it printed — all of it is history, and history belongs in the transcript,
    /// where it scrolls with the turn it explains. It used to be a two-row strip above the
    /// box that showed only the newest line, which read as a caption floating outside the
    /// conversation rather than as part of it.
    pub fn push_note(&mut self, note: String) {
        let line = format!("· {note}");
        // A repeat is the same fact restated, not a second fact: the screen should not grow
        // a second copy because the child printed its configuration twice.
        if self.lines.last() == Some(&line) {
            return;
        }
        self.push_lines(vec![line]);
    }

    /// Purpose: Fold every message the worker has sent into the screen state, once per pass of the draw loop.
    /// Serves: `surface::run`'s loop and the tests.
    /// Design: `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §1 and §5 for a new session and a resumed question.
    /// Up: `surface::run`; the tests call it directly with a channel.
    /// Down: `App::set_models`, `App::follow_model_change`, `App::push_lines`, `App::opened` and `App::remember_fold`.
    /// Verify: `cargo test -p loong-tui-app`.
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
                        "⚠ 确认提示已{}{}",
                        if on { "关闭" } else { "打开" },
                        if on {
                            " — 沙箱仍然生效；工作区之外的写入照样被拒"
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
                        self.push_lines(vec!["· 还没有可展开的工具结果".to_string()]);
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
                    // A turn that stopped for a question is **paused**, not finished: the
                    // question is the state on screen, and the turn still has work to resume.
                    // Recording an ending here put a tick in the transcript while the work
                    // was still to come — the one line that lies about what is happening.
                    let paused = pending.is_some();
                    self.pending = pending;
                    self.busy = false;
                    self.activity = None;
                    self.streaming.clear();
                    if !paused {
                        // How long the turn took, from the submit to the end. A pause for
                        // a question is part of it: the turn had not ended, and a clock
                        // that restarted afterwards would be timing something else.
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

    /// Purpose: Turn one key event into an `Action`, keeping the picker, folds, approval menu and prompt from stealing each other's keys.
    /// Serves: `surface::run`'s event loop and every keyboard test.
    /// Design: `docs/design/surfaces.md` §3 and §6.
    /// Up: `surface::run`; the tests.
    /// Down: nearly every `App` method, from `App::insert` to `App::answer`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The keys that mean something. While an approval is pending the input line is
    /// repurposed: typing a prompt then would be ignored anyway, and saying so is
    /// kinder than silently swallowing it.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // A menu put away by esc covered the text it had; the next key derives it
        // again, exactly as the `@` picker's dismissal works.
        self.commands_dismissed = false;
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
            // The command menu owns the list keys while it is open, for the same
            // reason the `@` picker does: a list on screen is what up/down mean.
            // Enter runs a complete command and only *completes* a prefix — `/ex`
            // is both `/exit` and `/expand`, and running the highlighted one could
            // leave the program on a name nobody finished typing.
            KeyCode::Esc if self.command_candidates().is_some() => {
                self.commands_dismissed = true;
                Action::None
            }
            KeyCode::Up if self.command_menu_len() > 0 => {
                self.move_command(-1);
                Action::None
            }
            KeyCode::Down if self.command_menu_len() > 0 => {
                self.move_command(1);
                Action::None
            }
            KeyCode::Tab if self.command_menu_len() > 0 => {
                self.move_command(1);
                Action::None
            }
            KeyCode::BackTab if self.command_menu_len() > 0 => {
                self.move_command(-1);
                Action::None
            }
            KeyCode::Enter if self.command_to_complete() => self.accept_command(),
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

    /// Purpose: Every fold marker in the transcript, oldest first, so the keyboard and the client agree on which lines are folds.
    /// Contract: called by `move_fold`, `opened` and the fold-count hint in `draw`.
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

    /// Purpose: Which fold a marker is, counting from the oldest: the name both sides of the channel use.
    /// Contract: called by `toggle_fold`; counts `loong_tui::is_fold_marker` lines before the marker.
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

    /// Purpose: Keep the full text of a fold the turn sent, bounded by `FOLD_KEEP` so a long session cannot grow without limit.
    /// Contract: called by `drain`'s `Fold` arm, `surface::run`'s replayed history and `enter_opens_a_fold_the_turn_sent_without_asking_the_log`.
    /// Keep the full text of a fold this view was shown. A number already held is replaced:
    /// the newest fold is the one anyone is about to open, so what must not survive is the
    /// oldest, and the log still has every one of them.
    fn remember_fold(&mut self, at: usize, lines: Vec<String>) {
        self.folded.insert(at, lines);
        while self.folded.len() > FOLD_KEEP {
            self.folded.pop_first();
        }
    }

    /// Purpose: Walk the keyboard's fold selection to the older or newer marker, wrapping at either end.
    /// Serves: `App::handle_key`'s Tab and Shift-Tab arms.
    /// Design: `docs/design/surfaces.md` §3 (fold and expand).
    /// Up: `App::handle_key`.
    /// Down: `App::folds`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: Move inside an open fold by rows, in the same direction the transcript scrolls.
    /// Contract: called by `handle_key`'s paging-over-a-fold arms and by `handle_mouse`.
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

    /// Purpose: Open the fold the keyboard is on, from the text the turn already sent or by asking the log; the same key closes it.
    /// Serves: `App::handle_key`'s Enter arm.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::handle_key`.
    /// Down: `App::fold_at`, `App::opened`, the `folded` cache and `ToWorker::ExpandAt`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: Open the full result the worker sent, matched by the fold's number rather than by its distance from the end.
    /// Serves: `App::toggle_fold` and `App::drain`'s `Expanded` arm.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::toggle_fold` and `App::drain`.
    /// Down: `App::folds` and `App::push_lines`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Take delivery of a result the reader asked to see in full. The reply is matched by
    /// the name that was asked for, not by where the fold sits from the end: the answer may
    /// arrive after the turn has written more folds, and the name is the one thing that
    /// still means the same result when it does.
    fn opened(&mut self, at: usize, lines: Vec<String>) -> Action {
        let folds = self.folds();
        let Some(marker) = folds.get(at).copied() else {
            self.push_lines(vec!["· 那条工具结果已经不在记录里了".to_string()]);
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

    /// Purpose: The whole transcript as one string, for `/copy`.
    /// Serves: `surface::run`'s Copy arm and the copy test.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `surface::run` and the copy test.
    /// Down: no call; it joins `lines`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Everything said so far, for `/copy`.
    pub fn transcript(&self) -> String {
        self.lines.join("\n")
    }

    /// Purpose: Scroll the conversation on a wheel event, or the open fold when one is on screen.
    /// Serves: `surface::run`'s mouse event arm.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `surface::run` and `the_wheel_scrolls_the_conversation`.
    /// Down: `App::scroll_fold`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: Turn the highlighted decision into a response: record it on screen, mark the turn busy and send the answer.
    /// Serves: `App::handle_key`'s Enter, letter and Esc arms while a decision is pending.
    /// Design: `docs/design/surfaces.md` §3 and §6 (the surface sends the decision; it does not make it).
    /// Up: `App::handle_key`.
    /// Down: `App::push_lines` and `ToWorker::Respond`.
    /// Verify: `cargo test -p loong-tui-app`.
    fn answer(&mut self, granted: bool, scope: loong_schema::generated::ApprovalScope) -> Action {
        let Some(request_id) = self.pending.take() else {
            return Action::None;
        };
        self.approval = None;
        let how = if !granted {
            "已拒绝"
        } else {
            match scope {
                loong_schema::generated::ApprovalScope::Once => "已批准（本次）",
                loong_schema::generated::ApprovalScope::ActionClass => "已批准（此类）",
                loong_schema::generated::ApprovalScope::Project => "已批准（本项目）",
            }
        };
        self.push_lines(vec![format!("⧗ {how}: {request_id}")]);
        self.busy = true;
        // The question paused the turn; this resumes it. The turn's clock keeps running
        // across that pause (a pause is not an ending), so it is only filled in when a
        // turn has none — `busy` with no clock is a title frozen at "⠋ working 0s".
        self.busy_since.get_or_insert_with(std::time::Instant::now);
        self.error = None;
        Action::Send(ToWorker::Respond {
            request_id,
            granted,
            scope,
        })
    }

    /// Purpose: The three answers, in the order they are drawn and selected.
    /// Contract: called by `draw_approval`, `draw`'s decision height and `handle_key`'s menu arms.
    /// The three answers, in the order they are drawn.
    fn options() -> [(&'static str, bool, loong_schema::generated::ApprovalScope); 3] {
        use loong_schema::generated::ApprovalScope;
        [
            ("本次允许", true, ApprovalScope::Once),
            ("本项目允许", true, ApprovalScope::Project),
            ("拒绝", false, ApprovalScope::Once),
        ]
    }

    /// Purpose: The transcript rows for a window of this height, including the live preview, the scroll position and a followed fold.
    /// Serves: `App::draw` and the scrolling tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw` and the tests.
    /// Down: `App::live_lines`, `App::max_scroll`, `App::transcript_line` and `styled`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: The answer being written this moment, as lines; empty once it settles into the transcript.
    /// Contract: called by `visible_lines` and `max_scroll`; splits `streaming`.
    /// The answer being written this moment, as lines. Empty once it settles into the
    /// transcript, which is the only copy that is recorded.
    fn live_lines(&self) -> Vec<&str> {
        if self.streaming.is_empty() {
            Vec::new()
        } else {
            self.streaming.split('\n').collect()
        }
    }

    /// Purpose: How far back the view can go with a window this tall, capped so a short transcript cannot scroll.
    /// Contract: called by `visible_lines` and `draw`; reads `lines` and `live_lines`.
    /// How far back the view can go with a window this tall: everything above it. A
    /// transcript shorter than the window has nothing above it, so the answer is zero.
    fn max_scroll(&self, rows: usize) -> usize {
        (self.lines.len() + self.live_lines().len()).saturating_sub(rows.max(1))
    }

    /// Purpose: One transcript row with its fold marker and keyboard highlight added, leaving the record itself unchanged.
    /// Serves: `App::visible_lines`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::visible_lines`.
    /// Down: `styled`, `style_for` and `loong_tui::is_fold_marker`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: The warning that stays in front of the prompt title while approval prompts are off.
    /// Contract: called by `draw`'s title arms; reads `App::yolo`.
    fn yolo_mark(&self) -> &'static str {
        if self.yolo {
            "⚠ 免确认 · "
        } else {
            ""
        }
    }

    /// Purpose: The prompt title's colour: red while approvals are off, otherwise whatever the status asked for.
    /// Contract: called by `draw`'s title arms; reads `App::yolo`.
    fn title_style(&self, status: Style) -> Style {
        if self.yolo {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        } else {
            status
        }
    }

    /// Purpose: Draw the whole screen: model bar, mark, notes, transcript, decision, picker, input and key hints.
    /// Serves: `surface::run`'s loop and every drawing test.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `surface::run` and the tests.
    /// Down: `draw_model_bar`, `draw_banner`, `draw_approval`, `draw_picker`, `draw_context`, `visible_lines`, `wrapped_input` and `spinner_frame`.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn draw(&mut self, frame: &mut Frame) {
        // The context panel takes the right third; the page keeps the rest, so turning it
        // on never hides what you were reading.
        let (conversation, panel) = if self.debug {
            let split = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
                .split(frame.area());
            (split[0], Some(split[1]))
        } else {
            (frame.area(), None)
        };
        // Two fixed bars and one page: the status on top, the keys at the bottom, and
        // everything else — the mark, the conversation, the question, the menus, the
        // prompt — in between, scrolling as one document.
        let areas = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                // The status bar and the rule under it: the chrome says where it ends, so
                // the first line of the page is not read as part of the status.
                Constraint::Length(2),
                Constraint::Min(3),
                Constraint::Length(1),
            ])
            .split(conversation);
        let (top, view, keys) = (areas[0], areas[1], areas[2]);
        let bar = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(top);
        let (status, rule) = (bar[0], bar[1]);
        self.draw_status_rule(frame, rule);

        if self.models.is_empty() {
            frame.render_widget(
                Paragraph::new(format!(
                    " loongagent — 会话 {}（协议 {}）",
                    self.session, self.schema
                ))
                .style(MARK_QUIET),
                status,
            );
        } else {
            self.draw_model_bar(frame, status);
        }

        // An open fold keeps its title: the output itself scrolls, but what it is and how
        // to leave it must stay on screen, which is what the transcript's title bar used to
        // do before there was one page.
        let view = match &self.open {
            Some(open) => {
                let split = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Length(1), Constraint::Min(1)])
                    .split(view);
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!(
                            "· 工具结果（{} 行）— esc 关闭，pgup/pgdn 滚动",
                            open.lines.len()
                        ),
                        MARK_QUIET,
                    )),
                    split[0],
                );
                split[1]
            }
            None => view,
        };
        let width = view.width;
        let input_rows = self.input_rows(width.saturating_sub(2)) as usize;
        // The prompt and the panels that belong to it are fixed to the bottom: what is
        // being typed must always be visible, and scrolling moves only the page above it.
        let decision_height = if self.pending.is_some() {
            Self::decision_rows(view.height)
        } else {
            0
        };
        let menu_height = if self.command_rows() == 0 {
            0
        } else {
            self.command_rows() + 2
        };
        let picker_height = if self.picker_rows() == 0 {
            0
        } else {
            self.picker_rows() + 2
        };
        let bottom_height = decision_height + menu_height + picker_height + input_rows as u16 + 2;
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(bottom_height.min(view.height.saturating_sub(3))),
            ])
            .split(view);
        let (view, bottom) = (split[0], split[1]);

        // The page: the mark (or the opening picture) and the transcript. Nothing else —
        // the question, the menus and the prompt live below it, out of the scroll.
        let splash = if self.lines.is_empty() {
            self.splash_art(frame.area())
        } else {
            None
        };
        // Gated on the whole window, not on the page: the question is whether the screen
        // can afford an eleven-row mark, and the prompt's fixed rows are part of the screen.
        let prefix: Vec<Line> = if !self.logo || !self.show_banner(conversation) {
            Vec::new()
        } else if let Some((art, columns)) = splash {
            self.splash_lines(width, art, columns)
        } else {
            self.banner_lines(width)
        };
        let body_rows = match &self.open {
            Some(open) => open.lines.len(),
            None => self.lines.len() + self.live_lines().len(),
        };
        let decision = if decision_height > 0 {
            self.approval_lines(width, decision_height)
        } else {
            Vec::new()
        };
        let menu = if menu_height > 0 {
            self.command_lines(width)
        } else {
            Vec::new()
        };
        let picker = if picker_height > 0 {
            self.picker_lines(width)
        } else {
            Vec::new()
        };
        let (input, caret_row, caret_column) = self.input_lines(width, input_rows);

        let total = prefix.len() + body_rows;
        // Sticky scroll: while the reader is away from the bottom, content that arrives
        // must not push the page. Adding the growth to the offset keeps the same rows on
        // screen — which is what makes selecting a long block possible while a turn runs.
        if self.scroll > 0 && total > self.scroll_total {
            self.scroll += total - self.scroll_total;
        }
        self.scroll_total = total;
        let rows = view.height as usize;
        // A page shorter than the window has nothing above it: pressing up must not leave
        // an offset that lies about what is on screen.
        self.scroll = self.scroll.min(total.saturating_sub(rows));
        let scroll = self.scroll;
        let first = total.saturating_sub(rows).saturating_sub(scroll);
        let last = (first + rows).min(total);
        let shown = first + 1;

        let body_start = prefix.len();
        let mut page: Vec<Line> = Vec::with_capacity(last - first);
        for at in first..last {
            if at < body_start {
                page.push(prefix[at].clone());
            } else {
                let index = at - body_start;
                page.push(match &self.open {
                    Some(open) => styled(&open.lines[index]),
                    None if index < self.lines.len() => self.transcript_line(index),
                    None => {
                        let live = self.live_lines();
                        live_line(live[index - self.lines.len()])
                    }
                });
            }
        }
        frame.render_widget(Paragraph::new(page), view);

        // The bottom stack: the question, then the menus, then the prompt — the answer
        // nearest the line it is typed into.
        let mut tail = decision;
        tail.extend(menu);
        tail.extend(picker);
        tail.extend(input);
        frame.render_widget(Paragraph::new(tail), bottom);

        // The prompt does not scroll, so the caret is wherever the prompt is.
        if self.pending.is_none() {
            frame.set_cursor_position((
                bottom.x + 1 + caret_column,
                bottom.y + decision_height + menu_height + picker_height + 1 + caret_row,
            ));
        }

        if let Some(panel) = panel {
            self.draw_context(frame, panel);
        }

        let mut hints = if self.open.is_some() {
            vec![
                Span::styled("esc", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" 关闭  "),
                Span::styled("pgup/pgdn", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" 滚动"),
            ]
        } else {
            vec![
                Span::styled("enter", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" 发送  "),
                Span::styled("pgup/pgdn", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" 滚动  "),
                Span::styled("f2", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" 上下文  "),
                Span::styled(
                    "ctrl-pgup/pgdn",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" 记录  "),
                Span::styled(
                    if self.yolo { "⚠ 免确认" } else { "" },
                    Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                ),
                Span::raw(if self.yolo { "  " } else { "" }),
                Span::styled("ctrl-c", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(if self.busy { " 中断" } else { " 退出" }),
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
                " {} 处折叠{}",
                folds.len(),
                if self.fold.is_some() {
                    " · 回车展开 · esc 放开"
                } else {
                    ""
                }
            )));
        }
        if self.scroll > 0 {
            hints.push(Span::raw(format!("  [{}/{}]", shown, total)));
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
                        format!("记忆 {}", context.memory_mode),
                        MARK_QUIET,
                    ));
                } else {
                    hints.push(Span::styled(
                        "记忆 ⚠ 降级",
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

/// Purpose: Draw the layered memory's mode, layer counts, worker statistics and every degradation, keeping off distinct from not-yet-produced.
/// Serves: `draw_context`.
/// Design: `docs/context/status.md` §11.1; `docs/design/surfaces.md` §3.
/// Up: `App::draw_context`.
/// Down: no call; it builds the panel's lines from `ContextExplain`.
/// Verify: `cargo test -p loong-tui-app`.
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
            Span::styled("记忆  ", MARK_QUIET),
            Span::styled(
                "关",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  没有分层记忆；压缩直接丢弃旧对话", MARK_QUIET),
        ]));
        return lines;
    }

    let summarizer = if explained.memory_summarizer.is_empty() {
        "无（记录模式）".to_string()
    } else {
        explained.memory_summarizer.clone()
    };
    lines.push(Line::from(vec![
        Span::styled("记忆  ", MARK_QUIET),
        Span::styled(
            explained.memory_mode.clone(),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  摘要器 {summarizer}"), MARK_QUIET),
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
            layers.push(format!("{label} {}（{} 令牌）", layer.count, layer.tokens));
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
        spans.push(Span::styled("  · 还没有 L4 图谱", MARK_QUIET));
    }
    lines.push(Line::from(spans));

    let worker = &explained.memory_worker;
    lines.push(
        Line::from(format!(
            "        后台 {} · {} 次调用 · {} 条已整理 · {} 条待处理 · {} 条延后",
            worker.state, worker.model_calls, worker.enriched, worker.pending, worker.deferrals
        ))
        .style(MARK_QUIET),
    );

    // Degradation is never optional. The engine-level reason and the worker's last
    // error are both shown when they are different facts.
    if !explained.memory_degraded.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("        降级：", Style::default().fg(Color::Yellow)),
            Span::styled(
                explained.memory_degraded.clone(),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    }
    if !worker.last_error.is_empty() && !explained.memory_degraded.contains(&worker.last_error) {
        lines.push(Line::from(vec![
            Span::styled("        上次错误：", Style::default().fg(Color::Yellow)),
            Span::styled(
                worker.last_error.clone(),
                Style::default().fg(Color::Yellow),
            ),
        ]));
    } else if worker.failed > 0 && explained.memory_degraded.is_empty() {
        lines.push(Line::from(vec![Span::styled(
            format!("        {} 条整理失败", worker.failed),
            Style::default().fg(Color::Yellow),
        )]));
    }
    lines
}

mod mark;
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
