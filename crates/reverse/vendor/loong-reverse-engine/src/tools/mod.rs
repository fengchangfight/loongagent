//! Tool modules. Each module exposes `specs()`; registered in [`all`].
//!
//! This is the reverse-only subset vendored from craft-anything (fc93bd0): only the modules
//! contributing the tool ids listed in `docs/design/reverse.md` survive. See
//! `docs/superpowers/specs/2026-10-24-loongagent-reverse-design.md` Task 1 Step 2.
//!
//! # Summary-first detail convention
//!
//! Every tool that can return a large result takes a uniform `detail` parameter:
//! `detail: "summary" | "complete"` (default `complete`). With `detail=summary` the tool still
//! builds and **retains** the full evidence record in the session ledger, but returns a compact
//! body: a one-line `summary`, light `counts`/extras, a `record` ([`Evidence::summary_view`]) and
//! an `saved_evidence` (`{"kind":"saved","evidence_id":"ev_…"}`) plus
//! `"continue_with":"core_evidence_browse"`. The caller then pages the full record facet by facet
//! with `core_evidence_browse` (facets, items, pages) without resending it. Build this with the
//! shared [`detailed`] helper (or [`Detail`]), so the shape is identical across tools. The MCP
//! server applies the same idea at the transport layer: a response over the connection's byte
//! budget collapses to this summary form (pointing at the retained record) rather than only an
//! error, whenever the result is a retained evidence record.

use crate::session::Session;
use loong_reverse_evidence::{Environment, EvidenceBuilder, ToolError};

pub mod session_tools;
// Shared artifact helpers for the web/js/android modules (no specs of its own).
pub mod artifact_tools;
// Browser over loopback CDP: transport and shared helpers for browser/website tools.
pub mod web_common;
// Readable Markdown/HTML investigation reports from findings + evidence (MCP resources).
pub mod investigation_tools;
// Shared helpers for the native modules.
pub mod native_annotate;
pub mod native_common;
pub mod native_formats;
pub mod native_function;
pub mod native_listing;
pub mod native_workflows;
// The Deep-mode (Ghidra) hook the native decompile path consults.
pub mod deep;
// Byte-signature and text search over a file, with native addresses.
pub mod byte_search;
// JavaScript bundle analysis.
pub mod js_tools;
// JavaScript source recovery and network-recording inspection.
pub mod web_tools;
// Website runtime internals (module tracing) in the page's own browser.
pub mod web_runtime_tools;
// Markdown reports written into the project.
pub mod report_tools;
// Websites as targets: a browser this session starts, page capture for rebuilding.
pub mod website_tools;
// Browser (CDP) tools: passive inspection, screenshots, comparisons, walkthroughs.
pub mod browser_tools;
// Android apps (APK, DEX, resources, Flutter, JEB).
pub mod android_tools;
// Shared input helpers for the built-in format tools.
pub mod static_input;

/// What this surface is: every tool the kept modules declare, with the bridges this build does not
/// ship (JEB, Frida, Ghidra, objdump/rizin/cutter/gdb, game archives, Electron) left out with their
/// modules. `docs/design/reverse.md` names the toolsets; `tool_list::MIN_TOOLS` is the ratchet.
///
/// Every tool the kept modules declare.
///
/// Purpose: one place decides what a session sees, and the single line a new module adds.
/// Contract: every spec is returned once; ordering follows the modules, then declaration order.
fn declared() -> Vec<crate::ToolSpec> {
    let mut v = Vec::new();
    v.extend(session_tools::specs());
    v.extend(investigation_tools::specs());
    v.extend(js_tools::specs());
    v.extend(web_tools::specs());
    v.extend(web_runtime_tools::specs());
    v.extend(browser_tools::specs());
    v.extend(website_tools::specs());
    v.extend(native_listing::specs());
    v.extend(native_function::specs());
    v.extend(native_annotate::specs());
    v.extend(native_workflows::specs());
    v.extend(native_formats::specs());
    v.extend(android_tools::specs());
    v.extend(byte_search::specs());
    v.extend(artifact_tools::specs());
    v.extend(report_tools::specs());
    v
}

/// Every tool a session may call, in registration order.
///
/// Purpose: the registry's input.
/// Contract: every id appears exactly once; ids are canonical and unique.
pub fn all() -> Vec<crate::ToolSpec> {
    declared()
}

/// The uniform summary-first detail control (see the module docs). `complete` is the default and
/// returns the full evidence record; `summary` returns a compact, pageable view.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Deserialize,
    serde::Serialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    #[default]
    Complete,
    Summary,
}

/// Return a built evidence record either in full (`detail=complete`) or as the compact,
/// summary-first body (`detail=summary`): the full record is retained in the session and the
/// caller is pointed at `core_evidence_browse` to page it. `extras` is merged into the summary
/// body (counts, a headline figure…); pass `Value::Null` for none.
pub fn detailed(
    ctx: &mut crate::tool::Ctx<'_>,
    e: loong_reverse_evidence::Evidence,
    detail: Detail,
    extras: serde_json::Value,
) -> Result<crate::tool::ToolOutput, ToolError> {
    use serde_json::{Value, json};
    if detail == Detail::Complete {
        return Ok(crate::tool::ToolOutput::evidence(e));
    }
    let id = e.id.clone();
    let summary = e.summary.clone();
    let view = e.summary_view();
    ctx.session.record(e)?;
    let mut body = json!({
        "saved_evidence": {"kind": "saved", "evidence_id": id},
        "summary": summary,
        "record": view,
        "continue_with": "core_evidence_browse",
    });
    if let (Value::Object(m), Value::Object(x)) = (&mut body, extras) {
        m.extend(x);
    }
    Ok(crate::tool::ToolOutput::Value(body))
}

/// Start an evidence record about the active target with the session's environment.
pub fn evidence_for_active(
    session: &Session,
    operation: &str,
) -> Result<EvidenceBuilder, ToolError> {
    let t = session.active()?;
    Ok(EvidenceBuilder::new(
        operation,
        t.target.subject(),
        Environment::builtin(session.clean_room()),
    ))
}

/// Parse an address given as `0x…` hex, plain hex with a leading letter, or decimal.
pub fn parse_address(s: &str) -> Result<u64, ToolError> {
    let t = s.trim();
    let parsed = if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else if t.chars().any(|c| c.is_ascii_alphabetic()) {
        u64::from_str_radix(t.trim_end_matches(['h', 'H']), 16).ok()
    } else {
        t.parse::<u64>().ok()
    };
    parsed.ok_or_else(|| ToolError::invalid(format!("not an address: {s:?} (use 0x-hex)")))
}

#[cfg(test)]
mod tests {
    use super::parse_address;

    #[test]
    fn addresses() {
        assert_eq!(parse_address("0x401000").unwrap(), 0x401000);
        assert_eq!(parse_address("401000h").unwrap(), 0x401000);
        assert_eq!(parse_address("4096").unwrap(), 4096);
        assert!(parse_address("0xZZ").is_err());
        assert!(parse_address("").is_err());
    }
}

#[cfg(test)]
mod registry_tests {
    /// Purpose: ids are unique, and no bridge this build drops is registered.
    #[test]
    fn the_surface_has_no_dropped_bridges() {
        let ids: Vec<&str> = super::all().into_iter().map(|s| s.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "duplicate ids");
        for id in &ids {
            for dropped in [
                "_jeb_",
                "engines_",
                "game_",
                "electron_",
                "_frida_",
                "rizin_",
                "cutter_",
                "gdb_",
                "ghidra_",
            ] {
                assert!(
                    !id.contains(dropped),
                    "{id} is a bridge this build does not ship"
                );
            }
        }
        for expected in [
            "core_target_open",
            "native_functions_list",
            "web_site_open",
            "android_package_show",
            "core_archive_show",
        ] {
            assert!(
                ids.contains(&expected),
                "{expected} is missing from the surface"
            );
        }
    }
}

#[cfg(test)]
mod pinned_surface {
    /// The tool ids this surface ships, exactly (`docs/design/reverse.md` §1). A new tool, a
    /// dropped one or a renamed one changes this list deliberately; nothing drifts in by itself.
    pub const PINNED: &[&str] = &[
        "android_class_show",
        "android_class_users_find",
        "android_classes_find",
        "android_components_list",
        "android_dart_snapshot_show",
        "android_flutter_identify",
        "android_method_show",
        "android_package_show",
        "android_resources_read",
        "apple_dispatch_show",
        "apple_objc_classes_list",
        "apple_objc_protocols_list",
        "apple_swift_types_list",
        "apple_ui_action_trace",
        "core_archive_extract",
        "core_archive_show",
        "core_archives_compare",
        "core_brief_write",
        "core_evidence_browse",
        "core_evidence_export",
        "core_evidence_import",
        "core_evidence_show",
        "core_feature_find",
        "core_finding_edit",
        "core_finding_record",
        "core_handoff_write",
        "core_hypothesis_record",
        "core_hypothesis_update",
        "core_investigation_show",
        "core_journal_read",
        "core_overview_show",
        "core_plan_update",
        "core_report_save",
        "core_report_write",
        "core_session_show",
        "core_target_close",
        "core_target_open",
        "core_target_select",
        "core_target_triage",
        "core_targets_list",
        "core_unknown_add",
        "core_unknown_edit",
        "core_unknown_verify",
        "core_unknowns_show",
        "js_app_map",
        "js_runtime_code_match",
        "js_source_locate",
        "js_sources_restore",
        "native_address_offset_show",
        "native_api_show",
        "native_bookmark_add",
        "native_bookmark_remove",
        "native_bookmarks_show",
        "native_bytes_find",
        "native_bytes_show",
        "native_call_graph_show",
        "native_call_path_find",
        "native_call_site_resolve",
        "native_comment_read",
        "native_comment_write",
        "native_crash_show",
        "native_cursor_address_show",
        "native_cursor_back",
        "native_cursor_forward",
        "native_cursor_function_show",
        "native_cursor_goto",
        "native_cursor_show",
        "native_data_show",
        "native_document_show",
        "native_dos_image_show",
        "native_function_address_show",
        "native_function_callees_list",
        "native_function_callers_list",
        "native_function_disassemble",
        "native_function_find",
        "native_function_instructions_list",
        "native_function_label",
        "native_function_pseudocode",
        "native_function_references_list",
        "native_function_show",
        "native_function_study",
        "native_functions_decompile",
        "native_functions_list",
        "native_functions_search",
        "native_inline_comment_read",
        "native_inline_comment_write",
        "native_instruction_show",
        "native_layout_show",
        "native_name_set",
        "native_name_show",
        "native_name_xrefs_list",
        "native_named_addresses_list",
        "native_names_set",
        "native_open_documents_list",
        "native_pe_resources_list",
        "native_sections_list",
        "native_strings_browse",
        "native_strings_find",
        "native_value_flow_show",
        "native_xrefs_list",
        "web_browser_connect",
        "web_coverage_measure",
        "web_listeners_list",
        "web_mcp_tools_list",
        "web_modules_trace",
        "web_page_js_analyze",
        "web_page_screenshot_take",
        "web_page_show",
        "web_page_states_capture",
        "web_page_watch",
        "web_pages_list",
        "web_pixels_compare",
        "web_recording_show",
        "web_scripts_save",
        "web_site_capture",
        "web_site_close",
        "web_site_open",
        "web_snapshots_compare",
        "web_walkthrough_check",
        "web_walkthrough_record",
    ];

    /// Purpose: the registered surface is exactly the pinned list, so drift is a red test.
    #[test]
    fn the_tool_list_is_the_pinned_one() {
        let mut ids: Vec<&str> = super::all().into_iter().map(|spec| spec.id).collect();
        ids.sort();
        let mut pinned: Vec<&str> = PINNED.to_vec();
        pinned.sort();
        assert_eq!(
            ids, pinned,
            "the registered tools differ from the pinned list"
        );
    }
}
