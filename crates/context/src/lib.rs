//! Purpose: Layered, reversible context memory: a disposable derived store over the event log.
//!
//! Serves: the ADR-0017 promise that the original is never lost — delete the sidecar and `Store::rebuild` reconstitutes it, while the comprehension layer always carries citations.
//!
//! # Contract
//! - **Design:** `docs/design/context-ledger.md`; the mechanism detail is owned by `docs/context/data-model.md` and `docs/context/pipeline.md`.
//! - **Upstream:** `loong-kernel`'s `ContextEngine` and `turn/recall.rs`, and `loongagent context status/verify/rebuild`; the crate itself reads `loong-eventlog`.
//! - **Downstream:** the re-exports below: `Store`, `Summary`, the `Summarizer` contract, the worker, and the maps the kernel carries in the foreground.
//! - **Verify:** `cargo test -p loong-context` · `./scripts/check.sh`.

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
