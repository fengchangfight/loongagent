//! Purpose: `loongagent run` — one prompt, the turn's events in order, then the answer.
//!
//! # Contract
//! - **Serves:** the `run` command and any script that pipes a prompt at the binary.
//! - **Design:** `docs/design/surfaces.md` §2 and §7.
//! - **Upstream:** `main.rs`'s `run` arm parses the positional prompt and calls `headless`.
//! - **Downstream:** `ChildTransport::spawn` for the server child and `loong_tui::Client` for attach and submit.
//! - **Verify:** `cargo test -p loongagent --test vertical_slice` · `./scripts/check.sh`

use super::*;

use crate::tui::ChildTransport;

/// Purpose: Run one prompt and print the turn's rendered lines in the order the log recorded them.
/// Serves: `loongagent run PROMPT`, the headless surface a script or a pipe uses.
/// Design: `docs/design/surfaces.md` §2 and §7.
/// Up: `main.rs`'s `run` arm; no other caller.
/// Down: `Args::ensure_parent`, `ChildTransport::spawn`, `Client::attach` and `Client::submit`.
/// Verify: `cargo test -p loongagent --test vertical_slice`.
pub(crate) fn headless(args: Args, prompt: &str) -> Result<(), String> {
    args.ensure_parent()?;
    let (transport, _, _) = ChildTransport::spawn(&args, false)?;
    let mut client = Client::new(transport, args.session_id());
    client.attach(&args.cwd.to_string_lossy(), "headless")?;
    // Headless output stays in order: the turn's events, then the answer. Streaming
    // would print the answer before the tool calls that produced it, which is
    // exactly wrong for a log someone reads afterwards. `repl`, `tui` and `web`
    // stream, because a human is watching.
    for line in client.submit(prompt)? {
        println!("{line}");
    }
    if !client.last_accepted() {
        eprintln!("the turn was refused; the reason is in the event above");
        std::process::exit(3);
    }
    if let Some(request_id) = client.pending_approval().map(|s| s.to_string()) {
        eprintln!("approval required for {request_id}; rerun with `tui` to answer");
        std::process::exit(3);
    }
    Ok(())
}
