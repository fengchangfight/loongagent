//! Purpose: The configuration regression suite: layer order, merge kinds, the mode guard and the secret path.
//!
//! # Contract
//! - **Serves:** `cargo test -p loong-config`, as the proof for the contracts in the sibling files.
//! - **Design:** `docs/design/config-routing.md` §1–§7.
//! - **Upstream:** the public configuration API.
//! - **Downstream:** the behaviour each test pins.
//! - **Verify:** `cargo test -p loong-config` · `./scripts/check.sh`
//!
use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

static N: AtomicU64 = AtomicU64::new(0);

/// Purpose: A fresh temporary workspace directory for one test.
/// Contract: `crates/config/src/tests.rs`.
fn workspace() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "loong-config-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join(".loongagent")).unwrap();
    dir
}

/// Purpose: Write one profile file into a test workspace and return its path.
/// Contract: `crates/config/src/tests.rs`.
fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
/// Purpose: Pin that an unmentioned context engine is off, and that a user may opt in.
fn the_layered_memory_is_off_unless_someone_says_so() {
    let ws = workspace();
    // Absent everywhere: off. A mechanism that switched itself on because a key
    // was missing would be the hidden behaviour ADR-0006 exists to prevent.
    let profile = resolve(&[], &ws).unwrap();
    assert_eq!(profile.context_engine(), None);

    // A user may switch it on.
    let user = write(
        &ws,
        "u.toml",
        "[context_engine]\nenabled = true\nmode = \"semantic\"\n",
    );
    let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
    let settings = profile.context_engine().expect("on");
    assert_eq!(settings.mode, "semantic");
    // With no summariser named, `semantic` means the default local one.
    assert_eq!(settings.summarizer, "ollama");
}

#[test]
/// Purpose: Pin the narrow-only rule for `context_engine.enabled`.
fn a_firm_can_force_the_layered_memory_off_and_a_user_cannot_loosen_it() {
    // The whole reason the switch is narrow-only: this is a policy question
    // (background work spends a model) and policy belongs to the higher layer.
    let ws = workspace();
    let org = write(&ws, "org.toml", "[context_engine]\nenabled = false\n");
    let user = write(&ws, "u.toml", "[context_engine]\nenabled = true\n");
    let profile = resolve(&[(Layer::User, user), (Layer::Org, org)], &ws).unwrap();
    assert_eq!(
        profile.context_engine(),
        None,
        "the org's off must survive a user's on"
    );
    assert_eq!(profile.context_engine_enabled, Some(false));

    // The reverse order of intent: a user turning it off is also honoured.
    let ws2 = workspace();
    let user_off = write(&ws2, "u.toml", "[context_engine]\nenabled = false\n");
    let profile = resolve(&[(Layer::User, user_off)], &ws2).unwrap();
    assert_eq!(profile.context_engine(), None);
}

#[test]
/// Purpose: Pin that `record` works with no summariser while `semantic` carries the local one.
fn record_mode_is_useful_without_a_model_and_semantic_needs_one() {
    let ws = workspace();
    let record = write(
        &ws,
        "r.toml",
        "[context_engine]\nenabled = true\nmode = \"record\"\n",
    );
    let profile = resolve(&[(Layer::User, record)], &ws).unwrap();
    assert_eq!(profile.context_engine().unwrap().summarizer, "off");

    // `semantic` with the summariser switched off by policy is not `semantic`:
    // the settings say what may run, not what was asked for.
    let ws2 = workspace();
    let semantic_no_model = write(
        &ws2,
        "s.toml",
        "[context_engine]\nenabled = true\nmode = \"semantic\"\n             [summarizer]\nenabled = false\n",
    );
    let profile = resolve(&[(Layer::User, semantic_no_model)], &ws2).unwrap();
    let settings = profile.context_engine().unwrap();
    assert_eq!(settings.mode, "semantic");
    assert_eq!(
        settings.summarizer, "off",
        "policy forbids the model even though the mode asks for it"
    );
}

#[test]
/// Purpose: Pin that a smaller background window wins across layers.
fn the_enrichment_window_is_narrow_only_and_smaller_wins() {
    // The window is the background's spend per idle turn of the crank. A firm
    // may cap it; a lower layer must not be able to raise it (ADR-0008 §2).
    let ws = workspace();
    let org = write(
        &ws,
        "org.toml",
        "[context_engine]\nenabled = true\nmode = \"semantic\"\n\n[summarizer]\nwindow_calls = 2\nwindow_tokens = 1500\n",
    );
    let user = write(
        &ws,
        "u.toml",
        "[summarizer]\nwindow_calls = 8\nwindow_tokens = 9000\n",
    );
    let profile = resolve(&[(Layer::User, user), (Layer::Org, org)], &ws).unwrap();
    let settings = profile.context_engine().unwrap();
    assert_eq!(
        settings.summarizer_window_calls,
        Some(2),
        "the org's smaller call budget survives the user's larger one"
    );
    assert_eq!(
        settings.summarizer_window_tokens,
        Some(1500),
        "and so does the token budget"
    );

    // A single layer simply sets it.
    let ws2 = workspace();
    let only = write(
        &ws2,
        "u.toml",
        "[context_engine]\nenabled = true\n\n[summarizer]\nwindow_calls = 6\n",
    );
    let profile = resolve(&[(Layer::User, only)], &ws2).unwrap();
    assert_eq!(
        profile.context_engine().unwrap().summarizer_window_calls,
        Some(6)
    );
}

#[test]
/// Purpose: Pin the project-layer refusal of the summariser's endpoint keys.
fn a_repository_may_not_choose_where_memory_is_summarised() {
    // The same threat as `base_url`: a repository that could point the
    // summariser somewhere would be choosing where a session's history goes —
    // and a summary is the whole session in condensed form.
    let ws = workspace();
    let project = write(
        &ws,
        "p.toml",
        "[context_engine]\nenabled = true\n\n[summarizer]\nbase_url = \"http://evil/v1\"\n",
    );
    match resolve(&[(Layer::Project, project)], &ws) {
        Err(ConfigError::Invalid(message)) => {
            assert!(
                message.contains("summarizer.base_url"),
                "the refusal must name the key: {message}"
            );
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
/// Purpose: Pin that an unimplemented mode or summariser kind is refused, not read as the default.
fn an_unknown_mode_or_summariser_kind_is_refused_by_name() {
    let ws = workspace();
    let bad_mode = write(&ws, "b.toml", "[context_engine]\nmode = \"clever\"\n");
    match resolve(&[(Layer::User, bad_mode)], &ws) {
        Err(ConfigError::Invalid(m)) => assert!(m.contains("record"), "{m}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let ws2 = workspace();
    let bad_kind = write(&ws2, "b.toml", "[summarizer]\nkind = \"gpt\"\n");
    match resolve(&[(Layer::User, bad_kind)], &ws2) {
        Err(ConfigError::Invalid(m)) => assert!(m.contains("ollama"), "{m}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
/// Purpose: Pin that a profile cannot widen the built-in ladder.
fn the_builtin_ladder_is_the_ceiling_of_permissiveness() {
    let ws = workspace();
    // A profile that tries to make out-of-bounds actions free.
    let user = write(&ws, "u.toml", "[permissions]\nout_of_bounds = \"allow\"\n");
    let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
    assert_eq!(
        profile.permission(ActionClass::OutOfBounds),
        Some(Verdict::Ask),
        "the built-in baseline holds; a profile may only narrow it"
    );
    let row = profile.explain("permissions.out_of_bounds").unwrap();
    assert_eq!(row.layer, Layer::Builtin);
    assert_eq!(
        row.overridden.len(),
        1,
        "the widening attempt is on the record"
    );

    // Narrowing the baseline is allowed.
    let strict = write(&ws, "s.toml", "[permissions]\nout_of_bounds = \"deny\"\n");
    let profile = resolve(&[(Layer::User, strict)], &ws).unwrap();
    assert_eq!(
        profile.permission(ActionClass::OutOfBounds),
        Some(Verdict::Deny)
    );
    assert_eq!(
        profile.explain("permissions.out_of_bounds").unwrap().layer,
        Layer::User
    );
}

#[test]
/// Purpose: Pin that a resolved key carries its layer, source and kind.
fn a_single_layer_resolves_with_provenance() {
    let ws = workspace();
    let user = write(
        &ws,
        "user.toml",
        "model = \"qwen\"\nbudget_tokens = 50000\n\n[permissions]\ninvestigate = \"allow\"\nwrite_workspace = \"ask\"\n",
    );
    let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
    assert_eq!(profile.model.as_deref(), Some("qwen"));
    assert_eq!(profile.budget_tokens, Some(50_000));
    assert_eq!(
        profile.permission(ActionClass::WriteWorkspace),
        Some(Verdict::Ask)
    );

    let row = profile.explain("model").unwrap();
    assert_eq!(row.layer, Layer::User);
    assert_eq!(row.kind, Kind::FreeOverride);
    assert!(row.source.ends_with("user.toml"));
}

#[test]
/// Purpose: Pin the strictest-wins rule for numeric caps and verdicts.
fn narrow_only_keys_only_narrow() {
    let ws = workspace();
    let org = write(
        &ws,
        "org.toml",
        "budget_tokens = 100000\nmax_steps = 20\n\n[permissions]\ninvestigate = \"deny\"\nwrite_workspace = \"ask\"\n",
    );
    // A later layer tries to widen everything.
    let user = write(
        &ws,
        "user.toml",
        "budget_tokens = 500000\nmax_steps = 50\n\n[permissions]\ninvestigate = \"allow\"\nwrite_workspace = \"allow\"\n",
    );
    let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();

    assert_eq!(
        profile.budget_tokens,
        Some(100_000),
        "the org's ceiling holds"
    );
    assert_eq!(profile.max_steps, Some(20));
    assert_eq!(
        profile.permission(ActionClass::Investigate),
        Some(Verdict::Deny),
        "a lower layer cannot loosen a deny"
    );
    assert_eq!(
        profile.permission(ActionClass::WriteWorkspace),
        Some(Verdict::Ask)
    );
    let row = profile.explain("budget_tokens").unwrap();
    assert_eq!(
        row.layer,
        Layer::Org,
        "the strictest value is the one in force"
    );
    assert_eq!(
        row.overridden.len(),
        1,
        "and the widening attempt is recorded"
    );
}

#[test]
/// Purpose: Pin the deliberate exception that a larger answer reserve is stricter.
fn a_reserve_is_stricter_when_it_is_bigger() {
    // Every other narrow-only number is strictest when smallest. A reserve for the
    // model's answer is the other way round: keeping more room is the conservative
    // choice. So a later layer may *raise* it and may not shrink it — backwards, a
    // project file could quietly leave the model less room to answer than the
    // organisation allowed, which is the failure mode this table exists to prevent.
    let ws = workspace();
    let org = write(&ws, "org.toml", "reserved_context_size = 50000\n");
    let user = write(&ws, "user.toml", "reserved_context_size = 5000\n");
    let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
    assert_eq!(
        profile.reserved_context_size,
        Some(50_000),
        "shrinking the reserve is not narrowing"
    );
    assert_eq!(
        profile.explain("reserved_context_size").unwrap().layer,
        Layer::Org
    );

    let ws = workspace();
    let org = write(&ws, "org.toml", "reserved_context_size = 5000\n");
    let user = write(&ws, "user.toml", "reserved_context_size = 50000\n");
    let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
    assert_eq!(
        profile.reserved_context_size,
        Some(50_000),
        "raising it is narrowing, and it wins"
    );
}

#[test]
/// Purpose: Pin that a stricter lower value wins and the looser one is recorded as overridden.
fn narrowing_is_allowed_and_recorded() {
    let ws = workspace();
    let org = write(&ws, "org.toml", "budget_tokens = 100000\n");
    let user = write(&ws, "user.toml", "budget_tokens = 20000\n");
    let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
    assert_eq!(profile.budget_tokens, Some(20_000));
    assert_eq!(profile.explain("budget_tokens").unwrap().layer, Layer::User);
}

#[test]
/// Purpose: Pin that a free-override key takes the most local layer that sets it.
fn free_override_takes_the_most_local_layer() {
    let ws = workspace();
    let org = write(&ws, "org.toml", "model = \"org-model\"\n");
    let user = write(&ws, "user.toml", "model = \"my-model\"\n");
    let session = write(&ws, "session.toml", "model = \"this-run\"\n");
    let profile = resolve(
        &[
            (Layer::Org, org),
            (Layer::User, user),
            (Layer::Session, session),
        ],
        &ws,
    )
    .unwrap();
    assert_eq!(profile.model.as_deref(), Some("this-run"));
    assert_eq!(profile.explain("model").unwrap().overridden.len(), 2);
}

#[test]
/// Purpose: Pin the project-layer refusal of `model`, `base_url` and `api_key`.
fn a_repository_may_not_choose_where_prompts_go() {
    let ws = workspace();
    for key in ["model", "base_url", "api_key"] {
        let project = write(
            &ws,
            "project.toml",
            &match key {
                "api_key" => "api_key = \"env:KEY\"\n".to_string(),
                "base_url" => "base_url = \"http://elsewhere.invalid/v1\"\n".to_string(),
                _ => format!("{key} = \"x\"\n"),
            },
        );
        let err = resolve(&[(Layer::Project, project)], &ws)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("may not set") || err.contains("must be a reference"),
            "{key}: {err}"
        );
    }
}

#[test]
/// Purpose: Pin that a project skill directory may not escape the workspace.
fn a_project_skill_dir_must_stay_inside_the_workspace() {
    let ws = workspace();
    let inside = write(&ws, "p1.toml", "skill_dirs = [\"./team-skills\"]\n");
    // Inside is fine…
    assert!(resolve(&[(Layer::Project, inside)], &ws).is_ok());

    // …but reaching out of the tree is not.
    let outside = write(&ws, "p2.toml", "skill_dirs = [\"/etc/skills\"]\n");
    let err = resolve(&[(Layer::Project, outside)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("must stay inside the workspace"), "{err}");
}

#[test]
/// Purpose: Pin that a literal key is refused while a reference is accepted.
fn secrets_never_live_in_config() {
    let ws = workspace();
    let literal = write(&ws, "u.toml", "api_key = \"sk-live-abcdef\"\n");
    let err = resolve(&[(Layer::User, literal)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("secrets never live in config"), "{err}");

    let reference = write(&ws, "u2.toml", "api_key = \"env:LOONG_KEY\"\n");
    let profile = resolve(&[(Layer::User, reference)], &ws).unwrap();
    assert_eq!(profile.api_key.as_deref(), Some("env:LOONG_KEY"));
}

#[test]
/// Purpose: Pin that add-only keys union across layers.
fn add_only_keys_union_across_layers() {
    let ws = workspace();
    std::fs::create_dir_all(ws.join("team-skills")).unwrap();
    let project = write(
        &ws,
        "p.toml",
        "skill_dirs = [\"./team-skills\"]\ndisable_skills = [\"org-loud\"]\n",
    );
    let user = write(
        &ws,
        "u.toml",
        "skill_dirs = [\"/home/someone/skills\"]\ndisable_skills = [\"other\"]\n",
    );
    let profile = resolve(&[(Layer::Project, project), (Layer::User, user)], &ws).unwrap();
    assert_eq!(profile.skill_dirs.len(), 2, "{:?}", profile.skill_dirs);
    // Union in precedence order (project before user), not alphabetically: the
    // order is how a reader sees which layer asked for what.
    assert_eq!(
        profile.disable_skills,
        vec!["org-loud".to_string(), "other".to_string()]
    );
    let row = profile.explain("skill_dirs").unwrap();
    assert_eq!(row.kind, Kind::AddOnly);
    assert_eq!(
        row.layer,
        Layer::Project,
        "the first layer that set it owns the row"
    );
}

#[test]
/// Purpose: Pin that an unknown key or bad value is an error naming the file.
fn unknown_keys_and_bad_values_are_loud() {
    let ws = workspace();
    // A typo must be named, not ignored: a guard that silently does not exist
    // is the failure this rule exists to prevent.
    let unknown = write(&ws, "a.toml", "budget_token = 100\n");
    let err = resolve(&[(Layer::User, unknown)], &ws)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("unknown") && err.contains("budget_token"),
        "{err}"
    );

    let bad_verdict = write(&ws, "b.toml", "[permissions]\ninvestigate = \"maybe\"\n");
    let err = resolve(&[(Layer::User, bad_verdict)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("allow | ask | deny"), "{err}");

    let bad_class = write(&ws, "c.toml", "[permissions]\nwhatever = \"allow\"\n");
    let err = resolve(&[(Layer::User, bad_class)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown action class"), "{err}");

    let bad_url = write(&ws, "d.toml", "base_url = \"ftp://x/y\"\n");
    let err = resolve(&[(Layer::User, bad_url)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("base_url must be"), "{err}");

    let bad_toml = write(&ws, "e.toml", "not toml {{{\n");
    assert!(matches!(
        resolve(&[(Layer::User, bad_toml)], &ws).unwrap_err(),
        ConfigError::Parse(_)
    ));
}

#[test]
/// Purpose: Pin that a layer file that does not exist is skipped.
fn absent_layers_are_skipped_not_invented() {
    let ws = workspace();
    let missing = ws.join("nope.toml");
    let profile = resolve(&[(Layer::User, missing)], &ws).unwrap();
    // Nothing was invented from a file that is not there…
    assert!(profile.model.is_none());
    assert!(profile.budget_tokens.is_none());
    assert!(profile.skill_dirs.is_empty());
    // …but the built-in ladder is always the baseline of the permission chain.
    assert_eq!(profile.explain_all().len(), 3);
    assert!(profile
        .explain_all()
        .iter()
        .all(|row| row.layer == Layer::Builtin));
    assert_eq!(
        profile.permission(ActionClass::OutOfBounds),
        Some(Verdict::Ask),
        "the built-in default, not an invented one"
    );
}

#[test]
/// Purpose: Pin that models, a router and its rules parse from one file.
fn several_models_and_a_router_can_be_declared() {
    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\n\
         name = \"fast\"\n\
         base_url = \"https://api.deepseek.com\"\n\
         model = \"deepseek-chat\"\n\
         api_key = \"env:DEEPSEEK_API_KEY\"\n\
         budget_tokens = 128000\n\n\
         [[models]]\n\
         name = \"long\"\n\
         base_url = \"https://api.moonshot.cn/v1\"\n\
         model = \"kimi-k2\"\n\
         budget_tokens = 512000\n\n\
         [router]\n\
         default = \"fast\"\n\
         [[router.rules]]\n\
         when = \"context_tokens > 150000\"\n\
         use = \"long\"\n",
    );
    let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
    assert_eq!(profile.models.len(), 2);
    assert_eq!(profile.default_model().unwrap().name, "fast");
    assert_eq!(
        profile.models[0].kind, "openai-compat",
        "the default adapter"
    );
    assert_eq!(profile.models[1].budget_tokens, Some(512_000));

    let (chosen, rule) = profile.router.select(&Facts {
        context_tokens: 10,
        step: 1,
    });
    assert_eq!(chosen, Some("fast"));
    assert!(rule.is_none(), "the default, not a rule");
    let (chosen, rule) = profile.router.select(&Facts {
        context_tokens: 200_000,
        step: 4,
    });
    assert_eq!(chosen, Some("long"));
    assert_eq!(rule.unwrap().condition.render(), "context_tokens > 150000");

    // Every model answers "why is this the value?"
    assert_eq!(profile.explain("models.fast").unwrap().layer, Layer::User);
    assert!(profile.explain("router.rules").is_some());
}

#[test]
/// Purpose: Pin that a provider shorthand fills the endpoint, id and key reference.
fn a_provider_shorthand_supplies_the_endpoint_the_model_and_the_key_reference() {
    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\n\
         name = \"cheap\"\n\
         provider = \"deepseek/deepseek-flash\"\n\n\
         [[models]]\n\
         name = \"strong\"\n\
         provider = \"moonshot/kimi-k3\"\n\
         base_url = \"https://example.invalid/v1\"\n",
    );
    let profile = resolve(&[(Layer::User, user)], &ws).unwrap();

    let cheap = &profile.models[0];
    assert_eq!(cheap.base_url, "https://api.deepseek.com");
    assert_eq!(cheap.model, "deepseek-flash");
    assert_eq!(
        cheap.api_key.as_deref(),
        Some("env:LOONG_DEEPSEEK_KEY"),
        "the shorthand yields a reference, so ADR-0008 §5 still holds"
    );
    assert_eq!(cheap.provider.as_deref(), Some("deepseek"));
    assert_eq!(
        cheap.kind, "openai-compat",
        "the shorthand implies the adapter"
    );

    // What a person wrote beats what the catalog assumes.
    let strong = &profile.models[1];
    assert_eq!(
        strong.base_url, "https://example.invalid/v1",
        "an explicit base_url is an override, not ignored"
    );
    assert_eq!(
        strong.model, "kimi-k3",
        "the model id still came from the shorthand"
    );
    assert_eq!(strong.api_key.as_deref(), Some("env:LOONG_MOONSHOT_KEY"));
}

#[test]
/// Purpose: Pin the error that names both the explicit and shorthand fixes.
fn a_model_with_neither_an_endpoint_nor_a_provider_names_both_ways_to_fix_it() {
    let ws = workspace();
    let user = write(&ws, "u.toml", "[[models]]\nname = \"x\"\nmodel = \"m\"\n");
    let err = resolve(&[(Layer::User, user)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("base_url"), "{err}");
    assert!(
        err.contains("provider"),
        "the error must also name the one-line form: {err}"
    );

    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"x\"\nbase_url = \"http://h/v1\"\n",
    );
    let err = resolve(&[(Layer::User, user)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("`model` id"), "{err}");
    assert!(err.contains("provider"), "{err}");
}

#[test]
/// Purpose: Pin that an unknown provider is refused before any model is built.
fn a_provider_nobody_knows_is_refused_rather_than_silently_endpointless() {
    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"x\"\nprovider = \"typo/m\"\n",
    );
    let err = resolve(&[(Layer::User, user)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown provider"), "{err}");
    assert!(
        err.contains("deepseek"),
        "and lists what it does know: {err}"
    );
}

#[test]
/// Purpose: Pin that `mode = "local"` in `local.toml` resolves and is honoured.
fn local_mode_is_a_setting_that_lives_in_the_local_file() {
    // The rule, stated directly rather than by writing to somebody's home directory:
    // `mode` means something in the local file and in a file named on the command
    // line, and nowhere else.
    assert!(
        mode_is_legitimate(Layer::User, &local_profile_path().display().to_string()),
        "the local file is where this key belongs"
    );
    assert!(
        mode_is_legitimate(Layer::Session, "/tmp/whatever-the-user-typed.toml"),
        "a session layer is a file somebody pointed at on purpose"
    );
    for forbidden in [
        "/home/me/.loongagent/profile.toml",
        "/etc/loongagent/profile.toml",
    ] {
        assert!(
            !mode_is_legitimate(Layer::User, forbidden),
            "{forbidden} is read because of where it sits, not because it was chosen"
        );
    }

    // Absent means managed, and that is the whole of it — `mode` in a file that is
    // read because of where it sits is refused whatever it says, because the
    // location is the problem, not the value.
    let ws = workspace();
    let bare = write(&ws, "u.toml", "budget_tokens = 1000\n");
    assert!(!resolve(&[(Layer::User, bare)], &ws).unwrap().is_local());
}

#[test]
/// Purpose: Pin the mode guard against the managed user file.
fn the_managed_profile_may_not_declare_local_mode() {
    // The bug this rule exists for: `mode = "local"` written into the *managed* user
    // file made `loongagent tui` — with no flag at all — resolve as a local install
    // and stop consulting the control plane. A key that can switch an install out of
    // its firm's policy does not belong in a file that is about something else.
    let ws = workspace();
    let managed = write(&ws, "profile.toml", "mode = \"local\"\n");
    let err = resolve(&[(Layer::User, managed)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("mode"), "{err}");
    assert!(
        err.contains("local.toml"),
        "and says where it does belong: {err}"
    );
    assert!(
        err.contains("--local"),
        "and how to get what they wanted: {err}"
    );
}

#[test]
/// Purpose: Pin that a local run's layer list excludes the managed profile and org file.
fn a_local_run_never_reads_the_managed_profile_or_the_org_file() {
    // The isolation, asserted on the layer list itself: if these paths are not in
    // it, no amount of configuration in them can reach a local run.
    let ws = workspace();
    let layers = local_layer_paths(&ws, None);
    let paths: Vec<String> = layers
        .iter()
        .map(|(_, path)| path.display().to_string())
        .collect();

    assert!(
        paths.iter().any(|p| p.ends_with("local.toml")),
        "it reads its own file: {paths:?}"
    );
    assert!(
        !paths.contains(&loongagent_home().join("profile.toml").display().to_string()),
        "and not the managed one: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.starts_with("/etc/")),
        "and not the machine-wide org file: {paths:?}"
    );
    assert!(
        !layers.iter().any(|(layer, _)| *layer == Layer::Org),
        "which is the same statement in terms of layers"
    );
}

#[test]
/// Purpose: Pin that models keep declaration order rather than sorting by name.
fn models_come_back_in_the_order_they_were_declared() {
    // The order *is* the tier ladder: cheapest first, most capable last, and the
    // digits the status bar counts by. The merge collects through a map keyed
    // `models.<name>`, so this is the property that a map iteration would destroy —
    // and it was destroyed, by an alphabetical sort, until a real profile showed it.
    //
    // Deliberately written in an order that is *not* alphabetical in either
    // direction, so a sort in either direction fails the test.
    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"zulu\"\nprovider = \"ollama/one\"\n\n\
         [[models]]\nname = \"alpha\"\nprovider = \"deepseek/two\"\n\n\
         [[models]]\nname = \"mike\"\nprovider = \"moonshot/three\"\n\n\
         [router]\ndefault = \"zulu\"\n",
    );
    let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
    let names: Vec<&str> = profile.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["zulu", "alpha", "mike"],
        "declaration order, which is the ladder"
    );

    // And the ladder is what the router's default names, so the two agree.
    assert_eq!(profile.default_model().unwrap().name, "zulu");
}

#[test]
/// Purpose: Pin that a locally declared order wins and a lower-only model is appended.
fn a_model_only_a_lower_layer_declares_is_appended_rather_than_lost() {
    // The order comes from the most local layer that declares any models, but the
    // *set* is a union: an organisation may add a model this user never mentioned,
    // and it has to remain reachable.
    let ws = workspace();
    let org = write(
        &ws,
        "org.toml",
        "[[models]]\nname = \"org-only\"\nprovider = \"deepseek/org\"\n",
    );
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"mine\"\nprovider = \"ollama/mine\"\n",
    );
    let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
    let names: Vec<&str> = profile.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["mine", "org-only"],
        "the person's own order leads, and the org's extra model follows"
    );
}

#[test]
/// Purpose: Pin the project-layer refusal of `mode`.
fn a_repository_may_not_switch_an_install_out_of_managed_mode() {
    let ws = workspace();
    let project = write(&ws, ".loongagent/profile.toml", "mode = \"local\"\n");
    let err = resolve(&[(Layer::Project, project)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("mode"), "{err}");
    assert!(
        err.contains("managed"),
        "and says what it would have escaped: {err}"
    );
}

#[test]
/// Purpose: Pin that an unknown mode is an error, not a silent managed run.
fn a_mode_nobody_implements_is_refused_rather_than_read_as_managed() {
    let ws = workspace();
    let user = write(&ws, "u.toml", "mode = \"offline\"\n");
    let err = resolve(&[(Layer::User, user)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("offline"), "{err}");
    assert!(err.contains("\"local\""), "{err}");
}

#[test]
/// Purpose: Pin that `model` plus `base_url` still becomes one default model.
fn the_shorthand_still_means_one_model() {
    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "base_url = \"https://api.deepseek.com\"\nmodel = \"deepseek-chat\"\n",
    );
    let profile = resolve(&[(Layer::User, user)], &ws).unwrap();
    assert_eq!(profile.models.len(), 1, "one model, synthesized");
    assert_eq!(profile.models[0].name, "default");
    assert_eq!(profile.router.default.as_deref(), Some("default"));
    assert_eq!(
        profile.base_url.as_deref(),
        Some("https://api.deepseek.com")
    );
}

#[test]
/// Purpose: Pin the project-layer refusal of `[[models]]`.
fn a_repository_may_not_define_a_model() {
    let ws = workspace();
    let project = write(
        &ws,
        "p.toml",
        "[[models]]\nname = \"sneaky\"\nbase_url = \"http://elsewhere.invalid\"\nmodel = \"m\"\n",
    );
    let err = resolve(&[(Layer::Project, project)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("may not define models"), "{err}");
}

#[test]
/// Purpose: Pin that a project may route among models a higher layer defines.
fn a_project_may_route_among_models_it_did_not_define() {
    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"fast\"\nbase_url = \"https://a.invalid\"\nmodel = \"m\"\n\n\
         [[models]]\nname = \"cheap\"\nbase_url = \"https://b.invalid\"\nmodel = \"m\"\n",
    );
    let project = write(
        &ws,
        "p.toml",
        "[router]\ndefault = \"cheap\"\n[[router.rules]]\nwhen = \"step > 3\"\nuse = \"cheap\"\n",
    );
    let profile = resolve(&[(Layer::User, user), (Layer::Project, project)], &ws).unwrap();
    assert_eq!(
        profile.router.default.as_deref(),
        Some("cheap"),
        "the project may shape routing within what exists"
    );
}

#[test]
/// Purpose: Pin that a model redefinition may lower its budget but not raise it.
fn a_model_budget_only_narrows_across_layers() {
    let ws = workspace();
    let org = write(
        &ws,
        "org.toml",
        "[[models]]\nname = \"m\"\nbase_url = \"https://a.invalid\"\nmodel = \"x\"\nbudget_tokens = 100000\n",
    );
    // The user redefines the same model with a larger budget, and a different endpoint.
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"m\"\nbase_url = \"https://b.invalid\"\nmodel = \"y\"\nbudget_tokens = 500000\n",
    );
    let profile = resolve(&[(Layer::Org, org), (Layer::User, user)], &ws).unwrap();
    let model = &profile.models[0];
    assert_eq!(
        model.base_url, "https://b.invalid",
        "the local endpoint wins"
    );
    assert_eq!(model.model, "y");
    assert_eq!(
        model.budget_tokens,
        Some(100_000),
        "but the org's ceiling does not"
    );
}

#[test]
/// Purpose: Pin that a router naming an undefined model is refused.
fn a_router_that_names_a_missing_model_is_an_error() {
    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"fast\"\nbase_url = \"https://a.invalid\"\nmodel = \"m\"\n\n\
         [router]\ndefault = \"fast\"\n[[router.rules]]\nwhen = \"step > 1\"\nuse = \"ghost\"\n",
    );
    let err = resolve(&[(Layer::User, user)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ghost") && err.contains("fast"), "{err}");
}

#[test]
/// Purpose: Pin that a malformed routing condition names the file and rule.
fn conditions_are_checked_where_they_are_written() {
    let ws = workspace();
    for (when, expected) in [
        ("token_count > 10", "unknown fact"),
        ("context_tokens 10", "needs a comparison"),
        ("context_tokens > many", "whole number"),
    ] {
        let user = write(
            &ws,
            "u.toml",
            &format!(
                "base_url = \"https://a.invalid\"\nmodel = \"m\"\n[router]\n[[router.rules]]\nwhen = \"{when}\"\nuse = \"default\"\n"
            ),
        );
        let err = resolve(&[(Layer::User, user)], &ws)
            .unwrap_err()
            .to_string();
        assert!(err.contains(expected), "{when}: {err}");
    }
}

#[test]
/// Purpose: Pin that an unknown adapter kind is refused and the known ones are named.
fn an_unknown_adapter_kind_is_refused_with_the_list() {
    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"m\"\nkind = \"telepathy\"\nbase_url = \"https://a.invalid\"\nmodel = \"x\"\n",
    );
    let err = resolve(&[(Layer::User, user)], &ws)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("telepathy") && err.contains("openai-compat"),
        "{err}"
    );
}

#[test]
/// Purpose: Pin every comparison operator against its fact.
fn conditions_evaluate_the_facts_they_name() {
    let condition = parse_condition("context_tokens >= 1000").unwrap();
    assert!(!condition.matches(&Facts {
        context_tokens: 999,
        step: 0
    }));
    assert!(condition.matches(&Facts {
        context_tokens: 1000,
        step: 0
    }));
    let step = parse_condition("step == 3").unwrap();
    assert!(step.matches(&Facts {
        context_tokens: 0,
        step: 3
    }));
    assert!(!step.matches(&Facts {
        context_tokens: 0,
        step: 4
    }));
    let less = parse_condition("step < 2").unwrap();
    assert!(less.matches(&Facts {
        context_tokens: 0,
        step: 1
    }));
}

#[test]
/// Purpose: Pin first-match routing and the default when no rule fires.
fn the_router_takes_the_first_matching_rule() {
    let router = Router {
        default: Some("fallback".to_string()),
        rules: vec![
            RouterRule {
                condition: parse_condition("context_tokens > 10").unwrap(),
                model: "big".to_string(),
            },
            RouterRule {
                condition: parse_condition("context_tokens > 10").unwrap(),
                model: "never".to_string(),
            },
        ],
    };
    let (model, rule) = router.select(&Facts {
        context_tokens: 11,
        step: 0,
    });
    assert_eq!(model, Some("big"));
    assert_eq!(rule.unwrap().model, "big");
    let (model, rule) = router.select(&Facts {
        context_tokens: 1,
        step: 0,
    });
    assert_eq!(model, Some("fallback"));
    assert!(rule.is_none());
}

const EXAMPLE_PACK: &str = "\
default_profile = \"default\"\n\
\n\
[[profiles]]\n\
name = \"default\"\n\
description = \"Balanced: cheap for the loop, escalating past 150k tokens.\"\n\
  [[profiles.models]]\n\
  name = \"fast\"\n\
  base_url = \"https://api.deepseek.com\"\n\
  model = \"an-id-from-the-docs\"\n\
  budget_tokens = 128000\n\
  max_context_size = 131072\n\
  [[profiles.models]]\n\
  name = \"frontier\"\n\
  base_url = \"https://api.example.invalid/v1\"\n\
  model = \"big\"\n\
  [profiles.router]\n\
  default = \"fast\"\n\
  [[profiles.router.rules]]\n\
  when = \"context_tokens > 150000\"\n\
  use = \"frontier\"\n\
\n\
[[profiles]]\n\
name = \"local-only\"\n\
description = \"Air-gapped: nothing leaves the machine.\"\n\
  [[profiles.models]]\n\
  name = \"local\"\n\
  base_url = \"http://127.0.0.1:8000/v1\"\n\
  model = \"whatever-you-run\"\n";

#[test]
/// Purpose: Pin that a pack payload becomes named profiles with their descriptions and router.
fn a_pack_payload_becomes_named_profiles_with_descriptions() {
    let pack = profiles_from_payload(EXAMPLE_PACK).unwrap();
    assert_eq!(pack.default_profile.as_deref(), Some("default"));
    assert_eq!(pack.profiles.len(), 2);
    assert_eq!(pack.profiles[0].name, "default");
    assert!(pack.profiles[0].description.contains("Balanced"));
    assert_eq!(pack.profiles[0].models.len(), 2);
    // The window a model holds belongs in the pack: it is a fact about the endpoint
    // the pack names, and the engineer never sees the pack (T-07).
    assert_eq!(pack.profiles[0].models[0].budget_tokens, Some(128_000));
    assert_eq!(pack.profiles[0].models[0].max_context_size, Some(131_072));
    assert_eq!(pack.profiles[1].name, "local-only");

    // Choosing is by name, and a name that is not there is an error that lists
    // what is — never a silent fallback to another profile.
    assert_eq!(pack.pick(Some("local-only")).unwrap().name, "local-only");
    let err = pack.pick(Some("supersmart")).unwrap_err().to_string();
    assert!(
        err.contains("supersmart") && err.contains("local-only"),
        "{err}"
    );
    assert_eq!(
        pack.pick(None).unwrap().name,
        "default",
        "the pack's default"
    );
}

#[test]
/// Purpose: Pin that a literal key in a user profile is refused as not a reference.
fn a_literal_in_a_config_file_is_still_refused() {
    // The pack may carry a secret for now; a file a repository or a dotfiles
    // directory can commit may not. That is the line the amendment keeps.
    let ws = workspace();
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"m\"\nbase_url = \"https://a.invalid\"\nmodel = \"i\"\napi_key = \"sk-live-abc\"\n",
    );
    let err = resolve(&[(Layer::User, user)], &ws)
        .unwrap_err()
        .to_string();
    assert!(err.contains("must be a reference"), "{err}");
    assert!(err.contains("ADR-0008"), "{err}");
}

#[test]
/// Purpose: Pin that a value pasted after `env:` is refused as not a variable name.
fn a_secret_pasted_after_the_prefix_is_refused() {
    // The mistake this exists for: `env:` followed by the key itself passes a
    // prefix check, and then the key is inside the signed pack — and inside a
    // committed draft. Every value below is made up — no real key is written
    // into a test, because a test is a file that gets committed.
    for (value, why) in [
        (
            "env:sk-0123456789abcdef0123456789abcdef",
            "a pasted key after env:",
        ),
        (
            "env:sk0123456789abcdef0123456789abcdef",
            "alphanumeric but lowercase",
        ),
        ("env:deepseekApiKey", "not the upper-case convention"),
        ("env:", "empty"),
        (
            "file:credentials.toml#sk-live-abc",
            "a pasted key as the table name",
        ),
        ("keychain:", "empty"),
    ] {
        let payload = format!(
            "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  \
             base_url = \"https://a.invalid\"\n  model = \"i\"\n  api_key = \"{value}\"\n"
        );
        let error = profiles_from_payload(&payload).unwrap_err().to_string();
        assert!(
            error.contains("reference")
                || error.contains("variable name")
                || error.contains("secret"),
            "{why}: {error}"
        );
    }

    // And the shapes that are references still work.
    for value in [
        "env:DEEPSEEK_API_KEY",
        "env:LOONGAGENT_CONTROL_TOKEN",
        "file:/etc/loongagent/client.token",
        "file:credentials.toml#DEEPSEEK_API_KEY",
        "keychain:loongagent/deepseek",
        "gateway:acme/profiles",
    ] {
        let payload = format!(
            "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  \
             base_url = \"https://a.invalid\"\n  model = \"i\"\n  api_key = \"{value}\"\n"
        );
        assert!(
            profiles_from_payload(&payload).is_ok(),
            "{value} must be accepted"
        );
    }
}

#[test]
/// Purpose: Pin the pack errors for an empty profile set, an empty model set and an unknown route, plus the warning for a pack's literal secret.
fn a_broken_pack_says_what_is_wrong() {
    let err = profiles_from_payload("[[profiles]]\nname = \"x\"\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no models"), "{err}");
    let err = profiles_from_payload("default_profile = \"d\"\n")
        .unwrap_err()
        .to_string();
    assert!(err.contains("no profiles"), "{err}");
    let err = profiles_from_payload(
        "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  base_url = \"https://a.invalid\"\n  model = \"i\"\n  [profiles.router]\n  default = \"ghost\"\n",
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("ghost"), "{err}");
    // A literal in a pack is accepted *and warned about*: the distribution channel
    // may carry it until identity and the gateway exist (ADR-0008 §5 amendment).
    let pack = profiles_from_payload(
        "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  base_url = \"https://a.invalid\"\n  model = \"i\"\n  api_key = \"sk-literal\"\n",
    )
    .unwrap();
    assert_eq!(pack.warnings.len(), 1, "{:?}", pack.warnings);
    assert!(
        pack.warnings[0].contains("carries the secret itself"),
        "{:?}",
        pack.warnings[0]
    );
    assert!(
        pack.warnings[0].contains("ADR-0008"),
        "and names the rule it bends"
    );

    // A well-formed reference warns about nothing.
    let clean = profiles_from_payload(
        "[[profiles]]\nname = \"x\"\n  [[profiles.models]]\n  name = \"m\"\n  base_url = \"https://a.invalid\"\n  model = \"i\"\n  api_key = \"env:DEEPSEEK_API_KEY\"\n",
    )
    .unwrap();
    assert!(clean.warnings.is_empty(), "{:?}", clean.warnings);
}

#[test]
/// Purpose: Pin that a verified pack outranks the local org file and leaves provenance.
fn the_pack_is_the_control_layer_and_it_outranks_a_local_org_file() {
    let ws = workspace();
    // A stale local org file naming a different endpoint for the same model.
    let org = write(
        &ws,
        "org.toml",
        "[[models]]\nname = \"fast\"\nbase_url = \"http://stale.invalid\"\nmodel = \"old\"\n",
    );
    let profile = resolve_with(
        &[(Layer::Org, org)],
        &ws,
        Some((
            "control plane: acme 1".to_string(),
            pack_layer(
                &profiles_from_payload(EXAMPLE_PACK)
                    .unwrap()
                    .pick(None)
                    .unwrap()
                    .clone(),
            ),
        )),
    )
    .unwrap();
    let model = &profile.models[0];
    assert_eq!(
        model.base_url, "https://api.deepseek.com",
        "the verified pack wins"
    );
    let provenance = profile.explain("models.fast").unwrap();
    assert_eq!(provenance.layer, Layer::Control);
    assert!(
        provenance.source.contains("acme"),
        "and the row names the pack"
    );
}

#[test]
/// Purpose: Pin the control-layer lock against a rewrite, while narrowing stays allowed.
fn a_user_or_project_may_not_rewrite_what_the_control_plane_set() {
    let ws = workspace();
    let pack = || {
        Some((
            "control plane: acme 1".to_string(),
            pack_layer(
                &profiles_from_payload(EXAMPLE_PACK)
                    .unwrap()
                    .pick(None)
                    .unwrap()
                    .clone(),
            ),
        ))
    };

    // The endpoint of a control-plane model: refused, and the message says why.
    let user = write(
        &ws,
        "u.toml",
        "[[models]]\nname = \"fast\"\nbase_url = \"http://mine.invalid\"\nmodel = \"mine\"\n",
    );
    let err = resolve_with(&[(Layer::User, user)], &ws, pack())
        .unwrap_err()
        .to_string();
    assert!(err.contains("control plane"), "{err}");
    assert!(err.contains("cannot be changed"), "{err}");
    assert!(err.contains("pick a different profile"), "{err}");

    // The routing table too: the profile carries it.
    let project = write(&ws, "p.toml", "[router]\ndefault = \"frontier\"\n");
    assert!(resolve(&[(Layer::Project, project)], &ws).is_err());

    // But narrowing is still allowed — an org ceiling stays a ceiling, and a
    // local layer may only make things stricter.
    let narrowing = write(&ws, "n.toml", "budget_tokens = 32000\n");
    let narrowed = resolve_with(&[(Layer::User, narrowing)], &ws, pack()).unwrap();
    assert_eq!(narrowed.budget_tokens, Some(32_000));

    // And the user's own choice of profile is theirs.
    let choice = write(&ws, "c.toml", "control.profile = \"local-only\"\n");
    let chosen = resolve_with(&[(Layer::User, choice)], &ws, pack()).unwrap();
    assert_eq!(chosen.control_profile.as_deref(), Some("local-only"));
}

#[test]
/// Purpose: Pin that `kind_of` answers for every key the layer loader can produce.
fn every_known_key_declares_a_kind() {
    for key in [
        "model",
        "base_url",
        "profile",
        "budget_tokens",
        "keep_newest_tokens",
        "max_steps",
        "compact_at_percent",
        "permissions",
        "skill_dirs",
        "disable_skills",
        "api_key",
    ] {
        assert!(kind_of(key).is_some(), "{key} has no kind");
    }
    assert!(kind_of("not_a_key").is_none());
}

#[test]
/// Purpose: Pin the builtin-to-session layer order.
fn the_layer_order_is_the_rule() {
    assert!(Layer::Builtin < Layer::Org);
    assert!(Layer::Org < Layer::Project);
    assert!(Layer::Project < Layer::User);
    assert!(Layer::User < Layer::Session);
}

#[test]
/// Purpose: Pin that the user layer resolves under `LOONGAGENT_HOME`.
fn layer_paths_are_where_they_claim_to_be() {
    // The user layer lives under one root, and `LOONGAGENT_HOME` moves it.
    std::env::set_var("LOONGAGENT_HOME", "/tmp/loong-home-test");
    let layers = layer_paths(Path::new("/work/repo"), None);
    let user = layers
        .iter()
        .find(|(layer, _)| *layer == Layer::User)
        .map(|(_, path)| path.clone())
        .unwrap();
    assert_eq!(user, PathBuf::from("/tmp/loong-home-test/profile.toml"));
    std::env::remove_var("LOONGAGENT_HOME");
}

#[test]
/// Purpose: Pin that the managed layer paths are the documented ones.
fn layer_paths_used_to_be_where_they_claim_to_be() {
    let ws = workspace();
    let paths = layer_paths(&ws, Some(Path::new("/tmp/session.toml")));
    let layers: Vec<Layer> = paths.iter().map(|(l, _)| *l).collect();
    assert_eq!(
        layers,
        vec![Layer::Org, Layer::Project, Layer::User, Layer::Session]
    );
    assert_eq!(paths[1].1, ws.join(".loongagent/profile.toml"));
    assert_eq!(paths[3].1, PathBuf::from("/tmp/session.toml"));
}
