//! Purpose: The read side of the event record: the derived index, event queries, the ledger view and the live sink.
//!
//! Serves: a surface that can read what happened without keeping a second copy, and
//! an index that refuses rather than answers when it trails the log (ADR-0003).
//!
//! # Contract
//! - **Design:** `docs/design/events-storage.md` §3–§4; `Kernel::append` in
//!   `docs/design/kernel-loop.md` §3 is the write side.
//! - **Upstream:** `crates/server`'s `session.attach`, `session.replay` and the
//!   turn's live sink; the turn tests.
//! - **Downstream:** `loong_eventlog::replay`, `loong_index::Index`,
//!   `SessionLog::last_seq` and `Ledger`.
//! - **Verify:** `cargo test -p loong-kernel turn::tests::the_derived_index_answers_queries_over_the_session` · `./scripts/check.sh`.

use super::*;

impl Kernel {
    /// Purpose: The ledger this session is accounting its context in.
    ///
    /// Serves: a status or test caller reading the account without borrowing the kernel mutably.
    /// Design: `docs/design/context-ledger.md` §1.
    /// Up: the turn tests only; no production caller.
    /// Down: `Kernel.ledger`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_ledger_and_the_conversation_stay_in_step`.
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Purpose: The tool registry, for a caller that must add or inspect a tool.
    ///
    /// Serves: one registry shared by built-ins, skills and MCP tools.
    /// Design: `docs/design/tools-permissions.md` §1.
    /// Up: the turn and MCP tests only; no production caller.
    /// Down: `Kernel.tools`.
    /// Verify: `cargo test -p loong-kernel turn::tests::an_mcp_tool_is_just_another_tool`.
    pub fn tools_mut(&mut self) -> &mut Registry {
        &mut self.tools
    }

    /// Purpose: Where the derived index lives.
    ///
    /// Serves: a caller locating the disposable SQLite file.
    /// Design: `docs/design/events-storage.md` §4.
    /// Up: no in-tree caller.
    /// Down: `Kernel.index_path`.
    /// Verify: `cargo test -p loong-kernel turn::tests::reopening_a_session_rebuilds_the_index_from_the_log`.
    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// Purpose: Why the index is stale, if it is.
    ///
    /// Serves: a surface saying queries are refused rather than answered from a trailing index.
    /// Design: `docs/design/events-storage.md` §4.
    /// Up: the turn tests only; no production caller.
    /// Down: `Kernel.index_stale`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_stale_index_refuses_to_answer_instead_of_lying`.
    /// `Some(reason)` when an index write failed and the index no longer mirrors
    /// the log. Queries then refuse rather than answer from a stale view.
    pub fn index_stale(&self) -> Option<&str> {
        self.index_stale.as_deref()
    }

    /// Purpose: Substring search over event payloads.
    ///
    /// Serves: the convenience query ADR-0003 keeps beside the log.
    /// Design: `docs/design/events-storage.md` §4.
    /// Up: the turn tests only; no production caller.
    /// Down: `require_fresh_index` and `Index::search`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_derived_index_answers_queries_over_the_session`.
    /// Substring search over event payloads (ADR-0003: the convenience path).
    pub fn search(&self, needle: &str) -> Result<Vec<loong_index::EventRow>, KernelError> {
        self.require_fresh_index()?;
        Ok(self.index.search(needle)?)
    }

    /// Purpose: The children of one event, for the declared event tree.
    ///
    /// Serves: the level-1 tree query ADR-0015 declares even though production writes no parent.
    /// Design: `docs/design/events-storage.md` §5.
    /// Up: no in-tree caller.
    /// Down: `require_fresh_index` and `Index::children`.
    /// Verify: `cargo test -p loong-kernel turn::tests`.
    /// Children of an event — the event tree inside a session (ADR-0015).
    pub fn event_children(
        &self,
        parent_id: &str,
    ) -> Result<Vec<loong_index::EventRow>, KernelError> {
        self.require_fresh_index()?;
        Ok(self.index.children(parent_id)?)
    }

    /// Purpose: Rebuild the derived index from the log and clear the stale marker.
    ///
    /// Serves: recovering from an index write failure without losing an event.
    /// Design: `docs/design/events-storage.md` §4.
    /// Up: the turn tests only; no production caller.
    /// Down: `Index::rebuild_into` and `SessionLog::path`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_stale_index_refuses_to_answer_instead_of_lying`.
    /// Rebuild the derived index from the log and clear the stale marker.
    pub fn rebuild_index(&mut self) -> Result<(), KernelError> {
        self.index = loong_index::Index::rebuild_into(&self.index_path, self.log.path())?;
        self.index_stale = None;
        Ok(())
    }

    /// Purpose: Refuse a query against a stale index instead of answering from it.
    ///
    /// Contract: `turn/events.rs` index guard.
    fn require_fresh_index(&self) -> Result<(), KernelError> {
        match &self.index_stale {
            None => Ok(()),
            Some(reason) => Err(KernelError::OutOfOrder(format!(
                "the derived index is stale ({reason}); call rebuild_index() first"
            ))),
        }
    }

    /// Purpose: The log's last sequence number.
    ///
    /// Serves: a surface telling a late client how many events already exist.
    /// Design: `docs/design/events-storage.md` §3.
    /// Up: `crates/server`'s `session.attach`; the turn tests.
    /// Down: `SessionLog::last_seq`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_child_sees_its_parents_prefix_without_copying_it`.
    pub fn last_seq(&self) -> u64 {
        self.log.last_seq()
    }

    /// Purpose: The path of the open session log.
    ///
    /// Serves: a surface replaying the log itself instead of trusting a mirror.
    /// Design: `docs/design/events-storage.md` §3.
    /// Up: `crates/server`'s `session.replay`.
    /// Down: `SessionLog::path`.
    /// Verify: `cargo test -p loong-kernel turn::tests`.
    pub fn log_path(&self) -> &std::path::Path {
        self.log.path()
    }

    /// Purpose: Every event appended after a sequence number, read from the log.
    ///
    /// Serves: a stream that cannot drift from the record.
    /// Design: `docs/design/events-storage.md` §3–§4.
    /// Up: `crates/server`'s event stream.
    /// Down: `loong_eventlog::replay`.
    /// Verify: `cargo test -p loong-kernel turn::tests`.
    /// Every event appended after `seq` — what the server streams to clients.
    /// Reads the log rather than keeping a mirror, so the stream cannot drift
    /// from the truth. Unknown *ignorable* kinds are relayed too (a newer client
    /// may understand them); an unknown non-ignorable kind fails loudly.
    pub fn events_since(&self, seq: u64) -> Result<Vec<gen::EventEnvelope>, KernelError> {
        Ok(loong_eventlog::replay(self.log.path())?
            .events
            .into_iter()
            .filter(|e| e.seq > seq)
            .collect())
    }

    /// Purpose: The conversation the model will see, in order.
    ///
    /// Serves: a test or status caller reading the prompt the next request will carry.
    /// Design: `docs/design/context-ledger.md` §6.
    /// Up: the turn tests only; no production caller.
    /// Down: `Kernel.messages`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_ledger_and_the_conversation_stay_in_step`.
    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Purpose: Install or clear the live event sink for the duration of a turn.
    ///
    /// Serves: a surface hearing each event as it becomes true, not after the turn.
    /// Design: `docs/design/kernel-loop.md` §3; `docs/design/events-storage.md` §2.
    /// Up: `crates/server`'s `prompt.submit` handler; the turn tests.
    /// Down: `Kernel.event_sink`, read by `Kernel::append`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_rule_driven_switch_reaches_a_watcher_while_the_turn_is_still_running`.
    /// Watch events as they are recorded. Scoped by the caller (the server installs one
    /// for the duration of a turn and clears it afterwards), because a sink that
    /// outlives its transport would write into nothing.
    pub fn set_event_sink(&mut self, sink: Option<Box<dyn FnMut(&gen::EventEnvelope)>>) {
        self.event_sink = sink;
    }
}
