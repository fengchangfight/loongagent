//! Purpose: L3 domain maps and the L4 global map, built deterministically from the L2 episodes.
//!
//! Serves: a map that exists in R mode, whose domain membership is a rule rather than a model's opinion, and whose L4 block has a hard token ceiling.
//!
//! # Contract
//! - **Design:** `docs/context/pipeline.md` §6.4; `docs/context/recall.md` §7.1; `docs/design/context-ledger.md` §2.
//! - **Upstream:** `crate::worker::step` and the kernel's `ContextEngine::refresh` call `Store::digest_domains`; `context_explain` and `loongagent context status` call `Store::global_map`.
//! - **Downstream:** The kernel carries `GlobalMap.content` in the foreground; `crate::summaries::put_summary` stores the L3 rows.
//! - **Verify:** `cargo test -p loong-context` · `./scripts/check.sh`.

use crate::store::{Store, StoreError};
use crate::summaries::{Summary, SummaryInput, SUMMARY_SCHEMA_VERSION};
use std::collections::{BTreeMap, BTreeSet};

/// How many L2 episodes a domain needs before it earns an L3 map. Below this the
/// episodes themselves are the answer, and inventing a one-episode "domain map"
/// would be noise dressed as structure.
pub const DOMAIN_ROLLUP_EPISODES: usize = 3;

/// The token ceiling for the L4 map. It is the one block that is always in the
/// foreground, so it is the one block with a hard budget.
pub const L4_MAX_TOKENS: u64 = 400;

/// The scope string for the global map.
pub const GLOBAL_SCOPE: &str = "global";

/// Purpose: The domain a file or module belongs to: its first two path segments.
/// Serves: one deterministic rollup rule, so changing granularity is one edit rather than a migration.
/// Design: `docs/context/pipeline.md` §6.4 and `docs/context/data-model.md` §5.4.
/// Up: `crate::summaries::tail_summary` and the synthesis tests.
/// Down: Formats the segments; no callee.
/// Verify: `cargo test -p loong-context --test synthesis a_domain_is_the_first_two_path_segments`.
/// The domain a file or module belongs to. One function, so changing the
/// granularity is one edit rather than a migration.
pub fn domain_of(module: &str) -> String {
    let cleaned = module.trim_start_matches("./").trim_end_matches('/');
    let segments: Vec<&str> = cleaned.split('/').filter(|s| !s.is_empty()).collect();
    match segments.len() {
        0 => "inbox".to_string(),
        1 => segments[0].to_string(),
        _ => format!("{}/{}", segments[0], segments[1]),
    }
}

/// Purpose: One domain's rollup: its episodes, modules, status tally and span.
/// Serves: L3 being an index of its children rather than a claim about the subsystem.
/// Design: `docs/context/pipeline.md` §6.4.
/// Up: `domains_of` builds it; `render_domain` and `global_map` read it.
/// Down: `render_domain` and `GlobalMap.domains`.
/// Verify: `cargo test -p loong-context --test synthesis episodes_roll_up_into_a_domain_map`.
/// One domain's rollup: what it is made of and where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainMap {
    pub domain: String,
    /// L2 episode ids, oldest first.
    pub episodes: Vec<String>,
    /// `(id, title)` of each episode, for the index part of the map.
    pub titles: Vec<(String, String)>,
    pub modules: BTreeSet<String>,
    /// `(id, status)` of the newest episode.
    pub latest: Option<(String, String)>,
    pub open: usize,
    pub done: usize,
    pub blocked: usize,
    pub seq_from: u64,
    pub seq_to: u64,
    pub tokens: u64,
}

/// Purpose: Group episodes into domains by their stored `domain` field.
/// Serves: membership coming from derivation, never re-parsed out of prose.
/// Design: `docs/context/pipeline.md` §6.4.
/// Up: `Store::digest_domains`, `Store::global_map` and the synthesis tests.
/// Down: Reads `Summary`'s fields; no other callee.
/// Verify: `cargo test -p loong-context --test synthesis two_domains_stay_separate`.
/// Group episodes into domains. Membership comes from the stored `domain` field,
/// which is set when the episode is derived — never re-parsed out of prose.
pub fn domains_of(episodes: &[Summary]) -> Vec<DomainMap> {
    let mut by_domain: BTreeMap<String, Vec<&Summary>> = BTreeMap::new();
    for episode in episodes {
        let domain = episode
            .domain
            .clone()
            .unwrap_or_else(|| "inbox".to_string());
        by_domain.entry(domain).or_default().push(episode);
    }

    let mut out = Vec::new();
    for (domain, mut episodes) in by_domain {
        episodes.sort_by_key(|s| s.seq_to);
        let mut open = 0;
        let mut done = 0;
        let mut blocked = 0;
        let mut modules = BTreeSet::new();
        for episode in &episodes {
            match episode.status.as_str() {
                "open" => open += 1,
                "done" => done += 1,
                "blocked" => blocked += 1,
                _ => {}
            }
            if let Some(module) = &episode.domain {
                if module.contains('/') {
                    modules.insert(module.clone());
                }
            }
        }
        let map = DomainMap {
            domain,
            episodes: episodes.iter().map(|s| s.id.clone()).collect(),
            titles: episodes
                .iter()
                .map(|s| (s.id.clone(), s.title.clone()))
                .collect(),
            modules,
            latest: episodes.last().map(|s| (s.id.clone(), s.status.clone())),
            open,
            done,
            blocked,
            seq_from: episodes.first().map(|s| s.seq_from).unwrap_or(0),
            seq_to: episodes.last().map(|s| s.seq_to).unwrap_or(0),
            tokens: 0,
        };
        out.push(map);
    }
    out
}

/// Purpose: Render an L3 map: module list, status tally and episode index.
/// Serves: a map a reader can jump from to the episodes it names.
/// Design: `docs/context/pipeline.md` §6.4.
/// Up: `Store::digest_domains`.
/// Down: Reads `DomainMap`'s fields; no callee.
/// Verify: `cargo test -p loong-context --test synthesis episodes_roll_up_into_a_domain_map`.
/// Render an L3 map: a module list, a status tally, and the episode index a reader
/// (or a model) can jump from.
pub fn render_domain(map: &DomainMap) -> String {
    let mut out = String::new();
    out.push_str(&format!("L3 · {}\n", map.domain));
    out.push_str(&format!(
        "段落：{} 段（done {} / open {} / blocked {}）\n",
        map.episodes.len(),
        map.done,
        map.open,
        map.blocked
    ));
    if !map.modules.is_empty() {
        let modules: Vec<&str> = map.modules.iter().map(String::as_str).collect();
        out.push_str(&format!("模块：{}\n", modules.join(", ")));
    }
    if let Some((id, status)) = &map.latest {
        out.push_str(&format!("最近：{id}（{status}）\n"));
    }
    let shown = 20;
    for (id, title) in map.titles.iter().take(shown) {
        out.push_str(&format!("- {id} {title}\n"));
    }
    if map.titles.len() > shown {
        out.push_str(&format!("…另有 {} 段\n", map.titles.len() - shown));
    }
    out
}

/// Purpose: The L4 map: the project's domains and state in one bounded block.
/// Serves: the one block always in the foreground, and therefore the one with a hard ceiling.
/// Design: `docs/context/recall.md` §7.1.
/// Up: `global_map` builds it; the kernel's `context_explain` and `loongagent context status` read it.
/// Down: The kernel's foreground message and the status surface.
/// Verify: `cargo test -p loong-context --test synthesis the_global_map_is_small_and_always_available`.
/// The L4 map: what this project is and where it stands, in one small block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalMap {
    pub content: String,
    pub tokens: u64,
    pub domains: Vec<DomainMap>,
    pub episodes: usize,
    pub seq_to: u64,
}

/// Purpose: Build the L4 map from the domain maps, cutting it at the token ceiling.
/// Serves: a global map that reflects the session now instead of growing into a second history.
/// Design: `docs/context/recall.md` §7.1.
/// Up: `Store::global_map`.
/// Down: `crate::digest::estimate_tokens` and `DomainMap`'s fields.
/// Verify: `cargo test -p loong-context --test synthesis the_global_map_is_small_and_always_available`.
/// Build the global map from the domain maps. Deliberately bounded: it names the
/// busiest domains and their state, and stops — an L4 that grows with the session
/// is not a global map, it is a second history.
pub fn global_map(domains: &[DomainMap]) -> GlobalMap {
    let mut ranked: Vec<&DomainMap> = domains.iter().collect();
    // Busiest first, then most recently touched, then name — a total order, so the
    // same session always produces the same map.
    ranked.sort_by(|a, b| {
        b.episodes
            .len()
            .cmp(&a.episodes.len())
            .then(b.seq_to.cmp(&a.seq_to))
            .then(a.domain.cmp(&b.domain))
    });

    let episodes: usize = domains.iter().map(|d| d.episodes.len()).sum();
    let seq_to = domains.iter().map(|d| d.seq_to).max().unwrap_or(0);
    let open: usize = domains.iter().map(|d| d.open).sum();

    let mut out = String::from("L4 · 全局认知地图\n");
    if domains.is_empty() {
        out.push_str(&format!("共 {episodes} 段情节，尚未形成模块域\n"));
    } else {
        out.push_str(&format!(
            "共 {} 个模块域，{} 段情节（未完成 {}）\n",
            domains.len(),
            episodes,
            open
        ));
    }
    let shown = 12;
    for map in ranked.iter().take(shown) {
        let (latest, title) = match map.titles.last() {
            Some((id, title)) => (id.clone(), title.clone()),
            None => (String::new(), String::new()),
        };
        let blocked = if map.blocked > 0 {
            format!(" / blocked {}", map.blocked)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "- {}（{} 段，done {}{}）最新 {} {}\n",
            map.domain,
            map.episodes.len(),
            map.done,
            blocked,
            latest,
            title
        ));
    }
    if ranked.len() > shown {
        out.push_str(&format!("…另有 {} 个域\n", ranked.len() - shown));
    }

    // The budget is a hard ceiling, not a hope: cut on a line boundary and say so.
    let tokens = crate::digest::estimate_tokens(&out);
    if tokens > L4_MAX_TOKENS {
        let keep = (L4_MAX_TOKENS * 4) as usize;
        let mut truncated: String = out.chars().take(keep).collect();
        if let Some(cut) = truncated.rfind('\n') {
            truncated.truncate(cut);
        }
        truncated.push_str("\n…[地图已截断]\n");
        out = truncated;
    }

    GlobalMap {
        tokens: crate::digest::estimate_tokens(&out),
        content: out,
        domains: domains.to_vec(),
        episodes,
        seq_to,
    }
}

impl Store {
    /// Purpose: Build L3 maps for domains that have changed and write them as summaries.
    /// Serves: the same "only re-derive what is behind" rule as L2, one level up.
    /// Design: `docs/context/pipeline.md` §6.4.
    /// Up: `crate::worker::step`, the kernel's `ContextEngine::refresh` and `loongagent context rebuild`.
    /// Down: `Store::summaries_at`, `domains_of`, `render_domain`, `l3_summary`, `same_l3` and `Store::put_summary`.
    /// Verify: `cargo test -p loong-context --test synthesis the_worker_keeps_the_maps_current`.
    /// Build the L3 maps from the L2 episodes and write them as summaries.
    ///
    /// A domain's L3 is re-derived when its episodes have changed and not
    /// otherwise — the same "only re-derive what is behind" rule as L2, one level
    /// up. Nothing here reads raw events, so this cannot grow with history: its
    /// input is the summaries, already a constant factor smaller.
    pub fn digest_domains(&mut self) -> Result<u64, StoreError> {
        let episodes = self.summaries_at(2, 100_000)?;
        if episodes.is_empty() {
            return Ok(0);
        }
        let mut written = 0;
        for map in domains_of(&episodes) {
            if map.episodes.len() < DOMAIN_ROLLUP_EPISODES {
                continue;
            }
            let content = render_domain(&map);
            let id = format!("s3-{}", sanitize(&map.domain));
            let candidate = l3_summary(&id, &map, content);
            let unchanged = self
                .summary(&id)?
                .map(|existing| same_l3(&existing, &candidate))
                .unwrap_or(false);
            if unchanged {
                continue;
            }
            self.put_summary(&candidate)?;
            written += 1;
        }
        Ok(written)
    }

    /// Purpose: Build the L4 map from the current L2 episodes, or `None` when there are none.
    /// Serves: the foreground having a map before any domain map exists.
    /// Design: `docs/context/recall.md` §7.1.
    /// Up: The kernel's `context_explain` and `loongagent context status`.
    /// Down: `Store::summaries_at`, `domains_of` and `global_map`.
    /// Verify: `cargo test -p loong-context --test synthesis the_global_map_exists_before_any_domain_does`.
    /// Build the L4 global map from the L3 maps. `None` until there is anything
    /// to map.
    pub fn global_map(&self) -> Result<Option<GlobalMap>, StoreError> {
        let episodes = self.summaries_at(2, 100_000)?;
        if episodes.is_empty() {
            return Ok(None);
        }
        let domains = domains_of(&episodes);
        Ok(Some(global_map(&domains)))
    }
}

/// Purpose: The L3 summary row for one domain map.
/// Serves: an L3 derived from its L2 children, never from its own previous version.
/// Design: `docs/context/pipeline.md` §6.4.
/// Up: `Store::digest_domains`.
/// Down: `crate::digest::estimate_tokens` and `SummaryInput`.
/// Verify: `cargo test -p loong-context --test synthesis a_domain_map_records_its_children`.
fn l3_summary(id: &str, map: &DomainMap, content: String) -> Summary {
    // Computed, like every other field here: an L3's cost is the length of the map
    // it wrote. Leaving it at `map.tokens` (which is 0) would make the honesty
    // surface report that L3 costs nothing.
    let tokens = crate::digest::estimate_tokens(&content);
    Summary {
        id: id.to_string(),
        level: 3,
        scope: format!("domain:{}", map.domain),
        title: format!("{} 模块地图", map.domain),
        content,
        // L3 does not conclude; it reports the state of its children. Reporting
        // "done" for a whole subsystem is a claim nothing supports.
        status: if map.open > 0 { "open" } else { "known" }.to_string(),
        producer: "deterministic".to_string(),
        schema_ver: SUMMARY_SCHEMA_VERSION,
        seq_from: map.seq_from,
        seq_to: map.seq_to,
        superseded_by: None,
        domain: Some(map.domain.clone()),
        tokens,
        created_at: 0,
        updated_at: 0,
        citations: Vec::new(),
        // The lineage the design note insists on: an L3 is derived from its L2
        // children, never from a previous version of itself.
        inputs: map
            .episodes
            .iter()
            .map(|id| SummaryInput {
                summary_id: Some(id.clone()),
                seq_from: None,
                seq_to: None,
            })
            .collect(),
    }
}

/// Purpose: Whether an L3 candidate says the same thing as the stored row.
/// Serves: an unchanged domain map being skipped, so the worker can idle.
/// Design: `docs/context/pipeline.md` §6.4.
/// Up: `Store::digest_domains`.
/// Down: Compares the fields; no other callee.
/// Verify: `cargo test -p loong-context --test synthesis an_idle_synthesis_pass_writes_nothing`.
fn same_l3(a: &Summary, b: &Summary) -> bool {
    // `tokens` is compared so an L3 written before the token was computed is
    // re-derived once rather than staying wrong forever.
    a.title == b.title
        && a.content == b.content
        && a.status == b.status
        && a.tokens == b.tokens
        && a.inputs == b.inputs
}

/// Purpose: Turn a domain name into a safe summary id.
/// Serves: an L3 id that is stable and usable as a scope.
/// Design: `docs/context/pipeline.md` §6.4.
/// Up: `Store::digest_domains`.
/// Down: Maps characters; no other callee.
/// Verify: `cargo test -p loong-context --test synthesis episodes_roll_up_into_a_domain_map`.
fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
