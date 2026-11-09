//! Purpose: `tool_search`: the one step that reveals a deferred tool's schema instead of putting every schema in every request.
//!
//! # Contract
//! - **Serves:** `Registry::specs`, by deciding which deferred tools are visible now.
//! - **Design:** `docs/design/mcp-client.md` §6 (open-topic T-15).
//! - **Upstream:** the kernel's `attach_mcp` in `crates/kernel/src/turn/build.rs`.
//! - **Downstream:** `Registry`'s shared activation set.
//! - **Verify:** `cargo test -p loong-tools` · `./scripts/check.sh`
//!
//! MCP schemas are the largest thing a session puts into every request, and most
//! of them are never called. A deferred tool is therefore *dispatchable but
//! invisible*: the model is told it can search, not what it would find.
//!
//! Searching is deliberately cheap. It matches the query's words against each
//! tool's name and description, returns the schema of the best few, and marks
//! them active for the rest of the session — so a second search is never needed
//! for the same tool. A query that matches nothing returns the beginning of the
//! catalogue rather than an empty answer: a wrong keyword should cost one step,
//! not a dead end.

use loong_schema::generated::{ActionClass, Json};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use crate::{ExecContext, Tool, ToolError};

/// How many matches a search returns when the model does not say.
pub const DEFAULT_LIMIT: usize = 5;

/// The most matches one search may return.
pub const MAX_LIMIT: usize = 20;

/// What a search may say about a tool it has not revealed yet. The schema is the
/// payload; the rest is what the model needs to choose sensibly.
#[derive(Debug, Clone, PartialEq)]
/// Purpose: A tool the model may call but cannot see until it searches.
/// Serves: `ToolSearch` and the kernel's `attach_mcp`.
/// Design: `docs/design/mcp-client.md` §6.
/// Up: `Registry::register_deferred` and `Registry::deferred`.
/// Down: `ToolSearch::search`.
/// Verify: `cargo test -p loong-tools`.
pub struct DeferredTool {
    pub name: String,
    pub server: String,
    pub description: String,
    pub parameters: Json,
}

impl DeferredTool {
    /// Purpose: Whether this tool answers the words in a query.
    /// Serves: `ToolSearch::search`.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `ToolSearch::search`.
    /// Down: the `name` and `description` fields.
    /// Verify: `cargo test -p loong-tools`.
    pub fn matches(&self, words: &[String]) -> bool {
        let haystack = format!("{} {} {}", self.name, self.server, self.description).to_lowercase();
        words.iter().any(|word| haystack.contains(word.as_str()))
    }
}

/// Purpose: Make a deferred tool's schema visible, and report which ones are.
/// Serves: the kernel's registry, which shows only active deferred tools to the model.
/// Design: `docs/design/mcp-client.md` §6.
/// Up: `attach_mcp` in the kernel build.
/// Down: the catalogue and the shared activation set.
/// Verify: `cargo test -p loong-tools`.
pub struct ToolSearch {
    catalogue: Arc<Mutex<BTreeMap<String, DeferredTool>>>,
    active: Arc<Mutex<BTreeSet<String>>>,
}

impl ToolSearch {
    /// Purpose: Build the tool over the deferred catalogue and the registry's shared set.
    /// Serves: `attach_mcp`.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `crates/kernel/src/turn/build.rs`.
    /// Down: the two fields.
    /// Verify: `cargo test -p loong-tools`.
    pub fn new(
        catalogue: Arc<Mutex<BTreeMap<String, DeferredTool>>>,
        active: Arc<Mutex<BTreeSet<String>>>,
    ) -> Self {
        Self { catalogue, active }
    }

    /// Purpose: The catalogue as it stands now, so a refresh is visible to the next search.
    /// Serves: `search` and `names`.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `search` and `names`.
    /// Down: the shared catalogue.
    /// Verify: `cargo test -p loong-tools`.
    fn entries(&self) -> Vec<DeferredTool> {
        match self.catalogue.lock() {
            Ok(catalogue) => catalogue.values().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Purpose: The names of every tool that is still deferred, for the no-match answer.
    /// Serves: `search`.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `search`.
    /// Down: the `catalogue` field.
    /// Verify: `cargo test -p loong-tools`.
    pub fn names(&self) -> Vec<String> {
        self.entries().into_iter().map(|tool| tool.name).collect()
    }

    /// Purpose: Activate every deferred tool named, and say how many were new.
    /// Serves: `search`; a name already active is not counted twice.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `search`.
    /// Down: the shared activation set.
    /// Verify: `cargo test -p loong-tools`.
    pub fn activate<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> usize {
        let Ok(mut active) = self.active.lock() else {
            return 0;
        };
        names
            .into_iter()
            .filter(|name| active.insert((*name).to_string()))
            .count()
    }
}

impl Tool for ToolSearch {
    /// Purpose: Register this tool as `tool_search`.
    /// Contract: `crates/tools/src/deferred.rs`.
    fn name(&self) -> &str {
        "tool_search"
    }
    /// Purpose: Tell the model that deferred tools exist and how to reveal them.
    /// Contract: `crates/tools/src/deferred.rs`.
    fn description(&self) -> &str {
        "Search the tools that are installed but not shown, and reveal the schemas of \
         the ones that match. Use it when no listed tool does what you need."
    }
    /// Purpose: Declare the `query` and optional `limit` arguments.
    /// Contract: `crates/tools/src/deferred.rs`.
    fn parameters(&self) -> Json {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "what you want to do, in a few words"},
                "limit": {"type": "integer", "description": "how many matches to reveal (default 5, at most 20)"}
            },
            "required": ["query"]
        })
    }
    /// Read-only by construction: it reveals schemas already in memory and changes
    /// nothing outside the session.
    /// Purpose: Classify a search as an investigate action.
    /// Contract: `crates/tools/src/deferred.rs`.
    fn action_class(&self, _args: &Json, _cwd: &std::path::Path) -> ActionClass {
        ActionClass::Investigate
    }
    /// Purpose: Search, activate the matches, and return their schemas as text.
    /// Serves: a model that has no visible tool for the job.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `Kernel::execute` through the registry.
    /// Down: `DeferredTool::matches`, `activate`.
    /// Verify: `cargo test -p loong-tools`.
    fn execute(&self, args: &Json, _ctx: &ExecContext<'_>) -> Result<String, ToolError> {
        let query = args
            .get("query")
            .and_then(|q| q.as_str())
            .ok_or_else(|| ToolError::Refused("missing string argument \"query\"".into()))?;
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .map(|l| l as usize)
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT);
        self.search(query, limit)
    }
}

impl ToolSearch {
    /// Purpose: Match a query against the catalogue and render the revealed schemas.
    /// Serves: `Tool::execute`.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: `Tool::execute` and tests.
    /// Down: `DeferredTool::matches` and `activate`.
    /// Verify: `cargo test -p loong-tools`.
    pub fn search(&self, query: &str, limit: usize) -> Result<String, ToolError> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|word| word.to_lowercase())
            .filter(|word| word.len() > 1)
            .collect();
        if words.is_empty() {
            return Err(ToolError::Refused(
                "the query needs at least one word of two characters or more".into(),
            ));
        }
        let catalogue = self.entries();
        let matches: Vec<&DeferredTool> = catalogue
            .iter()
            .filter(|tool| tool.matches(&words))
            .take(limit)
            .collect();
        if matches.is_empty() {
            let names = catalogue
                .iter()
                .take(MAX_LIMIT)
                .map(|tool| format!("{} (from {})", tool.name, tool.server))
                .collect::<Vec<_>>()
                .join(", ");
            return Ok(format!(
                "Nothing matched {query:?}. Installed but not shown: {names}. \
                 Search again with a word from one of those descriptions."
            ));
        }
        self.activate(matches.iter().map(|tool| tool.name.as_str()));
        let mut out = format!(
            "{} tool(s) revealed; they are callable from now on without searching again.\n",
            matches.len()
        );
        for tool in matches {
            let schema = serde_json::to_string_pretty(&tool.parameters)
                .unwrap_or_else(|_| tool.parameters.to_string());
            out.push_str(&format!(
                "\n### {}\n{}\n(from MCP server {:?})\ninput schema:\n{}\n",
                tool.name, tool.description, tool.server, schema
            ));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NoConfinement;
    use std::path::Path;

    /// Purpose: A catalogue of three tools from two servers.
    ///
    /// Serves: the `tool_search` tests, which need more tools than a request would reveal.
    /// Design: `docs/design/mcp-client.md` §6.
    /// Up: the tests in this module.
    /// Down: `DeferredTool` values only; no further call.
    /// Verify: `cargo test -p loong-tools`.
    fn catalogue() -> Arc<Mutex<BTreeMap<String, DeferredTool>>> {
        let tools = vec![
            DeferredTool {
                name: "mcp__playwright__navigate".into(),
                server: "playwright".into(),
                description: "Open a URL in a browser".into(),
                parameters: json!({"type": "object", "required": ["url"]}),
            },
            DeferredTool {
                name: "mcp__playwright__screenshot".into(),
                server: "playwright".into(),
                description: "Capture the page as a PNG".into(),
                parameters: json!({"type": "object"}),
            },
            DeferredTool {
                name: "mcp__github__create_issue".into(),
                server: "github".into(),
                description: "Open a browser-free issue on GitHub".into(),
                parameters: json!({"type": "object"}),
            },
        ];
        Arc::new(Mutex::new(
            tools
                .into_iter()
                .map(|tool| (tool.name.clone(), tool))
                .collect(),
        ))
    }

    #[test]
    /// Purpose: Pin that a search reveals matching schemas and activates them.
    fn a_search_reveals_and_activates_matches() {
        let active = Arc::new(Mutex::new(BTreeSet::new()));
        let tool = ToolSearch::new(catalogue(), active.clone());
        let ctx = ExecContext::new(Path::new("/tmp"), &NoConfinement);

        let out = tool.execute(&json!({"query": "screenshot"}), &ctx).unwrap();
        assert!(out.contains("mcp__playwright__screenshot"), "{out}");
        assert!(out.contains("input schema:"), "{out}");
        assert!(out.contains("PNG"), "{out}");
        assert!(
            !out.contains("mcp__github__create_issue"),
            "only the matches are revealed: {out}"
        );
        assert_eq!(
            active.lock().unwrap().iter().cloned().collect::<Vec<_>>(),
            vec!["mcp__playwright__screenshot".to_string()]
        );
    }

    #[test]
    /// Purpose: Pin that a search with no match names what exists instead of answering empty.
    fn a_search_without_a_match_names_the_catalogue() {
        let active = Arc::new(Mutex::new(BTreeSet::new()));
        let tool = ToolSearch::new(catalogue(), active.clone());
        let ctx = ExecContext::new(Path::new("/tmp"), &NoConfinement);
        let out = tool.execute(&json!({"query": "database"}), &ctx).unwrap();
        assert!(out.contains("Nothing matched"), "{out}");
        assert!(out.contains("mcp__playwright__navigate"), "{out}");
        assert!(out.contains("from playwright"), "{out}");
        assert!(active.lock().unwrap().is_empty(), "nothing was activated");
    }

    #[test]
    /// Purpose: Pin the limit, its ceiling and the refusal of a query with no usable word.
    fn the_limit_is_bounded_and_a_blank_query_is_refused() {
        let active = Arc::new(Mutex::new(BTreeSet::new()));
        let tool = ToolSearch::new(catalogue(), active.clone());
        let ctx = ExecContext::new(Path::new("/tmp"), &NoConfinement);

        // "a" is one character and is dropped; "object" matches nothing, "browser"
        // matches two tools.
        let out = tool
            .execute(&json!({"query": "browser page", "limit": 1}), &ctx)
            .unwrap();
        assert!(out.contains("1 tool(s) revealed"), "{out}");

        let err = tool.execute(&json!({"query": "a"}), &ctx).unwrap_err();
        assert!(err.to_string().contains("two characters"), "{err}");
        assert!(tool.execute(&json!({}), &ctx).is_err(), "query is required");
    }

    #[test]
    /// Purpose: Pin that an already-active tool is not counted as new.
    fn activation_is_idempotent() {
        let active = Arc::new(Mutex::new(BTreeSet::new()));
        let tool = ToolSearch::new(catalogue(), active);
        assert_eq!(tool.activate(["mcp__github__create_issue"]), 1);
        assert_eq!(tool.activate(["mcp__github__create_issue"]), 0);
        assert_eq!(tool.names().len(), 3);
    }

    #[test]
    /// Purpose: Pin that a search is an investigate action.
    fn a_search_is_an_investigate_action() {
        let tool = ToolSearch::new(catalogue(), Arc::new(Mutex::new(BTreeSet::new())));
        assert_eq!(
            tool.action_class(&json!({}), Path::new("/tmp")),
            ActionClass::Investigate
        );
    }
}
