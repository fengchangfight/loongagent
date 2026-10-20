//! Purpose: Line mode: the same client as the TUI, reading prompts from stdin and streaming a turn's deltas.
//!
//! # Contract
//! - **Serves:** `loongagent repl`, the surface that works under a pipe or in any terminal.
//! - **Design:** `docs/design/surfaces.md` §2 and §7.
//! - **Upstream:** `main.rs`'s `repl` arm parses the flags and calls `repl`.
//! - **Downstream:** `ChildTransport`, `loong_tui::Client` (`attach`, `replay`, `submit_with`, `respond`) and `print_streaming`.
//! - **Verify:** `cargo test -p loongagent --test tui_surface` · `./scripts/check.sh`

use super::*;

use crate::tui::ChildTransport;

/// Purpose: Print `model.delta` pieces as they arrive and return the rendered lines that are not already on screen.
/// Serves: `repl`, which must not show a streamed answer twice.
/// Design: `docs/design/surfaces.md` §7 (the one transient notification).
/// Up: `repl` on every prompt; no other caller.
/// Down: `Client::submit_with` and `std::io::stdout`.
/// Verify: `cargo test -p loongagent --test tui_surface`.
/// Print a turn's deltas as they arrive, and return the rendered lines that are
/// not already on screen. A streamed answer would otherwise appear twice: once as
/// it was written and once when the turn finished.
fn print_streaming(client: &mut Client<ChildTransport>, text: &str) -> Result<Vec<String>, String> {
    let mut streamed = String::new();
    let lines = client.submit_with(text, &mut |method, params| {
        if method != loong_schema::generated::notification::MODEL_DELTA {
            return;
        }
        if let Some(piece) = params.get("text").and_then(|t| t.as_str()) {
            streamed.push_str(piece);
            print!("{piece}");
            let _ = std::io::stdout().flush();
        }
    })?;
    if streamed.is_empty() {
        return Ok(lines);
    }
    if !streamed.ends_with('\n') {
        println!();
    }
    Ok(lines.into_iter().filter(|line| line != &streamed).collect())
}

/// Purpose: Run the line-mode loop: attach to the session, replay it, then prompt, approve or deny until stdin closes.
/// Serves: `loongagent repl` and the pipe test that proves a non-TTY surface works.
/// Design: `docs/design/surfaces.md` §2 and §7.
/// Up: `main.rs`'s `repl` arm; no other caller.
/// Down: `Args::ensure_parent`, `ChildTransport::spawn`, `Client::{attach, replay, pending_approval, respond}` and `print_streaming`.
/// Verify: `cargo test -p loongagent --test tui_surface`.
pub(crate) fn repl(args: Args) -> Result<(), String> {
    args.ensure_parent()?;
    let (transport, _) = ChildTransport::spawn(&args, false)?;
    let mut client = Client::new(transport, args.session_id());
    let cwd = args.cwd.to_string_lossy().to_string();
    let attached = client.attach(&cwd, "tui")?;
    println!(
        "loongagent — session {} (schema {}, {} events). Ctrl-D to quit.",
        attached.session_id, attached.schema_version, attached.existing_events
    );
    // Show what the session already contains, for the same reason the TUI does: a
    // reopened conversation should look reopened.
    for line in client.replay().unwrap_or_default() {
        println!("{line}");
    }

    let stdin = std::io::stdin();
    loop {
        let pending = client.pending_approval().map(|s| s.to_string());
        match &pending {
            Some(request_id) => print!("[a]pprove / [d]eny {request_id}: "),
            None => print!("> "),
        }
        std::io::stdout().flush().map_err(|e| e.to_string())?;

        let mut line = String::new();
        if stdin.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            println!();
            return Ok(());
        }
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let result = match pending {
            Some(request_id) => {
                let granted = matches!(line.as_str(), "a" | "approve" | "y" | "yes");
                client.respond(&request_id, granted, ApprovalScope::Once, None)
            }
            None => print_streaming(&mut client, &line),
        };
        match result {
            Ok(lines) => {
                for l in lines {
                    println!("{l}");
                }
            }
            Err(e) => eprintln!("error: {e}"),
        }
    }
}
