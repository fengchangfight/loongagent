//! Purpose: Local mode: its own `local.toml`, the first-run wizard, the `local` subcommands and the fixtures their tests need.
//!
//! # Contract
//! - **Serves:** `loongagent local …`, the first run of `tui --local`, and the local-mode integration tests.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/config-routing.md` §1, §3 and §8.
//! - **Upstream:** `main.rs`'s `local` arm; `tui.rs`'s `full_screen` calls `configure_local`, `session_needs_a_model` and `spawn_model_health`.
//! - **Downstream:** `loong_config`'s local layer, catalog and profile types, `loong_providers::probe`, and `loong_tui_app`'s wizard and health types.
//! - **Verify:** `cargo test -p loongagent --test local_mode` · `./scripts/check.sh`

use super::*;

use crate::config_cmd::resolve_reference;
use crate::server::{DEFAULT_SUMMARIZER_MODEL, DEFAULT_SUMMARIZER_URL};

/// Purpose: Dispatch the `local` subcommands, always resolving the local file whichever mode the caller typed.
/// Serves: `loongagent local [list|add|delete|edit|check|ping|semantic]`.
/// Design: `docs/design/config-routing.md` §1 and §3.
/// Up: `main.rs`'s `local` arm; no other caller.
/// Down: `local_list`, `configure_local`, `local_delete`, `local_edit`, `local_check`, `local_ping` and `local_semantic`.
/// Verify: `cargo test -p loongagent --test local_mode`.
/// The local first run: get a key, prove it, and write the two files that make the
/// next run uneventful.
///
/// It runs **before** the server child is spawned, which is the whole reason it is a
/// separate function: the child resolves its models once, at startup, so a key written
/// afterwards would not take effect until the next run — the first run would look
/// broken in exactly the case it exists to fix.
///
/// Two ways in, and the quiet one comes first:
///
/// * a key **already exported** under a name the catalog knows identifies its provider
///   exactly — the variable's name *is* the fact, so there is nothing to guess and the
///   user is asked for nothing at all;
/// * otherwise, one screen: paste a key, confirm the provider, watch it validate.
/// `loongagent local` — the local install's own configuration, and the way to add a
/// provider without starting a session for it.
///
/// Local mode's file is separate from the managed profile on purpose, so these
/// subcommands only ever touch `local.toml`.
pub(crate) fn local(mut args: Args) -> Result<(), String> {
    // Every `local` subcommand is about the local file, so this run resolves the local
    // layers whether or not `--local` was typed. Without this, `loongagent local` showed
    // the control plane's models and `local add` would have written into a config the
    // managed mode owns — which is the whole class of mistake this file exists to end.
    args.local = true;
    // From the parsed positionals, not from a fixed slot: `loongagent local --cwd .` has
    // no subcommand at all, and reading `rest[1]` would take `--cwd` for one.
    let words: Vec<String> = args.positional.clone();
    let sub = words.first().map(String::as_str).unwrap_or("");
    let rest = words.get(1).map(String::as_str);
    // Everything after the subcommand, for the one whose arguments are not a single
    // word (`local semantic on <model>`).
    let tail: Vec<String> = words.iter().skip(1).cloned().collect();
    let path = loong_config::local_profile_path();

    match sub {
        "" | "list" | "show" => local_list(&args, &path),
        "add" => {
            let (settings, _) = args.settings_with_pack()?;
            configure_local(&settings, true, rest)
        }
        "delete" | "rm" | "remove" => {
            let selector = rest.ok_or_else(|| {
                "usage: loongagent local delete <slot|model|provider> \
                 (see `loongagent local`)"
                    .to_string()
            })?;
            local_delete(&path, selector)
        }
        "edit" => local_edit(&path),
        "check" => local_check(rest),
        "ping" => {
            let (settings, _) = args.settings_with_pack()?;
            local_ping(&settings, rest)
        }
        // The layered memory's switch (ADR-0017). Here rather than at top level
        // because it writes `local.toml` — the same file `local add/edit` own, and
        // the only place a *summariser endpoint* may be chosen (a repository is
        // refused it, ADR-0008).
        "semantic" | "memory" => local_semantic(&args, &path, &tail),
        other => Err(format!(
            "unknown local subcommand {other:?} — `loongagent local` lists what is \
             configured; `local add|delete|edit|semantic` change it"
        )),
    }
}

/// Purpose: Refuse to open a local session that has nothing to answer with, naming the command that fixes it.
/// Serves: `tui.rs`'s `full_screen`, before the terminal is taken over.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `tui.rs`'s `full_screen`; the `local.rs` tests.
/// Down: no further call; it reads the resolved profile.
/// Verify: `cargo test -p loongagent --test local_mode`.
/// Whether a session may open at all.
///
/// A session with no model cannot answer anything, and its first few minutes are spent
/// discovering that. A local install is the one place the person can do something about
/// it, so it is the one place worth refusing — and the message names the command rather
/// than opening a screen that will fail on its first prompt.
///
/// A **managed** install keeps the old behaviour on purpose: its models arrive in a pack
/// the user cannot edit, so refusing would strand them with no interface and no way to fix
/// it. There the server runs an `UnconfiguredModel` that says so from inside a working
/// session, which is what that type exists for.
///
/// `--model` names a provider double (`demo-write`, `echo`) and is its own answer: a
/// session asked for explicitly is a session that has a model.
pub(crate) fn session_needs_a_model(
    args: &Args,
    local: bool,
    profile: &loong_config::Profile,
) -> Result<(), String> {
    if !local || args.model.is_some() || !profile.models.is_empty() {
        return Ok(());
    }
    Err("还没有配置任何模型 — 先运行 `loongagent local add`\
         （或 `loongagent local add ollama` 用本机 ollama）"
        .to_string())
}

/// Purpose: Show what this install will actually run: the semantic switch, the models in slot order, and where the file is.
/// Serves: `loongagent local`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `local`; no other caller.
/// Down: `Args::settings_with_pack` and `Profile::{context_engine, default_model}`.
/// Verify: `cargo test -p loongagent --test local_mode`.
/// `loongagent local` — what this install will actually run, and where it says so.
fn local_list(args: &Args, path: &Path) -> Result<(), String> {
    let (settings, _) = args.settings_with_pack()?;
    println!("{}", path.display());
    // Say whether the layered memory is on before anything else about models: it is a
    // setting nobody should have to learn by editing this file, and it is independent
    // of how many chat models are configured.
    match settings.context_engine() {
        Some(engine) if engine.mode == "semantic" && engine.summarizer != "off" => println!(
            "分层记忆：on（摘要模型 {}）— `local semantic` 看详情",
            engine
                .summarizer_model
                .as_deref()
                .unwrap_or(DEFAULT_SUMMARIZER_MODEL)
        ),
        _ => println!("分层记忆：off — `local semantic on` 打开"),
    }
    if settings.models.is_empty() {
        println!();
        println!("  (还没配置模型)");
        println!();
        println!("  加一个：loongagent local add");
        return Ok(());
    }
    println!();
    for (index, model) in settings.models.iter().enumerate() {
        let current = settings
            .default_model()
            .map(|first| first.name == model.name)
            .unwrap_or(false);
        println!(
            "  {}[{}] {:<24} {:<12} {}",
            if current { "▸" } else { " " },
            index + 1,
            model.name,
            model.provider.as_deref().unwrap_or("-"),
            model
                .max_context_size
                .map(|t| format!("ctx {t}"))
                .unwrap_or_default()
        );
    }
    println!();
    println!("  进入会话：loongagent tui --local");
    println!("  加 / 删 / 改：local add [服务商] · local delete <序号|名字|服务商> · local edit");
    println!("  分层记忆：local semantic [on [模型] | off]");
    Ok(())
}

/// Purpose: The layered memory's switch: status, on with a summariser choice, or off.
/// Serves: `loongagent local semantic [status|on|off]` (ADR-0017).
/// Design: `docs/design/surfaces.md` §2; `docs/design/context-ledger.md` §4.
/// Up: `local`; no other caller.
/// Down: `semantic_status`, `semantic_on` and `write_semantic_config`.
/// Verify: `cargo test -p loongagent --test local_mode`.
/// The layered memory's local switch: `loongagent local semantic [status|on|off]`.
///
/// One command, because the two questions a person actually has are "is it on?" and
/// "which model does it use?" — and ADR-0017 makes both settings, not code. It writes
/// the same file `local add` writes, with the same text-surgery rule: the comments in
/// `local.toml` are that file's only documentation, so nothing is re-serialized.
fn local_semantic(args: &Args, path: &Path, words: &[String]) -> Result<(), String> {
    let sub = words.first().map(String::as_str).unwrap_or("");
    let model = words.get(1).map(String::as_str);
    match sub {
        "" | "status" | "show" => semantic_status(args, path),
        "on" | "enable" => semantic_on(args, path, model),
        "off" | "disable" => {
            write_semantic_config(path, &SemanticWrite::Off)?;
            println!("semantic   off — 只剩确定性检索层（原文仍然可检索、可 rebuild）");
            println!("  {}", path.display());
            Ok(())
        }
        other => Err(format!(
            "unknown `local semantic` subcommand {other:?}\n\
             usage:\n\
             \x20 loongagent local semantic                      看当前状态\n\
             \x20 loongagent local semantic on [provider/]<model>  打开，选摘要模型\n\
             \x20 loongagent local semantic on --record           只做确定性派生\n\
             \x20 loongagent local semantic off                  关闭"
        )),
    }
}

/// Purpose: The three states `local semantic` writes, which are not the same setting: off, record-only, semantic.
/// Serves: `local_semantic`, `semantic_on` and `write_semantic_config`.
/// Design: `docs/design/context-ledger.md` §4.
/// Up: `local_semantic` and `semantic_on` build it; `write_semantic_config` consumes it.
/// Down: no further call; it is data.
/// Verify: `cargo test -p loongagent`.
/// What `local semantic` writes. `Off` and `Record` are different states: one is the
/// old compaction path, the other is the full retrieval layer with no model.
enum SemanticWrite {
    Off,
    Record,
    Semantic {
        base_url: String,
        model: String,
        window_calls: usize,
        window_tokens: u64,
    },
}

/// Purpose: Say whether the engine is on and whether the chosen model is actually installed, which are different facts.
/// Serves: `loongagent local semantic`.
/// Design: `docs/design/context-ledger.md` §4 and §7.
/// Up: `local_semantic`; no other caller.
/// Down: `Args::settings_with_pack`, `Profile::context_engine` and `ollama_models`.
/// Verify: `cargo test -p loongagent`.
/// `loongagent local semantic` — what is configured, and whether the model is actually
/// installed. "Configured" and "working" are different facts and the gap between them
/// is exactly what a person is asking about.
fn semantic_status(args: &Args, path: &Path) -> Result<(), String> {
    let (settings, _) = args.settings_with_pack()?;
    println!("{}", path.display());
    println!();
    let engine = settings.context_engine();
    match &engine {
        Some(engine) if engine.mode == "semantic" && engine.summarizer != "off" => {
            let model = engine
                .summarizer_model
                .as_deref()
                .unwrap_or(DEFAULT_SUMMARIZER_MODEL);
            let base = engine
                .summarizer_base_url
                .as_deref()
                .unwrap_or(DEFAULT_SUMMARIZER_URL);
            println!("  semantic   on");
            println!("  mode       semantic");
            println!("  provider   {}", engine.summarizer);
            println!("  model      {model}");
            println!("  base_url   {base}");
            println!(
                "  window     {} calls / {} tokens",
                engine
                    .summarizer_window_calls
                    .unwrap_or(loong_kernel::context_engine::DEFAULT_WINDOW_CALLS),
                engine
                    .summarizer_window_tokens
                    .unwrap_or(loong_kernel::context_engine::DEFAULT_WINDOW_TOKENS),
            );
            match ollama_models(base) {
                Ok(models) if models.iter().any(|m| m == model) => {
                    println!("  installed  yes")
                }
                Ok(models) if models.is_empty() => {
                    println!("  installed  no — {base} 上一个模型都没有；先 `ollama pull {model}`")
                }
                Ok(_) => println!("  installed  no — 先 `ollama pull {model}`"),
                Err(e) => println!("  installed  unknown — {e}"),
            }
        }
        Some(engine) => {
            println!("  semantic   record — 确定性派生，不调用模型");
            println!("  mode       {}", engine.mode);
        }
        None => {
            println!("  semantic   off");
            println!();
            println!("  打开：loongagent local semantic on");
        }
    }
    println!("  生效范围：loongagent tui --local");
    Ok(())
}

/// Purpose: Pick and write a summariser, then read the file back through the same resolver a session uses.
/// Serves: `loongagent local semantic on [provider/]<model>`.
/// Design: `docs/design/context-ledger.md` §4.
/// Up: `local_semantic`; no other caller.
/// Down: `parse_summarizer_selector`, `summarizer_base_url`, `ollama_models`, `choose_model`, `write_semantic_config` and `Args::settings_with_pack`.
/// Verify: `cargo test -p loongagent`.
/// `loongagent local semantic on [provider/]<model>`.
fn semantic_on(args: &Args, path: &Path, model_arg: Option<&str>) -> Result<(), String> {
    if args.record {
        write_semantic_config(path, &SemanticWrite::Record)?;
        println!("semantic   record — 只做确定性派生，不调用模型");
        println!("  {}", path.display());
        return Ok(());
    }
    // The provider is checked before anything is written: a name the summariser cannot
    // speak must fail here, not at the first episode.
    let (provider, wanted) = parse_summarizer_selector(model_arg)?;
    let base_url = summarizer_base_url(args.summarizer_base_url.as_deref());
    let installed = ollama_models(&base_url).unwrap_or_default();
    let existing = existing_summarizer_model(path);
    let model = match wanted {
        Some(model) => model,
        None => choose_model(&installed, existing.as_deref())?,
    };
    if !installed.is_empty() && !installed.iter().any(|m| m == &model) {
        eprintln!(
            "[semantic] 注意：{base_url} 上没有 {model}；先 `ollama pull {model}`，\
             否则每一段都会退回 digest（会在 F2 与 context status 里显示为 degraded）"
        );
    }
    write_semantic_config(
        path,
        &SemanticWrite::Semantic {
            base_url: base_url.clone(),
            model: model.clone(),
            window_calls: loong_kernel::context_engine::DEFAULT_WINDOW_CALLS,
            window_tokens: loong_kernel::context_engine::DEFAULT_WINDOW_TOKENS,
        },
    )?;
    println!("semantic   on");
    println!("  provider   {provider}");
    println!("  model      {model}");
    println!("  base_url   {base_url}");
    println!("  {}", path.display());
    // Read back through the same resolver a session uses: a file that does not load,
    // or that a higher layer overrules, must be reported now rather than discovered as
    // an engine that silently stayed off.
    let (resolved, _) = args.settings_with_pack()?;
    match resolved.context_engine() {
        Some(engine) if engine.mode == "semantic" && engine.summarizer != "off" => {}
        _ => {
            return Err(format!(
                "{} 已写入，但用同一套分层解析出来语义引擎仍然是关的 — \
                 检查更高优先级的一层是否把它强制关掉了",
                path.display()
            ))
        }
    }
    println!();
    println!("  生效范围：loongagent tui --local（local.toml 只被 local 模式读取）");
    Ok(())
}

/// Purpose: Read `[provider/]model`, refusing a provider the summariser cannot speak and saying why.
/// Serves: `semantic_on`.
/// Design: `docs/design/context-ledger.md` §4.
/// Up: `semantic_on`; the `local.rs` tests.
/// Down: no further call; it splits the selector.
/// Verify: `cargo test -p loongagent`.
/// `[provider/]model` -> `("ollama", Some(model))`.
///
/// Only one provider exists, and that is a fact about the design rather than a stage
/// of implementation: the summariser speaks Ollama's native `/api/chat` because the
/// OpenAI-compatible shim was **measured** to ignore `think:false` and spend the whole
/// budget on a reasoning trace. Saying so beats accepting `deepseek/…` and quietly
/// doing nothing.
fn parse_summarizer_selector(
    selector: Option<&str>,
) -> Result<(&'static str, Option<String>), String> {
    let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(("ollama", None));
    };
    match selector.split_once('/') {
        Some((provider, _)) if provider != "ollama" => Err(format!(
            "摘要后端目前只有 ollama（本机），不能选 {provider:?}：它走 Ollama 原生 /api/chat，\
             而 OpenAI 兼容的 /v1 实测会忽略 think:false、把预算花在推理链上。\
             用 `loongagent local semantic on ollama/<模型>`，或直接写模型名"
        )),
        Some((_, model)) if model.trim().is_empty() => {
            Err("`ollama/` 后面要跟模型名，例如 ollama/qwen2.5:7b-instruct".to_string())
        }
        Some((_, model)) => Ok(("ollama", Some(model.trim().to_string()))),
        None => Ok(("ollama", Some(selector.to_string()))),
    }
}

/// Purpose: The model to use when none was named: the configured one, the measured default, or the only installed one.
/// Serves: `semantic_on`.
/// Design: `docs/design/context-ledger.md` §4.
/// Up: `semantic_on`; the `local.rs` tests.
/// Down: no further call; it compares names.
/// Verify: `cargo test -p loongagent`.
/// Which model to use when nobody named one: the one already configured, else the
/// measured default, else the only installed one. With several installed and none of
/// those, it **refuses and lists them** — guessing between 0.6B and 7B is a quality
/// decision, and the faithfulness measurements in the design note are the reason it is
/// not the tool's to make.
fn choose_model(installed: &[String], existing: Option<&str>) -> Result<String, String> {
    if let Some(existing) = existing {
        if installed.is_empty() || installed.iter().any(|m| m == existing) {
            return Ok(existing.to_string());
        }
    }
    if installed.iter().any(|m| m == DEFAULT_SUMMARIZER_MODEL) {
        return Ok(DEFAULT_SUMMARIZER_MODEL.to_string());
    }
    match installed {
        [] => Err(format!(
            "连接不到本机 ollama，或它一个模型都没有。先 `ollama serve` 和 \
             `ollama pull {}`，或者指名一个已经有的：\
             `loongagent local semantic on <模型>`",
            DEFAULT_SUMMARIZER_MODEL
        )),
        [only] => Ok(only.clone()),
        many => Err(format!(
            "ollama 里有 {} 个模型，这里不替你猜用哪个：\n  {}\n用 \
             `loongagent local semantic on <模型>` 选一个",
            many.len(),
            many.join("\n  ")
        )),
    }
}

/// Purpose: The summariser endpoint, with the OpenAI shim's `/v1` suffix removed.
/// Contract: `local.rs` semantic switch; `semantic_on` and its test call it.
/// The summariser endpoint. Ollama's native API has no `/v1`; a person who types one
/// (because that is the shim's address) is corrected here rather than at the first
/// request.
fn summarizer_base_url(input: Option<&str>) -> String {
    let raw = input
        .unwrap_or(DEFAULT_SUMMARIZER_URL)
        .trim()
        .trim_end_matches('/');
    raw.strip_suffix("/v1").unwrap_or(raw).to_string()
}

/// Purpose: The model names this Ollama can serve, or why it could not be asked.
/// Contract: `local.rs` semantic switch; `semantic_status` and `semantic_on` call it.
/// The model names this Ollama can serve, or why it could not be asked.
fn ollama_models(base: &str) -> Result<Vec<String>, String> {
    let url = format!("{}/api/tags", base.trim_end_matches('/'));
    match loong_providers::probe::probe_models_url(&url, None) {
        loong_providers::probe::Probe::Ok { models } => {
            Ok(models.into_iter().map(|model| model.id).collect())
        }
        other => Err(loong_providers::probe::render(&other)),
    }
}

/// Purpose: The `model` already written in this file's `[summarizer]`, so a switch keeps the last choice.
/// Contract: `local.rs` semantic switch; `semantic_on` is the only caller.
/// The `model = "…"` already in this file's `[summarizer]`, so switching the engine on
/// again keeps the model the person chose last time.
fn existing_summarizer_model(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let (_, lines) = take_section(&text, "summarizer", &[]);
    lines
        .iter()
        .find_map(|line| quoted_value(line.trim(), "model"))
}

/// Purpose: Write or remove the memory tables in `local.toml` by text surgery, leaving every other byte alone.
/// Serves: `local semantic on|off|on --record`.
/// Design: `docs/design/context-ledger.md` §4.
/// Up: `local_semantic` and `semantic_on`; the `local.rs` tests.
/// Down: `take_section`, `toml_escape` and `std::fs::write`.
/// Verify: `cargo test -p loongagent`.
/// Write (or remove) the `[context_engine]` and `[summarizer]` tables in `local.toml`,
/// leaving every other byte of the file alone.
fn write_semantic_config(path: &Path, write: &SemanticWrite) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let (rest, ctx_extra) = take_section(&existing, "context_engine", &["enabled", "mode"]);
    let (rest, sum_extra) = take_section(
        &rest,
        "summarizer",
        &[
            "enabled",
            "kind",
            "base_url",
            "model",
            "window_calls",
            "window_tokens",
        ],
    );

    let mut text = rest.trim_end().to_string();
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    // No banner comment above the table: every write appends at the end, so a comment
    // written *outside* the section is kept by the next `take_section` and copied
    // again — one more line per switch. The tables are the documentation, and
    // `local semantic` is the way to read them.
    text.push_str("[context_engine]\n");
    match write {
        SemanticWrite::Off => text.push_str("enabled = false\n"),
        SemanticWrite::Record => text.push_str("enabled = true\nmode = \"record\"\n"),
        SemanticWrite::Semantic { .. } => text.push_str("enabled = true\nmode = \"semantic\"\n"),
    }
    for line in &ctx_extra {
        text.push_str(line);
        text.push('\n');
    }
    match write {
        SemanticWrite::Semantic {
            base_url,
            model,
            window_calls,
            window_tokens,
        } => {
            text.push('\n');
            text.push_str("[summarizer]\n");
            text.push_str("enabled = true\n");
            text.push_str("kind = \"ollama\"\n");
            text.push_str(&format!("base_url = \"{}\"\n", toml_escape(base_url)));
            text.push_str(&format!("model = \"{}\"\n", toml_escape(model)));
            text.push_str(&format!("window_calls = {window_calls}\n"));
            text.push_str(&format!("window_tokens = {window_tokens}\n"));
            for line in &sum_extra {
                text.push_str(line);
                text.push('\n');
            }
        }
        // Off/record need no model, but a hand-written key in the old section is not
        // this command's to delete.
        _ if !sum_extra.is_empty() => {
            text.push('\n');
            text.push_str("[summarizer]\n");
            for line in &sum_extra {
                text.push_str(line);
                text.push('\n');
            }
        }
        _ => {}
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Purpose: Remove one section and return it, keeping comments and keys this command does not own.
/// Serves: `write_semantic_config` and `existing_summarizer_model`.
/// Design: `docs/design/context-ledger.md` §4.
/// Up: `write_semantic_config` and `existing_summarizer_model`; no other caller.
/// Down: no further call; it walks lines.
/// Verify: `cargo test -p loongagent`.
/// Remove one `[header]` table, returning the rest of the text and the section's body
/// lines that are **not** keys this command owns. Comments, blank lines and unknown
/// keys come back so a hand-written note in the section is not a casualty of a switch.
fn take_section(text: &str, header: &str, managed: &[&str]) -> (String, Vec<String>) {
    let marker = format!("[{header}]");
    let mut rest = String::new();
    let mut kept: Vec<String> = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == marker {
            inside = true;
            continue;
        }
        if inside && trimmed.starts_with('[') {
            inside = false;
        }
        if inside {
            let key = trimmed.split('=').next().unwrap_or("").trim();
            if !managed.contains(&key) {
                kept.push(line.to_string());
            }
            continue;
        }
        rest.push_str(line);
        rest.push('\n');
    }
    while kept
        .first()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        kept.remove(0);
    }
    while kept
        .last()
        .map(|line| line.trim().is_empty())
        .unwrap_or(false)
    {
        kept.pop();
    }
    (rest, kept)
}

/// Purpose: Delete the models a slot, a model name or a provider names, and keep the router default valid.
/// Serves: `loongagent local delete <slot|model|provider>`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `local`; no other caller.
/// Down: `read_local_blocks`, `select_local`, `write_local_blocks` and `std::println`.
/// Verify: `cargo test -p loongagent --test local_mode`.
/// `loongagent local delete <slot|model|provider>`.
///
/// By three names because all three are things a person has in their head at different
/// moments: the slot they see in the list, the model id they typed, or the provider they
/// pasted a bad key for and want gone entirely.
fn local_delete(path: &Path, selector: &str) -> Result<(), String> {
    let mut blocks = read_local_blocks(path);
    if blocks.is_empty() {
        return Err(format!("{} 还没有配置模型", path.display()));
    }
    let doomed = select_local(&blocks, selector)?;
    let removed: Vec<String> = doomed
        .iter()
        .map(|index| blocks[*index].model.name.clone())
        .collect();
    // Backwards, so the indices stay valid as the list shrinks.
    for index in doomed.iter().rev() {
        blocks.remove(*index);
    }
    // An empty `[router]` naming nothing would not load; `write_local_blocks` drops the
    // section when the last model goes.
    write_local_blocks(path, &blocks)?;
    println!("已删除：{}", removed.join("、"));
    println!("剩下 {} 个模型 — `loongagent local` 看全部", blocks.len());
    Ok(())
}

/// The prompt `local ping` sends when nobody supplies one.
///
/// Two things in one cheap call. The self-report is the part people ask for and the part
/// that cannot be trusted — models misname themselves routinely — so it is never the proof.
/// The reversal is the proof: it is deterministic, so a correct answer means the request
/// reached *that* model and came back coherent, and it is the harness that says which model
/// that was.
const PING_PROMPT: &str = "只输出一行，格式严格为：<你自称的模型名>/<把 loongagent 倒过来写>\n                           不要解释，不要多余标点。不确定自己叫什么就写 unknown。";

/// Purpose: One model's answer or the reason there was none, with the label and endpoint to print.
/// Serves: `local_ping`.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `local_ping` builds the vector; no other caller.
/// Down: no further call; it is data.
/// Verify: `cargo test -p loongagent`.
/// One model's answer, or the reason there was none.
struct Ping {
    label: String,
    endpoint: String,
    result: Result<(String, u128), String>,
}

/// Purpose: Ask every configured model the same prompt and print the answers side by side.
/// Serves: `loongagent local ping [prompt]`, the check a session cannot do.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `local`; no other caller.
/// Down: `resolve_reference` and `loong_providers::probe::complete_once`.
/// Verify: `cargo test -p loongagent`.
/// `loongagent local ping [prompt]` — one prompt, every configured model.
///
/// The question "are all four paths working?" has no answer inside a session, because a
/// session is on one model at a time and switching loses the comparison. This asks every
/// configured model the same thing and prints the answers side by side, so a broken path
/// is visible as itself rather than as a bad answer three turns later.
fn local_ping(settings: &loong_config::Profile, prompt: Option<&str>) -> Result<(), String> {
    if settings.models.is_empty() {
        return Err("还没有配置任何模型 — 先 `loongagent local add`".to_string());
    }
    let prompt = prompt.unwrap_or(PING_PROMPT);
    let base = loong_config::local_profile_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let pings: Vec<Ping> = settings
        .models
        .iter()
        .map(|spec| {
            let key = resolve_reference(spec.api_key.as_deref(), &base);
            let started = std::time::Instant::now();
            let result = loong_providers::probe::complete_once(
                &spec.base_url,
                &spec.model,
                key.as_deref(),
                prompt,
                // A cap, not a target: a reasoning model spends this first and writes its
                // answer afterwards, so a tight number is how a working model comes back
                // looking mute.
                1024,
                std::time::Duration::from_secs(120),
            )
            .map(|answer| (answer, started.elapsed().as_millis()));
            Ping {
                label: match spec.provider.as_deref() {
                    Some(provider) => format!("{}  {}", spec.name, provider),
                    None => spec.name.clone(),
                },
                endpoint: format!("{} @ {}", spec.model, spec.base_url),
                result,
            }
        })
        .collect();

    println!("同一个 prompt，发给 {} 个模型：", pings.len());
    println!("  {prompt}");
    println!();
    let mut broken = 0;
    for ping in &pings {
        match &ping.result {
            Ok((answer, millis)) => {
                println!("  ✓ {}  ({millis}ms)", ping.label);
                for line in answer.lines().take(4) {
                    println!("      {line}");
                }
                if answer.trim().is_empty() {
                    println!("      （回答是空的 —— 路通了，但这个模型没说话）");
                }
            }
            Err(e) => {
                broken += 1;
                println!("  ✗ {}", ping.label);
                println!("      {}", ping.endpoint);
                println!("      {e}");
            }
        }
    }
    println!();
    if broken == 0 {
        println!("{} 条路全部有回应。", pings.len());
        println!(
            "注意：这验证的是「能连上、能回答、模型 id 正确」，不是「能调工具」——\
             那是另一件事，只有真正跑一个 turn 才知道。"
        );
        Ok(())
    } else {
        Err(format!("{broken} 条路没通（见上）。"))
    }
}

/// Purpose: Print what each provider's endpoint actually says: the request, the status, the body and what it parsed into.
/// Serves: `loongagent local check [provider]`.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `local`; no other caller.
/// Down: `loong_config::catalog`, `provider_key` and `loong_providers::probe::{raw_models_answer, parse_models}`.
/// Verify: `cargo test -p loongagent`.
/// `loongagent local check [provider]` — what the endpoint actually says.
///
/// The wizard reduces every reply to "worked" or "did not", which is right for a form and
/// useless for finding out why. This prints the request, the status, the body and what
/// those parsed into — which is how a truncated body was told apart from a vendor that
/// lists nothing. Two very different problems, one identical symptom, and only the bytes
/// say which.
fn local_check(selector: Option<&str>) -> Result<(), String> {
    let providers = loong_config::catalog::PROVIDERS;
    let chosen: Vec<&loong_config::catalog::Provider> = match selector {
        Some(name) => providers
            .iter()
            .filter(|provider| provider.id == name)
            .collect(),
        None => providers.iter().collect(),
    };
    if chosen.is_empty() {
        return Err(format!(
            "服务商目录里没有 {:?} — 有的是：{}",
            selector.unwrap_or(""),
            loong_config::catalog::ids().join(", ")
        ));
    }

    for provider in chosen {
        let url = provider
            .models_url()
            .unwrap_or_else(|| format!("{}/models", provider.base_url.trim_end_matches('/')));
        println!("{}", provider.label);
        println!("  GET {url}");
        match provider_key(provider) {
            None => {
                println!(
                    "  （环境里没有 {}，所以没发请求 —— 先 `local add {}`）",
                    provider.key_env, provider.id
                );
            }
            Some((var, key)) => {
                println!("  key: {var}");
                match loong_providers::probe::raw_models_answer(&url, Some(&key)) {
                    Err(e) => println!("  连不上：{e}"),
                    Ok((status, body)) => {
                        let body = body.trim();
                        let shown: String = body.chars().take(400).collect();
                        println!("  HTTP {status}");
                        if shown.is_empty() {
                            println!("  （响应体是空的）");
                        } else {
                            println!(
                                "  {shown}{}",
                                if body.chars().count() > 400 {
                                    "…"
                                } else {
                                    ""
                                }
                            );
                        }
                        if (200..300).contains(&status) {
                            let models = loong_providers::probe::parse_models(body);
                            if models.is_empty() {
                                println!(
                                    "  ↑ HTTP 200 但解析不出模型 —— 要么这个端点不列模型，\
                                     要么响应体是残缺的（这本身是个 bug，值得报）"
                                );
                            } else {
                                let names: Vec<String> = models
                                    .iter()
                                    .map(|model| match model.context_window {
                                        Some(tokens) => format!("{} (ctx {tokens})", model.id),
                                        None => model.id.clone(),
                                    })
                                    .collect();
                                println!("  解析出 {} 个：{}", names.len(), names.join("、"));
                            }
                        }
                    }
                }
            }
        }
        println!();
    }
    Ok(())
}

/// Purpose: Which entries a selector names: a slot, a model name, or every model of a provider.
/// Serves: `local_delete`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `local_delete` and the `local.rs` tests; no other caller.
/// Down: no further call; it compares blocks.
/// Verify: `cargo test -p loongagent`.
/// Which entries a selector names. Index, then model name, then provider.
fn select_local(blocks: &[LocalBlock], selector: &str) -> Result<Vec<usize>, String> {
    if let Ok(slot) = selector.parse::<usize>() {
        if slot >= 1 && slot <= blocks.len() {
            return Ok(vec![slot - 1]);
        }
        return Err(format!("没有第 {slot} 个模型（共 {} 个）", blocks.len()));
    }
    let by_name: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.model.name == selector)
        .map(|(index, _)| index)
        .collect();
    if !by_name.is_empty() {
        return Ok(by_name);
    }
    let by_provider: Vec<usize> = blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.model.provider.split('/').next() == Some(selector))
        .map(|(index, _)| index)
        .collect();
    if !by_provider.is_empty() {
        return Ok(by_provider);
    }
    Err(format!(
        "{selector:?} 没匹配到任何模型 — `loongagent local` 列出已配置的"
    ))
}

/// Purpose: Hand `local.toml` to the editor named by `$VISUAL` or `$EDITOR`, and say so when there is none.
/// Serves: `loongagent local edit`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `local`; no other caller.
/// Down: `std::process::Command` and the editor environment variables.
/// Verify: `cargo test -p loongagent`.
/// `loongagent local edit` — hand the file to the editor named by `$VISUAL`/`$EDITOR`.
///
/// The file is meant to be read and edited by a person: it is commented, and the comments
/// are the documentation. Opening it is more honest than a form that can only express the
/// settings somebody anticipated.
fn local_edit(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!(
            "{} 还不存在 — 先用 `loongagent local add` 配一个",
            path.display()
        ));
    }
    // `$VISUAL`, then `$EDITOR`. Windows has neither convention set by default and every
    // install has Notepad, so it falls back rather than telling someone to set a variable
    // their shell never sets. The value is a *program*, not a command line (`code --wait`
    // would be one argument here) — the same rule as on Unix.
    let editor = match std::env::var("VISUAL").or_else(|_| std::env::var("EDITOR")) {
        Ok(editor) => editor,
        Err(_) if cfg!(windows) => "notepad".to_string(),
        Err(_) => {
            return Err(format!(
                "没有设置 $VISUAL 或 $EDITOR — 文件在 {}",
                path.display()
            ))
        }
    };
    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .map_err(|e| format!("运行 {editor:?} 失败：{e}"))?;
    if !status.success() {
        return Err(format!("{editor} 退出状态 {status}"));
    }
    Ok(())
}

/// Purpose: Ask for a provider and key, validate it, and write the secret and the models where a local install keeps them.
/// Serves: `local add` and `tui --local`'s first run.
/// Design: `docs/design/surfaces.md` §2; `docs/design/config-routing.md` §3.
/// Up: `local`'s `add` arm and `tui.rs`'s `full_screen`; no other caller.
/// Down: `exported_keys`, `probe_provider`, `provider_key_for_probe`, `offerable_models`, `loong_tui_app::local_wizard` and `adopt_local_model`.
/// Verify: `cargo test -p loongagent --test local_mode`.
/// Ask for one API key, validate it, and write it where a local install keeps things.
///
/// `always` decides whether an already-configured install is asked again. A first run
/// asks because there is nothing to run otherwise; `local add` asks because asking is
/// the whole point of it.
/// `only` names a provider to configure directly, which skips the list — the difference
/// between `local add` and `local add dashscope`, and the reason re-configuring a provider
/// you already have does not make you find it in a menu again.
pub(crate) fn configure_local(
    settings: &loong_config::Profile,
    always: bool,
    only: Option<&str>,
) -> Result<(), String> {
    let already = !settings.models.is_empty();
    if already && !always {
        return Ok(());
    }
    let home = config_dir();

    // Ask. The catalog is offered in its own order; the key's shape is not used to
    // guess, because no vendor here documents a format to guess from.
    //
    // A key this machine already exported — from the shell, or from the workspace's
    // `.env` — is **offered**, never taken. Adopting it silently was the first version
    // of this and it was wrong twice over: it configured models nobody chose, and it did
    // so from a file (`<repo>/.env`) that belongs to a repository rather than to the
    // person, so what looked like "your key" could be a checkout's.
    // A local server first: it is the cheapest tier by definition, it is the one that
    // needs nothing pasted, and if a key *is* found in the environment the prefill
    // moves the highlight to that provider anyway.
    let choices: Vec<loong_tui_app::ProviderChoice> = {
        let mut ordered: Vec<&loong_config::catalog::Provider> = loong_config::catalog::PROVIDERS
            .iter()
            .filter(|p| p.local)
            .collect();
        ordered.extend(loong_config::catalog::PROVIDERS.iter().filter(|p| !p.local));
        if let Some(named) = only {
            ordered.retain(|p| p.id == named);
            if ordered.is_empty() {
                return Err(format!(
                    "服务商目录里没有 {named:?} — 有的是：{}",
                    loong_config::catalog::ids().join(", ")
                ));
            }
        }
        ordered
            .into_iter()
            .map(|p| loong_tui_app::ProviderChoice {
                id: p.id.to_string(),
                label: p.label.to_string(),
                needs_key: !p.local,
            })
            .collect()
    };
    // Asked for only once the arguments are known to make sense — a wrong provider name
    // is a wrong provider name whether or not there is a terminal to draw a form on, and
    // reporting the terminal first would send somebody to check their TTY over a typo.
    loong_tui_app::full_screen_needs_a_terminal()?;
    let offered = exported_keys();

    let mut validate = |id: &str, key: &str| -> loong_tui_app::Validation {
        let Some(provider) = loong_config::catalog::provider(id) else {
            return loong_tui_app::Validation {
                ok: false,
                summary: format!("catalog has no provider {id:?}"),
                ..Default::default()
            };
        };
        // A loopback provider gets no key, and passing an empty string as one would
        // put an `Authorization: Bearer ` header on a request that needs none.
        let probe = probe_provider(provider, provider_key_for_probe(provider, Some(key)));
        // On a refusal the key's shape is finally worth something — not as a decision,
        // but as an ordering hint for the next attempt that costs no extra request.
        let suggest_order = match &probe {
            loong_providers::probe::Probe::Ok { .. } => Vec::new(),
            _ => loong_config::catalog::guess_from_key(key)
                .into_iter()
                .map(str::to_string)
                .collect(),
        };
        // A rejected key is the moment the wrong door is worth naming. Several of these
        // vendors sell two products on two hosts with two key types, and a generic
        // "rejected" sends somebody to re-issue a credential that was never the problem.
        let mut summary = loong_providers::probe::render(&probe);
        if let loong_providers::probe::Probe::Rejected { .. } = &probe {
            if let Some(hint) = provider.wrong_door {
                summary.push_str(&format!(" — {hint}"));
            }
        }
        let (models, curated) = offerable_models(provider, &probe);
        if curated {
            summary = format!(
                "连接成功（密钥有效，只是这个端点不返回模型列表）— \
                 以下是 {} 文档里列的 {} 个",
                provider.label,
                provider.curated_models.len()
            );
        }
        loong_tui_app::Validation {
            ok: probe.is_ok(),
            summary,
            models,
            suggest_order,
        }
    };

    let Some(choice) = loong_tui_app::local_wizard(choices, offered, &mut validate)? else {
        eprintln!(
            "[local] 跳过配置 — 本次会话没有模型。随时重跑 `loongagent tui --local`，\
             或先试 `--model demo-write`。"
        );
        return Ok(());
    };
    let provider = loong_config::catalog::provider(&choice.provider)
        .ok_or_else(|| format!("catalog has no provider {:?}", choice.provider))?;
    let probe = loong_providers::probe::Probe::Ok {
        models: choice
            .validation
            .models
            .iter()
            .map(|(id, window)| loong_providers::probe::ModelInfo {
                id: id.clone(),
                context_window: *window,
            })
            .collect(),
    };
    adopt_local_model(&home, provider, &choice.key, &probe, already)?;
    Ok(())
}

/// Purpose: Check every distinct provider on a thread and report health, never removing a model on one bad answer.
/// Serves: `tui.rs`'s `full_screen`, so the interface opens before the network answers.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `tui.rs`'s `full_screen`; no other caller.
/// Down: `loong_config::catalog`, `provider_key`, `probe_provider`, `provider_key_for_probe` and `std::thread::spawn`.
/// Verify: `cargo test -p loongagent`.
/// Check every model this session offers, **without holding up the session**.
///
/// One probe per distinct provider, on a thread, because the alternative is worse in
/// both directions: checking before the interface opens would add a network round trip
/// to every start including the ones that work, and not checking at all would leave a
/// revoked key to be discovered by a failed turn.
///
/// Models are reported, never removed. A key revoked at midnight and a network hiccup
/// on the train look identical from here, and quietly deleting somebody's model over
/// the second would be worse than saying the check did not get through.
pub(crate) fn spawn_model_health(
    settings: &loong_config::Profile,
) -> Option<std::sync::mpsc::Receiver<loong_tui_app::ModelHealth>> {
    use std::sync::mpsc::channel;

    // Group by provider: two DeepSeek models share one key and one endpoint, and
    // probing twice would be paying twice for the same answer.
    let mut by_provider: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for spec in &settings.models {
        if let Some(provider) = &spec.provider {
            by_provider
                .entry(provider.clone())
                .or_default()
                .push(spec.name.clone());
        }
    }
    if by_provider.is_empty() {
        // Hand-written endpoints have no provider to probe, and inventing one from a
        // `base_url` would be guessing at somebody's gateway.
        return None;
    }

    let (sender, receiver) = channel();
    std::thread::spawn(move || {
        for (id, names) in by_provider {
            let Some(provider) = loong_config::catalog::provider(&id) else {
                for name in names {
                    let _ = sender.send(loong_tui_app::ModelHealth {
                        name,
                        ok: false,
                        detail: format!("服务商目录里没有 {id:?}"),
                    });
                }
                continue;
            };
            let key = provider_key(provider);
            let probe = probe_provider(
                provider,
                provider_key_for_probe(provider, key.as_ref().map(|(_, key)| key.as_str())),
            );
            let (ok, detail) = (probe.is_ok(), loong_providers::probe::render(&probe));
            for name in names {
                if sender
                    .send(loong_tui_app::ModelHealth {
                        name,
                        ok,
                        detail: detail.clone(),
                    })
                    .is_err()
                {
                    return; // the interface went away; nothing left to tell
                }
            }
        }
    });
    Some(receiver)
}

/// Purpose: A provider's key from the environment, with the variable it came from.
/// Contract: `local.rs` catalog probing; `local_check`, `spawn_model_health` and `exported_keys` call it.
/// The key for a provider, from wherever it lives, with the variable it came from.
fn provider_key(provider: &loong_config::catalog::Provider) -> Option<(String, String)> {
    let names = std::iter::once(provider.key_env).chain(provider.aliases.iter().copied());
    names.into_iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(|value| (name.to_string(), value))
    })
}

/// Purpose: Every provider this machine already has a key for, so the wizard can offer rather than adopt.
/// Serves: `configure_local`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `configure_local`; no other caller.
/// Down: `provider_key` and `loong_config::catalog::PROVIDERS`.
/// Verify: `cargo test -p loongagent`.
/// Every provider this machine already has a key for, so the wizard can *offer* them.
///
/// The variable *name* is the whole signal, which is why this needs no heuristics:
/// `MOONSHOT_API_KEY` is Moonshot's because the catalog lists it under Moonshot. An
/// unfamiliar variable is not guessed at — and finding one is not permission to use it.
/// The wizard names it on the form and the person presses enter, or types their own.
fn exported_keys() -> Vec<loong_tui_app::OfferedKey> {
    loong_config::catalog::PROVIDERS
        .iter()
        .filter(|provider| !provider.local)
        .filter_map(|provider| {
            provider_key(provider).map(|(var, key)| loong_tui_app::OfferedKey {
                provider: provider.id.to_string(),
                var,
                key,
            })
        })
        .collect()
}

/// Purpose: Which models to put in front of somebody, and whether the vendor's documented list filled in.
/// Serves: `configure_local`'s validation result.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `configure_local`; the `local.rs` tests.
/// Down: `loong_providers::probe::model_ids` and the catalog's curated models.
/// Verify: `cargo test -p loongagent`.
/// Which models to put in front of somebody for this provider, and whether they came from
/// the catalog rather than from the endpoint.
///
/// Some hosts authenticate and then name nothing. When that happens the vendor's
/// documented ids are a reference rather than a guess, and four rows somebody can tick are
/// far better than the empty picker that leaves a working key with no way to use it.
///
/// A fallback, not a substitute: if the endpoint names models, those win.
///
/// The context windows travel with the curated ids on purpose. They are what the kernel
/// compacts against, and a model with no window is one it will never compact on time.
fn offerable_models(
    provider: &loong_config::catalog::Provider,
    probe: &loong_providers::probe::Probe,
) -> (Vec<(String, Option<u64>)>, bool) {
    let named = loong_providers::probe::model_ids(probe);
    if named.is_empty() && probe.is_ok() && !provider.curated_models.is_empty() {
        return (
            provider
                .curated_models
                .iter()
                .map(|model| (model.id.to_string(), model.context_window))
                .collect(),
            true,
        );
    }
    let models = named
        .into_iter()
        .map(|id| {
            let window = match probe {
                loong_providers::probe::Probe::Ok { models } => models
                    .iter()
                    .find(|m| m.id == id)
                    .and_then(|m| m.context_window),
                _ => None,
            };
            (id, window)
        })
        .collect();
    (models, false)
}

/// Purpose: What to send as the bearer token, which is nothing for a loopback server.
/// Contract: `local.rs` probing; `configure_local` and `spawn_model_health` call it.
/// What to send as the bearer token when checking this provider.
///
/// `None` for a loopback server. Sending `Some("")` would put `Authorization: Bearer `
/// on a request that wants no header at all — some servers tolerate that and some reject
/// it, which is not a difference worth discovering on somebody's first run.
fn provider_key_for_probe<'a>(
    provider: &loong_config::catalog::Provider,
    typed: Option<&'a str>,
) -> Option<&'a str> {
    if provider.local {
        return None;
    }
    typed.filter(|key| !key.trim().is_empty())
}

/// Purpose: Check one provider's key by the cheapest route its catalog entry names.
/// Serves: `configure_local` and `spawn_model_health`.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `configure_local` and `spawn_model_health`; no other caller.
/// Down: `Catalog::models_url` and `loong_providers::probe::{probe_models_url, probe_with_completion}`.
/// Verify: `cargo test -p loongagent`.
/// Check one provider's key, using the free route where the vendor publishes one.
fn probe_provider(
    provider: &loong_config::catalog::Provider,
    key: Option<&str>,
) -> loong_providers::probe::Probe {
    match (provider.models_url(), provider.probe_model) {
        // The URL the catalog names, not `{base}/models`: for Ollama those differ, and
        // the one that knows the context window is the one worth asking.
        (Some(url), _) => loong_providers::probe::probe_models_url(&url, key),
        (None, Some(model)) => {
            loong_providers::probe::probe_with_completion(provider.base_url, model, key)
        }
        // A catalog entry with neither cannot be validated at all, and saying so is
        // better than a probe against a route we know is not there.
        (None, None) => loong_providers::probe::Probe::Unreachable {
            detail: format!(
                "{} 没有 /models 路由，也没有可用于校验的模型 — 无法校验这个 key",
                provider.label
            ),
        },
    }
}

/// Purpose: Write the key to the env file and the models to `local.toml`, appending rather than replacing.
/// Serves: `configure_local` once the wizard returns a validated choice.
/// Design: `docs/design/config-routing.md` §3 and §5.
/// Up: `configure_local`; no other caller.
/// Down: `write_env_secret`, `replacement_models`, `upsert_local_models` and `std::env::set_var`.
/// Verify: `cargo test -p loongagent --test local_mode`.
/// Write the key where a secret belongs, and the models into the **local** file.
///
/// Appending, not replacing: a second run is how somebody adds their *second* provider,
/// and the models they configured last time must survive it. Re-adding a provider
/// replaces that provider's own models and leaves every other provider alone.
fn adopt_local_model(
    home: &Path,
    provider: &loong_config::catalog::Provider,
    key: &str,
    probe: &loong_providers::probe::Probe,
    had_models: bool,
) -> Result<(), String> {
    // A loopback provider has no secret, and writing an empty one would create a file
    // that looks like a key store with a blank key in it.
    if !key.trim().is_empty() {
        write_env_secret(home, provider.key_env, key)?;
        // The child inherits this process's environment, so setting it here is what
        // makes the *first* run use the key rather than only the second one.
        std::env::set_var(provider.key_env, key);
    }

    let path = loong_config::local_profile_path();
    let changed = match replacement_models(probe, &provider.id) {
        Some(models) => upsert_local_models(&path, &provider.id, models)?,
        // The provider answered without naming anything. The key is worth keeping — it
        // checked out — but the *models* are not ours to remove, and that is the whole
        // point of this arm.
        None => {
            eprintln!(
                "[local] {} 已写入，但 {} 既没有返回模型列表、目录里也没有它的模型。\
                 已配置的模型保持不动。手工加一个：`loongagent local edit`，写入\n\
                 \n    [[models]]\n    name = \"<模型 id>\"\n    provider = \"{}/<模型 id>\"\n",
                home.join("env").display(),
                provider.label,
                provider.id
            );
            Vec::new()
        }
    };
    if !changed.is_empty() {
        eprintln!(
            "[local] 已写入 {}（{}）",
            path.display(),
            changed.join("、")
        );
    }
    if !had_models {
        eprintln!(
            "[local] 默认走第一个（最便宜那层）。换模型改 `provider` 那一行，\
             或用 `loongagent tui --local` 再配一个服务商。"
        );
    }
    Ok(())
}

/// Purpose: What a probe's answer should replace a provider's entries with, and `None` when it named nothing.
/// Serves: `adopt_local_model`, so an empty answer never deletes configured models.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `adopt_local_model`; the `local.rs` tests.
/// Down: no further call; it maps the probe.
/// Verify: `cargo test -p loongagent`.
/// What a probe's answer should replace this provider's entries with.
///
/// `None` for "the provider named nothing, so leave what is already there alone". That
/// distinction is the whole reason this is a function: an empty list *is* a legitimate
/// thing to write, and it deletes, so "nothing came back" and "nothing should remain" must
/// not be the same value. Getting them confused means re-adding a provider whose list came
/// back empty silently removes the models already configured for it — data loss dressed up
/// as a refresh.
fn replacement_models(
    probe: &loong_providers::probe::Probe,
    provider_id: &str,
) -> Option<Vec<LocalModel>> {
    let loong_providers::probe::Probe::Ok { models } = probe else {
        return None;
    };
    if models.is_empty() {
        return None;
    }
    Some(
        models
            .iter()
            .map(|model| LocalModel {
                name: model.id.clone(),
                provider: format!("{provider_id}/{}", model.id),
                context_window: model.context_window,
            })
            .collect(),
    )
}

/// Purpose: One model in `local.toml`, as this file understands it.
/// Serves: `LocalBlock`, `read_local_blocks` and `replacement_models`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `block_model` and `replacement_models` build it; `LocalBlock` carries it.
/// Down: no further call; it is data.
/// Verify: `cargo test -p loongagent`.
/// One model in `local.toml`, as this file understands it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalModel {
    name: String,
    provider: String,
    context_window: Option<u64>,
}

/// Purpose: A model and the exact text it is written as, so a person's comments survive a write.
/// Serves: `read_local_blocks`, `write_local_blocks` and `upsert_local_models`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `read_local_blocks` and `LocalBlock::render` build it; `write_local_blocks` writes it.
/// Down: `text` and `model`; no further call.
/// Verify: `cargo test -p loongagent`.
/// A model **and the text it is written as**.
///
/// The text is kept because this file is meant to be edited by a person: a comment inside
/// one model's block has to survive adding a model for a different provider, and rebuilding
/// every block from parsed fields deletes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalBlock {
    text: String,
    model: LocalModel,
}

impl LocalBlock {
    /// Purpose: Render a fresh `[[models]]` block for a model this program is adding.
    /// Serves: `upsert_local_models`.
    /// Design: `docs/design/config-routing.md` §3.
    /// Up: `upsert_local_models`; the `local.rs` tests.
    /// Down: `toml_escape`; no further call.
    /// Verify: `cargo test -p loongagent`.
    /// Render a block for a model this program is adding.
    fn render(model: LocalModel) -> Self {
        let mut text = String::from("[[models]]\n");
        text.push_str(&format!("name = \"{}\"\n", toml_escape(&model.name)));
        text.push_str(&format!(
            "provider = \"{}\"\n",
            toml_escape(&model.provider)
        ));
        if let Some(tokens) = model.context_window {
            text.push_str(&format!(
                "max_context_size = {tokens}   # 服务商自己报的上下文窗口\n"
            ));
        }
        LocalBlock { text, model }
    }
}

/// Purpose: Add or replace one provider's models in `local.toml` by text surgery, keeping everything else byte for byte.
/// Serves: `adopt_local_model` and `local_delete`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `adopt_local_model`; the `local.rs` tests.
/// Down: `read_local_blocks`, `write_local_blocks` and `LocalBlock::render`.
/// Verify: `cargo test -p loongagent`.
/// Add or replace one provider's models in `local.toml`, keeping everything else.
///
/// Deliberately **text surgery, not a re-serialize**: the file is meant to be read and
/// edited by a person, and the comments in it are the only documentation the models
/// have. Round-tripping through a TOML serializer would quietly delete every one of
/// them, so the blocks for this provider are swapped out in place and the rest of the
/// file is preserved byte for byte.
///
/// Returns the names it wrote, which is empty when the provider offered no models.
fn upsert_local_models(
    path: &Path,
    provider_id: &str,
    models: Vec<LocalModel>,
) -> Result<Vec<String>, String> {
    let mut blocks = read_local_blocks(path);
    // This provider's previous entries are being replaced, not duplicated — pasting a new
    // key for a provider already configured should update it. Every other provider's block
    // is left exactly as it was, comments and all.
    let prefix = format!("{provider_id}/");
    blocks.retain(|block| !block.model.provider.starts_with(&prefix));
    let names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    blocks.extend(models.into_iter().map(LocalBlock::render));
    if blocks.is_empty() {
        // Nothing was configured and nothing is being added: leave the file alone rather
        // than writing a header with no models under it.
        return Ok(names);
    }
    write_local_blocks(path, &blocks)?;
    Ok(names)
}

/// Purpose: Every model the file declares, in declaration order.
/// Contract: `local.rs` file editing; `local_delete`, `upsert_local_models` and the tests call it.
/// Every model the local file declares, in the order it declares them.
///
/// Read as data and written back as text, which is the shape this file needs: the
/// comments in it are the only documentation these settings have, so a pass through a
/// TOML serializer would quietly delete every one of them.
fn read_local_blocks(path: &Path) -> Vec<LocalBlock> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let (_, blocks, _) = split_model_blocks(&existing);
    blocks
        .into_iter()
        .filter_map(|text| {
            let model = block_model(&text)?;
            Some(LocalBlock { text, model })
        })
        .collect()
}

/// Purpose: Rewrite the model section, keeping the header and everything after it.
/// Serves: `local_delete` and `upsert_local_models`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `local_delete`, `upsert_local_models` and the `local.rs` tests.
/// Down: `split_model_blocks`, `local_header`, `route_tail` and `std::fs::write`.
/// Verify: `cargo test -p loongagent`.
/// Write the model section, keeping the header and everything after it.
fn write_local_blocks(path: &Path, blocks: &[LocalBlock]) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let (head, _, tail) = split_model_blocks(&existing);

    let mut text = if head.trim().is_empty() {
        local_header().to_string()
    } else {
        head
    };
    for block in blocks {
        // Trimmed, then one separator the writer owns. The text carries whatever blank
        // space followed it in the file, and writing that *plus* a separator added a line
        // on every write — ten of them after three deletions, which reads as a file that
        // has come apart even though nothing was lost.
        text.push_str(block.text.trim());
        text.push_str("\n\n");
    }
    text.push_str(&route_tail(&tail, blocks));

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(())
}

/// Purpose: What follows the models, with `router.default` pointed at a model that exists.
/// Serves: `write_local_blocks`, so a delete cannot leave a file that will not load.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `write_local_blocks`; no other caller.
/// Down: `toml_escape`; no further call.
/// Verify: `cargo test -p loongagent`.
/// What follows the models, with the router's default pointed at something that exists.
///
/// `router.default` is "the model a turn starts with", and the resolver **refuses** a
/// default naming a model that is not there — so removing the model it named has to move
/// it, and removing the last model has to take the section with it. Getting this wrong
/// leaves a file that will not load, which is a worse outcome than any of the edits.
fn route_tail(tail: &str, blocks: &[LocalBlock]) -> String {
    let Some(first) = blocks.first() else {
        return String::new();
    };
    let default_line = format!("default = \"{}\"\n", toml_escape(&first.model.name));
    if !tail.contains("[router]") {
        return format!("[router]\n{default_line}");
    }
    if !tail
        .lines()
        .any(|line| line.trim_start().starts_with("default"))
    {
        // The section is there but says nothing; the default goes directly under it, and
        // any comment somebody left in the section stays where it was.
        return tail.replacen("[router]", &format!("[router]\n{default_line}"), 1);
    }
    let mut out = String::new();
    let mut done = false;
    for line in tail.lines() {
        if !done && line.trim_start().starts_with("default") {
            out.push_str(&default_line);
            done = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Purpose: One `[[models]]` block read back as data, or `None` when it names no model.
/// Serves: `read_local_blocks`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `read_local_blocks`; no other caller.
/// Down: `quoted_value` and `number_value`.
/// Verify: `cargo test -p loongagent`.
/// One `[[models]]` block read back as data.
fn block_model(block: &str) -> Option<LocalModel> {
    let (mut name, mut provider, mut window) = (None, None, None);
    for line in block.lines() {
        let line = line.trim();
        if let Some(value) = quoted_value(line, "name") {
            name = Some(value);
        } else if let Some(value) = quoted_value(line, "provider") {
            provider = Some(value);
        } else if let Some(value) = number_value(line, "max_context_size") {
            window = Some(value);
        }
    }
    Some(LocalModel {
        name: name?,
        provider: provider?,
        context_window: window,
    })
}

/// Purpose: A `key = "value"` line's value, ignoring a trailing comment.
/// Contract: `local.rs` file parsing; `block_model` and `existing_summarizer_model` call it.
/// `key = "value"`, ignoring anything after the closing quote (this writer leaves a
/// trailing comment there).
fn quoted_value(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.trim_start().strip_prefix('=')?.trim();
    let rest = rest.strip_prefix('"')?;
    Some(rest.split('"').next()?.to_string())
}

/// Purpose: A `key = 4096` line's number, ignoring a trailing comment.
/// Contract: `local.rs` file parsing; `block_model` is the only caller.
/// `key = 4096`, ignoring a trailing comment.
fn number_value(line: &str, key: &str) -> Option<u64> {
    let rest = line.strip_prefix(key)?;
    let rest = rest.trim_start().strip_prefix('=')?.trim();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Purpose: Split a `local.toml` into the header, the model blocks and whatever follows them.
/// Serves: `read_local_blocks` and `write_local_blocks`.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `read_local_blocks` and `write_local_blocks`; the `local.rs` tests.
/// Down: `hand_comments_forward`; no further call.
/// Verify: `cargo test -p loongagent`.
/// Split a `local.toml` into what comes before the models, the `[[models]]` blocks
/// themselves, and whatever follows them (`[router]`, and anything hand-written).
///
/// The first model is the cheapest, and the router's `default` names it, so the tail is
/// left exactly where it was rather than being regenerated.
fn split_model_blocks(text: &str) -> (String, Vec<String>, String) {
    let mut head = String::new();
    let mut blocks: Vec<String> = Vec::new();
    let mut tail = String::new();
    let mut current: Option<String> = None;

    for line in text.lines() {
        if line.trim_start().starts_with("[[models]]") {
            match current.take() {
                Some(block) => blocks.push(block),
                None => {
                    // Everything before the first model block is the header, minus the
                    // blank line the writer leaves between them.
                    head = head.trim_end().to_string();
                    head.push('\n');
                    if !head.trim().is_empty() {
                        head.push('\n');
                    }
                }
            }
            current = Some(format!("{line}\n"));
            continue;
        }
        // A new top-level section ends the model blocks.
        if current.is_some() && line.trim_start().starts_with('[') {
            blocks.push(current.take().unwrap());
            tail.push_str(line);
            tail.push('\n');
            continue;
        }
        match &mut current {
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            }
            None => {
                if tail.is_empty() {
                    head.push_str(line);
                    head.push('\n');
                } else {
                    tail.push_str(line);
                    tail.push('\n');
                }
            }
        }
    }
    if let Some(block) = current.take() {
        blocks.push(block);
    }
    hand_comments_forward(&mut blocks);
    (head, blocks, tail)
}

/// Purpose: Give each block the comments written above it, which a line walk would otherwise leave on the one before.
/// Serves: `split_model_blocks`, so a note moves with the model it describes.
/// Design: `docs/design/config-routing.md` §3.
/// Up: `split_model_blocks`; no other caller.
/// Down: no further call; it moves lines.
/// Verify: `cargo test -p loongagent`.
/// Give each block the comments written **above** it.
///
/// The splitter is a line walk, so a note between two models lands at the end of the one
/// above. That is the wrong owner twice over: deleting the model below would leave the
/// note orphaned, and deleting the model *above* would take a note about something else
/// with it. A note written above a model is about that model, so it moves down.
fn hand_comments_forward(blocks: &mut [String]) {
    for index in 0..blocks.len().saturating_sub(1) {
        let lines: Vec<&str> = blocks[index].lines().collect();
        // The trailing run of blank and comment lines.
        let mut cut = lines.len();
        while cut > 0 {
            let line = lines[cut - 1].trim();
            if line.is_empty() || line.starts_with('#') {
                cut -= 1;
            } else {
                break;
            }
        }
        // Only a real comment is worth moving; blank lines are spacing, and the writer
        // owns spacing.
        if cut == lines.len() || !lines[cut..].iter().any(|l| l.trim_start().starts_with('#')) {
            continue;
        }
        let moved: String = lines[cut..]
            .iter()
            .map(|line| format!("{line}\n"))
            .collect();
        let kept: String = lines[..cut]
            .iter()
            .map(|line| format!("{line}\n"))
            .collect();
        blocks[index] = kept;
        blocks[index + 1] = format!("{moved}{}", blocks[index + 1]);
    }
}

/// Purpose: The commented header a freshly created `local.toml` gets.
/// Contract: `local.rs` file writing; `write_local_blocks` is the only caller.
/// The header a freshly created `local.toml` gets. The comments are the only
/// documentation these settings have.
fn local_header() -> &'static str {
    "# loongagent — 本地模式专用。与 `profile.toml`（受管模式）完全独立，互不影响。\n\
     #\n\
     # `[[models]]` 的顺序就是能力从低到高的顺序，也是状态栏里 1、2、3… 的顺序。\n\
     # 第一个是启动默认；把它放在最便宜的那一层，升级就只按一次键。\n\
     # 换模型只改 `provider` 这一行：`<服务商>/<模型 id>`，base_url 由服务商目录提供，\n\
     # 密钥按约定名（如 LOONG_DEEPSEEK_KEY）从 `~/.loongagent/env` 或环境变量读取。\n\
     #\n\
     # 这个文件只在 `loongagent tui --local` 时被读取。\n\n"
}

/// Purpose: Escape a value for a TOML basic string.
/// Contract: `local.rs` file writing; `render`, `route_tail` and `write_semantic_config` call it.
/// Escape a value for a TOML basic string. Model ids are vendor-supplied and short,
/// but the file is written by us and read by a parser, so the two must agree.
fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Purpose: Put a secret in the `env` file, replacing only the previous definition of the same name.
/// Serves: `adopt_local_model`.
/// Design: `docs/design/config-routing.md` §5.
/// Up: `adopt_local_model`; no other caller.
/// Down: `std::fs::write` and `restrict_permissions`.
/// Verify: `cargo test -p loongagent`.
/// Put a secret in the `env` file, replacing any previous value for the same name.
///
/// `~/.loongagent/env` is read at startup (see [`read_env_files`]) and is the one place
/// a local install keeps secrets — never in `profile.toml`, which ADR-0008 §5 will
/// refuse to load if it finds a literal there.
fn write_env_secret(home: &Path, name: &str, value: &str) -> Result<(), String> {
    std::fs::create_dir_all(home).map_err(|e| format!("{}: {e}", home.display()))?;
    let path = home.join("env");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|line| {
            // Drop the old definition of this name, and nothing else: the file may
            // hold keys for providers the user configured earlier.
            let line = line.trim().trim_start_matches("export ").trim_start();
            let name_part = line.split(['=', ':']).next().unwrap_or_default().trim();
            name_part != name
        })
        .map(str::to_string)
        .collect();
    lines.push(format!("{name}={value}"));
    let mut text = lines.join("\n");
    text.push('\n');
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    restrict_permissions(&path);
    Ok(())
}

/// Purpose: Make the secret file readable only by its owner.
/// Contract: `local.rs`; `write_env_secret` is the only caller.
/// A secret file that the world can read is not a secret file.
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_cmd::{env_file_paths, read_credential, read_env_files};
    use crate::control::{control_config, merge_control};

    // ------------------------------------------------ the local model file

    /// Purpose: A fresh `local.toml` path under the test's own directory.
    /// Contract: `local.rs` tests; every file-editing test calls it.
    fn local_temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loongagent-localfile-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("local.toml")
    }

    /// Purpose: A `LocalModel` fixture with a fixed context window.
    /// Contract: `local.rs` tests; the file-editing tests call it.
    fn model(name: &str, provider: &str) -> LocalModel {
        LocalModel {
            name: name.to_string(),
            provider: provider.to_string(),
            context_window: Some(4096),
        }
    }

    /// Purpose: How many real section headers a file has, ignoring the header comment that mentions them.
    /// Contract: `local.rs` tests; the file-shape tests call it.
    /// How many real sections a file has. Counting occurrences of the text would also
    /// count the header comments that *mention* `[[models]]`.
    fn sections(text: &str, header: &str) -> usize {
        text.lines()
            .filter(|line| line.trim_start().starts_with(header))
            .count()
    }

    // ------------------------------------------------ the semantic switch

    /// Purpose: A semantic-write fixture with fixed window budgets.
    /// Contract: `local.rs` tests; the semantic-switch tests call it.
    fn semantic(model: &str) -> SemanticWrite {
        SemanticWrite::Semantic {
            base_url: DEFAULT_SUMMARIZER_URL.to_string(),
            model: model.to_string(),
            window_calls: 4,
            window_tokens: 6000,
        }
    }

    /// Purpose: Resolve one file as a user layer, the way a session would read it.
    /// Contract: `local.rs` tests; the semantic-switch tests call it.
    fn resolved(path: &Path) -> loong_config::Profile {
        loong_config::resolve(
            &[(loong_config::Layer::User, path.to_path_buf())],
            path.parent().unwrap(),
        )
        .unwrap()
    }

    /// Purpose: Turning semantic on writes a file a session resolves, and the configured models and comments survive.
    #[test]
    fn semantic_on_writes_a_file_that_resolves_and_keeps_the_models() {
        let path = local_temp("semantic-on");
        std::fs::write(
            &path,
            "# a comment somebody wrote\n[[models]]\nname = \"cheap\"\n\
             provider = \"ollama/cheap\"\n\n[router]\ndefault = \"cheap\"\n",
        )
        .unwrap();

        write_semantic_config(&path, &semantic("qwen2.5:7b-instruct")).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# a comment somebody wrote"), "{text}");
        assert!(text.contains("name = \"cheap\""), "{text}");
        assert!(text.contains("default = \"cheap\""), "{text}");

        let profile = resolved(&path);
        assert_eq!(profile.models.len(), 1, "the models survived");
        let engine = profile.context_engine().expect("semantic is on");
        assert_eq!(engine.mode, "semantic");
        assert_eq!(engine.summarizer, "ollama");
        assert_eq!(
            engine.summarizer_model.as_deref(),
            Some("qwen2.5:7b-instruct")
        );
        assert_eq!(
            engine.summarizer_base_url.as_deref(),
            Some(DEFAULT_SUMMARIZER_URL)
        );
        assert_eq!(engine.summarizer_window_calls, Some(4));
    }

    /// Purpose: Repeating the switch replaces its tables instead of appending a duplicate, and is byte-idempotent.
    #[test]
    fn switching_semantic_on_twice_does_not_duplicate_its_tables() {
        // A duplicate `[context_engine]` is a file that will not load, so the second
        // `on` has to replace rather than append. Repeating it must also be *byte*
        // idempotent: anything written outside the section would be copied forward on
        // every switch, which is how a file comes apart without anything being lost.
        let path = local_temp("semantic-twice");
        write_semantic_config(&path, &semantic("qwen2.5:3b")).unwrap();
        write_semantic_config(&path, &semantic("qwen2.5:7b-instruct")).unwrap();
        let once = std::fs::read_to_string(&path).unwrap();
        assert_eq!(sections(&once, "[context_engine]"), 1, "{once}");
        assert_eq!(sections(&once, "[summarizer]"), 1, "{once}");
        assert!(
            !once.contains("qwen2.5:3b"),
            "the old model is gone: {once}"
        );
        write_semantic_config(&path, &semantic("qwen2.5:7b-instruct")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            once,
            "writing the same setting twice is a no-op"
        );
    }

    /// Purpose: Off means the engine is off, not merely empty, and the models remain.
    #[test]
    fn semantic_off_is_a_real_off_and_keeps_the_models() {
        let path = local_temp("semantic-off");
        std::fs::write(
            &path,
            "[[models]]\nname = \"cheap\"\nprovider = \"ollama/cheap\"\n\n[router]\ndefault = \"cheap\"\n",
        )
        .unwrap();
        write_semantic_config(&path, &semantic("qwen2.5:7b-instruct")).unwrap();
        write_semantic_config(&path, &SemanticWrite::Off).unwrap();

        let profile = resolved(&path);
        assert_eq!(profile.context_engine(), None, "off means off");
        assert_eq!(profile.context_engine_enabled, Some(false));
        assert_eq!(profile.models.len(), 1);
    }

    /// Purpose: Record mode needs no model and keeps the deterministic retrieval layer.
    #[test]
    fn record_mode_turns_the_model_off_and_keeps_the_retrieval_layer() {
        let path = local_temp("semantic-record");
        write_semantic_config(&path, &SemanticWrite::Record).unwrap();
        let engine = resolved(&path).context_engine().expect("record is on");
        assert_eq!(engine.mode, "record");
        assert_eq!(engine.summarizer, "off", "record needs no model");
    }

    /// Purpose: Only Ollama is accepted as a summariser, and the refusal says why.
    #[test]
    fn a_remote_summariser_provider_is_refused_with_the_reason() {
        // The provider list is short on purpose. Accepting `deepseek/…` here and
        // silently doing nothing is the failure this check exists to prevent.
        let error = parse_summarizer_selector(Some("deepseek/deepseek-flash")).unwrap_err();
        assert!(error.contains("ollama"), "{error}");
        assert!(error.contains("think"), "and says why: {error}");
        assert_eq!(
            parse_summarizer_selector(Some("ollama/qwen2.5:3b"))
                .unwrap()
                .1
                .as_deref(),
            Some("qwen2.5:3b")
        );
        assert_eq!(
            parse_summarizer_selector(Some("qwen2.5:3b"))
                .unwrap()
                .1
                .as_deref(),
            Some("qwen2.5:3b")
        );
        assert_eq!(parse_summarizer_selector(None).unwrap(), ("ollama", None));
    }

    /// Purpose: The model choice keeps the configured one, prefers the measured default, and refuses to guess among several.
    #[test]
    fn choosing_a_model_refuses_to_guess_among_several() {
        let several = vec!["qwen3:0.6b".to_string(), "qwen2.5:3b".to_string()];
        assert!(
            choose_model(&several, None).is_err(),
            "picking between 0.6B and 3B is a quality decision, not a coin toss"
        );
        // What the person configured last stays theirs while it is still installed.
        assert_eq!(
            choose_model(&several, Some("qwen2.5:3b")).unwrap(),
            "qwen2.5:3b"
        );
        // The measured default wins over an arbitrary first entry.
        let with_default = vec!["qwen3:0.6b".to_string(), "qwen2.5:7b-instruct".to_string()];
        assert_eq!(
            choose_model(&with_default, None).unwrap(),
            "qwen2.5:7b-instruct"
        );
        assert_eq!(
            choose_model(&["llama3:8b".to_string()], None).unwrap(),
            "llama3:8b"
        );
        assert!(choose_model(&[], None).unwrap_err().contains("ollama"));
    }

    /// Purpose: A summariser URL typed with `/v1` is corrected to Ollama's native endpoint.
    #[test]
    fn the_summariser_endpoint_drops_the_openai_shim_suffix() {
        assert_eq!(
            summarizer_base_url(Some("http://127.0.0.1:11434/v1/")),
            "http://127.0.0.1:11434"
        );
        assert_eq!(summarizer_base_url(None), DEFAULT_SUMMARIZER_URL);
        assert_eq!(
            summarizer_base_url(Some("http://box:11434/")),
            "http://box:11434"
        );
    }

    /// Purpose: A key or comment the command does not own survives a semantic switch.
    #[test]
    fn a_hand_written_key_survives_a_switch() {
        // The command owns `enabled`/`mode`/the endpoint keys. A key it does not own —
        // or a comment — is not its to delete.
        let path = local_temp("semantic-extra");
        std::fs::write(
            &path,
            "[context_engine]\n# my note\nenabled = true\nmode = \"semantic\"\nepisode_events = 32\n",
        )
        .unwrap();
        write_semantic_config(&path, &SemanticWrite::Off).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my note"), "{text}");
        assert!(text.contains("episode_events = 32"), "{text}");
        assert!(text.contains("enabled = false"), "{text}");
        assert_eq!(sections(&text, "[context_engine]"), 1, "{text}");
    }

    /// Purpose: A local session with no model is refused and names `local add`, while a managed one is left alone.
    #[test]
    fn a_local_session_needs_a_model_before_the_terminal_is_taken_over() {
        // A session with nothing to run cannot answer anything, and its first minutes are
        // spent discovering that. Cancelling the wizard is the case this exists for.
        let mut args = Args {
            session: None,
            session_spec: None,
            new_session: false,
            session_source: SessionSource::Created,
            cwd: std::env::temp_dir(),
            model: None,
            profile: None,
            local: true,
            bind: "127.0.0.1".to_string(),
            control_url: None,
            control_key: None,
            control_token: None,
            control_system: false,
            port: 0,
            stdio: false,
            budget: None,
            keep_newest: None,
            max_steps: None,
            summarizer_base_url: None,
            record: false,
            debug_model: false,
            name: None,
            git_ref: None,
            project: false,
            user: false,
            skills: None,
            bootstrap: Vec::new(),
            force: false,
            positional: Vec::new(),
        };
        let empty = loong_config::Profile::default();
        let err = session_needs_a_model(&args, true, &empty).unwrap_err();
        assert!(err.contains("local add"), "and names the command: {err}");

        // With a model, the session opens.
        let mut with = loong_config::Profile::default();
        with.models.push(named("cheap"));
        assert!(session_needs_a_model(&args, true, &with).is_ok());

        // `--model demo-write` is its own answer: a session asked for explicitly has one.
        args.model = Some("demo-write".to_string());
        assert!(session_needs_a_model(&args, true, &empty).is_ok());
        args.model = None;

        // A managed install is left alone: its models come from a pack the user cannot
        // edit, so refusing would strand them with no interface and no way to fix it.
        assert!(session_needs_a_model(&args, false, &empty).is_ok());
    }

    /// Purpose: A `ModelSpec` fixture with a name and a placeholder endpoint.
    /// Contract: `local.rs` tests; the model-necessity test calls it.
    /// A `ModelSpec` with a name and nothing else.
    fn named(name: &str) -> loong_config::ModelSpec {
        loong_config::ModelSpec {
            name: name.to_string(),
            kind: "openai-compat".to_string(),
            base_url: "http://127.0.0.1:1/v1".to_string(),
            model: name.to_string(),
            api_key: None,
            budget_tokens: None,
            max_context_size: None,
            provider: None,
        }
    }

    /// Purpose: A host that authenticates but lists nothing falls back to the vendor's documented models, with windows.
    #[test]
    fn a_host_that_names_no_models_falls_back_to_the_ones_its_vendor_documents() {
        // A host can authenticate and then name nothing, and the result is a working key
        // with no way to use it: "没有返回可用的模型列表" and a configured-models-unchanged
        // message, which is a dead end at the one moment everything else had gone right.
        use loong_providers::probe::Probe;

        let kimi = loong_config::catalog::provider("kimi-code").expect("in the catalog");
        assert!(
            !kimi.curated_models.is_empty(),
            "the vendor documents four ids; the catalog has to carry them"
        );

        let silent = Probe::Ok { models: Vec::new() };
        let (models, curated) = offerable_models(kimi, &silent);
        assert!(curated, "and the fallback is what filled the list");
        let ids: Vec<&str> = models.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "k3",
                "k3-256k",
                "kimi-for-coding",
                "kimi-for-coding-highspeed"
            ]
        );
        // The windows matter as much as the names: they are what the kernel compacts
        // against, and a model with no window is one it never compacts on time.
        assert_eq!(
            models.iter().find(|(id, _)| id == "k3").unwrap().1,
            Some(1_048_576)
        );

        // A host that *does* enumerate is believed, and the fallback stays out of it.
        let named = Probe::Ok {
            models: vec![loong_providers::probe::ModelInfo {
                id: "whatever-they-serve".to_string(),
                context_window: Some(4096),
            }],
        };
        let (models, curated) = offerable_models(kimi, &named);
        assert!(!curated, "a real answer is not second-guessed");
        assert_eq!(
            models,
            vec![("whatever-they-serve".to_string(), Some(4096))]
        );

        // A provider with no curated list and no answer offers nothing, as before.
        let plain = loong_config::catalog::provider("deepseek").unwrap();
        assert!(plain.curated_models.is_empty());
        let (models, curated) = offerable_models(plain, &silent);
        assert!(models.is_empty() && !curated);

        // And a *failed* probe never falls back: a rejected key must not be dressed up
        // as a set of models to choose from.
        let rejected = Probe::Rejected {
            status: 401,
            detail: "no".to_string(),
            region: false,
        };
        let (models, curated) = offerable_models(kimi, &rejected);
        assert!(models.is_empty() && !curated);
    }

    /// Purpose: Repeated writes and deletes never grow the file's blank space.
    #[test]
    fn a_write_leaves_one_blank_line_between_models_however_often_it_runs() {
        // The bug: each block's text carried the blank space that *followed* it, and the
        // writer added a separator on top. Every write grew the file by a line — ten of
        // them after three deletions, which looks like a file coming apart even though
        // nothing was lost.
        let path = local_temp("no-blank-growth");
        let mut blocks = vec![
            LocalBlock::render(model("a", "deepseek/a")),
            LocalBlock::render(model("b", "deepseek/b")),
        ];
        write_local_blocks(&path, &blocks).unwrap();
        let first = std::fs::read_to_string(&path).unwrap();
        assert_eq!(first.matches("\n\n\n").count(), 0, "{first:?}");

        // Re-writing the same content, and deleting then adding, must not drift.
        for _ in 0..5 {
            write_local_blocks(&path, &blocks).unwrap();
        }
        let later = std::fs::read_to_string(&path).unwrap();
        assert_eq!(later, first, "five more writes changed nothing");

        blocks.remove(0);
        write_local_blocks(&path, &blocks).unwrap();
        let after_delete = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after_delete.matches("\n\n\n").count(),
            0,
            "a deletion does not leave the hole behind: {after_delete:?}"
        );
    }

    /// Purpose: A note written above a model moves with it when the model below or above is deleted.
    #[test]
    fn a_comment_above_a_model_belongs_to_that_model() {
        // The splitter is a line walk, so a note written above a model ends up at the end
        // of the one before it. That is wrong twice: deleting the model below orphans the
        // note, and deleting the model above takes a note about something else with it.
        let path = local_temp("comment-owner");
        std::fs::write(
            &path,
            "[[models]]\nname = \"a\"\nprovider = \"deepseek/a\"\n\n\
             [[models]]\nname = \"b\"\nprovider = \"deepseek/b\"\n\n\
             # b 是旗舰\n[[models]]\nname = \"c\"\nprovider = \"deepseek/c\"\n\n\
             [router]\ndefault = \"a\"\n",
        )
        .unwrap();

        let mut blocks = read_local_blocks(&path);
        assert_eq!(blocks.len(), 3);
        assert!(
            blocks[2].text.contains("# b 是旗舰"),
            "the note moved down to the block it describes: {:?}",
            blocks[2].text
        );
        assert!(
            !blocks[1].text.contains("# b 是旗舰"),
            "and left the one above: {:?}",
            blocks[1].text
        );

        // Deleting what is above it leaves the note with its model.
        blocks.remove(1);
        write_local_blocks(&path, &blocks).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# b 是旗舰"), "{text}");
        assert!(text.contains("name = \"c\""), "{text}");
    }

    /// Purpose: An empty, rejected or unreachable probe leaves configured models alone.
    #[test]
    fn a_probe_that_named_nothing_does_not_delete_what_is_already_there() {
        // "Nothing came back" and "nothing should remain" are different things, and an
        // empty list means the second. Confusing them makes `local add <provider>` silently
        // remove that provider's models whenever its list endpoint hiccups.
        use loong_providers::probe::{ModelInfo, Probe};

        assert!(
            replacement_models(&Probe::Ok { models: Vec::new() }, "deepseek").is_none(),
            "an empty answer leaves the file alone"
        );
        for unreachable in [
            Probe::Unreachable {
                detail: "timeout".to_string(),
            },
            Probe::Rejected {
                status: 401,
                detail: "no".to_string(),
                region: false,
            },
        ] {
            assert!(
                replacement_models(&unreachable, "deepseek").is_none(),
                "and so does one that never arrived"
            );
        }

        let answered = Probe::Ok {
            models: vec![ModelInfo {
                id: "deepseek-flash".to_string(),
                context_window: Some(1_048_576),
            }],
        };
        let written = replacement_models(&answered, "deepseek").expect("a named model is written");
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].name, "deepseek-flash");
        assert_eq!(written[0].provider, "deepseek/deepseek-flash");
        assert_eq!(written[0].context_window, Some(1_048_576));
    }

    /// Purpose: Deleting the model the router names moves the default to one that still exists.
    #[test]
    fn a_deleted_model_moves_the_router_default_rather_than_leaving_it_dangling() {
        // `router.default` naming a model that is not there does not load, so removing the
        // model it named has to move it. Getting this wrong leaves a file that cannot be
        // read, which is worse than any of the edits that lead to it.
        let path = local_temp("delete-default");
        upsert_local_models(
            &path,
            "deepseek",
            vec![model("ds-flash", "deepseek/ds-flash")],
        )
        .unwrap();
        upsert_local_models(&path, "moonshot", vec![model("kimi", "moonshot/kimi")]).unwrap();

        let mut blocks = read_local_blocks(&path);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].model.name, "ds-flash");
        blocks.remove(0);
        write_local_blocks(&path, &blocks).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("default = \"kimi\""), "{text}");
        assert!(!text.contains("ds-flash"), "{text}");
    }

    /// Purpose: Deleting the last model removes the router section, so the file still loads.
    #[test]
    fn deleting_the_last_model_takes_the_router_section_with_it() {
        // A `[router]` whose default names nothing is a file that will not load.
        let path = local_temp("delete-last");
        upsert_local_models(&path, "ollama", vec![model("qwen", "ollama/qwen")]).unwrap();
        write_local_blocks(&path, &[]).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        // Counted as sections, not as substrings: the header comment mentions
        // `[[models]]` to explain what the file is.
        assert_eq!(sections(&text, "[router]"), 0, "{text}");
        assert_eq!(sections(&text, "[[models]]"), 0, "{text}");
        assert!(
            text.contains("loongagent"),
            "the header stays, so the file still explains itself: {text}"
        );
    }

    /// Purpose: The delete selector accepts a slot, a model name or a provider, and refuses what matches nothing.
    #[test]
    fn a_selector_names_a_slot_a_model_or_a_whole_provider() {
        let path = local_temp("select");
        upsert_local_models(
            &path,
            "deepseek",
            vec![model("ds-flash", "deepseek/ds-flash")],
        )
        .unwrap();
        upsert_local_models(
            &path,
            "moonshot",
            vec![
                model("kimi-k3", "moonshot/kimi-k3"),
                model("kimi-k2", "moonshot/kimi-k2"),
            ],
        )
        .unwrap();
        let blocks = read_local_blocks(&path);
        assert_eq!(blocks.len(), 3);

        assert_eq!(select_local(&blocks, "1").unwrap(), vec![0]);
        assert_eq!(select_local(&blocks, "kimi-k3").unwrap(), vec![1]);
        assert_eq!(
            select_local(&blocks, "moonshot").unwrap(),
            vec![1, 2],
            "a provider names all of its models, for a key that turned out wrong"
        );
        assert_eq!(
            select_local(&blocks, "deepseek").unwrap(),
            vec![0],
            "and a provider with one model names that one"
        );

        let err = select_local(&blocks, "7").unwrap_err();
        assert!(err.contains("没有第 7 个"), "{err}");
        let err = select_local(&blocks, "nope").unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    /// Purpose: Deleting one provider keeps every other block and the hand-written comments.
    #[test]
    fn deleting_a_provider_leaves_the_other_providers_text_alone() {
        let path = local_temp("delete-provider");
        std::fs::write(
            &path,
            "# mine\n\n[[models]]\nname = \"ds\"\nprovider = \"deepseek/ds\"\n\
             # keep this note\n\n[[models]]\nname = \"kimi\"\nprovider = \"moonshot/kimi\"\n\n\
             [router]\ndefault = \"ds\"\n",
        )
        .unwrap();

        let mut blocks = read_local_blocks(&path);
        let doomed = select_local(&blocks, "deepseek").unwrap();
        for index in doomed.iter().rev() {
            blocks.remove(*index);
        }
        write_local_blocks(&path, &blocks).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("deepseek/ds"), "{text}");
        assert!(text.contains("moonshot/kimi"), "{text}");
        assert!(text.contains("# mine"), "{text}");
        assert!(text.contains("default = \"kimi\""), "{text}");
    }

    /// Purpose: The first provider writes a header, its models and an explicit router default.
    #[test]
    fn a_first_provider_writes_a_header_its_models_and_a_router_default() {
        let path = local_temp("first");
        let written = upsert_local_models(
            &path,
            "deepseek",
            vec![model("deepseek-flash", "deepseek/deepseek-flash")],
        )
        .unwrap();
        assert_eq!(written, vec!["deepseek-flash".to_string()]);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[[models]]"), "{text}");
        assert!(
            text.contains("provider = \"deepseek/deepseek-flash\""),
            "{text}"
        );
        assert!(text.contains("max_context_size = 4096"), "{text}");
        assert!(
            text.contains("[router]") && text.contains("default = \"deepseek-flash\""),
            "the default is written, not implied by an ordering somebody has to know: {text}"
        );
    }

    /// Purpose: The local file never records `mode`, which is what would switch a managed install out of its policy.
    #[test]
    fn the_local_file_never_declares_a_mode() {
        // The bug that poisoned a managed install: writing `mode = "local"` here — or
        // anywhere a managed run reads — switched the whole install out of its firm's
        // policy. `--local` is a fact about one run, and this file must not record it.
        let path = local_temp("no-mode");
        upsert_local_models(&path, "deepseek", vec![model("m", "deepseek/m")]).unwrap();
        upsert_local_models(&path, "moonshot", vec![model("k", "moonshot/k")]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        // A real assignment, not the substring: `[[models]]` contains "mode" too.
        assert!(
            !text
                .lines()
                .any(|line| line.trim_start().starts_with("mode")),
            "the local file must not say what mode we are in: {text}"
        );
    }

    /// Purpose: Adding a second provider keeps the first provider's models and the original default.
    #[test]
    fn adding_a_second_provider_keeps_the_first_one_s_models() {
        // A second run is how somebody adds their second provider. Losing the first
        // would make the wizard something to be afraid of.
        let path = local_temp("second");
        upsert_local_models(
            &path,
            "deepseek",
            vec![model("ds-flash", "deepseek/ds-flash")],
        )
        .unwrap();
        upsert_local_models(
            &path,
            "moonshot",
            vec![model("kimi-k3", "moonshot/kimi-k3")],
        )
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("deepseek/ds-flash"), "{text}");
        assert!(text.contains("moonshot/kimi-k3"), "{text}");
        assert_eq!(sections(&text, "[[models]]"), 2, "{text}");
        assert_eq!(
            sections(&text, "[router]"),
            1,
            "and there is still exactly one router section: {text}"
        );
        assert!(
            text.contains("default = \"ds-flash\""),
            "the original default is not silently moved to whatever was added last: {text}"
        );
    }

    /// Purpose: Re-adding a provider updates its own models without duplicating or disturbing the others.
    #[test]
    fn re_adding_a_provider_replaces_only_that_provider_s_models() {
        // Pasting a new key for a provider you already configured should update it, not
        // duplicate it — and not disturb the others.
        let path = local_temp("replace");
        upsert_local_models(&path, "deepseek", vec![model("ds-old", "deepseek/ds-old")]).unwrap();
        upsert_local_models(&path, "moonshot", vec![model("kimi", "moonshot/kimi")]).unwrap();
        upsert_local_models(&path, "deepseek", vec![model("ds-new", "deepseek/ds-new")]).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("deepseek/ds-new"), "{text}");
        assert!(
            !text.contains("deepseek/ds-old"),
            "the replaced model is gone: {text}"
        );
        assert!(
            text.contains("moonshot/kimi"),
            "the other provider is untouched: {text}"
        );
        assert_eq!(sections(&text, "[[models]]"), 2, "{text}");
    }

    /// Purpose: Hand-written comments and the existing router survive adding a provider.
    #[test]
    fn what_a_person_wrote_around_the_models_survives_a_new_provider() {
        // The file is meant to be read and edited by a person, and its comments are the
        // only documentation these settings have. Round-tripping through a TOML
        // serializer would delete every one of them.
        let path = local_temp("comments");
        std::fs::write(
            &path,
            "# my own note about this machine\n\
             [[models]]\n\
             name = \"ds\"\n\
             provider = \"deepseek/ds\"\n\
             # hand-written: this one is cheap at night\n\n\
             [router]\n\
             default = \"ds\"\n\
             # I raised this on purpose\n",
        )
        .unwrap();

        upsert_local_models(&path, "moonshot", vec![model("kimi", "moonshot/kimi")]).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my own note about this machine"), "{text}");
        assert!(
            text.contains("# hand-written: this one is cheap at night"),
            "{text}"
        );
        assert!(text.contains("# I raised this on purpose"), "{text}");
        assert!(
            text.contains("moonshot/kimi"),
            "and the new provider arrived: {text}"
        );
        assert!(
            !text.contains("[router]\n[router]"),
            "and no second router section was bolted on: {text}"
        );
    }

    /// Purpose: A provider that offered no models writes no file at all.
    #[test]
    fn a_provider_that_offered_no_models_leaves_the_file_alone() {
        // Somebody whose `/models` came back empty should get told, not given a header
        // with nothing under it.
        let path = local_temp("empty");
        let written = upsert_local_models(&path, "deepseek", Vec::new()).unwrap();
        assert!(written.is_empty());
        assert!(!path.exists(), "no file is created for no models");
    }

    /// Purpose: The splitter recovers the header, the model blocks and the router from the writer's own output.
    #[test]
    fn splitting_a_written_file_finds_its_header_blocks_and_router() {
        // The exact shape the writer produces, fed back in. The router is what a second
        // provider must not lose.
        let written = "# header line\n\n\
                       [[models]]\nname = \"a\"\nprovider = \"p/a\"\n\n\
                       [router]\ndefault = \"a\"\n";
        let (head, blocks, tail) = split_model_blocks(written);
        assert_eq!(blocks.len(), 1, "one model block: {blocks:?}");
        assert!(
            blocks[0].contains("name = \"a\""),
            "and it is the model: {blocks:?}"
        );
        assert!(
            !blocks[0].contains("[router]"),
            "the router section is not part of a model block: {blocks:?}"
        );
        assert!(
            tail.contains("[router]") && tail.contains("default = \"a\""),
            "the router is in the tail: {tail:?}"
        );
        assert!(
            head.contains("# header line"),
            "and the header is the header: {head:?}"
        );
    }

    /// Purpose: The control file round-trips, and a key that is not 32 bytes of hex is refused before any request.
    #[test]
    fn the_control_file_round_trips_and_its_key_is_checked() {
        let file = ControlFile {
            url: "https://control.acme.internal".to_string(),
            public_key: "ab".repeat(32),
            token: Some("env:LOONGAGENT_CONTROL_TOKEN".to_string()),
        };
        let text = toml::to_string(&file).unwrap();
        let back: ControlFile = toml::from_str(&text).unwrap();
        assert_eq!(back.url, file.url);
        assert_eq!(back.public_key, file.public_key);
        assert_eq!(back.token, file.token);
        let (config, _) = control_config(&back).unwrap();
        assert_eq!(config.base_url, "https://control.acme.internal");
        assert_eq!(config.public_key.len(), 32);

        // A key that is not a 32-byte hex Ed25519 key is refused before anything is
        // sent anywhere.
        let short = ControlFile {
            public_key: "abcd".to_string(),
            ..back.clone()
        };
        assert!(control_config(&short).unwrap_err().contains("32-byte"));
        let not_hex = ControlFile {
            public_key: "zz".repeat(32),
            ..back.clone()
        };
        assert!(control_config(&not_hex).unwrap_err().contains("hex"));

        // And the token is never a literal: only a reference resolves.
        let literal = ControlFile {
            token: Some("a-real-secret".to_string()),
            ..back
        };
        assert_eq!(
            resolve_reference(literal.token.as_deref(), Path::new("/tmp")),
            Some("a-real-secret".to_string())
        );
    }

    /// Purpose: Merging the two control files lets the user supply a token but never a different address or key.
    #[test]
    fn a_user_file_may_supply_a_token_but_not_a_different_pin() {
        let machine = ControlFile {
            url: "https://control.acme.internal".to_string(),
            public_key: "ab".repeat(32),
            token: None,
        };
        let mine = ControlFile {
            url: "https://control.evil.invalid".to_string(),
            public_key: "cd".repeat(32),
            token: Some("file:/home/me/.loongagent/client.token".to_string()),
        };
        let merged = merge_control(Some(machine.clone()), Some(mine.clone())).unwrap();
        assert_eq!(
            merged.url, machine.url,
            "the machine decides who we talk to"
        );
        assert_eq!(merged.public_key, machine.public_key, "and what we trust");
        assert_eq!(
            merged.token, mine.token,
            "the user supplies their own token"
        );

        // An unmanaged install is the user's file, whole.
        assert_eq!(merge_control(None, Some(mine.clone())).unwrap(), mine);
        // A managed one with no user file is the machine's, whole.
        assert_eq!(merge_control(Some(machine.clone()), None).unwrap(), machine);
        assert!(merge_control(None, None).is_none());
    }

    /// Purpose: Env files are read with the user's file winning over the workspace, and missing files are not errors.
    #[test]
    fn environment_files_are_read_with_the_user_winning_and_the_shell_above_both() {
        let dir = std::env::temp_dir().join(format!("loong-env-{}", std::process::id()));
        let user = dir.join("cfg");
        let workspace = dir.join("ws");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            &user.join("env"),
            "# the user's own file\nexport DEEPSEEK_API_KEY=\"sk-user\"\nSHARED: from-user\n",
        )
        .unwrap();
        std::fs::write(
            &workspace.join(".env"),
            "DEEPSEEK_API_KEY=sk-workspace\nSHARED=from-workspace\nONLY_HERE=yes\n",
        )
        .unwrap();

        let pairs = read_env_files(&env_file_paths(&user, &workspace));
        assert_eq!(
            pairs
                .iter()
                .find(|(n, _)| n == "DEEPSEEK_API_KEY")
                .unwrap()
                .1,
            "sk-user",
            "the user's file wins over a checked-out repository"
        );
        assert_eq!(
            pairs.iter().find(|(n, _)| n == "SHARED").unwrap().1,
            "from-user"
        );
        assert_eq!(
            pairs.iter().find(|(n, _)| n == "ONLY_HERE").unwrap().1,
            "yes",
            "and the workspace can still supply what the user did not"
        );

        // A missing file is not an error, and neither is an empty one.
        let none = read_env_files(&[dir.join("absent"), dir.join("also-absent")]);
        assert!(none.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Purpose: A literal control-plane token is refused while the two reference shapes are accepted.
    #[test]
    fn a_literal_control_plane_token_is_refused() {
        let literal = ControlFile {
            url: "https://control.acme.internal".to_string(),
            public_key: "ab".repeat(32),
            token: Some("ct-abcdef123456".to_string()),
        };
        let error = control_config(&literal).unwrap_err();
        assert!(error.contains("reference"), "{error}");
        assert!(error.contains("ADR-0008"), "{error}");
        // The two shapes that are not literals still work.
        for reference in [
            "env:LOONGAGENT_CONTROL_TOKEN",
            "file:/etc/loongagent/client.token",
        ] {
            let file = ControlFile {
                token: Some(reference.to_string()),
                ..literal.clone()
            };
            assert!(
                control_config(&file).is_ok(),
                "{reference} must be accepted"
            );
        }
    }

    /// Purpose: A plain-HTTP address for another machine is refused before a token can be sent.
    #[test]
    fn a_control_plane_address_that_would_leak_a_token_is_refused() {
        let file = ControlFile {
            url: "http://control.acme.internal".to_string(),
            public_key: "ab".repeat(32),
            token: Some("env:X".to_string()),
        };
        let error = control_config(&file).unwrap_err();
        assert!(error.contains("TLS"), "{error}");
    }

    /// Purpose: A `file:` reference reads either a bare token file or a named key in a table.
    #[test]
    fn a_file_reference_reads_either_a_table_or_a_bare_secret() {
        let dir = std::env::temp_dir().join(format!("loong-secret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // What `loongagent-control --client-token-file` writes: the file is the token.
        let bare = dir.join("client.token");
        std::fs::write(&bare, "ct-secret\n").unwrap();
        assert_eq!(
            resolve_reference(Some(&format!("file:{}", bare.display())), &dir),
            Some("ct-secret".to_string()),
            "trailing whitespace is not part of a token"
        );
        // An empty file is a mistake, not an empty token.
        let empty = dir.join("empty.token");
        std::fs::write(&empty, "   \n").unwrap();
        assert_eq!(
            resolve_reference(Some(&format!("file:{}", empty.display())), &dir),
            None
        );
        // And the table form still works, with the key named.
        let table = dir.join("credentials.toml");
        std::fs::write(&table, "[keys]\nCONTROL_TOKEN = \"from-table\"\n").unwrap();
        assert_eq!(
            resolve_reference(
                Some(&format!("file:{}#CONTROL_TOKEN", table.display())),
                &dir
            ),
            Some("from-table".to_string())
        );
        // A relative path is read beside the profile in use.
        assert_eq!(
            resolve_reference(Some("file:client.token"), &dir),
            Some("ct-secret".to_string())
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Purpose: The credential reader accepts the shapes operators write and never treats an empty value as a secret.
    #[test]
    fn a_secret_file_is_read_in_the_shapes_operators_actually_write() {
        let dir = std::env::temp_dir().join(format!("loong-creds-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("secrets");

        // `KEY: value` — a `.env` written by hand.
        std::fs::write(&path, "# comment\nDEEPSEEK_API_KEY: sk-colon\nOTHER: x\n").unwrap();
        assert_eq!(
            read_credential(&path, "DEEPSEEK_API_KEY").as_deref(),
            Some("sk-colon")
        );

        // `KEY=value`, quoted, with `export`, and a value containing `=`.
        std::fs::write(
            &path,
            "export DEEPSEEK_API_KEY=\"sk-equals=\"\nEMPTY=\n#NAME=ignored\n",
        )
        .unwrap();
        assert_eq!(
            read_credential(&path, "DEEPSEEK_API_KEY").as_deref(),
            Some("sk-equals=")
        );
        assert_eq!(
            read_credential(&path, "EMPTY"),
            None,
            "an empty value is not a secret"
        );
        assert_eq!(
            read_credential(&path, "NAME"),
            None,
            "a comment is not a definition"
        );

        // A TOML credentials file still works, table header and quotes included.
        std::fs::write(&path, "[keys]\nDEEPSEEK_API_KEY = \"sk-toml\"\n").unwrap();
        assert_eq!(
            read_credential(&path, "DEEPSEEK_API_KEY").as_deref(),
            Some("sk-toml")
        );

        assert_eq!(read_credential(&path, "ABSENT"), None);
        assert_eq!(read_credential(&dir.join("absent"), "ANY"), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
