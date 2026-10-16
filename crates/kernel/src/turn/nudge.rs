/// Whether a reply is a **tool call printed as text** rather than emitted.
///
/// Measured in this project's own log (`47x75pvh.jsonl`, `qwen2.5:7b-instruct` on
/// Ollama's OpenAI shim): asked to summarise a report into a file, the model answered
/// with `{"name": "write_file", "arguments": {…}}` wrapped in `<tool_call>` **inside
/// `content`**. The provider passes that through as ordinary text, so no `tool.call`
/// event exists, the turn ends "successfully", and the workspace is unchanged — the
/// same class of failure as pasting the file, with a different serialization.
///
/// It returns the tool's name so the correction can name it too (telling a model that
/// tried to `edit_file` to call `write_file` would be a second wrong instruction). The
/// match is deliberately narrow — a quoted `"name"` next to `"arguments"`, or an
/// explicit `<tool_call>` marker — so prose that merely *mentions* a tool is not
/// mistaken for an attempted call.
pub(super) fn leaked_tool_call(text: &str) -> Option<String> {
    const KNOWN: &[&str] = &[
        "write_file",
        "edit_file",
        "read_file",
        "run_command",
        "glob",
        "grep",
        "recall",
        "skill",
    ];
    // `"name": "write_file"` or `"name":"write_file"`, wherever it sits. The text may be
    // cut off on either side (the measured case began mid-word), so this scans rather
    // than parsing the object.
    let quoted_name = || -> Option<String> {
        let at = text.find("\"name\"")?;
        let rest = text[at + "\"name\"".len()..].trim_start();
        let rest = rest.strip_prefix(':')?.trim_start();
        let rest = rest.strip_prefix('"')?;
        Some(rest.split('"').next()?.to_string())
    };
    let has_arguments = text.contains("\"arguments\"");
    if let Some(name) = quoted_name() {
        if has_arguments && KNOWN.contains(&name.as_str()) {
            return Some(name);
        }
    }
    let tagged = text.contains("<tool_call>") || text.contains("</tool_call>");
    if tagged {
        // A marker with no parseable name still says what happened; name the tool if the
        // text names one, so the correction is not generic for no reason.
        if let Some(name) = KNOWN.iter().find(|tool| text.contains(**tool)) {
            return Some((*name).to_string());
        }
    }
    None
}

/// Whether a reply is a file pasted into chat rather than a report.
///
/// Deliberately narrow: a fenced block whose opening line names a file, or a shell
/// paste that names a tool. Merely quoting code is normal — explaining a fix with a
/// snippet is not the failure this looks for. The failure is a *whole file* offered
/// for the person to save, which leaves the workspace unchanged while the reply
/// reads like success.
pub(super) fn looks_like_pasted_file(text: &str) -> bool {
    let mut in_fence = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            if in_fence {
                in_fence = false;
                continue;
            }
            in_fence = true;
            let info = trimmed.trim_start_matches("```").trim().to_lowercase();
            // A fence labelled with a file, a language, or a tool name. An unlabelled
            // fence is a snippet and is left alone.
            if info.is_empty() {
                continue;
            }
            if info.contains('.')
                || [
                    "html",
                    "css",
                    "javascript",
                    "js",
                    "python",
                    "sh",
                    "bash",
                    "yaml",
                    "json",
                    "toml",
                    "sql",
                    "rust",
                    "go",
                    "java",
                    "ts",
                    "typescript",
                ]
                .iter()
                .any(|lang| info.starts_with(lang))
            {
                return true;
            }
            if info.starts_with("write_file") || info.starts_with("edit_file") {
                return true;
            }
        }
        // `write_file path - "..."`: a tool call written out as prose.
        if trimmed.starts_with("write_file ") || trimmed.starts_with("edit_file ") {
            return true;
        }
    }
    false
}
