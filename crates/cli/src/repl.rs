use super::*;

use crate::tui::ChildTransport;

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
