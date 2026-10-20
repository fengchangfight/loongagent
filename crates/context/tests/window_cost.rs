//! Purpose: A re-measurable window cost: how many episodes one idle window enriches, and at what price.
//!
//! Serves: the quantity attached to "the model call belongs off the critical path", so the configuration choice rests on a measurement.
//!
//! # Contract
//! - **Design:** `docs/context/status.md` §11.1; `docs/context/config.md` §11.
//! - **Upstream:** `loong_context::{OllamaSummarizer, Store, WindowBudget}` and the console-session fixture below.
//! - **Downstream:** `Store::rebuild`, `Store::digest` and `Store::synthesize_window`.
//! - **Verify:** `cargo test -p loong-context --test window_cost -- --ignored --nocapture`.

use loong_context::worker::IdleForeground;
use loong_context::{OllamaSummarizer, Store, WindowBudget};
use loong_eventlog::SessionLog;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A session of many small finished tasks, the shape the enrichment queue actually has.
/// Serves: a realistic window measurement: one episode per task, each with an edit and a failing-then-passing test.
/// Design: `docs/context/pipeline.md` §6.3.
/// Up: `one_idle_window_cost`.
/// Down: `loong_eventlog::SessionLog`; no other callee.
/// Verify: `cargo test -p loong-context --test window_cost`.
/// A session of many small finished tasks, the shape the enrichment queue actually
/// has: one episode per task, each with an edit and a failing-then-passing test.
fn console_session(path: &PathBuf, tasks: usize) {
    let mut log = SessionLog::create(path).unwrap();
    for i in 0..tasks {
        let file = [
            "src/config/merge.rs",
            "src/kernel/turn.rs",
            "crates/context/src/store.rs",
        ][i % 3];
        log.append(
            "message.appended",
            serde_json::json!({"role": "user", "text": format!("第 {i} 个任务：配置层叠失效了，帮我查一下")}),
        )
        .unwrap();
        log.append(
            "tool.call",
            serde_json::json!({"call_id": format!("e{i}"), "tool": "edit_file",
                "args": {"path": file, "new_string": "x"}, "action_class": "write_workspace"}),
        )
        .unwrap();
        log.append(
            "tool.result",
            serde_json::json!({"call_id": format!("e{i}"), "ok": false,
                "output": format!("test layer_merge_{i} ... FAILED")}),
        )
        .unwrap();
        log.append(
            "tool.call",
            serde_json::json!({"call_id": format!("c{i}"), "tool": "run_command",
                "args": {"command": format!("cargo test layer_merge_{i}")}, "action_class": "investigate"}),
        )
        .unwrap();
        log.append(
            "tool.result",
            serde_json::json!({"call_id": format!("c{i}"), "ok": true,
                "output": format!("test layer_merge_{i} ... ok")}),
        )
        .unwrap();
    }
}

/// Purpose: An environment variable parsed as a `usize`, or the default.
/// Contract: `window_cost.rs` measurement configuration.
fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Purpose: Measure one idle window's calls, enrichment, tokens and wall clock against the deterministic foreground cost.
#[test]
#[ignore = "needs a local Ollama; a measurement, not a gate"]
fn one_idle_window_cost() {
    let base =
        std::env::var("LOONG_OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    let model = std::env::var("LOONG_SUMMARIZER_MODEL")
        .unwrap_or_else(|_| "qwen2.5:7b-instruct".to_string());
    let window_calls = env_usize("LOONG_WINDOW_CALLS", 4);
    let tasks = env_usize("LOONG_WINDOW_TASKS", 24);
    let window_tokens = std::env::var("LOONG_WINDOW_TOKENS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(6000);

    let path = std::env::temp_dir().join(format!(
        "loong-window-cost-{}-{}.jsonl",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let db = path.with_extension("context.db");
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&db);

    console_session(&path, tasks);
    let mut store = Store::open(&db).unwrap();
    // The deterministic half, which the foreground does synchronously — this is the
    // part the turn waits for.
    let t = Instant::now();
    store.rebuild(&path).unwrap();
    store.digest().unwrap();
    println!(
        "{tasks} tasks: rebuild + digest in {:?} ({} atoms, {} episodes)",
        t.elapsed(),
        store.atom_count().unwrap(),
        store.summary_count(2).unwrap()
    );

    let mut summarizer = OllamaSummarizer::new(&base, &model).with_max_tokens(400);
    let budget = WindowBudget::new(window_calls, window_tokens);
    println!(
        "model {model} at {base}; window = {window_calls} call(s) / {window_tokens} tokens\n\
         {:>4}  {:>10}  {:>6}  {:>8}  {:>7}  {:>9}  {:>6}",
        "win", "wall", "calls", "enriched", "in tok", "out tok", "left"
    );

    let mut window = 0usize;
    let mut total_calls = 0u64;
    let mut total_enriched = 0u64;
    let started = Instant::now();
    loop {
        let t = Instant::now();
        let outcome = store
            .synthesize_window(&mut summarizer, budget, &IdleForeground)
            .unwrap();
        let elapsed = t.elapsed();
        window += 1;
        total_calls += outcome.calls;
        total_enriched += outcome.enriched;
        println!(
            "{window:>4}  {elapsed:>10.2?}  {:>6}  {:>8}  {:>7}  {:>9}  {:>6}",
            outcome.calls,
            outcome.enriched,
            outcome.input_tokens,
            outcome.output_tokens,
            outcome.remaining,
        );
        if outcome.remaining == 0 || outcome.calls == 0 || window >= 12 {
            break;
        }
    }
    println!(
        "\ntotal: {total_calls} call(s), {total_enriched} episode(s) enriched in {:?} \
         across {window} window(s)",
        started.elapsed()
    );

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&db);
}
