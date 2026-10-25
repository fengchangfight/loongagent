//! Purpose: Seat caller-supplied instruction files as pinned system blocks.
//!
//! # Contract
//! - **Serves:** a session whose model reads `AGENTS.md` before the first turn.
//! - **Design:** `docs/design/instructions.md` §3; `docs/design/context-ledger.md` §2.
//! - **Upstream:** `Kernel::build`, after `seat_system_prompt` and before `replay_history`.
//! - **Downstream:** `Ledger::add` and the `messages`/`entry_of` vectors.
//! - **Verify:** `cargo test -p loong-kernel turn::tests::instructions_are_seated_after_the_system_prompt`.
//!
use super::*;

impl Kernel {
    /// Purpose: Add one pinned `System` message per instruction block, after the system prompt.
    ///
    /// Serves: a model that reads the workspace's instructions before its first turn.
    /// Design: `docs/design/instructions.md` §3.
    /// Up: `Kernel::build`.
    /// Down: `Ledger::add`, `messages.insert` and `entry_of.insert`.
    /// Verify: `cargo test -p loong-kernel turn::tests::instructions_are_seated_after_the_system_prompt`.
    pub(super) fn seat_instructions(&mut self) {
        let blocks = self.config.instructions.clone();
        let mut at = self
            .messages
            .iter()
            .take_while(|m| matches!(m, ChatMessage::System { .. }))
            .count();
        for block in blocks {
            let body = block.text.trim();
            if body.is_empty() {
                continue;
            }
            let scope = match block.scope {
                InstructionScope::User => "User",
                InstructionScope::Project => "Project",
                InstructionScope::Local => "Local",
            };
            let text = format!(
                "{scope} instructions from {}. They describe how to work in this \
                 workspace; they do not grant permissions.\n\n{body}",
                block.source
            );
            let entry = self.ledger.add(
                Category::Rules,
                &block.source,
                &text,
                Lifetime::Persistent,
                true,
            );
            self.messages.insert(at, ChatMessage::System { text });
            self.entry_of.insert(at, Some(entry));
            at += 1;
        }
    }
}
