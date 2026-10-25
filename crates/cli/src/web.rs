//! Purpose: The browser surface: the same JSON-RPC lines over a WebSocket, with a token on the handshake.
//!
//! # Contract
//! - **Serves:** `loongagent web`, the localhost page a browser drives.
//! - **Design:** `docs/design/surfaces.md` §4 and §7.
//! - **Upstream:** `main.rs`'s `web` arm parses the flags and calls `web`.
//! - **Downstream:** `ChildTransport` for the server child, `loong_web::{random_token, bind, serve}` and `Profile::explain_all`.
//! - **Verify:** `cargo test -p loongagent --test web_surface` · `./scripts/check.sh`

use super::*;

use crate::tui::ChildTransport;

/// Purpose: The bridge state: one server child plus the working directory the browser cannot know.
/// Serves: `web` and the `loong_web::Session` impl it installs.
/// Design: `docs/design/surfaces.md` §4.
/// Up: `web` constructs it and hands it to `loong_web::serve`.
/// Down: `ChildTransport` for the exchange and the `cwd` it fills into `session.attach`.
/// Verify: `cargo test -p loongagent --test web_surface`.
/// The browser surface: the same JSON-RPC lines, carried over a WebSocket.
struct WebSession {
    transport: ChildTransport,
    cwd: String,
}

impl loong_web::Session for WebSession {
    /// Purpose: Carry one browser line to the child, fill the attach cwd, and forward every line until this request's response.
    /// Serves: the `loong_web::Session` contract; `web` installs the impl.
    /// Design: `docs/design/surfaces.md` §4 and §7.
    /// Up: `loong_web::serve`, through the `Session` trait object.
    /// Down: `ChildTransport::{send, recv_line}` and the `sink` callback.
    /// Verify: `cargo test -p loongagent --test web_surface`.
    fn exchange(
        &mut self,
        line: &str,
        sink: &mut dyn FnMut(String) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut request: serde_json::Value =
            serde_json::from_str(line).map_err(|e| format!("not JSON-RPC: {e}"))?;
        // The browser cannot know the server's working directory, so the surface
        // fills it in. Everything else is passed through untouched.
        if request.get("method").and_then(|m| m.as_str())
            == Some(loong_schema::generated::method::SESSION_ATTACH)
        {
            if let Some(params) = request.get_mut("params") {
                let empty = params
                    .get("cwd")
                    .and_then(|c| c.as_str())
                    .map(|c| c.is_empty())
                    .unwrap_or(true);
                if empty {
                    params["cwd"] = serde_json::json!(self.cwd);
                }
            }
        }
        let id = request.get("id").cloned();
        self.transport.send(&request.to_string())?;

        loop {
            let Some(line) = self.transport.recv_line()? else {
                return Err("the server closed the connection".to_string());
            };
            if line.trim().is_empty() {
                continue;
            }
            let parsed: serde_json::Value =
                serde_json::from_str(&line).unwrap_or(serde_json::Value::Null);
            let is_response = parsed.get("id").is_some() && parsed.get("method").is_none();
            let mine = id.is_some() && parsed.get("id") == id.as_ref();
            // Straight through: a delta written by the model reaches the browser now,
            // not when the turn ends.
            sink(line)?;
            if is_response && mine {
                return Ok(());
            }
        }
    }
}

/// Purpose: Resolve settings, spawn the server child, bind a port and serve the page until the process ends.
/// Serves: `loongagent web`.
/// Design: `docs/design/surfaces.md` §4.
/// Up: `main.rs`'s `web` arm; no other caller.
/// Down: `Args::settings_with_pack`, `ChildTransport::spawn`, `loong_web::{random_token, bind, serve}`.
/// Verify: `cargo test -p loongagent --test web_surface`.
pub(crate) fn web(args: Args) -> Result<(), String> {
    args.ensure_parent()?;
    let (settings, pack) = args.settings_with_pack()?;
    if let Some(state) = &pack {
        eprintln!(
            "[control] {} {} — profile: {}",
            state.provenance.issuer,
            state.provenance.version,
            state.summary()
        );
    }
    let (transport, notes, _control) = ChildTransport::spawn(&args, true)?;
    if let Some(notes) = notes {
        // The server's own diagnostics go to this terminal, where the operator is.
        std::thread::spawn(move || {
            while let Ok(line) = notes.recv() {
                eprintln!("[server] {line}");
            }
        });
    }
    let session = std::sync::Arc::new(std::sync::Mutex::new(WebSession {
        transport,
        cwd: args.cwd.display().to_string(),
    }));
    let token = loong_web::random_token()?;
    let (listener, addr) = loong_web::bind(&args.bind, args.port)?;

    for row in settings.explain_all() {
        eprintln!(
            "[config] {:<28} {:<9} {}",
            row.key,
            row.value.render(),
            row.source
        );
    }
    println!("loongagent web — open this URL:");
    println!();
    println!("  http://{addr}/#token={token}");
    println!();
    println!("The token is in the URL fragment: browsers never send it to a server, and");
    println!("another page cannot open the socket without it. Bound to {addr}.");
    loong_web::serve(session, listener, token);
    Ok(())
}
