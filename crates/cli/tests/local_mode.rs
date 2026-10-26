//! Purpose: Local mode end to end: the flag, the local file and the `local` subcommands, against the real binary.
//!
//! # Contract
//! - **Serves:** the promise that `--local` removes the control plane rather than merely ignoring it.
//! - **Design:** `docs/design/surfaces.md` §2; `docs/design/config-routing.md` §3.
//! - **Upstream:** runs `CARGO_BIN_EXE_loongagent`; nothing imports this file.
//! - **Downstream:** the `config`, `local` and `tui` commands, and the files a test-owned home holds.
//! - **Verify:** `cargo test -p loongagent --test local_mode`
//!
//! `loongagent tui --local` (ADR-0009's surface, ADR-0008's layers): a mode that
//! removes the control plane rather than merely ignoring it.
//!
//! The tests here are black-box on purpose. "Does local mode skip the control plane?"
//! is a question about *observable behaviour* — whether a network call happens and
//! whether a prompt appears — not about which function ran. Pointing the control
//! plane at a closed port makes the answer falsifiable: if the control plane is
//! consulted, the run fails; if it is not, the run succeeds.

use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A fresh home and workspace for one test.
/// Contract: `local_mode.rs` fixtures; every test calls it.
fn workspace() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loongagent-local-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("ws")).unwrap();
    dir
}

/// Purpose: A home whose control plane is a closed port, so a sync cannot accidentally pass.
/// Contract: `local_mode.rs` fixtures; the control-plane tests call it.
/// A home whose control plane is a closed port: reachable-looking, never reachable.
/// Port 1 is reserved and nothing listens there, so a sync cannot accidentally pass.
fn home_with_a_dead_control_plane() -> std::path::PathBuf {
    let dir = workspace();
    std::fs::write(
        dir.join("control.toml"),
        "url = \"http://127.0.0.1:1/\"\n\
         public_key = \"0000000000000000000000000000000000000000000000000000000000000000\"\n",
    )
    .unwrap();
    dir
}

/// Purpose: Run the binary against a given home and workspace, with every catalog key removed from the environment.
/// Serves: every test in this file, so a developer's exported key cannot change the result.
/// Design: `docs/design/config-routing.md` §5.
/// Up: the tests in this file; no other caller.
/// Down: `std::process::Command` and `loong_config::catalog::PROVIDERS`.
/// Verify: `cargo test -p loongagent --test local_mode`.
fn run(home: &std::path::Path, args: &[&str]) -> (bool, String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_loongagent"));
    command.env("LOONGAGENT_HOME", home);
    // The workspace of *this* repository has a `.env`, and a developer's shell may have
    // any of these exported. Either would let a test pass — or fail — for a reason that
    // has nothing to do with the code, and the names the local first run looks for are
    // exactly the ones most likely to be set.
    for provider in loong_config::catalog::PROVIDERS {
        command.env_remove(provider.key_env);
        for alias in provider.aliases {
            command.env_remove(alias);
        }
    }
    let out = command
        .args(args)
        .arg("--cwd")
        .arg(home.join("ws"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the binary runs");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Purpose: `--centered` is the explicit managed run, and a dead control plane fails it.
#[test]
fn the_control_plane_is_really_consulted_under_centered() {
    // The control half of the experiment. Without this, the default-local test below
    // would pass just as well against a build that ignored the control plane always.
    let home = home_with_a_dead_control_plane();
    let (ok, _stdout, stderr) = run(&home, &["config", "show", "--centered"]);
    assert!(
        !ok,
        "a dead control plane must fail a managed run, or the flag means nothing"
    );
    assert!(stderr.contains("unreachable"), "and say why: {stderr}");
}

/// Purpose: The same dead control plane does not matter under `--local`, and the run says which mode it is in.
#[test]
fn local_mode_never_touches_the_control_plane() {
    let home = home_with_a_dead_control_plane();
    std::fs::write(
        home.join("profile.toml"),
        "[[models]]\nname = \"cheap\"\nprovider = \"deepseek/deepseek-flash\"\n\
         [router]\ndefault = \"cheap\"\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = run(&home, &["config", "show", "--local"]);
    assert!(
        ok,
        "the same dead control plane must not matter in local mode: {stderr}"
    );
    assert!(
        !stderr.contains("unreachable"),
        "nothing may have tried to reach it: {stderr}"
    );
    assert!(
        stdout.contains("mode") && stdout.contains("--local (this run)"),
        "the run says out loud which mode it is in: {stdout}"
    );
}

/// Purpose: Local is the default even when the local file does not say so.
#[test]
fn local_is_the_default_even_without_a_mode_line() {
    let home = home_with_a_dead_control_plane();
    std::fs::write(
        home.join("local.toml"),
        "[[models]]\nname = \"cheap\"\n\
         provider = \"deepseek/deepseek-flash\"\n[router]\ndefault = \"cheap\"\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = run(&home, &["config", "show"]);
    assert!(
        ok,
        "a dead control plane must not matter by default: {stderr}"
    );
    assert!(
        stdout.contains("--local (default)"),
        "the run says it is local without a flag: {stdout}"
    );
    assert!(
        stdout.contains("local.toml"),
        "and reads the local stack: {stdout}"
    );
}

/// Purpose: Local and centered are different modes; passing both is a usage error.
#[test]
fn the_two_mode_flags_conflict() {
    let home = workspace();
    for args in [
        &["config", "show", "--local", "--centered"][..],
        &["config", "show", "--centered", "--local"][..],
    ] {
        let (ok, _stdout, stderr) = run(&home, args);
        assert!(!ok, "both flags must be refused: {stderr}");
        assert!(stderr.contains("pass only one"), "and say why: {stderr}");
    }
}

/// Purpose: `--centered` ignores a stale `mode = "local"` and reaches the control plane.
#[test]
fn centered_ignores_a_local_mode_line() {
    let home = home_with_a_dead_control_plane();
    std::fs::write(
        home.join("local.toml"),
        "mode = \"local\"\n\n[[models]]\nname = \"cheap\"\n\
         provider = \"deepseek/deepseek-flash\"\n[router]\ndefault = \"cheap\"\n",
    )
    .unwrap();
    let (ok, _stdout, stderr) = run(&home, &["config", "show", "--centered"]);
    assert!(
        !ok,
        "centered must consult the managed control plane: {stderr}"
    );
    assert!(stderr.contains("unreachable"), "and say why: {stderr}");
}

/// Purpose: A managed profile cannot switch the install out of its firm's policy by declaring itself local.
#[test]
fn the_managed_profile_cannot_declare_local_mode() {
    // The bug this rule exists for, as a test that runs the real binary: `mode = "local"`
    // in the *managed* profile made a plain `loongagent tui` resolve as a local install
    // and stop consulting the control plane. It is a way to switch an install out of its
    // firm's policy by editing a file about something else, so it is refused.
    let home = home_with_a_dead_control_plane();
    std::fs::write(home.join("profile.toml"), "mode = \"local\"\n").unwrap();

    let (ok, _stdout, stderr) = run(&home, &["config", "show", "--centered"]);
    assert!(
        !ok,
        "a managed profile must not be able to say this: {stderr}"
    );
    assert!(
        stderr.contains("local.toml"),
        "and says where it goes: {stderr}"
    );
    assert!(
        !stderr.contains("unreachable"),
        "and the failure is the key, not a sync attempt: {stderr}"
    );
}

/// Purpose: A catalog provider shorthand resolves to the vendor's endpoint and model id without a hand-written URL.
#[test]
fn a_provider_shorthand_reaches_the_command_line_without_an_endpoint() {
    let home = workspace();
    std::fs::write(
        home.join("local.toml"),
        "mode = \"local\"\n\n[[models]]\nname = \"cheap\"\n\
         provider = \"deepseek/deepseek-flash\"\n\n[[models]]\nname = \"strong\"\n\
         provider = \"moonshot/kimi-k3\"\n\n[router]\ndefault = \"cheap\"\n",
    )
    .unwrap();

    let (ok, stdout, stderr) = run(&home, &["config", "show"]);
    assert!(ok, "{stderr}");
    assert!(
        stdout.contains("https://api.deepseek.com"),
        "the catalog supplied the endpoint: {stdout}"
    );
    assert!(
        stdout.contains("https://api.moonshot.cn/v1"),
        "and did so per model, not once: {stdout}"
    );
    assert!(
        stdout.contains("deepseek-flash") && stdout.contains("kimi-k3"),
        "the model ids came from the shorthand too: {stdout}"
    );
}

/// Purpose: A first local run with no key and no terminal refuses cleanly and leaves no half configuration.
#[test]
fn a_local_first_run_with_no_key_refuses_cleanly_and_writes_nothing() {
    // No key anywhere, and no terminal to ask on. The failure must be the *terminal*,
    // said plainly — not a hang, and not half a configuration left on disk for the
    // next run to trip over.
    let home = workspace();
    let (ok, _stdout, stderr) = run(&home, &["tui", "--local"]);
    assert!(!ok, "a pipe is not a terminal to ask on");
    assert!(
        stderr.contains("terminal"),
        "and the reason is the terminal: {stderr}"
    );
    assert!(
        !home.join("profile.toml").exists(),
        "nothing is written when nothing was configured"
    );
    assert!(
        !home.join("env").exists(),
        "and no secret file is created for a key that was never given"
    );
}

/// Purpose: A checked-out repository cannot declare the install local, which would drop its firm's policy.
#[test]
fn a_repository_may_not_declare_the_install_local() {
    // The same threat as a repo choosing its own endpoint: `mode = "local"` in a
    // checked-out file would switch a managed install out of its firm's policy.
    let home = workspace();
    std::fs::create_dir_all(home.join("ws/.loongagent")).unwrap();
    std::fs::write(
        home.join("ws/.loongagent/profile.toml"),
        "mode = \"local\"\n",
    )
    .unwrap();

    let (ok, _stdout, stderr) = run(&home, &["config", "show", "--local"]);
    assert!(!ok, "the project layer must not be able to say this");
    assert!(stderr.contains("managed"), "{stderr}");
}

// ---------------------------------------------------- the `local` subcommands

/// Purpose: A home with three models from two providers, as `local add` would have left it.
/// Serves: the `local` listing and delete tests.
/// Design: `docs/design/config-routing.md` §3.
/// Up: the `local` subcommand tests; no other caller.
/// Down: `workspace` and `std::fs::write`.
/// Verify: `cargo test -p loongagent --test local_mode`.
/// A home with three models from two providers, as `local add` would have left it.
fn home_with_models() -> std::path::PathBuf {
    let home = workspace();
    std::fs::write(
        home.join("local.toml"),
        "[[models]]\nname = \"qwen2.5:3b\"\nprovider = \"ollama/qwen2.5:3b\"\n\
         max_context_size = 32768\n\n\
         [[models]]\nname = \"deepseek-flash\"\nprovider = \"deepseek/deepseek-flash\"\n\n\
         [[models]]\nname = \"deepseek-v4-pro\"\nprovider = \"deepseek/deepseek-v4-pro\"\n\n\
         [router]\ndefault = \"qwen2.5:3b\"\n",
    )
    .unwrap();
    home
}

/// Purpose: The model names a `local` listing shows, in order.
/// Contract: `local_mode.rs` fixtures; the listing tests call it.
/// The model names a `local` listing shows, in order.
fn listed(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter(|line| line.trim_start().starts_with('[') || line.trim_start().starts_with("▸["))
        .filter_map(|line| line.split(']').nth(1))
        .map(|rest| rest.split_whitespace().next().unwrap_or("").to_string())
        .filter(|name| !name.is_empty())
        .collect()
}

/// Purpose: `local` lists the configured models, their context windows and the commands that change them.
#[test]
fn local_lists_what_is_configured_and_where_it_lives() {
    let home = home_with_models();
    let (ok, stdout, stderr) = run(&home, &["local"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("local.toml"), "the file is named: {stdout}");
    assert_eq!(
        listed(&stdout),
        vec!["qwen2.5:3b", "deepseek-flash", "deepseek-v4-pro"],
        "{stdout}"
    );
    assert!(
        stdout.contains("ctx 32768"),
        "and what the vendor said about it: {stdout}"
    );
    assert!(
        stdout.contains("local delete"),
        "and how to change it: {stdout}"
    );
}

/// Purpose: An unconfigured install says so and names `local add` instead of showing an empty list.
#[test]
fn local_is_empty_before_anything_is_configured() {
    let home = workspace();
    let (ok, stdout, stderr) = run(&home, &["local"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("还没配置模型"), "{stdout}");
    assert!(stdout.contains("local add"), "and says how: {stdout}");
}

/// Purpose: Deleting by slot removes that model and moves the router default to one that still exists.
#[test]
fn local_delete_by_slot_removes_one_and_moves_the_default() {
    let home = home_with_models();
    let (ok, stdout, stderr) = run(&home, &["local", "delete", "1"]);
    assert!(ok, "{stderr}");
    assert!(stdout.contains("qwen2.5:3b"), "it says what went: {stdout}");

    let text = std::fs::read_to_string(home.join("local.toml")).unwrap();
    assert!(!text.contains("ollama/"), "{text}");
    assert!(
        text.contains("default = \"deepseek-flash\""),
        "the default followed the deletion instead of dangling: {text}"
    );

    let (_, stdout, _) = run(&home, &["local"]);
    assert_eq!(
        listed(&stdout),
        vec!["deepseek-flash", "deepseek-v4-pro"],
        "{stdout}"
    );
}

/// Purpose: Deleting by provider removes that provider's models and leaves the others alone.
#[test]
fn local_delete_by_provider_takes_all_of_that_provider() {
    let home = home_with_models();
    let (ok, _, stderr) = run(&home, &["local", "delete", "deepseek"]);
    assert!(ok, "{stderr}");

    let text = std::fs::read_to_string(home.join("local.toml")).unwrap();
    assert!(!text.contains("deepseek/"), "{text}");
    assert!(text.contains("ollama/qwen2.5:3b"), "{text}");
    assert!(text.contains("default = \"qwen2.5:3b\""), "{text}");
}

/// Purpose: A selector that matches nothing fails before writing, and points at the listing.
#[test]
fn local_delete_says_what_is_configured_when_the_selector_matches_nothing() {
    let home = home_with_models();
    let (ok, _stdout, stderr) = run(&home, &["local", "delete", "gpt-nothing"]);
    assert!(!ok);
    assert!(stderr.contains("gpt-nothing"), "{stderr}");
    assert!(
        stderr.contains("loongagent local"),
        "and points at the listing: {stderr}"
    );
    // Nothing was written: a failed delete must not half-apply.
    let text = std::fs::read_to_string(home.join("local.toml")).unwrap();
    assert!(text.contains("deepseek-v4-pro"), "{text}");
}

/// Purpose: A provider name the catalog does not know is refused, with the known names listed.
#[test]
fn local_add_named_provider_refuses_a_name_the_catalog_does_not_know() {
    let home = workspace();
    let (ok, _stdout, stderr) = run(&home, &["local", "add", "notaprovider"]);
    assert!(!ok);
    assert!(stderr.contains("notaprovider"), "{stderr}");
    assert!(
        stderr.contains("ollama"),
        "and lists what it does know: {stderr}"
    );
}

/// Purpose: A missing selector or an unknown subcommand prints usage and the commands that exist.
#[test]
fn the_local_subcommands_say_what_there_is_instead_of_guessing() {
    let home = workspace();
    let (ok, _stdout, stderr) = run(&home, &["local", "delete"]);
    assert!(!ok);
    assert!(stderr.contains("usage"), "{stderr}");

    let (ok, _stdout, stderr) = run(&home, &["local", "frobnicate"]);
    assert!(!ok);
    assert!(stderr.contains("frobnicate"), "{stderr}");
    assert!(stderr.contains("local add"), "{stderr}");
}
