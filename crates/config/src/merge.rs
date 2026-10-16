use super::*;
use crate::layer::parse_class;

/// Merge the layers that exist, lowest precedence first.
pub fn resolve(layers: &[(Layer, PathBuf)], workspace: &Path) -> Result<Profile, ConfigError> {
    resolve_with(layers, workspace, None)
}

/// As [`resolve`], plus an optional pack from the control plane.
///
/// The pack arrives as values rather than as a path because it was *verified* before
/// it got here: nothing that reaches this function may be an unverified file a user
/// could edit (ADR-0008 §6).
pub fn resolve_with(
    layers: &[(Layer, PathBuf)],
    workspace: &Path,
    pack: Option<(String, Vec<(String, Value)>)>,
) -> Result<Profile, ConfigError> {
    let mut present: Vec<LayerValues> = vec![LayerValues::builtin_defaults()];
    let mut sources = std::collections::BTreeMap::new();
    for (layer, path) in layers {
        if !path.exists() {
            continue;
        }
        let values = LayerValues::from_file(*layer, path, Some(workspace))?;
        sources.insert(*layer, path.display().to_string());
        present.push(values);
    }
    if let Some((source, values)) = pack {
        sources.insert(Layer::Control, source.clone());
        present.push(LayerValues {
            layer: Layer::Control,
            source,
            values,
        });
    }
    present.sort_by_key(|l| l.layer);
    let mut profile = merge(&present)?;
    finish(&mut profile)?;
    // `mode` decides whether the control plane is consulted at all, so where it is
    // written matters more than what it says. It belongs in `local.toml` — the file a
    // local install owns — or in a session layer somebody named on the command line.
    // Honoured from the *managed* user file it becomes a way to switch a whole install
    // out of its firm's policy by editing a file that has nothing to do with local
    // mode, which is exactly what happened the first time this key existed.
    if let Some(row) = profile.explain("mode") {
        if !mode_is_legitimate(row.layer, &row.source) {
            return Err(ConfigError::Invalid(format!(
                "{}: `mode` belongs in {} — the managed profile must not be able to \
                 switch an install into local mode (run `loongagent tui --local` \
                 instead, which reads that file and nothing else)",
                row.source,
                local_profile_path().display()
            )));
        }
    }
    Ok(profile)
}

/// Whether a `mode` value from this layer and file is allowed to mean anything.
///
/// Split out so the rule can be stated — and tested — without a test having to write to
/// the real user's home directory.
pub(super) fn mode_is_legitimate(layer: Layer, source: &str) -> bool {
    // A session layer is a file somebody named on the command line, which is as
    // deliberate as it gets. Everything else is a file that was read because of where
    // it sits, and `mode` is too consequential to be decided by geography.
    layer == Layer::Session || Path::new(source) == local_profile_path()
}

/// Rules that only make sense once every layer has been merged: the shorthand form,
/// and a router that may only name models that exist.
fn finish(profile: &mut Profile) -> Result<(), ConfigError> {
    // `model`/`base_url`/`api_key` on their own are the one-model shorthand, which
    // is what every profile written before `[[models]]` existed looks like.
    if profile.models.is_empty() {
        if let (Some(base_url), Some(model)) = (profile.base_url.clone(), profile.model.clone()) {
            profile.models.push(ModelSpec {
                name: "default".to_string(),
                kind: "openai-compat".to_string(),
                base_url,
                model,
                api_key: profile.api_key.clone(),
                budget_tokens: profile.budget_tokens,
                max_context_size: profile.max_context_size,
                provider: None,
            });
        }
    }
    if profile.router.default.is_none() {
        profile.router.default = profile.models.first().map(|m| m.name.clone());
    }

    let known: Vec<String> = profile.models.iter().map(|m| m.name.clone()).collect();
    let mut referenced: Vec<&str> = profile
        .router
        .rules
        .iter()
        .map(|r| r.model.as_str())
        .collect();
    if let Some(default) = profile.router.default.as_deref() {
        referenced.push(default);
    }
    for name in referenced {
        if !known.iter().any(|k| k == name) {
            return Err(ConfigError::Invalid(format!(
                "the router names a model {name:?} that no layer defines (defined: {})",
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            )));
        }
    }

    // Keep the convenience mirrors pointed at the model a turn starts with.
    if let Some(first) = profile.default_model().cloned() {
        profile.model = Some(first.model.clone());
        profile.base_url = Some(first.base_url.clone());
        profile.api_key = first.api_key.clone();
    }
    Ok(())
}

fn merge(layers: &[LayerValues]) -> Result<Profile, ConfigError> {
    let mut profile = Profile::default();
    // Key → the winning row so far, plus what lost.
    let mut winners: BTreeMap<String, Provenance> = BTreeMap::new();

    for layer in layers {
        for (key, value) in &layer.values {
            let base = key.split('.').next().unwrap_or(key).to_string();
            // Locked: once the control plane has set a key, a lower layer may narrow
            // it but never rewrite it. This is what makes "engineers pick a profile,
            // not a vendor" a property rather than advice (T-07, ADR-0016 §5).
            if let Some(current) = winners.get(key) {
                if current.layer == Layer::Control && layer.layer > Layer::Control {
                    let kind = kind_of(key).or_else(|| kind_of(&base));
                    if !matches!(kind, Some(Kind::NarrowOnly) | Some(Kind::AddOnly)) {
                        return Err(ConfigError::Invalid(format!(
                            "{}: `{key}` is set by the control plane ({}) and cannot be changed \
                             by the {} layer — pick a different profile instead",
                            layer.source,
                            current.source,
                            layer.layer.as_str()
                        )));
                    }
                }
            }
            // The full key first: `router.default` is a key of its own, while
            // `permissions.*` and `models.*` take their kind from the table.
            let kind = kind_of(key)
                .or_else(|| kind_of(&base))
                .expect("checked when loading");
            match kind {
                Kind::NarrowOnly => {
                    // Strictest wins: numbers by minimum, verdicts by rank.
                    match winners.get_mut(key) {
                        None => {
                            winners.insert(
                                key.clone(),
                                Provenance {
                                    key: key.clone(),
                                    kind,
                                    value: value.clone(),
                                    layer: layer.layer,
                                    source: layer.source.clone(),
                                    overridden: Vec::new(),
                                },
                            );
                        }
                        Some(current) => {
                            let (keep_new, lose_old) = stricter_for(key, &current.value, value);
                            if keep_new {
                                let old = current.value.clone();
                                current.value = value.clone();
                                current.layer = layer.layer;
                                current.source = layer.source.clone();
                                current.overridden.push((layer.layer, old));
                            } else if lose_old {
                                current.overridden.push((layer.layer, value.clone()));
                            }
                        }
                    }
                }
                Kind::FreeOverride if key.starts_with("models.") => {
                    // A model definition is free-override (the most local layer decides
                    // which endpoint *this user* talks to), but its budget is not: an
                    // organisation's ceiling survives a local redefinition.
                    match (winners.get_mut(key), value) {
                        (Some(current), Value::Model(new)) => {
                            if let Value::Model(old) = &current.value {
                                let mut merged = new.clone();
                                merged.budget_tokens = match (old.budget_tokens, new.budget_tokens)
                                {
                                    (Some(a), Some(b)) => Some(a.min(b)),
                                    (Some(a), None) => Some(a),
                                    (None, b) => b,
                                };
                                // The window a model holds is a fact about the model, not a
                                // choice: a local redefinition may declare a *smaller* one
                                // (which compacts earlier and is therefore safe) and must
                                // not be able to lose or widen the one an organisation
                                // declared — losing it would turn compaction off.
                                merged.max_context_size =
                                    match (old.max_context_size, new.max_context_size) {
                                        (Some(a), Some(b)) => Some(a.min(b)),
                                        (Some(a), None) => Some(a),
                                        (None, b) => b,
                                    };
                                let old_value = current.value.clone();
                                let old_layer = current.layer;
                                current.value = Value::Model(merged);
                                current.layer = layer.layer;
                                current.source = layer.source.clone();
                                current.overridden.push((old_layer, old_value));
                            }
                        }
                        (None, _) => {
                            winners.insert(
                                key.clone(),
                                Provenance {
                                    key: key.clone(),
                                    kind,
                                    value: value.clone(),
                                    layer: layer.layer,
                                    source: layer.source.clone(),
                                    overridden: Vec::new(),
                                },
                            );
                        }
                        _ => {}
                    }
                }
                Kind::FreeOverride | Kind::Reference => {
                    // The most local layer that sets it wins (session > user >
                    // project > org > defaults).
                    match winners.get_mut(key) {
                        None => {
                            winners.insert(
                                key.clone(),
                                Provenance {
                                    key: key.clone(),
                                    kind,
                                    value: value.clone(),
                                    layer: layer.layer,
                                    source: layer.source.clone(),
                                    overridden: Vec::new(),
                                },
                            );
                        }
                        Some(current) => {
                            let old = current.value.clone();
                            let old_layer = current.layer;
                            current.value = value.clone();
                            current.layer = layer.layer;
                            current.source = layer.source.clone();
                            current.overridden.push((old_layer, old));
                        }
                    }
                }
                Kind::AddOnly => match winners.get_mut(key) {
                    None => {
                        winners.insert(
                            key.clone(),
                            Provenance {
                                key: key.clone(),
                                kind,
                                value: Value::List(vec![value.render()]),
                                layer: layer.layer,
                                source: layer.source.clone(),
                                overridden: Vec::new(),
                            },
                        );
                    }
                    Some(current) => {
                        if let Value::List(items) = &mut current.value {
                            let rendered = value.render();
                            if !items.contains(&rendered) {
                                items.push(rendered);
                            }
                        }
                    }
                },
            }
        }
    }

    // The typed view, derived from the winners so the two cannot disagree.
    for (key, row) in &winners {
        match (key.as_str(), &row.value) {
            ("model", Value::Text(v)) => profile.model = Some(v.clone()),
            ("mode", Value::Text(v)) => profile.mode = Some(v.clone()),
            ("base_url", Value::Text(v)) => profile.base_url = Some(v.clone()),
            ("api_key", Value::Text(v)) => profile.api_key = Some(v.clone()),
            ("budget_tokens", Value::Number(n)) => profile.budget_tokens = Some(*n),
            ("keep_newest_tokens", Value::Number(n)) => profile.keep_newest_tokens = Some(*n),
            ("max_steps", Value::Number(n)) => profile.max_steps = Some(*n as usize),
            ("compact_at_percent", Value::Number(n)) => profile.compact_at_percent = Some(*n),
            ("max_context_size", Value::Number(n)) => profile.max_context_size = Some(*n),
            ("reserved_context_size", Value::Number(n)) => profile.reserved_context_size = Some(*n),
            // The context engine (ADR-0017). Kept as the raw resolved facts; the
            // typed view a caller wants is `Profile::context_engine()`.
            ("context_engine.enabled", Value::Number(n)) => {
                profile.context_engine_enabled = Some(*n != 0)
            }
            ("context_engine.mode", Value::Text(v)) => {
                profile.context_engine_mode = Some(v.clone())
            }
            ("context_engine.episode_events", Value::Number(n)) => {
                profile.context_engine_episode_events = Some(*n as usize)
            }
            ("context_engine.l4_max_tokens", Value::Number(n)) => {
                profile.context_engine_l4_tokens = Some(*n)
            }
            ("summarizer.enabled", Value::Number(n)) => profile.summarizer_enabled = Some(*n != 0),
            ("summarizer.kind", Value::Text(v)) => profile.summarizer_kind = Some(v.clone()),
            ("summarizer.base_url", Value::Text(v)) => {
                profile.summarizer_base_url = Some(v.clone())
            }
            ("summarizer.model", Value::Text(v)) => profile.summarizer_model = Some(v.clone()),
            ("summarizer.window_calls", Value::Number(n)) => {
                profile.summarizer_window_calls = Some(*n as usize)
            }
            ("summarizer.window_tokens", Value::Number(n)) => {
                profile.summarizer_window_tokens = Some(*n)
            }
            // Models are collected after this loop, in declaration order — see below.
            (key, Value::Model(_)) if key.starts_with("models.") => {}
            ("router.default", Value::Text(name)) => profile.router.default = Some(name.clone()),
            ("router.rules", Value::Rules(rules)) => profile.router.rules = rules.clone(),
            ("control.profile", Value::Text(name)) => profile.control_profile = Some(name.clone()),
            ("skill_dirs", Value::List(items)) => {
                profile.skill_dirs = items.iter().map(PathBuf::from).collect()
            }
            ("disable_skills", Value::List(items)) => profile.disable_skills = items.clone(),
            (other, Value::Verdict(verdict)) => {
                if let Some(class) = other.strip_prefix("permissions.").and_then(parse_class) {
                    profile.permissions.push((class, *verdict));
                }
            }
            _ => {}
        }
    }
    // **Declaration order**, not alphabetical.
    //
    // The loop above iterates a map keyed by `models.<name>`, so collecting models there
    // handed them back sorted by name — which throws away the one thing the order means.
    // A profile's `[[models]]` is written cheapest-first, most capable-last; that order
    // *is* the tier ladder, it is what the router's default and the status bar's digits
    // refer to, and sorting it would silently renumber every hotkey somebody had learned.
    //
    // The most local layer that declares models sets the order — that is the file the
    // person actually wrote — and a model that only a lower layer declares is appended
    // after it, so nothing becomes unreachable by being out of order.
    let mut order: Vec<String> = Vec::new();
    for layer in layers.iter().rev() {
        for (key, value) in &layer.values {
            if let Some(name) = key.strip_prefix("models.") {
                if matches!(value, Value::Model(_)) && !order.iter().any(|seen| seen == name) {
                    order.push(name.to_string());
                }
            }
        }
    }
    for name in &order {
        if let Some(row) = winners.get(&format!("models.{name}")) {
            if let Value::Model(spec) = &row.value {
                profile.models.push(spec.clone());
            }
        }
    }

    profile.provenance = winners;
    Ok(profile)
}
/// Which of two narrow-only values is stricter. Returns
/// `(new_is_stricter, old_should_be_recorded_as_overridden)`.
/// Which of two narrow-only values is stricter.
///
/// Most caps are strictest when *smallest* — a smaller budget, fewer steps. A **reserve**
/// is the other way round: keeping more room for the answer is the conservative choice, so
/// a lower layer may raise it but never shrink it. Getting that backwards would let a
/// project file quietly leave the model less room to answer than the organisation allowed.
fn stricter_for(key: &str, current: &Value, candidate: &Value) -> (bool, bool) {
    if key == "reserved_context_size" {
        if let (Value::Number(a), Value::Number(b)) = (current, candidate) {
            return (b > a, b < a);
        }
    }
    stricter(current, candidate)
}

fn stricter(current: &Value, candidate: &Value) -> (bool, bool) {
    match (current, candidate) {
        (Value::Number(a), Value::Number(b)) => {
            if b < a {
                (true, false)
            } else if b > a {
                (false, true)
            } else {
                (false, false)
            }
        }
        (Value::Verdict(a), Value::Verdict(b)) => {
            let rank = |v: &Verdict| match v {
                Verdict::Allow => 0,
                Verdict::Ask => 1,
                Verdict::Deny => 2,
            };
            if rank(b) > rank(a) {
                (true, false)
            } else if rank(b) < rank(a) {
                (false, true)
            } else {
                (false, false)
            }
        }
        _ => (false, false),
    }
}
