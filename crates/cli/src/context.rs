//! Purpose: The three `context` commands: what the derived memory holds, whether it agrees with the log, and rebuilding it.
//!
//! # Contract
//! - **Serves:** `loongagent context status|verify|rebuild`.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/context/principles.md` §4.1.
//! - **Upstream:** `main.rs`'s `context` arms dispatch here.
//! - **Downstream:** `loong_context::Store` (`open`, `ingest`, `verify`, `rebuild`, `digest`, `summary_count`) and the session's JSONL log.
//! - **Verify:** `cargo test -p loongagent` · `./scripts/check.sh`

use super::*;

/// The three `context` commands, exercised end to end.
#[cfg(test)]
mod context_command_tests {
    use super::*;

    /// Purpose: Write a small session log whose attempts and abandoned edit the digest can be checked against.
    /// Serves: the two `context` command tests in this module.
    /// Design: `docs/context/pipeline.md` §6.2.
    /// Up: `status_names_the_store_and_says_the_memory_is_off_rather_than_empty` and `rebuild_then_verify_round_trips_and_the_digest_holds_the_attempt_sequence`.
    /// Down: `std::fs::write` and `std::env::temp_dir`.
    /// Verify: `cargo test -p loongagent`.
    fn session_with_a_fix(dir: &Path) -> PathBuf {
        let log = dir.join("s.jsonl");
        let events = [
            (
                1,
                "message.appended",
                r#"{"role":"user","text":"配置层叠失效了"}"#,
            ),
            (
                2,
                "tool.call",
                r#"{"call_id":"c1","tool":"edit_file","args":{"path":"src/config/merge.rs","new_string":"深拷贝后再合并"},"action_class":"write_workspace"}"#,
            ),
            (
                3,
                "tool.call",
                r#"{"call_id":"t1","tool":"run_command","args":{"command":"cargo test layer_merge"},"action_class":"investigate"}"#,
            ),
            (
                4,
                "tool.result",
                r#"{"call_id":"t1","ok":false,"output":"test layer_merge ... FAILED"}"#,
            ),
            (
                5,
                "tool.call",
                r#"{"call_id":"c2","tool":"edit_file","args":{"path":"src/config/merge.rs","new_string":"改为按层号排序后合并"},"action_class":"write_workspace"}"#,
            ),
            (
                6,
                "tool.call",
                r#"{"call_id":"t2","tool":"run_command","args":{"command":"cargo test layer_merge"},"action_class":"investigate"}"#,
            ),
            (
                7,
                "tool.result",
                r#"{"call_id":"t2","ok":true,"output":"test layer_merge ... ok"}"#,
            ),
        ];
        let body: String = events
            .iter()
            .map(|(seq, kind, data)| {
                format!(
                    r#"{{"seq":{seq},"id":"e{seq}","kind":"{kind}","ignorable":false,"time_ms":{},"data":{data}}}"#,
                    1000 + seq * 100
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&log, format!("{body}\n")).unwrap();
        log
    }

    /// Purpose: Parse the minimum argument list for one of the context tests.
    /// Contract: `context.rs` tests; the two command tests are the only callers.
    fn args_for(session: &Path) -> Args {
        Args::parse(&["--session".to_string(), session.display().to_string()]).expect("args parse")
    }

    /// Purpose: A missing store is reported as the engine being off, not as an empty memory.
    #[test]
    fn status_names_the_store_and_says_the_memory_is_off_rather_than_empty() {
        // The distinction that matters most: "the engine is off" and "the memory is
        // empty" look identical in a count, and only one of them is a problem.
        let dir = std::env::temp_dir().join(format!("loong-ctx-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let session = session_with_a_fix(&dir);
        let missing = context_status(args_for(&session));
        assert!(missing.is_err(), "no store yet");
        let message = missing.unwrap_err();
        assert!(message.contains("no context store"), "{message}");
        assert!(
            message.contains("off unless a profile turns it on"),
            "the message must say *why* it is missing, not just that it is: {message}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Purpose: Rebuild, verify and the derived digest survive a round trip through the store.
    #[test]
    fn rebuild_then_verify_round_trips_and_the_digest_holds_the_attempt_sequence() {
        let dir = std::env::temp_dir().join(format!("loong-ctx-cli2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let session = session_with_a_fix(&dir);

        context_rebuild(args_for(&session)).expect("rebuild works");
        assert!(context_store_path(&session).exists());
        context_verify(args_for(&session)).expect("a rebuilt store verifies");

        // And the computed abandonment survives the round trip: this is the field
        // that used to be a model's job and is now derived.
        let store = open_context_store(&session, false).unwrap();
        let episode = store.summaries_at(2, 10).unwrap().remove(0);
        assert!(
            episode.content.contains("被否决"),
            "the abandoned attempt must be in the digest: {}",
            episode.content
        );
        assert!(
            episode.content.contains("深拷贝"),
            "and it names what was tried: {}",
            episode.content
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// Purpose: The sidecar store beside a session's log.
/// Contract: `context.rs` commands; `open_context_store`, `context_status` and `context_rebuild` call it.
/// The sidecar store beside a session's log.
fn context_store_path(session: &Path) -> PathBuf {
    session.with_extension("context.db")
}

/// Purpose: Open the session's derived store, creating it only when the caller is the writing command.
/// Serves: all three `context` commands.
/// Design: `docs/context/data-model.md` §5.1–§5.2; `docs/context/pipeline.md` §6.6.
/// Up: `context_status`, `context_verify` and `context_rebuild`; no other caller.
/// Down: `context_store_path`, `loong_context::Store::{open, ingest}` and the session file's existence check.
/// Verify: `cargo test -p loongagent`.
/// Open the derived store for a session, creating it if the session has one.
///
/// Reading commands do **not** build it if it is missing: a status command that
/// quietly ran a full derivation would be a diagnostic that changes what it is
/// diagnosing. `rebuild` is the one that writes, and it says so.
fn open_context_store(session: &Path, create: bool) -> Result<loong_context::Store, String> {
    let store_path = context_store_path(session);
    if !session.exists() {
        return Err(format!(
            "no session at {} — nothing to report on",
            session.display()
        ));
    }
    if !create && !store_path.exists() {
        return Err(format!(
            "no context store for this session yet ({} is absent).\n\
             The layered memory is off unless a profile turns it on, and the store is \
             built the first time a session runs with it enabled.\n\
             `loongagent context rebuild` builds it from the log now.",
            store_path.display()
        ));
    }
    let mut store = loong_context::Store::open(&store_path).map_err(|e| e.to_string())?;
    store.ingest(session).map_err(|e| e.to_string())?;
    Ok(store)
}

/// Purpose: Print what the layered memory holds, what has spent tokens getting there, and whether it is healthy.
/// Serves: `loongagent context status`, the diagnostic that must not diagnose by rebuilding.
/// Design: `docs/context/data-model.md` §5.2 (the tables this report reads).
/// Up: `main.rs`'s `context` → `status` arm; no other caller.
/// Down: `open_context_store` and `Store::{verify, summary_count, global_map, summaries_at, pending_synthesis}`.
/// Verify: `cargo test -p loongagent`.
/// What the layered memory currently holds, and what it has spent getting there.
pub(crate) fn context_status(args: Args) -> Result<(), String> {
    let session = args.session_path();
    let store = open_context_store(&session, false)?;
    let report = store.verify().map_err(|e| e.to_string())?;
    println!("session   {}", session.display());
    println!("store     {}", context_store_path(&session).display());
    println!(
        "atoms     {}  (watermark {})",
        report.atoms, report.watermark
    );
    println!(
        "entities  {}  links {}{}",
        report.entities,
        report.links,
        if report.skipped > 0 {
            format!("  skipped {}", report.skipped)
        } else {
            String::new()
        }
    );
    for (level, label) in [(2, "L2 episodes"), (3, "L3 domains")] {
        let count = store.summary_count(level).map_err(|e| e.to_string())?;
        println!("{label:<10} {count}");
    }
    // L4 is **not** a stored row: the global map is re-derived from the L2 episodes on
    // demand, so its "count" is 1 (there is a map) or 0 (there is not). Printing
    // `summary_count(4)` here reported a permanent 0, which reads as "no map" even on
    // a session that has one — the same mistake the TUI's `memory_layers` avoids.
    let l4 = match store.global_map().map_err(|e| e.to_string())? {
        Some(map) if !map.content.is_empty() => format!("1 ({} tok)", map.tokens),
        _ => "0".to_string(),
    };
    println!("{:<10} {l4}", "L4 map");
    println!(
        "enrich     {} pending, {} degraded",
        store.pending_synthesis().map_err(|e| e.to_string())?,
        store.degraded_synthesis().map_err(|e| e.to_string())?,
    );
    // Which model wrote which part, so "the memory is thin" can be told apart from
    // "the memory was never enriched". A failure also starts with `digest+model:` —
    // counting it as "enriched" made one degraded episode print as both enriched and
    // failed, which is exactly the kind of number a person would read the wrong way.
    let summaries = store.summaries_at(2, 10_000).map_err(|e| e.to_string())?;
    let deterministic = summaries
        .iter()
        .filter(|s| s.producer == "deterministic")
        .count();
    let failed = summaries
        .iter()
        .filter(|s| s.producer.contains("(failed:"))
        .count();
    let enriched = summaries
        .iter()
        .filter(|s| s.producer.starts_with("digest+model:") && !s.producer.contains("(failed:"))
        .count();
    println!("producers  {deterministic} deterministic, {enriched} enriched, {failed} failed");
    if !report.is_healthy() {
        println!(
            "\n⚠ derived state is not healthy: {} missing atom(s), {} orphan link(s). \
             Run `loongagent context rebuild`.",
            report.missing_atoms, report.orphan_links
        );
    }
    Ok(())
}

/// Purpose: Check the derived store against its own records and every summary's citations against the log.
/// Serves: `loongagent context verify`.
/// Design: `docs/context/principles.md` §4.1 (the citation invariant).
/// Up: `main.rs`'s `context` → `verify` arm; no other caller.
/// Down: `open_context_store` and `Store::{verify, summaries_at, atom}`.
/// Verify: `cargo test -p loongagent`.
/// Check the derived state against its own records, and say what is wrong.
pub(crate) fn context_verify(args: Args) -> Result<(), String> {
    let session = args.session_path();
    let store = open_context_store(&session, false)?;
    let report = store.verify().map_err(|e| e.to_string())?;
    let episodes = store.summaries_at(2, 10_000).map_err(|e| e.to_string())?;
    // Every citation must land on events that exist. A summary whose evidence is
    // gone is a summary nothing can check, which is the one thing this design says
    // must never happen.
    let mut dangling = 0usize;
    for summary in &episodes {
        for (from, to) in &summary.citations {
            for seq in *from..=*to {
                if store.atom(seq).map_err(|e| e.to_string())?.is_none() {
                    dangling += 1;
                    break;
                }
            }
        }
    }
    if report.is_healthy() && dangling == 0 {
        println!(
            "ok — {} atoms, {} episodes, every citation resolves",
            report.atoms,
            episodes.len()
        );
        return Ok(());
    }
    if !report.is_healthy() {
        println!(
            "unhealthy: missing_atoms={} orphan_links={} dangling_entity_links={}",
            report.missing_atoms, report.orphan_links, report.dangling_entity_links
        );
    }
    if dangling > 0 {
        println!("{dangling} cited range(s) point at events that are not there");
    }
    Err(
        "the derived store disagrees with itself; `loongagent context rebuild` is safe to run"
            .to_string(),
    )
}

/// Purpose: Throw the derived store away and rebuild it from the log, leaving the log untouched.
/// Serves: `loongagent context rebuild`, the one writing context command.
/// Design: `docs/context/principles.md` §4.1; `docs/context/data-model.md` §5.1–§5.2.
/// Up: `main.rs`'s `context` → `rebuild` arm; no other caller.
/// Down: `open_context_store`, `Store::{rebuild, digest, digest_domains}` and `context_store_path`.
/// Verify: `cargo test -p loongagent`.
/// Throw the derived store away and rebuild it from the log. The log is untouched.
pub(crate) fn context_rebuild(args: Args) -> Result<(), String> {
    let session = args.session_path();
    let store_path = context_store_path(&session);
    let mut store = open_context_store(&session, true)?;
    let atoms = store.rebuild(&session).map_err(|e| e.to_string())?;
    let episodes = store.digest().map_err(|e| e.to_string())?;
    let domains = store.digest_domains().map_err(|e| e.to_string())?;
    println!(
        "rebuilt {} from the log: {atoms} atoms, {episodes} episodes, {domains} domain maps",
        store_path.display()
    );
    Ok(())
}
