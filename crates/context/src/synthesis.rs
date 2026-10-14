//! L3 and L4 synthesis — the part of the "map" a deterministic pass can build.
//!
//! L3 answers "how does this subsystem work, and what happened to it?"; L4 answers
//! "what is this project and where are we?". Both are built here **without a
//! model**, from the L2 digests beneath them, so:
//!
//! * the maps exist in R mode (no model installed), which is what makes the
//!   optional-model story honest rather than a euphemism for "unusable without
//!   Ollama";
//! * the *membership* of a domain is a deterministic rule over `module` entities,
//!   not a model's opinion, which is what makes the dirty set computable and the
//!   whole thing testable;
//! * P5 can later add a model-written *narrative* on top of these facts without
//!   changing which parts are facts.
//!
//! # Granularity (the open question in the design note §14, made cheap to change)
//!
//! A domain is the first **two** path segments of a file (`src/kernel`,
//! `crates/context`) — the same rule [`crate::materialize`] uses for its `module`
//! entity, so the index and the map agree by construction. [`domain_of`] is the
//! single place that rule lives; finer granularity is one edit plus a re-derive,
//! and re-deriving is mechanical.

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

/// The L4 map: what this project is and where it stands, in one small block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalMap {
    pub content: String,
    pub tokens: u64,
    pub domains: Vec<DomainMap>,
    pub episodes: usize,
    pub seq_to: u64,
}

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

fn same_l3(a: &Summary, b: &Summary) -> bool {
    // `tokens` is compared so an L3 written before the token was computed is
    // re-derived once rather than staying wrong forever.
    a.title == b.title
        && a.content == b.content
        && a.status == b.status
        && a.tokens == b.tokens
        && a.inputs == b.inputs
}

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
