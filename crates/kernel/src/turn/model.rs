//! Purpose: Which model runs the session: list the offerings, switch, record the choice, and describe what a status bar needs.
//!
//! Serves: a person choosing a model and a router choosing per step, both leaving the
//! same auditable `model.selected` fact (ADR-0008).
//!
//! # Contract
//! - **Design:** `docs/design/kernel-loop.md` §2; `docs/design/config-routing.md`
//!   owns the router and the model list.
//! - **Upstream:** `crates/server`'s `model.list` and `model.select`; the turn tests.
//! - **Downstream:** `Router`, `Kernel.selected_model`, `Ledger::total_tokens` and
//!   `Kernel::record_model_selected`.
//! - **Verify:** `cargo test -p loong-kernel turn::tests::the_model_list_is_the_profile_order_and_a_person_can_choose_from_it` · `./scripts/check.sh`.

use super::*;

impl Kernel {
    /// Purpose: The models this session can switch to, in the profile's declaration order.
    ///
    /// Serves: a status bar numbering models the way the profile ranked them, not alphabetically.
    /// Design: `docs/design/config-routing.md` §2.
    /// Up: `Kernel::model_list`; the turn tests.
    /// Down: `Kernel.models` and `Profile.models`; no fallible callee.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_model_list_is_the_profile_order_and_a_person_can_choose_from_it`.
    /// What the next request will actually carry, and why — the debug view behind a
    /// "context" panel. It changes nothing: no event, no ledger movement.
    /// The models this session can switch to, in the order the configuration declared
    /// them.
    ///
    /// Declaration order, **not** the map's: that order is the profile's, which is the
    /// user's own ranking from least to most capable, and it is what a status bar
    /// counts when it labels a model `1`, `2`, `3`. Sorting these alphabetically
    /// would silently renumber everybody's keyboard.
    pub fn model_names(&self) -> Vec<String> {
        let declared: Vec<String> = self
            .config
            .settings
            .models
            .iter()
            .map(|spec| spec.name.clone())
            .filter(|name| self.models.contains_key(name))
            .collect();
        if declared.is_empty() {
            // No declaration to go on (a bare `--model`, or a session opened without a
            // profile): fall back to whatever exists, so the list is never empty when
            // models are.
            return self.models.keys().cloned().collect();
        }
        // A model built but not declared (nothing does this today) would otherwise be
        // unreachable; appending keeps it switchable rather than invisible.
        let mut names = declared;
        for name in self.models.keys() {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    /// Purpose: The model currently running the session.
    ///
    /// Serves: a surface showing what a switch changed.
    /// Design: `docs/design/config-routing.md` §2.
    /// Up: `crates/server`'s `model.list` answer; the turn tests.
    /// Down: `Kernel.selected_model`.
    /// Verify: `cargo test -p loong-kernel turn::tests::choosing_the_model_already_running_records_nothing`.
    /// What the session is running, which is what a switch changes.
    pub fn selected_model(&self) -> &str {
        &self.selected_model
    }

    /// Purpose: The model offerings a status bar draws: name, provider, window and wire name.
    ///
    /// Serves: one list that agrees with what routing can actually pick.
    /// Design: `docs/design/config-routing.md` §2.
    /// Up: `crates/server`'s `model.list` and `model.select` answers.
    /// Down: `model_names` and `Profile.models`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_model_list_is_the_profile_order_and_a_person_can_choose_from_it`.
    /// Everything a status bar needs about the models on offer.
    pub fn model_list(&self) -> Vec<gen::ModelOffering> {
        let declared: std::collections::BTreeMap<&str, &loong_config::ModelSpec> = self
            .config
            .settings
            .models
            .iter()
            .map(|spec| (spec.name.as_str(), spec))
            .collect();
        self.model_names()
            .into_iter()
            .map(|name| {
                let spec = declared.get(name.as_str());
                gen::ModelOffering {
                    model: spec
                        .map(|s| s.model.clone())
                        .unwrap_or_else(|| name.clone()),
                    provider: spec.and_then(|s| s.provider.clone()),
                    context_window: spec.and_then(|s| s.max_context_size),
                    name,
                }
            })
            .collect()
    }

    /// Purpose: Switch the session's model, recording the choice and making it the router's default.
    ///
    /// Serves: a person's choice that survives the next turn and is visible in the log.
    /// Design: `docs/design/kernel-loop.md` §2; `docs/design/config-routing.md` §2.
    /// Up: `crates/server`'s `model.select` handler; the CLI/TUI through the server; the turn tests.
    /// Down: `record_model_selected`, `Router.default` and `model_names`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_model_list_is_the_profile_order_and_a_person_can_choose_from_it`.
    /// Switch the model this session uses, from now on.
    ///
    /// Recorded as a `model.selected` event with the same shape a routing rule writes,
    /// so "why this model?" is answered the same way whether a rule chose it or a
    /// person did. An unknown name is refused rather than stored — a session that
    /// silently kept its old model while reporting a new one would be worse than an
    /// error.
    pub fn select_model(&mut self, name: &str) -> Result<(), KernelError> {
        if !self.models.contains_key(name) {
            return Err(KernelError::OutOfOrder(format!(
                "no model named {name:?} in this session (it has: {})",
                self.model_names().join(", ")
            )));
        }
        if name == self.selected_model {
            return Ok(());
        }
        let facts = loong_config::Facts {
            context_tokens: self.ledger.total_tokens(),
            step: 0,
        };
        self.record_model_selected(name, Some("chosen by the user".to_string()), &facts)?;
        // The router's `default` **is** "the model a turn starts with" — that is what
        // `Profile::default_model` says, and what routing reads on every call. So a
        // person's choice has to become it. Without this the next call would route
        // straight back to whatever the profile declared and the switch would appear to
        // have done nothing, which is the worst possible outcome for a control somebody
        // has just used. Rules still outrank it, so an escalation still fires.
        self.router.default = Some(name.to_string());
        Ok(())
    }
}
