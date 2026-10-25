//! Purpose: The local first-run wizard: one provider, one API key, and the models the vendor answers with.
//!
//! # Contract
//! - **Serves:** `crates/cli/src/local.rs`'s first run, through `local_wizard` and the choice types re-exported here.
//! - **Design:** `docs/design/surfaces.md` §3 and §6.
//! - **Upstream:** `crates/cli/src/local.rs` calls `local_wizard` and builds `ProviderChoice`, `Validation` and `OfferedKey`.
//! - **Downstream:** `ratatui` for the forms, `setup::full_screen_needs_a_terminal`, and the `MARK_*` palette in this crate.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
use super::*;

/// Purpose: One provider the wizard may be pointed at, passed in so this crate never reads the catalog itself.
/// Serves: `LocalWizard`, `draw_local_wizard` and `crates/cli/src/local.rs`.
/// Design: `docs/design/surfaces.md` §3 and §6.
/// Up: `crates/cli/src/local.rs` builds them; the wizard tests build them.
/// Down: no call; `LocalWizard::{selected, ready, handle_key}` and `draw_local_wizard` read it.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: What one key-validation attempt found: whether it worked, one rendered summary, the models and an ordering hint.
/// Serves: `KeyChoice`, `local_wizard` and `crates/cli/src/local.rs`'s validate callback.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `local_wizard` receives it from the callback; `crates/cli/src/local.rs` builds it.
/// Down: no call; `local_wizard` and `draw_local_wizard` read it.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: A key that passed validation, with the models chosen to keep.
/// Serves: `local_wizard`'s return value and `crates/cli/src/local.rs`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `local_wizard` returns it; `crates/cli/src/local.rs` consumes it.
/// Down: no call; it is data.
/// Verify: `cargo test -p loong-tui-app`.
/// A key that passed validation, and what it turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyChoice {
    pub provider: String,
    pub key: String,
    pub validation: Validation,
}

/// Purpose: One model's background-check result, reported so the bar can mark a failure without removing the model.
/// Serves: `App::note_health` and `crates/cli/src/local.rs`'s health thread.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `crates/cli/src/local.rs` sends it; `surface::run` forwards it; the tests build it.
/// Down: no call; `App::note_health` reads it.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: Which phase the local wizard is in: editing, validating, refused, accepted or choosing models.
/// Serves: `LocalWizard`, `draw_local_wizard` and `LocalWizard::handle_key`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `LocalWizard::handle_key` and `local_wizard` set it; the tests set it to arrange a phase.
/// Down: `handle_key`, `visible`, `chosen` and `draw_local_wizard` match it.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: A key this machine already had, named by its variable and offered rather than typed into the field.
/// Serves: `LocalWizard::{offer, offered_key, effective_key}` and `crates/cli/src/local.rs`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `crates/cli/src/local.rs` builds them; the tests build them.
/// Down: no call; `LocalWizard` reads it.
/// Verify: `cargo test -p loong-tui-app`.
/// The local first run: paste a key, pick who it belongs to, find out.
///
/// A key this machine already had, for one provider.
///
/// **Offered, never taken.** The field is left empty so that typing replaces rather than
/// appends — a prefilled secret is one somebody cannot correct without clearing it first,
/// and an API key is the one string here that cannot be checked by looking at it. `enter`
/// on the empty field takes this; anything typed wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfferedKey {
    pub provider: String,
    /// The variable it came from, so the screen can say where "already had" means.
    pub var: String,
    pub key: String,
}

/// Purpose: The form's state: the typed key, the provider list and choice, the phase and the offered keys.
/// Serves: `draw_local_wizard`, `local_wizard` and the wizard tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `local_wizard` builds it; the tests build it directly.
/// Down: no call; its methods below change it.
/// Verify: `cargo test -p loong-tui-app`.
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
    /// One per provider that has a key in this environment.
    pub offered: Vec<OfferedKey>,
}

/// Purpose: How a turn ended: done, stopped by the person, or failed.
/// Serves: `Finished`, `App::finished` and the prompt title.
/// Design: `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §5 (a stop is not a completion).
/// Up: `App::drain` computes it.
/// Down: `Finished::line` and `Finished::style` match it.
/// Verify: `cargo test -p loong-tui-app`.
/// How a turn ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It ran to a stop on its own.
    Done,
    /// The person asked it to stop.
    Stopped,
    /// It ended in an error.
    Failed,
}

/// Purpose: The last turn's ending and its duration, held on the prompt box until the next task starts.
/// Serves: `App::finished`, the prompt title in `App::draw` and the tick tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `App::drain` builds it; the tests build it.
/// Down: `Finished::line` and `Finished::style`.
/// Verify: `cargo test -p loong-tui-app`.
/// The last turn's ending, held on screen until the next prompt.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Finished {
    /// Absent when the turn never got far enough to be timed.
    pub seconds: Option<f64>,
    pub outcome: Outcome,
}

impl Finished {
    /// Purpose: The one line for this ending, used in both the transcript and the prompt title so the two cannot disagree.
    /// Serves: `App::drain` and `App::draw`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::drain`, `App::draw` and the tick tests.
    /// Down: `took_words`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The one line that says it, used both as the closing transcript line and as the
    /// prompt's title, so the two can never disagree.
    pub fn line(&self) -> String {
        let took = self
            .seconds
            .map(|seconds| format!(" · {}", took_words(seconds)))
            .unwrap_or_default();
        match self.outcome {
            Outcome::Done => format!("✓ 完成{took}"),
            Outcome::Stopped => format!("⏹ 已停止{took}"),
            Outcome::Failed => format!("✗ 失败{took}"),
        }
    }

    /// Purpose: The colour for this ending: green for done, yellow for stopped, red for failed.
    /// Serves: `App::draw`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `App::draw` and `the_tick_says_how_the_turn_ended_and_which_way`.
    /// Down: no call; it maps `Outcome` to a `Style`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Its colour, which is half of what makes it readable at a glance.
    pub fn style(&self) -> Style {
        match self.outcome {
            Outcome::Done => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            Outcome::Stopped => Style::default().fg(Color::Yellow),
            Outcome::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        }
    }
}

/// Purpose: Seconds the way a person says them, as the tick line's duration.
/// Contract: called by `Finished::line`.
/// Seconds the way a person says them.
fn took_words(seconds: f64) -> String {
    if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else {
        format!(
            "{}m{:02}s",
            (seconds / 60.0) as u64,
            (seconds % 60.0) as u64
        )
    }
}

/// Purpose: What the wizard's keyboard asks the event loop to do.
/// Serves: `LocalWizard::handle_key`, `local_wizard` and the wizard tests.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `LocalWizard::handle_key` returns it.
/// Down: `local_wizard` matches it; the tests assert it.
/// Verify: `cargo test -p loong-tui-app`.
#[derive(Debug, PartialEq, Eq)]
pub enum WizardAction {
    None,
    Submit,
    Cancel,
}

impl LocalWizard {
    /// Purpose: An empty editing wizard over the given providers.
    /// Serves: `local_wizard` and the wizard tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `local_wizard` and the tests.
    /// Down: no call; it initialises the `LocalWizard` fields.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn new(providers: Vec<ProviderChoice>) -> Self {
        LocalWizard {
            key: String::new(),
            providers,
            provider: 0,
            state: WizardState::Editing,
            offered: Vec::new(),
        }
    }

    /// Purpose: The provider the caret is on, or nothing for an empty list.
    /// Serves: `ready`, `handle_key`, `draw_local_wizard`, `offered_key` and `local_wizard`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: those methods.
    /// Down: no call; it indexes `providers`.
    /// Verify: `cargo test -p loong-tui-app`.
    pub fn selected(&self) -> Option<&ProviderChoice> {
        self.providers.get(self.provider)
    }

    /// Purpose: Whether the form can be submitted: no key needed, an offer present, or a typed key long enough for a vendor.
    /// Serves: `LocalWizard::handle_key`'s Enter arm and the wizard tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `LocalWizard::handle_key` and the tests.
    /// Down: `selected` and `offered_key`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Whether a key is worth sending anywhere. Not a format check — a length one:
    /// a vendor's own key is at least this long, and an empty field is the only
    /// mistake predictable enough to catch before spending a round trip.
    ///
    /// A provider that wants no secret is always ready: its check is whether the local
    /// server answers, which is a question the probe can ask on its own.
    pub fn ready(&self) -> bool {
        match self.selected() {
            Some(provider) if !provider.needs_key => true,
            // An offer counts as an answer: the key is on this machine and `enter` means
            // use it. The length check is only for something somebody typed.
            _ if self.offered_key().is_some() => true,
            _ => self.key.trim().len() >= 8,
        }
    }

    /// Purpose: The key as the form draws it: a three-character prefix then bullets, never the whole secret.
    /// Serves: `draw_local_wizard` and the never-draws-the-key test.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `draw_local_wizard` and the tests.
    /// Down: no call; it reads `key`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// What the field shows. The whole key is never drawn: the person typing it knows
    /// what they pasted, and a screen is a thing other people can see.
    pub fn masked_key(&self) -> String {
        let chars: Vec<char> = self.key.chars().collect();
        let visible = chars.len().min(3);
        let prefix: String = chars[..visible].iter().collect();
        format!("{prefix}{}", "•".repeat(chars.len() - visible))
    }

    /// Purpose: The picker rows matching the filter, as indices into the full list so ticks stay put.
    /// Serves: `handle_choosing`, `draw_model_picker` and the picker tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `handle_choosing`, `draw_model_picker` and the tests.
    /// Down: no call; it reads `WizardState::Choosing`.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: The models that will be written, in the provider's own order.
    /// Serves: `local_wizard` and the picker tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `local_wizard` and the tests.
    /// Down: no call; it reads the ticks.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: The model picker's keyboard: filter, move, tick and finish, with no command stealing a typeable character.
    /// Serves: `LocalWizard::handle_key`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `LocalWizard::handle_key`.
    /// Down: `LocalWizard::visible` and the `WizardState::Choosing` fields.
    /// Verify: `cargo test -p loong-tui-app`.
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
        // Every key that does something other than produce a character is a key somebody
        // cannot type. That is the rule this screen got wrong twice: `a` toggled
        // everything until somebody tried to filter for `flash`, and *space* toggled the
        // row under the cursor until somebody tried to type two words. Both are ordinary
        // characters in the field being edited, so neither may be a command.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
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
            KeyCode::Up => {
                *cursor = cursor.saturating_sub(1);
            }
            KeyCode::Down => {
                if count > 0 {
                    *cursor = (*cursor + 1).min(count - 1);
                }
            }
            // `Tab` ticks the row and moves on, `shift+tab` ticks and moves back — the
            // gesture people know from every fuzzy finder, and it means a run of models
            // costs one key each instead of two.
            KeyCode::Tab => {
                if let Some(index) = visible.get(*cursor) {
                    ticked[*index] = !ticked[*index];
                }
                if count > 0 {
                    *cursor = (*cursor + 1).min(count - 1);
                }
            }
            KeyCode::BackTab => {
                if let Some(index) = visible.get(*cursor) {
                    ticked[*index] = !ticked[*index];
                }
                *cursor = cursor.saturating_sub(1);
            }
            // `ctrl+a`, the gesture every list has used for "all of them" since before
            // terminals had lists. It is 0x01, so unlike `a` it cannot arrive as text —
            // though `screen` uses it as its prefix, so `Tab` remains the way through.
            KeyCode::Char('a') if ctrl => {
                let any_off = visible.iter().any(|index| !ticked[*index]);
                for index in visible {
                    ticked[index] = any_off;
                }
            }
            KeyCode::Backspace => {
                filter.pop();
                *cursor = 0;
            }
            // Everything else is text, including a space: this is a field somebody types
            // in, and the only characters it refuses are the ones bound above.
            KeyCode::Char(c) => {
                filter.push(c);
                *cursor = 0;
            }
            _ => {}
        }
        WizardAction::None
    }

    /// Purpose: Start on the provider an offered key belongs to, without claiming anything about the key.
    /// Serves: `local_wizard`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `local_wizard`.
    /// Down: no call; it sets `provider` and returns the state to editing.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Start on a particular provider, without claiming anything about its key.
    pub fn preselect(&mut self, provider: &str) {
        if let Some(index) = self.providers.iter().position(|p| p.id == provider) {
            self.provider = index;
        }
        self.state = WizardState::Editing;
    }

    /// Purpose: Remember the keys this machine already has, to offer but never type in.
    /// Serves: `local_wizard` and the wizard tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `local_wizard` and the tests.
    /// Down: no call; it stores `offered`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// Remember keys this machine already had. They are *offered* from here on: named on
    /// the form, used when `enter` is pressed on an empty field, never typed in on
    /// somebody's behalf.
    pub fn offer(&mut self, offered: Vec<OfferedKey>) {
        self.offered = offered;
    }

    /// Purpose: The offered key for the provider now selected, if this machine has one.
    /// Serves: `ready`, `effective_key` and `draw_local_wizard`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: those methods and the tests.
    /// Down: `selected`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The key already on this machine for the provider now selected, if any.
    pub fn offered_key(&self) -> Option<&OfferedKey> {
        let id = self.selected()?.id.as_str();
        self.offered.iter().find(|offer| offer.provider == id)
    }

    /// Purpose: The key that would be used: what was typed, else what was offered.
    /// Serves: `ready` and `local_wizard`.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `local_wizard` and the tests.
    /// Down: `offered_key`.
    /// Verify: `cargo test -p loong-tui-app`.
    /// The key that would be used: what was typed, else what was offered.
    pub fn effective_key(&self) -> String {
        if !self.key.trim().is_empty() {
            return self.key.trim().to_string();
        }
        self.offered_key()
            .map(|offer| offer.key.clone())
            .unwrap_or_default()
    }

    /// Purpose: Float the providers a refusal suggested, keeping the rest in order and never inventing one.
    /// Serves: `local_wizard` after a failed validation.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `local_wizard` and `a_refusal_can_re_rank_the_list_without_asking_anyone_again`.
    /// Down: no call; it reorders `providers` and resets the caret.
    /// Verify: `cargo test -p loong-tui-app`.
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

    /// Purpose: The form's keyboard: type or offer a key, move provider, submit or leave, and freeze while validating.
    /// Serves: `local_wizard` and the wizard tests.
    /// Design: `docs/design/surfaces.md` §3.
    /// Up: `local_wizard` and the tests.
    /// Down: `handle_choosing`, `ready` and `selected`.
    /// Verify: `cargo test -p loong-tui-app`.
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
            // `!ctrl`, because a control chord is a command everywhere else in this program
            // and never text — and in *this* field a stray `ctrl+a` would paste a letter
            // into somebody's API key, which is the one string here that cannot be checked
            // by looking at it. Alt is allowed through: on many layouts AltGr arrives as
            // Alt plus the character it produces.
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
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

/// Purpose: Draw the API-key form, or hand over to the model picker when the vendor answered with models.
/// Serves: `local_wizard`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `local_wizard`.
/// Down: `draw_model_picker`, `masked_key`, `offered_key` and `selected`.
/// Verify: `cargo test -p loong-tui-app`.
pub(super) fn draw_local_wizard(frame: &mut ratatui::Frame, wizard: &LocalWizard) {
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
    let offer = wizard.offered_key();
    // What the field *says* when nothing has been typed: either where a key already on
    // this machine would come from, or that there is nothing. Never a prefilled secret —
    // a masked one looks the same whether it is right or wrong, and cannot be corrected
    // by typing without clearing it first.
    let field = if !wants_key {
        "（本机服务，无需密钥）".to_string()
    } else if wizard.key.is_empty() {
        match offer {
            Some(offer) => format!("回车使用环境变量里的 {}（或在此粘贴你自己的）", offer.var),
            None => String::new(),
        }
    } else {
        wizard.masked_key()
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

/// Purpose: Draw the model list with ticks, a filter field, a scrolling window and the count of what is chosen.
/// Serves: `draw_local_wizard`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `draw_local_wizard`.
/// Down: `LocalWizard::visible` and the mark palette.
/// Verify: `cargo test -p loong-tui-app`.
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

    // The filter leads, because on a vendor that serves forty ids it is the only way to
    // reach the two you want without walking past the other thirty-eight.
    let picked = ticked.iter().any(|t| *t);
    let help = if picked {
        "输入筛选 · tab 勾选 · ctrl+a 全选当前筛选 · enter 写入 · esc 回上一步"
    } else {
        "先输入关键字缩小范围（如 flash、3.7、kimi），再按 tab 勾选"
    };
    frame.render_widget(
        Paragraph::new(help.to_string()).style(if picked {
            MARK_QUIET
        } else {
            Style::default().fg(Color::Red)
        }),
        chunks[3],
    );
}

/// Purpose: What to do after a provider answers: accept when it named nothing, choose when it named models.
/// Serves: `local_wizard`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `local_wizard` compares a validation against it.
/// Down: no call; `after_validation` returns it.
/// Verify: `cargo test -p loong-tui-app`.
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

/// Purpose: The one rule for whether a model list is shown: any named model is shown, none is accepted silently.
/// Serves: `local_wizard`.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `local_wizard`.
/// Down: no call; it returns the enum.
/// Verify: `cargo test -p loong-tui-app`.
/// One model is a choice too.
pub fn after_validation(models: &[(String, Option<u64>)]) -> AfterValidation {
    if models.is_empty() {
        AfterValidation::Accept
    } else {
        AfterValidation::Choose
    }
}

/// Purpose: Run the full-screen first run: ask for a key, validate it, show the models and return the human's choice.
/// Serves: `crates/cli/src/local.rs`'s first run.
/// Design: `docs/design/surfaces.md` §3.
/// Up: `crates/cli/src/local.rs`.
/// Down: `full_screen_needs_a_terminal`, `LocalWizard`, `draw_local_wizard` and `after_validation`.
/// Verify: `cargo test -p loong-tui-app`.
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
    offered: Vec<OfferedKey>,
    validate: &mut dyn FnMut(&str, &str) -> Validation,
) -> Result<Option<KeyChoice>, String> {
    full_screen_needs_a_terminal()?;
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use ratatui::Terminal;

    let mut wizard = LocalWizard::new(providers);
    // A key this machine already had is *offered*, never taken. Start on the provider it
    // belongs to and name the variable on the form; the field itself stays empty, so
    // typing replaces rather than appends to a secret somebody cannot read.
    if let Some(first) = offered.first() {
        wizard.preselect(&first.provider);
    }
    wizard.offer(offered);
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
        // Crossterm's Windows backend reports a Release alongside every Press;
        // acting on both makes one keystroke do the thing twice.
        if let Event::Key(key) = event::read().map_err(|e| e.to_string())? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
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

                    let first = wizard.effective_key();
                    let result = validate(&provider.id, &first);
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
                            key: wizard.effective_key(),
                            validation: result,
                        });
                    }
                    wizard.state = WizardState::Choosing {
                        filter: String::new(),
                        cursor: 0,
                        // **Nothing starts ticked**, because the work should be proportional
                        // to what somebody wants, not to what the vendor happens to serve.
                        // Pre-ticking reads as a kindness on a two-model provider and as
                        // twenty untickings on a real one — and a real one is exactly where
                        // this screen is needed. `a` ticks everything the filter is showing,
                        // so "all of them" is still one keystroke once the list is narrow.
                        ticked: vec![false; result.models.len()],
                        all: result.models.clone(),
                    };
                    // The key and the summary travel with the choice; this screen only
                    // decides which of the models come along.
                    pending = Some((provider.id.clone(), wizard.effective_key(), result));
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
