use super::*;

use crate::tui::ChildTransport;

pub(crate) fn headless(args: Args, prompt: &str) -> Result<(), String> {
    args.ensure_parent()?;
    let (transport, _) = ChildTransport::spawn(&args, false)?;
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
