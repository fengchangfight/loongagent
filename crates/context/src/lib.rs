//! Layered, reversible context memory (ADR-0017).
//!
//! The event log (ADR-0003) is the truth; this crate builds a **derived**,
//! disposable memory over it. Deleting the sidecar database loses nothing but
//! the cost already spent deriving it — [`Store::rebuild`] reconstitutes it from
//! the log alone, and a test asserts exactly that.
//!
//! The design separates two things that are easy to conflate:
//!
//! * the **retrieval layer** — L1 atomic records, entities and keywords. It is
//!   deterministic, needs no model, and always exists. This is what makes
//!   "the original is never lost" true rather than aspirational.
//! * the **comprehension layer** — L2/L3/L4 summaries. It is model-produced,
//!   therefore fallible, therefore always carries citations and never replaces
//!   the record it was derived from.
//!
//! Implemented so far (`docs/06-context-engineering.md` §13):
//!
//! * **P1** — the retrieval layer: [`materialize`] turns an event into an
//!   [`Atom`]; [`Store`] keeps the atoms, entities and keywords, with an ingest
//!   watermark, [`Store::rebuild`] and [`Store::verify`].
//! * **P2** — the deterministic half of the comprehension layer: [`digest`]
//!   segments episodes and computes an L2 summary whose *computable* fields are
//!   computed rather than generated; [`Store::recall`] finds summaries and
//!   [`Store::expand`] hands back the raw evidence alongside one.
//!
//! * **P3** — the background worker: [`tail`] reads only what was appended,
//!   [`Store::sync`] and [`Store::digest_pending`] keep the derived state current
//!   without re-reading the session, and [`worker`] runs one bounded, yielding
//!   step at a time.
//!
//! * **P4** — the maps: [`synthesis`] rolls L2 episodes up into L3 domain maps
//!   and a bounded L4 global map, which the kernel carries in the foreground.
//!
//! * **P5** — the optional local model: [`summarizer`] asks Ollama for the fields a
//!   deterministic pass cannot compute, and [`Store::synthesize_window`] bounds how
//!   much one idle window may ask. Nothing here depends on a model being installed;
//!   without one the memory is complete, just free of opinions.

pub mod digest;
pub mod materialize;
pub mod store;
pub mod summaries;
pub mod summarizer;
pub mod synthesis;
pub mod tail;
pub mod worker;

pub use digest::{estimate_tokens, Digest, Segment, TestRecord};
pub use materialize::{fnv1a64, materialize, Atom, Entity, EntityKind, MAX_TEXT_CHARS};
pub use store::{
    build, Hit, SearchQuery, Store, StoreError, SyncOutcome, VerifyReport, SCHEMA_VERSION,
};
pub use summaries::{
    AtomView, Expansion, Summary, SummaryInput, WindowBudget, WindowOutcome, SUMMARY_SCHEMA_VERSION,
};
pub use summarizer::{
    DigestInput, NoSummarizer, OllamaSummarizer, Summarizer, SummarizerError, Synthesis,
};
pub use synthesis::{domain_of, DomainMap, GlobalMap, DOMAIN_ROLLUP_EPISODES, L4_MAX_TOKENS};
pub use worker::{Budget, Foreground, IdleForeground, Step, WorkerConfig, WorkerStats};
