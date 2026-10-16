use super::*;

impl Kernel {
    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn tools_mut(&mut self) -> &mut Registry {
        &mut self.tools
    }

    pub fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// `Some(reason)` when an index write failed and the index no longer mirrors
    /// the log. Queries then refuse rather than answer from a stale view.
    pub fn index_stale(&self) -> Option<&str> {
        self.index_stale.as_deref()
    }

    /// Substring search over event payloads (ADR-0003: the convenience path).
    pub fn search(&self, needle: &str) -> Result<Vec<loong_index::EventRow>, KernelError> {
        self.require_fresh_index()?;
        Ok(self.index.search(needle)?)
    }

    /// Children of an event — the event tree inside a session (ADR-0015).
    pub fn event_children(
        &self,
        parent_id: &str,
    ) -> Result<Vec<loong_index::EventRow>, KernelError> {
        self.require_fresh_index()?;
        Ok(self.index.children(parent_id)?)
    }

    /// Rebuild the derived index from the log and clear the stale marker.
    pub fn rebuild_index(&mut self) -> Result<(), KernelError> {
        self.index = loong_index::Index::rebuild_into(&self.index_path, self.log.path())?;
        self.index_stale = None;
        Ok(())
    }

    fn require_fresh_index(&self) -> Result<(), KernelError> {
        match &self.index_stale {
            None => Ok(()),
            Some(reason) => Err(KernelError::OutOfOrder(format!(
                "the derived index is stale ({reason}); call rebuild_index() first"
            ))),
        }
    }

    pub fn last_seq(&self) -> u64 {
        self.log.last_seq()
    }

    pub fn log_path(&self) -> &std::path::Path {
        self.log.path()
    }

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

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// Watch events as they are recorded. Scoped by the caller (the server installs one
    /// for the duration of a turn and clears it afterwards), because a sink that
    /// outlives its transport would write into nothing.
    pub fn set_event_sink(&mut self, sink: Option<Box<dyn FnMut(&gen::EventEnvelope)>>) {
        self.event_sink = sink;
    }
}
