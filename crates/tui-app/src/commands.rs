//! Purpose: The slash commands this screen answers itself: their names, aliases and one-line meanings, and the prefix match behind the command menu.
//!
//! # Contract
//! - **Serves:** `App::surface_command`, `App::command_candidates`, `App::draw_commands` and the command-menu tests.
//! - **Design:** `docs/design/surfaces.md` §3.
//! - **Upstream:** `crates/tui-app/src/lib.rs` reads the table and the matches.
//! - **Downstream:** no crate; every name here is dispatched by `App::surface_command`.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
//!
//! One table, three readers: the menu that lists the commands, the completion that
//! writes a name into the prompt, and `surface_command`, which decides what a name
//! means. Keeping them on one table is why an alias cannot exist in the menu and be
//! unknown to the dispatcher — the drift that made `/qui` look like a command while
//! enter sent it to the model.

/// Purpose: One command the screen answers itself: its name, aliases and one-line meaning.
///
/// Serves: the command menu, completion and `App::surface_command`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `COMMANDS`.
/// Down: `Command::name`, `aliases` and `description`.
/// Verify: `cargo test -p loong-tui-app`.
#[derive(Debug, Clone, Copy)]
pub struct Command {
    /// The name the menu shows and completes to, with its slash.
    pub name: &'static str,
    /// Other spellings that mean the same command, each with its slash.
    pub aliases: &'static [&'static str],
    /// What it does, in one line: the menu's second column and the prompt title
    /// when the typed text only prefixes it.
    pub description: &'static str,
}

/// Every command this surface answers itself, in menu order.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "/new",
        aliases: &["/clear", "/reset"],
        description: "starts a new session — this one is kept, its context is not",
    },
    Command {
        name: "/exit",
        aliases: &["/quit", "/q"],
        description: "leaves loongagent (ctrl-c does too)",
    },
    Command {
        name: "/yolo",
        aliases: &[],
        description: "turns approval prompts on or off (the sandbox is untouched)",
    },
    Command {
        name: "/expand",
        aliases: &[],
        description: "shows the last tool result in full",
    },
    Command {
        name: "/copy",
        aliases: &[],
        description: "copies the whole transcript",
    },
    Command {
        name: "/logo",
        aliases: &[],
        description: "shows or hides the mark",
    },
    Command {
        name: "/mouse",
        aliases: &[],
        description: "turns mouse tracking on or off (shift selects)",
    },
    Command {
        name: "/r",
        aliases: &[],
        description: "asks the last prompt again, of the current model",
    },
];

/// Purpose: The command this exact text runs, if the screen answers it itself.
///
/// Serves: `is_exact`, `matches` and `App::surface_command`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `App::surface_command`, `is_exact` and `matches`.
/// Down: `COMMANDS`.
/// Verify: `cargo test -p loong-tui-app`.
pub fn primary(typed: &str) -> Option<&'static str> {
    let typed = typed.trim().to_ascii_lowercase();
    COMMANDS
        .iter()
        .find(|command| {
            command.name == typed || command.aliases.iter().any(|alias| *alias == typed)
        })
        .map(|command| command.name)
}

/// Purpose: Whether this exact text is one of the screen's commands — a name, an alias, or a `/1`..`/9` model slot.
///
/// Serves: `handle_key`'s enter arm, which tells "run it" from "complete it".
/// Design: `docs/design/surfaces.md` §3.
/// Up: `App::handle_key`.
/// Down: `primary` and `slot`.
/// Verify: `cargo test -p loong-tui-app`.
pub fn is_exact(typed: &str) -> bool {
    primary(typed).is_some() || slot(typed).is_some()
}

/// Purpose: The number of the model slot the text names, if it names one exactly (`/1`..`/9`).
///
/// Serves: `is_exact`, `matches` and `App::surface_command`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `is_exact`, `matches` and `App::surface_command`.
/// Down: no callee; it reads only the text.
/// Verify: `cargo test -p loong-tui-app`.
pub fn slot(typed: &str) -> Option<u8> {
    let rest = typed.trim().strip_prefix('/')?;
    let mut chars = rest.chars();
    let digit = chars.next()?.to_digit(10)?;
    if chars.next().is_some() || !(1..=9).contains(&digit) {
        return None;
    }
    Some(digit as u8)
}

/// Purpose: One row of the command menu: what choosing it writes, and what it does.
///
/// Serves: `App::command_candidates` and the menu drawing.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `matches`.
/// Down: `Candidate::insert` and `description`.
/// Verify: `cargo test -p loong-tui-app`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What choosing it writes into the prompt.
    pub insert: String,
    /// What it does, shown beside it.
    pub description: &'static str,
}

/// Purpose: The commands whose name or an alias starts with the text, in menu order, plus the model slot the text names.
///
/// Serves: `App::command_candidates`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `App::command_candidates`.
/// Down: `primary`, `slot` and `COMMANDS`.
/// Verify: `cargo test -p loong-tui-app`.
pub fn matches(typed: &str) -> Vec<Candidate> {
    let typed = typed.trim().to_ascii_lowercase();
    if !typed.starts_with('/') {
        return Vec::new();
    }
    // `/r` is both a command and the first letters of `/reset`, so an exact name
    // must offer itself alone: the alias rows it also prefixes are not what the
    // person meant, and a menu that showed them would be answering a question
    // nobody asked.
    if let Some(name) = primary(&typed) {
        return COMMANDS
            .iter()
            .filter(|command| command.name == name)
            .map(|command| Candidate {
                insert: command.name.to_string(),
                description: command.description,
            })
            .collect();
    }
    let mut found = Vec::new();
    // The slot shortcut is a family, not a table row: `/1`..`/9` each name a model,
    // and a row per slot would crowd the real commands out of the menu.
    if let Some(slot) = slot(&typed) {
        found.push(Candidate {
            insert: format!("/{slot}"),
            description: "switch to that model slot",
        });
    }
    found.extend(COMMANDS.iter().filter_map(|command| {
        let prefixed = command.name.starts_with(&typed)
            || command
                .aliases
                .iter()
                .any(|alias| alias.starts_with(&typed));
        prefixed.then(|| Candidate {
            insert: command.name.to_string(),
            description: command.description,
        })
    }));
    found
}
