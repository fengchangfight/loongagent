//! Purpose: Build a session's kernel: open the log, engine, models, MCP servers, skills, hooks, PDP ladder and confinement, then seat and replay its context.
//!
//! Serves: one construction path, so the live kernel and a later `session.new` cannot
//! disagree about what the session may do (ADR-0004, ADR-0007, ADR-0018).
//!
//! # Contract
//! - **Design:** `docs/design/kernel-loop.md` §2; `docs/design/tools-permissions.md`
//!   §4 owns the PEP and `docs/design/extension-model.md` the MCP and skill wiring.
//! - **Upstream:** `crates/cli/src/server.rs`'s kernel factory; the server and turn
//!   tests that build a kernel directly.
//! - **Downstream:** `ContextEngine::open`, `SessionLog`, `Index`, `Registry`,
//!   `McpClient`, `SkillSet`, `Pdp`, `HookRunner` and `Kernel::seat_system_prompt`.
//! - **Verify:** `cargo test -p loong-kernel turn::tests` · `./scripts/check.sh`.

use super::*;

/// Purpose: The bridge from `mcp` hooks to this session's live MCP servers.
///
/// Serves: hook kinds that call a server without the hook engine learning MCP.
/// Design: `docs/design/extension-model.md` §3.
/// Up: `Kernel::build` builds it; `HookRunner::with_mcp` and `SkillTool::with_fetcher` hold it.
/// Down: `McpClient::{name, get_prompt, call_tool}`.
/// Verify: `cargo test -p loong-kernel turn::tests::an_mcp_hook_can_gate_a_builtin_tool`.
/// The bridge from `mcp` hooks to this session's servers. The hook engine stays
/// ignorant of MCP; the kernel, which owns the servers, supplies the capability.
pub(super) struct KernelMcp {
    clients: Vec<Arc<Mutex<McpClient>>>,
}

impl PromptFetcher for KernelMcp {
    /// Purpose: Fetch one prompt body from the named server, mapping the invocation's arguments.
    ///
    /// Serves: a command whose body lives on an MCP server loading like any other skill.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: `SkillTool` through the `PromptFetcher` trait; `Kernel::fetch_mcp_prompt` reaches it.
    /// Down: `McpClient::get_prompt` and `loong_mcp::map_prompt_arguments`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_model_can_fetch_an_mcp_prompt_body_with_the_tool`.
    fn fetch(
        &self,
        server: &str,
        prompt: &str,
        argument_names: &[String],
        args: &str,
    ) -> Result<String, String> {
        let client = self
            .clients
            .iter()
            .find(|c| c.lock().map(|c| c.name() == server).unwrap_or(false))
            .ok_or_else(|| format!("no MCP server named {server:?} is running"))?;
        let arguments = loong_mcp::map_prompt_arguments(argument_names, args);
        let mut client = client
            .lock()
            .map_err(|_| format!("the {server} client is poisoned"))?;
        client
            .get_prompt(prompt, &arguments)
            .map_err(|e| e.to_string())
    }
}

impl McpCaller for KernelMcp {
    /// Purpose: Call an MCP tool through the named server for an `mcp` hook.
    ///
    /// Contract: `turn/build.rs` hook bridge.
    fn call(&self, server: &str, tool: &str, args: &Json) -> Result<String, String> {
        let client = self
            .clients
            .iter()
            .find(|c| c.lock().map(|c| c.name() == server).unwrap_or(false))
            .ok_or_else(|| format!("no MCP server named {server:?} is running"))?;
        let mut client = client
            .lock()
            .map_err(|_| format!("the {server} client is poisoned"))?;
        client.call_tool(tool, args).map_err(|e| e.to_string())
    }
}

/// Purpose: What attaching MCP produced: a note, the live clients and the prompts that became commands.
///
/// Serves: one build step returning server state without a second query.
/// Design: `docs/design/extension-model.md` §3.
/// Up: `attach_mcp` returns it; `Kernel::build` destructures it.
/// Down: the `KernelMcp` bridge and the prompt skills.
/// Verify: `cargo test -p loong-kernel turn::tests::an_mcp_tool_is_just_another_tool`.
/// Start the trusted MCP servers and register their tools.
///
/// A server that is `required` (the default) and cannot be used is fatal: the
/// operator asked for it, and quietly running without a guard rail they believe is
/// present would be the worst outcome. `required = false` opts into a note.
struct AttachedMcp {
    note: String,
    clients: Vec<Arc<Mutex<McpClient>>>,
    prompt_skills: Vec<Skill>,
}

/// Purpose: Start the trusted MCP servers and register their tools and prompts.
///
/// Serves: external tools behind the same registry and gate as the built-ins, with a required server fatal when unusable.
/// Design: `docs/design/extension-model.md` §3.
/// Up: `Kernel::build`.
/// Down: `McpSet::load`, `McpClient::start`, `list_tools`, `list_prompts` and `Registry::register`.
/// Verify: `cargo test -p loong-kernel turn::tests::a_required_server_that_cannot_start_is_fatal`.
fn attach_mcp(workspace: &Path, tools: &mut Registry) -> Result<AttachedMcp, KernelError> {
    let set = McpSet::load(workspace).map_err(|e| KernelError::Mcp(e.to_string()))?;
    let mut note = set.summary();
    let mut clients = Vec::new();
    let mut prompt_skills = Vec::new();
    for spec in set.active() {
        let mut client = match McpClient::start(spec) {
            Ok(client) => client,
            Err(e) if spec.required => {
                return Err(KernelError::Mcp(format!("server {:?}: {e}", spec.name)))
            }
            Err(e) => {
                note.push_str(&format!("; {} unavailable: {e}", spec.name));
                continue;
            }
        };
        let listed = match client.list_tools() {
            Ok(listed) => listed,
            Err(e) if spec.required => {
                return Err(KernelError::Mcp(format!("server {:?}: {e}", spec.name)))
            }
            Err(e) => {
                note.push_str(&format!("; {} unavailable: {e}", spec.name));
                continue;
            }
        };
        let count = listed.len();
        let client = Arc::new(Mutex::new(client));
        for info in listed {
            let class = spec.class_for(&info.name);
            tools.register(McpTool::new(&spec.name, info, class, Arc::clone(&client)));
        }
        // A server's prompts become commands: same catalogue, same /name syntax,
        // body fetched from the server on invoke.
        let prompts = match client.lock() {
            Ok(mut locked) => locked.list_prompts(),
            Err(_) => Err(loong_mcp::McpError(format!(
                "the {} client is poisoned",
                spec.name
            ))),
        };
        match prompts {
            Ok(prompts) => {
                if !prompts.is_empty() {
                    note.push_str(&format!(", {} prompt(s)", prompts.len()));
                }
                for prompt in prompts {
                    let names: Vec<String> =
                        prompt.arguments.iter().map(|a| a.name.clone()).collect();
                    let hint = if names.is_empty() {
                        None
                    } else {
                        Some(
                            names
                                .iter()
                                .map(|n| {
                                    if prompt.arguments.iter().any(|a| &a.name == n && a.required) {
                                        format!("<{n}>")
                                    } else {
                                        format!("[{n}]")
                                    }
                                })
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                    };
                    prompt_skills.push(Skill {
                        // Namespaced like MCP tools, so a prompt can never shadow a
                        // workspace skill.
                        name: format!("mcp__{}__{}", spec.name, prompt.name),
                        description: prompt.description,
                        argument_hint: hint,
                        allowed_tools: None,
                        origin: Origin::Mcp {
                            server: spec.name.clone(),
                            prompt: prompt.name,
                            arguments: names,
                        },
                    });
                }
            }
            Err(e) => {
                note.push_str(&format!("; {} prompts unavailable: {e}", spec.name));
            }
        }
        clients.push(client);
        note.push_str(&format!("; {} exposed {count} tool(s)", spec.name));
    }
    Ok(AttachedMcp {
        note,
        clients,
        prompt_skills,
    })
}
impl Kernel {
    /// Purpose: Create a fresh session log and a kernel with one model.
    ///
    /// Serves: the simplest construction a caller or a test needs.
    /// Design: `docs/design/kernel-loop.md` §1.
    /// Up: `crates/server`'s tests and the turn tests; no production caller (`crates/cli/src/server.rs` uses `create_with_models`).
    /// Down: `SessionLog::create` and `Kernel::build`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_tool_runs_and_the_turn_finishes`.
    pub fn create(
        log_path: impl AsRef<Path>,
        config: KernelConfig,
        model: Box<dyn Model>,
    ) -> Result<Self, KernelError> {
        Self::build(
            SessionLog::create(log_path)?,
            config,
            vec![("default".to_string(), model)],
        )
    }

    /// Purpose: Open an existing session log and a kernel with one model.
    ///
    /// Serves: resume for a caller or a test that has one model.
    /// Design: `docs/design/sessions-lineage.md` §5.
    /// Up: `turn::tests` only; no production caller (`crates/server`'s tests and the
    /// CLI use `create` or `open_with_models`).
    /// Down: `SessionLog::open` and `Kernel::build`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_reopened_session_hands_the_model_its_own_history`.
    pub fn open(
        log_path: impl AsRef<Path>,
        config: KernelConfig,
        model: Box<dyn Model>,
    ) -> Result<Self, KernelError> {
        Self::build(
            SessionLog::open(log_path)?,
            config,
            vec![("default".to_string(), model)],
        )
    }

    /// Purpose: Create a fresh session log and a kernel with the profile's models.
    ///
    /// Serves: the CLI's one construction path for a new session.
    /// Design: `docs/design/kernel-loop.md` §1.
    /// Up: `crates/cli/src/server.rs`'s kernel factory; the server tests.
    /// Down: `SessionLog::create` and `Kernel::build`.
    /// Verify: `cargo test -p loong-kernel turn::tests::the_router_can_switch_models_and_the_log_says_why`.
    /// A session with several models and the router that chooses between them.
    pub fn create_with_models(
        log_path: impl AsRef<Path>,
        config: KernelConfig,
        models: Vec<(String, Box<dyn Model>)>,
    ) -> Result<Self, KernelError> {
        Self::build(SessionLog::create(log_path)?, config, models)
    }

    /// Purpose: Open an existing session log and a kernel with the profile's models.
    ///
    /// Serves: resume from the CLI with the same model set a new session would get.
    /// Design: `docs/design/sessions-lineage.md` §5.
    /// Up: `crates/cli/src/server.rs`'s kernel factory; the server tests.
    /// Down: `SessionLog::open` and `Kernel::build`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_reopened_session_hands_the_model_its_own_history`.
    pub fn open_with_models(
        log_path: impl AsRef<Path>,
        config: KernelConfig,
        models: Vec<(String, Box<dyn Model>)>,
    ) -> Result<Self, KernelError> {
        Self::build(SessionLog::open(log_path)?, config, models)
    }

    /// Purpose: Build the kernel: open the engine, resolve models and limits, wire tools, skills, hooks and confinement, seat the prompt and replay history.
    ///
    /// Serves: one place where every capability is chosen and every narrowing is applied.
    /// Design: `docs/design/kernel-loop.md` §1–§2; `docs/design/tools-permissions.md` §4.
    /// Up: the four public constructors.
    /// Down: `ContextEngine::open`, `Ledger::new`, `Index::rebuild_into`, `attach_mcp`, `SkillSet::load_in`, `Pdp::tighten_from`, `HookSet::load`, `seat_system_prompt` and `replay_history`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_profile_narrows_the_ladder_and_the_rule_names_the_layer`.
    fn build(
        log: SessionLog,
        mut config: KernelConfig,
        models: Vec<(String, Box<dyn Model>)>,
    ) -> Result<Self, KernelError> {
        // Open the layered memory before anything else can fail, so a store that
        // cannot be opened is reported once, at session start, rather than
        // discovered later as a missing map.
        let engine = match config.context_engine.take() {
            Some(engine_config) if engine_config.enabled => {
                match crate::context_engine::ContextEngine::open(engine_config) {
                    Ok(engine) => Some(engine),
                    Err(e) => {
                        // Not fatal: the session runs without the map and the
                        // reason is kept for the status surface.
                        eprintln!("context engine unavailable: {e}");
                        None
                    }
                }
            }
            _ => None,
        };
        let router = config.settings.router.clone();
        let mut by_name = std::collections::BTreeMap::new();
        for (name, model) in models {
            by_name.insert(name, model);
        }
        // The router's default, else whatever exists. A rule that names a model this
        // session does not have falls back here instead of failing the turn.
        let selected_model = router
            .default
            .clone()
            .filter(|name| by_name.contains_key(name))
            .or_else(|| by_name.keys().next().cloned())
            .ok_or_else(|| {
                KernelError::Model(ModelError::Transport("no model configured".to_string()))
            })?;
        // Narrow-only keys (ADR-0008 §2): the strictest of what the caller asked for
        // and what the layered configuration says. One place does this arithmetic,
        // so a session flag can never widen a cap that config set.
        config.budget_tokens = effective_budget(config.budget_tokens, &config.settings);
        // Larger is stricter for a reserve — keeping more room for the answer is the
        // conservative choice — so a layer may raise it but never shrink it. `0` and unset
        // both mean "use the default".
        if let Some(size) = config
            .settings
            .reserved_context_size
            .filter(|size| *size > 0)
        {
            config.reserved_context_size = config.reserved_context_size.max(size);
        }
        config.keep_newest_tokens = config
            .keep_newest_tokens
            .min(config.settings.keep_newest_tokens.unwrap_or(u64::MAX));
        config.compact_at_percent = config
            .compact_at_percent
            .min(config.settings.compact_at_percent.unwrap_or(u64::MAX));
        config.max_steps = steps_or_unlimited(config.max_steps).min(
            config
                .settings
                .max_steps
                .map(steps_or_unlimited)
                .unwrap_or(usize::MAX),
        );
        let ledger = Ledger::new(config.budget_tokens);
        let index_path = log.path().with_extension("sqlite");
        // Rebuild rather than attach incrementally: the index is disposable, and
        // a stale index that silently disagrees with the log is worse than a
        // few milliseconds of replay.
        let index = loong_index::Index::rebuild_into(&index_path, log.path())?;
        // External tools join the same registry as the built-ins (T-15): one
        // interface, one PDP, one event stream, one audit trail.
        let mut tools = Registry::with_builtins();
        let attached = attach_mcp(&config.cwd, &mut tools)?;
        let (mcp_note, mcp_clients, mut prompt_skills) =
            (attached.note, attached.clients, attached.prompt_skills);
        // Configuration can disable an entry by identity (ADR-0008 add-only rule:
        // content is advisory, so a project may switch off an org's skill — and it
        // is recorded rather than silently dropped).
        let disabled_config = &config.settings.disable_skills;
        let mut disabled: Vec<String> = Vec::new();
        prompt_skills.retain(|skill| {
            if disabled_config.iter().any(|name| name == &skill.name) {
                disabled.push(skill.name.clone());
                false
            } else {
                true
            }
        });
        let mcp_caller: Option<Arc<KernelMcp>> = (!mcp_clients.is_empty()).then(|| {
            Arc::new(KernelMcp {
                clients: mcp_clients.clone(),
            })
        });
        // Skills are one concept with commands (T-18) — and MCP prompts are skills
        // whose body lives on the server (T-15): the model sees the catalogue and
        // asks for a body by name.
        let skills = Arc::new(
            SkillSet::load_in(&config.cwd, &config.settings.skill_dirs, disabled_config)
                .map_err(|e| KernelError::Skills(e.to_string()))?
                .with_external(prompt_skills)
                .map_err(|e| KernelError::Skills(e.to_string()))?,
        );
        disabled.extend(skills.disabled.iter().cloned());
        disabled.sort();
        disabled.dedup();
        // A plugin can ask for a skill's body to be seated at session start (ADR-0018).
        // The name is resolved against the catalogue that was just built, so what the
        // prompt carries and what `/help` lists cannot disagree.
        let mut bootstrap = Vec::new();
        for name in &config.bootstrap_skills {
            // Config disabling the name is a deliberate "not here", not a broken
            // declaration: the plugin is installed, this session turned it off.
            if disabled_config.iter().any(|off| off == name) {
                continue;
            }
            let skill = skills.get(name).ok_or_else(|| {
                KernelError::Skills(format!(
                    "{name:?} is declared as a plugin bootstrap skill but is not in this \
                     session's catalogue — reinstall the plugin that declares it"
                ))
            })?;
            let body = skill.body().ok_or_else(|| {
                KernelError::Skills(format!(
                    "{name:?} is declared as a plugin bootstrap skill but its body lives on an \
                     MCP server, which cannot be seated before the session starts"
                ))
            })?;
            bootstrap.push(format!(
                "# Skill: {} (from {})\n\n{}",
                skill.name,
                skill.source_label(),
                body
            ));
        }
        match &mcp_caller {
            Some(caller) => tools.register(SkillTool::with_fetcher(
                Arc::clone(&skills),
                Arc::clone(caller) as Arc<dyn PromptFetcher>,
            )),
            None => tools.register(SkillTool::new(Arc::clone(&skills))),
        }
        let bootstrap_notes = config.bootstrap_notes.clone();

        // Configuration narrows the ladder before the first decision (ADR-0008 §2:
        // narrow-only keys, strictest wins — and the rule records which layer won).
        let mut pdp = Pdp::new();
        for class in [
            ActionClass::Investigate,
            ActionClass::WriteWorkspace,
            ActionClass::OutOfBounds,
        ] {
            if let Some((verdict, layer)) = config.settings.permission_with_source(class) {
                // The built-in baseline is not "a profile": name it as what it is.
                let source = match layer {
                    loong_config::Layer::Builtin => "builtin".to_string(),
                    other => format!("profile:{}", other.as_str()),
                };
                pdp.tighten_from(class, verdict, &source)
                    .map_err(|e| KernelError::Config(e.to_string()))?;
            }
        }

        let mut kernel;
        // The PEP is chosen here, once, and handed to every tool call.
        let (confinement, confinement_note): (Box<dyn Confinement>, String) =
            match WorkspacePolicy::new(&config.cwd) {
                Ok(policy) => (
                    Box::new(LandlockConfinement { policy }),
                    "landlock: writes are confined to the workspace".to_string(),
                ),
                Err(e) => (Box::new(NoConfinement), format!("unconfined: {e}")),
            };
        // A malformed manifest stopped a guard from existing: that is fatal, not
        // a warning. Untrusted hooks load but do not run (see `HookSet::active`).
        let hook_set = HookSet::load(&config.cwd).map_err(|e| KernelError::Hooks(e.to_string()))?;
        let hooks_note = hook_set.summary();
        // `mcp` hooks reach the servers this session started; without any, they
        // fail closed and say why.
        let caller: Option<Arc<dyn McpCaller>> = mcp_caller
            .as_ref()
            .map(|caller| Arc::clone(caller) as Arc<dyn McpCaller>);
        let hooks = HookRunner::new(&config.cwd, hook_set.active().to_vec()).with_mcp(caller);
        let session_start_fields = json!({"cwd": config.cwd, "profile": config.profile});
        kernel = Self {
            log,
            ledger,
            tools,
            pdp,
            models: by_name,
            router,
            selected_model,
            config,
            messages: Vec::new(),
            auto_approve: false,
            event_sink: None,
            interrupted: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            entry_of: Vec::new(),
            pending: None,
            approvals: 0,
            confinement,
            confinement_note,
            hooks,
            hooks_note,
            mcp_clients,
            mcp_note,
            skills,
            disabled_skills: disabled,
            bootstrap,
            bootstrap_notes,
            mcp_caller,
            index,
            index_path,
            index_stale: None,
            context_engine: engine,
            map_entry: None,
            map_index: None,
        };
        // The catalogue enters context before any work does.
        kernel.seat_system_prompt()?;
        // Then the conversation the log already holds — its own events, preceded by any
        // ancestor prefix (ADR-0015). Without this, resuming a session hands the model a
        // conversation it cannot see and an instruction to carry on.
        kernel.replay_history()?;
        // The session's first moment (ADR-0007 §7): what a guard sees before any
        // work happens.
        let mut startup = Vec::new();
        kernel.fire_hooks(HookEvent::SessionStart, session_start_fields, &mut startup)?;
        Ok(kernel)
    }

    /// Purpose: What MCP servers are in play, as a status note.
    ///
    /// Serves: a startup surface saying which servers started, were disabled or were unavailable.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: the turn tests only; no production caller (the CLI prints `McpSet::summary` instead).
    /// Down: `Kernel.mcp_note`.
    /// Verify: `cargo test -p loong-kernel turn::tests::an_optional_server_is_only_a_note`.
    /// What MCP servers are in play, for the startup log and `mcp list`.
    pub fn mcp_note(&self) -> &str {
        &self.mcp_note
    }

    /// Purpose: How many MCP servers this session started.
    ///
    /// Serves: a status surface counting live servers without listing them.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: the turn tests only; no production caller.
    /// Down: `Kernel.mcp_clients`.
    /// Verify: `cargo test -p loong-kernel turn::tests::an_untrusted_mcp_manifest_starts_nothing`.
    pub fn mcp_servers(&self) -> usize {
        self.mcp_clients.len()
    }

    /// Purpose: The skills and commands this workspace offers.
    ///
    /// Serves: a caller reading the same catalogue the system prompt was built from.
    /// Design: `docs/design/extension-model.md` §2.
    /// Up: the turn tests only; no production caller (`/help` in the CLI uses its own session).
    /// Down: `Kernel.skills`.
    /// Verify: `cargo test -p loong-kernel turn::tests::a_server_without_prompts_contributes_none`.
    /// The skills and commands this workspace offers.
    pub fn skills(&self) -> &SkillSet {
        &self.skills
    }

    /// Purpose: Fetch an MCP prompt's body for a command invocation, with its token estimate.
    ///
    /// Serves: a `/mcp__server__prompt` invocation whose body is not in the local catalogue.
    /// Design: `docs/design/extension-model.md` §3.
    /// Up: `Kernel::submit_with`'s command path.
    /// Down: `SkillSet::get`, the `KernelMcp` bridge and `loong_skills::estimate_tokens`.
    /// Verify: `cargo test -p loong-kernel turn::tests::an_mcp_prompt_becomes_a_command_with_its_body_fetched_on_invoke`.
    /// Fetch an MCP prompt's body for a command invocation, naming its arguments.
    pub(super) fn fetch_mcp_prompt(&self, name: &str, args: &str) -> Result<(String, u64), String> {
        let skill = self
            .skills
            .get(name)
            .ok_or_else(|| format!("no skill named {name:?}"))?;
        let Origin::Mcp {
            server,
            prompt,
            arguments,
        } = &skill.origin
        else {
            return Err(format!("{name} has no body to fetch"));
        };
        let Some(caller) = &self.mcp_caller else {
            return Err(format!(
                "MCP server {server:?} is not running in this session"
            ));
        };
        let body = caller.fetch(server, prompt, arguments, args)?;
        Ok((body.clone(), loong_skills::estimate_tokens(&body)))
    }

    /// Purpose: Build and seat the one pinned system prompt: frame, skill catalogue, and any plugin bootstrap bodies.
    ///
    /// Serves: a model that is told how to work and which skills exist before the first turn.
    /// Design: `docs/design/context-ledger.md` §6; `docs/design/extension-model.md` §2.
    /// Up: `Kernel::build`, before history replay.
    /// Down: `Ledger::add` (pinned, Persistent), `SkillSet::catalogue`, `Skill::body` and `Kernel::append` (`skills.loaded`).
    /// Verify: `cargo test -p loong-kernel turn::tests::the_catalogue_is_in_context_and_the_body_is_not`.
    /// The one place the system prompt is built: a short frame, the description-only
    /// skill catalogue (ADR-0006 progressive disclosure), and the bodies of any skills
    /// an installed plugin asked to have seated here (ADR-0018).
    fn seat_system_prompt(&mut self) -> Result<(), KernelError> {
        // The tool names are listed rather than left to the schema, and the last
        // paragraph is not politeness. Measured on this project's own sessions: when
        // a person asked a local model to write a single-page HTML game, the model
        // pasted the whole file into the reply and told them to save it themselves —
        // 6 assistant messages and 0 tool calls in the log — and answered "I will
        // create typing.html now" while emitting no call at all. Small models follow
        // what the prompt says about *how to work*; a prompt that never mentions
        // tools gets prose instead of actions.
        let tool_names: Vec<String> = self.tools.names().into_iter().map(String::from).collect();
        let mut body = format!(
            "You are loongagent, a coding agent working in a user's workspace. Prefer reading \
             over guessing, keep changes small, and say plainly when you are unsure.\n\n\
             You change the workspace only by calling tools. The tools available are: {}.\n\n\
             Write files with `write_file`, change them with `edit_file`, and run commands with \
             the shell tool. Never paste a file's contents into your reply and tell the person \
             to save it: that is not doing the task. Never write a tool call as text either — \
             no `<tool_call>` blocks, no JSON like {{\"name\": \"write_file\", ...}} in the \
             reply; a call that is printed instead of emitted runs nothing. When someone asks \
             you to create or change a file, emit the tool call, then report what you did in one \
             or two sentences. A claim that you created a file must be true — the person can see \
             the workspace.\n\n\
             Skills and commands available here are listed below as `/name — description`. They \
             are not loaded yet: call the `skill` tool with a name to load its full instructions \
             before doing work that matches it.\n\n{}",
            tool_names.join(", "),
            self.skills.catalogue()
        );
        // The always-on half: a plugin can ask for a skill's body to be here from the
        // first turn rather than behind a description (ADR-0018). It is appended to the
        // same pinned entry, so it is as inspectable — and as un-evictable — as the
        // catalogue it sits under.
        if !self.bootstrap.is_empty() {
            body.push_str(
                "\n\nThe skills below are loaded in full because an installed plugin asks for \
                 them at the start of every session. Their instructions apply from now on; the \
                 skills listed above still load on demand.\n",
            );
            for note in &self.bootstrap_notes {
                body.push_str(&format!("\n- {note}"));
            }
            for block in &self.bootstrap {
                body.push_str("\n\n");
                body.push_str(block);
            }
        }
        // Pinned and Persistent: system prompt tokens are never evicted, and
        // compaction must not touch them (ADR-0006).
        let entry = self.ledger.add(
            Category::SystemPrompt,
            "system",
            &body,
            Lifetime::Persistent,
            true,
        );
        self.messages.insert(0, ChatMessage::System { text: body });
        self.entry_of.insert(0, Some(entry));
        self.append(
            gen::event::SKILLS_LOADED,
            &gen::SkillsLoaded {
                count: self.skills.skills.len() as u64,
                tokens: loong_skills::estimate_tokens(&self.skills.catalogue()),
                names: self.skills.names().join(", "),
                disabled: self.disabled_skills.join(", "),
            },
        )?;
        Ok(())
    }
}
