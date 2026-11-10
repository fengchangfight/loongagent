//! Purpose: Serve the embedded browser page and bridge WebSocket lines to the same JSON-RPC server.
//!
//! # Contract
//! - **Serves:** a browser surface that speaks exactly what the TUI speaks, with a token on the handshake (surfaces.md §4).
//! - **Design:** `docs/design/web-surface.md` (ADR-0009).
//! - **Upstream:** `loongagent web` serves it; a browser opens the page and the socket.
//! - **Downstream:** `Session`, `tungstenite`, the TCP listener and the embedded page.
//! - **Verify:** `cargo test -p loong-web` · `./scripts/check.sh`
//!
//! The web surface (ADR-0009): the same session, in a browser.
//!
//! This layer is deliberately **ignorant of the session protocol**: it carries
//! JSON-RPC lines between a WebSocket and whatever session is behind it, and
//! serves the page. That is what the "one brain, many faces" split buys — the
//! browser speaks exactly what the TUI speaks, the kernel does not know a
//! browser exists, and there is no second implementation of the loop to keep in
//! step. What *is* local to the surface lives in `app`: the workspaces the
//! browser can sit on, and the session catalogue it lists, opens and deletes.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{accept_hdr, Message};

pub mod app;
mod sessions;

/// The page, compiled in: one file, no build step, no CDN.
pub const INDEX: &str = include_str!("index.html");

/// The page's behaviour, compiled in beside it.
pub const APP_JS: &str = include_str!("app.js");

/// The page's styling, compiled in beside it.
pub const STYLE_CSS: &str = include_str!("style.css");

/// Purpose: The site icon, compiled in — one copy for every page this crate serves.
///
/// The chat page and the study tab live on one origin, so both ask for the same path and the
/// browser shows a recognisable tab. It is the same logo the extension ships, copied rather
/// than referenced: the two crates must not depend on each other for an image.
/// Serves: `handle_routed`, for a request to `FAVICON_PATH`.
/// Design: `docs/design/reverse-ui.md` §Layout.
/// Up: `handle_routed`.
/// Down: the file `assets/favicon.png`, compiled in by `include_bytes!`.
/// Verify: `cargo test -p loong-web favicon`.
pub const FAVICON: &[u8] = include_bytes!("../assets/favicon.png");

/// Purpose: The path a page names in `<link rel="icon">`; the browsers' own `/favicon.ico`
/// request answers the same bytes.
/// Contract: `crates/web/src/lib.rs`.
pub const FAVICON_PATH: &str = "/favicon.png";

/// What the web layer needs from a session: a line in, lines out.
///
/// The lines are exactly what the stdio server writes — events first, then the
/// response — so a WebSocket message maps one-to-one onto a `handle_line` call.
/// Purpose: What the web layer needs from a session: one line in, answer lines out as they arrive.
///
/// Serves: the browser speaks exactly what the stdio server speaks (surfaces.md §4).
/// Design: `docs/design/surfaces.md` (ADR-0009).
/// Up: `serve` and `handle` hold one behind a mutex.
/// Down: `exchange`, implemented by the server bridge and by `Echo`.
/// Verify: `cargo test -p loong-web`.
pub trait Session: Send {
    /// Send one line, and hand back each line of the answer **as it arrives**.
    ///
    /// The sink exists for one reason: a model writes its answer over seconds, and
    /// making the browser wait for the last byte would waste the whole point of the
    /// event stream. A session that only produces its lines at the end is still
    /// correct — it simply calls the sink late.
    /// Purpose: Send one line and hand back each answer line as it arrives.
    fn exchange(
        &mut self,
        line: &str,
        sink: &mut dyn FnMut(String) -> Result<(), String>,
    ) -> Result<(), String>;
}

/// A token no other page can guess, read from the OS entropy pool.
/// Purpose: Read 16 bytes from the OS entropy pool and return them as hex.
///
/// Serves: the handshake token no other page can guess (surfaces.md §4).
/// Design: `docs/design/surfaces.md` (ADR-0009).
/// Up: `loongagent web` before it prints the URL.
/// Down: `/dev/urandom` and `format!` per byte.
/// Verify: `cargo test -p loong-web`.
pub fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| format!("reading /dev/urandom: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Purpose: Bind the web listener on the requested host and port and report the actual address.
///
/// Serves: loopback by default, with `--bind` as the operator's choice (surfaces.md §4).
/// Design: `docs/design/surfaces.md` (ADR-0009).
/// Up: `loongagent web`.
/// Down: `TcpListener::bind` and `local_addr`.
/// Verify: `cargo test -p loong-web`.
pub fn bind(host: &str, port: u16) -> Result<(TcpListener, SocketAddr), String> {
    let listener =
        TcpListener::bind((host, port)).map_err(|e| format!("binding {host}:{port}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    Ok((listener, addr))
}

/// A surface mounted inside the portal, answering its own URL prefix.
///
/// The portal is one listener and one token; a study surface like `reverse`
/// registers a `Router` and gets the connections whose path starts with its
/// prefix, while `/` (the chat page) and `/ws` (the socket) stay the portal's.
/// Purpose: A surface mounted inside the portal, answering the connections for one URL prefix.
///
/// Serves: the reverse study tab and any later surface beside it (reverse.md §4).
/// Design: `docs/design/reverse.md` §4 and `docs/design/surfaces.md` §4.
/// Up: `serve_with` asks it first; `loong_reverse_ui` implements it.
/// Down: `handle_routed`.
/// Verify: `cargo test -p loong-web`.
pub trait Router: Send + Sync {
    /// The URL prefix this surface owns, e.g. `/reverse`. The portal keeps `/` and `/ws`.
    /// Purpose: The URL prefix this surface owns.
    fn prefix(&self) -> &str;
    /// Answer one request on the connection; the request head is already read into `head`.
    /// Purpose: Answer one request whose head is already read, or decline it.
    fn answer(&self, head: &str, stream: &mut TcpStream) -> Result<(), String>;
}

/// Accept connections until the process ends. Each connection gets a thread; the
/// session is behind a mutex, so two browsers cannot interleave one request.
/// Purpose: Accept connections until the process ends, one thread each, one session behind a mutex.
///
/// Serves: one request at a time reaches the session while responses stream out (surfaces.md §4).
/// Design: `docs/design/surfaces.md` (ADR-0009).
/// Up: `loongagent web` after `bind`.
/// Down: `handle` and `std::thread::spawn`.
/// Verify: `cargo test -p loong-web`.
pub fn serve(session: Arc<Mutex<dyn Session>>, listener: TcpListener, token: String) {
    serve_with(session, listener, token, None);
}

/// Accept connections until the process ends, with the browser workspace layer on the socket.
/// Purpose: Accept connections until the process ends, seating every tab on the home workspace.
///
/// Serves: `loongagent web` — the multi-workspace chat surface (web-surface.md §1).
/// Design: `docs/design/web-surface.md` §§1–3.
/// Up: `crates/cli/src/web.rs`.
/// Down: `handle_web` and `std::thread::spawn`.
/// Verify: `cargo test -p loong-web` and `cargo test -p loongagent --test web_surface`.
pub fn serve_web(
    listener: TcpListener,
    token: String,
    home: PathBuf,
    start_session: Arc<
        dyn Fn(&std::path::Path, Option<&str>) -> Result<(Arc<Mutex<dyn Session>>, String), String>
            + Send
            + Sync,
    >,
) {
    let state = Arc::new(Mutex::new(app::State::new()));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(&state);
        let token = token.clone();
        let home = home.clone();
        let start_session = Arc::clone(&start_session);
        std::thread::spawn(move || {
            if let Err(e) = handle_web(stream, state, token, home, start_session) {
                eprintln!("[web] {e}");
            }
        });
    }
}

/// Like `serve`, but the connections under the mounted surface's prefix go to it
/// first; the chat page and the socket stay the portal's either way.
/// Purpose: Accept connections, giving a mounted surface the ones under its prefix before the portal.
///
/// Serves: a session that carries a study surface beside the chat tab (reverse.md §4).
/// Design: `docs/design/reverse.md` §4.
/// Up: `loongagent reverse` after `bind`.
/// Down: `handle_routed` and `std::thread::spawn`.
/// Verify: `cargo test -p loong-web`.
pub fn serve_with(
    session: Arc<Mutex<dyn Session>>,
    listener: TcpListener,
    token: String,
    router: Option<Arc<dyn Router>>,
) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let session = Arc::clone(&session);
        let token = token.clone();
        let router = router.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle_routed(stream, session, token, router) {
                eprintln!("[web] {e}");
            }
        });
    }
}

/// Purpose: Refuse a request whose token is missing or wrong.
///
/// Serves: a loopback surface is not authorisation on its own (reverse.md §4.2).
/// Design: `docs/design/reverse.md` §4.2.
/// Up: `handle_routed` for a mounted surface's data route.
/// Down: the 401 write.
/// Verify: `cargo test -p loong-web`.
fn refuse_without_token(mut stream: TcpStream) -> Result<(), String> {
    // Drain the request first: closing a socket with unread bytes makes the
    // kernel answer RST, and the client then sees a reset instead of the 401.
    let mut buffer = [0u8; 4096];
    let _ = stream.read(&mut buffer);
    let body = "missing or wrong token; open the URL printed by loongagent reverse";
    let head = format!(
        "HTTP/1.1 401 Unauthorized\r\nContent-Type: text/plain; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.flush())
        .map_err(|e| e.to_string())
}

/// Purpose: Read the token from a request head — `x-loong-token` header or `token` query.
///
/// Serves: the portal gates every mounted surface's data routes (reverse.md §4.2).
/// Design: `docs/design/reverse.md` §4.2.
/// Up: `handle_routed`.
/// Down: the head's lines and its request-line query.
/// Verify: `cargo test -p loong-web`.
fn presented_token(head: &str) -> Option<String> {
    let from_header = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.trim().eq_ignore_ascii_case("x-loong-token") {
            Some(value.trim().to_string())
        } else {
            None
        }
    });
    if from_header.is_some() {
        return from_header;
    }
    let target = head.lines().next()?.split_whitespace().nth(1)?;
    let query = target.split_once('?')?.1;
    query
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
        .map(|token| token.to_string())
}

/// Purpose: Route one connection on the workspace surface: the page and assets, the icon,
/// the workspace picker's directory listing, or the socket.
/// Serves: `serve_web` for every accepted connection.
/// Design: `docs/design/web-surface.md` §5.
/// Up: `serve_web`.
/// Down: `websocket_web`, `static_page`, `icon`, `asset`, `browse`.
/// Verify: `cargo test -p loong-web` and `cargo test -p loongagent --test web_surface`.
fn handle_web(
    stream: TcpStream,
    state: Arc<Mutex<app::State>>,
    token: String,
    home: PathBuf,
    start_session: Arc<
        dyn Fn(&std::path::Path, Option<&str>) -> Result<(Arc<Mutex<dyn Session>>, String), String>
            + Send
            + Sync,
    >,
) -> Result<(), String> {
    // Peek (not read): the WebSocket handshake must see the request itself, while
    // a static request is ours to answer.
    let mut peek = [0u8; 4096];
    let peeked = stream.peek(&mut peek).map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&peek[..peeked]).to_string();
    let request_line = head.lines().next().unwrap_or_default().to_string();
    let is_upgrade = head.to_ascii_lowercase().contains("upgrade: websocket");
    if request_line.contains("/ws") || is_upgrade {
        return websocket_web(stream, state, token, home, start_session);
    }
    let asked = request_line.split_whitespace().nth(1).unwrap_or("");
    let bare = asked.split('?').next().unwrap_or(asked);
    match bare {
        "/app.js" => return asset(stream, APP_JS, "application/javascript; charset=utf-8"),
        "/style.css" => return asset(stream, STYLE_CSS, "text/css; charset=utf-8"),
        FAVICON_PATH | "/favicon.ico" => return icon(stream),
        "/api/browse" => {
            // The picker lists directories; it carries the token like the study tab's
            // data routes (web-surface.md §5).
            if presented_token(&head).as_deref() != Some(token.as_str()) {
                return refuse_without_token(stream);
            }
            return browse(stream, asked, &home);
        }
        _ => static_page(stream),
    }
}

/// Purpose: Route one connection to the mounted surface, the socket, or the page.
///
/// Serves: a session with a study surface mounted beside the chat tab (reverse.md §4).
/// Design: `docs/design/reverse.md` §4.
/// Up: `serve` and `serve_with` for every accepted connection.
/// Down: `Router::answer`, `websocket`, `static_page`.
/// Verify: `cargo test -p loong-web`.
fn handle_routed(
    mut stream: TcpStream,
    session: Arc<Mutex<dyn Session>>,
    token: String,
    router: Option<Arc<dyn Router>>,
) -> Result<(), String> {
    // Peek (not read): the WebSocket handshake must see the request itself, while
    // a static request is ours to answer.
    let mut peek = [0u8; 4096];
    let peeked = stream.peek(&mut peek).map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&peek[..peeked]).to_string();
    let request_line = head.lines().next().unwrap_or_default().to_string();
    let is_upgrade = head.to_ascii_lowercase().contains("upgrade: websocket");
    if request_line.contains("/ws") || is_upgrade {
        return websocket(stream, session, token);
    }
    // One icon for every page on this origin, before the router gets a say: the study tab asks
    // for it too, and its own router has no reason to know about images.
    let asked = request_line.split_whitespace().nth(1).unwrap_or("");
    if asked == FAVICON_PATH || asked == "/favicon.ico" {
        return icon(stream);
    }
    if let Some(router) = router {
        let path = request_line.split_whitespace().nth(1).unwrap_or("");
        let prefix = router.prefix();
        if path == prefix || path.starts_with(&format!("{prefix}/")) {
            // The surface's own page carries no secret (like the chat page); its
            // data routes do — one of them lists the filesystem (reverse.md §4.2).
            let bare_path = path.split('?').next().unwrap_or(path);
            let is_page = bare_path == prefix || bare_path == format!("{prefix}/");
            if !is_page && presented_token(&head).as_deref() != Some(token.as_str()) {
                return refuse_without_token(stream);
            }
            // The surface consumes the head itself: peek left it on the wire.
            let mut sink = [0u8; 8192];
            let read = stream.read(&mut sink).map_err(|e| e.to_string())?;
            let request_head = String::from_utf8_lossy(&sink[..read]).to_string();
            return router.answer(&request_head, &mut stream);
        }
    }
    static_page(stream)
}

/// Purpose: Answer a request for the site icon with the compiled-in PNG.
///
/// Serves: the chat page and the study tab, both of which name `/favicon.png`.
/// Design: `docs/design/reverse-ui.md` §Layout.
/// Up: `handle_routed`.
/// Down: `FAVICON` and the TCP writes.
/// Verify: `cargo test -p loong-web favicon`.
fn icon(mut stream: TcpStream) -> Result<(), String> {
    // Drain the request: an unread buffer makes the kernel answer RST instead of our bytes.
    let mut buffer = [0u8; 4096];
    let _ = stream.read(&mut buffer);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\
         Cache-Control: max-age=86400\r\nConnection: close\r\n\r\n",
        FAVICON.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(FAVICON))
        .and_then(|_| stream.flush())
        .map_err(|e| e.to_string())
}

/// Purpose: Answer a plain GET with the embedded page.
///
/// Serves: a demo needs no asset server and no bundler (surfaces.md §4).
/// Design: `docs/design/surfaces.md` (ADR-0009).
/// Up: `handle` for a non-WebSocket request.
/// Down: `INDEX` and the TCP writes.
/// Verify: `cargo test -p loong-web`.
fn static_page(mut stream: TcpStream) -> Result<(), String> {
    // Consume the request headers; the page does not depend on them.
    let mut buffer = [0u8; 4096];
    let _ = stream.read(&mut buffer);
    let body = INDEX.as_bytes();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body))
        .and_then(|_| stream.flush())
        .map_err(|e| e.to_string())
}

/// Purpose: Answer a GET for one compiled-in asset with its content type.
/// Serves: `handle_web` for `/app.js` and `/style.css`.
/// Design: `docs/design/web-surface.md` §5.
/// Up: `handle_web`.
/// Down: the TCP writes.
/// Verify: `cargo test -p loong-web` (`asset_`).
fn asset(mut stream: TcpStream, body: &str, kind: &str) -> Result<(), String> {
    let mut buffer = [0u8; 4096];
    let _ = stream.read(&mut buffer);
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .and_then(|_| stream.flush())
        .map_err(|e| e.to_string())
}

/// Purpose: Answer the workspace picker's directory listing.
/// Serves: `handle_web` for `/api/browse`.
/// Design: `docs/design/web-surface.md` §5 — the picker lists directories only.
/// Up: `handle_web`.
/// Down: `sessions::browse`.
/// Verify: `cargo test -p loong-web` (`browse_`).
fn browse(mut stream: TcpStream, asked: &str, home: &std::path::Path) -> Result<(), String> {
    let mut buffer = [0u8; 4096];
    let _ = stream.read(&mut buffer);
    let (status, body) = match sessions::browse(asked, home) {
        Ok(body) => ("200 OK", body),
        Err(e) => ("400 Bad Request", json!({"error": e.0}).to_string()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\n\
         Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .and_then(|_| stream.flush())
        .map_err(|e| e.to_string())
}

/// Purpose: Accept the WebSocket handshake with the token check, then bridge messages to the session.
///
/// Serves: the browser drives the same JSON-RPC session as the TUI (surfaces.md §4).
/// Design: `docs/design/surfaces.md` (ADR-0009).
/// Up: `handle` when the request upgrades.
/// Down: `tungstenite::accept_hdr`, `Session::exchange` and the socket writes.
/// Verify: `cargo test -p loong-web`.
fn websocket(
    stream: TcpStream,
    session: Arc<Mutex<dyn Session>>,
    token: String,
) -> Result<(), String> {
    let mut socket = accept_hdr(stream, |request: &Request, response: Response| {
        let presented = request
            .uri()
            .query()
            .unwrap_or_default()
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="));
        if presented == Some(token.as_str()) {
            return Ok(response);
        }
        // Refuse the handshake: a page that cannot present the token never gets a
        // socket, so a stray tab cannot drive the agent.
        Err(ErrorResponse::from(
            tungstenite::http::Response::builder()
                .status(tungstenite::http::StatusCode::UNAUTHORIZED)
                .body(Some(
                    "missing or wrong token; open the URL printed by `loongagent web`".to_string(),
                ))
                .expect("a static response is valid"),
        ))
    })
    .map_err(|e| format!("websocket handshake refused: {e}"))?;

    loop {
        let message = match socket.read() {
            Ok(message) => message,
            Err(tungstenite::Error::ConnectionClosed) | Err(tungstenite::Error::Protocol(_)) => {
                return Ok(())
            }
            Err(e) => return Err(format!("reading from the browser: {e}")),
        };
        match message {
            Message::Text(text) => {
                // Held for the whole exchange: one request at a time reaches the
                // session, while each line goes out the moment it exists.
                let mut failure = None;
                {
                    let socket_ref = &mut socket;
                    let outcome = match session.lock() {
                        Ok(mut session) => session.exchange(text.as_str(), &mut |line| {
                            socket_ref
                                .send(Message::text(line))
                                .map_err(|e| format!("sending to the browser: {e}"))
                        }),
                        Err(_) => Err("the session is poisoned".to_string()),
                    };
                    if let Err(e) = outcome {
                        failure = Some(e);
                    }
                }
                match failure {
                    None => {}
                    Some(e) => {
                        // A transport failure is an event like any other: the page
                        // shows it instead of freezing with a dead socket.
                        let notice = json!({
                            "jsonrpc": "2.0",
                            "method": "event",
                            "params": {
                                "seq": 0, "id": "e-local", "parent_id": null,
                                "kind": "client.error", "ignorable": true,
                                "time_ms": 0, "data": {"message": e}
                            }
                        })
                        .to_string();
                        socket
                            .send(Message::text(notice))
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            Message::Close(_) => return Ok(()),
            Message::Ping(payload) => {
                socket
                    .send(Message::Pong(payload))
                    .map_err(|e| e.to_string())?;
            }
            _ => {}
        }
    }
}

/// Purpose: Accept the WebSocket handshake with the token check, then hand the socket to the
/// workspace bridge.
/// Serves: `handle_web` when the request upgrades.
/// Design: `docs/design/web-surface.md` §3.
/// Up: `handle_web`.
/// Down: `tungstenite::accept_hdr`, `app::handle_ws`.
/// Verify: `cargo test -p loong-web` and `cargo test -p loongagent --test web_surface`.
fn websocket_web(
    stream: TcpStream,
    state: Arc<Mutex<app::State>>,
    token: String,
    home: PathBuf,
    start_session: Arc<
        dyn Fn(&std::path::Path, Option<&str>) -> Result<(Arc<Mutex<dyn Session>>, String), String>
            + Send
            + Sync,
    >,
) -> Result<(), String> {
    let mut socket = accept_hdr(stream, |request: &Request, response: Response| {
        let presented = request
            .uri()
            .query()
            .unwrap_or_default()
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="));
        if presented == Some(token.as_str()) {
            return Ok(response);
        }
        Err(ErrorResponse::from(
            tungstenite::http::Response::builder()
                .status(tungstenite::http::StatusCode::UNAUTHORIZED)
                .body(Some(
                    "missing or wrong token; open the URL printed by `loongagent web`".to_string(),
                ))
                .expect("a static response is valid"),
        ))
    })
    .map_err(|e| format!("websocket handshake refused: {e}"))?;

    app::handle_ws(&mut socket, &state, &home, &*start_session)
}

/// A session that answers from a script: the web layer's own tests, and a way to
/// demo the page without a model behind it.
/// Purpose: A session that answers from a script, for tests and for a page demo without a model.
///
/// Serves: the web layer's own tests and a model-free demo (surfaces.md §4).
/// Design: `docs/design/surfaces.md` (ADR-0009).
/// Up: the tests in this module; no in-tree demo caller.
/// Down: `Session::exchange` and `serde_json`.
/// Verify: `cargo test -p loong-web`.
pub struct Echo(pub Vec<String>);

impl Session for Echo {
    /// Purpose: Emit the scripted lines, then a generic accepted result for the request id.
    ///
    /// Serves: a deterministic answer without a kernel (surfaces.md §4).
    /// Design: `docs/design/surfaces.md` (ADR-0009).
    /// Up: `websocket` through `Session`.
    /// Down: the scripted lines and the `json!` result.
    /// Verify: `cargo test -p loong-web`.
    fn exchange(
        &mut self,
        line: &str,
        sink: &mut dyn FnMut(String) -> Result<(), String>,
    ) -> Result<(), String> {
        let request: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        for line in self.0.clone() {
            sink(line)?;
        }
        sink(
            json!({"jsonrpc": "2.0", "id": id, "result": {"accepted": true, "turn_id": 1}})
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tungstenite::client::IntoClientRequest;

    /// Purpose: Start a server with a fresh token on loopback for a test.
    ///
    /// Contract: `crates/web/src/lib.rs`.
    fn start(session: Arc<Mutex<dyn Session>>) -> (SocketAddr, String) {
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = random_token().unwrap();
        let for_server = token.clone();
        std::thread::spawn(move || serve(session, listener, for_server));
        (addr, token)
    }

    /// Purpose: Pins that the page is served without a token because the socket is the gated part.
    #[test]
    fn the_page_is_served_without_a_token() {
        // The page itself carries no secret: it is the socket that is gated, so a
        // plain GET is fine over loopback.
        let session = Arc::new(Mutex::new(Echo(vec![])));
        let (addr, _) = start(session);
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        assert!(body.starts_with("HTTP/1.1 200 OK"), "{body}");
        assert!(body.contains("text/html"), "{body}");
        assert!(
            body.contains("loongagent"),
            "the page identifies itself: {body}"
        );
    }

    /// Purpose: Pins that the site icon is served as a PNG, at both paths a browser may ask for,
    /// so the tab is recognisable on the chat page and on the study tab.
    #[test]
    fn favicon_is_served_as_a_png() {
        let session = Arc::new(Mutex::new(Echo(vec![])));
        let (addr, _) = start(session);
        for path in [FAVICON_PATH, "/favicon.ico"] {
            let mut stream = TcpStream::connect(addr).unwrap();
            let request =
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            stream.write_all(request.as_bytes()).unwrap();
            let mut body = Vec::new();
            stream.read_to_end(&mut body).unwrap();
            let head = String::from_utf8_lossy(&body).to_string();
            assert!(head.starts_with("HTTP/1.1 200 OK"), "{path}: {head}");
            assert!(head.contains("Content-Type: image/png"), "{path}: {head}");
            assert!(body.ends_with(FAVICON), "{path}: the bytes are the icon");
        }
        // And the page names it, or no browser would ask.
        assert!(
            INDEX.contains("rel=\"icon\"") && INDEX.contains(&format!("href=\"{FAVICON_PATH}\"")),
            "the page points at the icon"
        );
    }

    /// Purpose: Pins that a WebSocket carries a request and gets the event and then the answer.
    #[test]
    fn a_websocket_carries_a_request_and_gets_the_answer_back() {
        let session = Arc::new(Mutex::new(Echo(vec![
            json!({"jsonrpc": "2.0", "method": "event", "params": {"kind": "message.appended", "data": {"role": "assistant", "text": "hello from the brain"}}}).to_string(),
        ])));
        let (addr, token) = start(session);
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();

        socket
            .send(Message::text(
                json!({"jsonrpc": "2.0", "id": 7, "method": "prompt.submit", "params": {"session_id": "s", "text": "hi"}}).to_string(),
            ))
            .unwrap();

        let first = socket.read().unwrap().into_text().unwrap().to_string();
        let second = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(
            first.contains("message.appended"),
            "the event came first: {first}"
        );
        assert!(second.contains("\"id\":7"), "then the response: {second}");
        assert!(second.contains("accepted"), "{second}");
    }

    /// Purpose: Pins that a WebSocket without the token is refused.
    #[test]
    fn a_websocket_without_the_token_is_refused() {
        let session = Arc::new(Mutex::new(Echo(vec![])));
        let (addr, _) = start(session);
        let request = format!("ws://{addr}/ws").into_client_request().unwrap();
        match tungstenite::connect(request) {
            Err(e) => {
                let message = e.to_string();
                assert!(
                    message.contains("401") || message.contains("token"),
                    "{message}"
                );
            }
            Ok(_) => panic!("a connection without the token must not be accepted"),
        }
    }

    /// Purpose: Pins that lines reach the browser as the session produces them and the response comes last.
    #[test]
    fn lines_reach_the_browser_as_the_session_produces_them() {
        /// Purpose: A session that emits two delta lines and then a response, to pin streaming order.
        ///
        /// Serves: `lines_reach_the_browser_as_the_session_produces_them`.
        /// Design: `docs/design/surfaces.md` (ADR-0009).
        /// Up: that test only.
        /// Down: `Session::exchange`.
        /// Verify: `cargo test -p loong-web`.
        struct Scripted;
        impl Session for Scripted {
            /// Purpose: Emit two deltas, then the response, and return — the order is the point.
            ///
            /// Serves: `lines_reach_the_browser_as_the_session_produces_them`.
            /// Design: `docs/design/surfaces.md` (ADR-0009).
            /// Up: that test only.
            /// Down: the sink calls.
            /// Verify: `cargo test -p loong-web`.
            fn exchange(
                &mut self,
                _line: &str,
                sink: &mut dyn FnMut(String) -> Result<(), String>,
            ) -> Result<(), String> {
                sink(
                    json!({"jsonrpc": "2.0", "method": "model.delta", "params": {"text": "he"}})
                        .to_string(),
                )?;
                sink(
                    json!({"jsonrpc": "2.0", "method": "model.delta", "params": {"text": "llo"}})
                        .to_string(),
                )?;
                sink(json!({"jsonrpc": "2.0", "id": 1, "result": {"accepted": true}}).to_string())
            }
        }

        let session = Arc::new(Mutex::new(Scripted));
        let (addr, token) = start(session);
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc": "2.0", "id": 1, "method": "prompt.submit", "params": {}})
                    .to_string(),
            ))
            .unwrap();
        let one = socket.read().unwrap().into_text().unwrap().to_string();
        let two = socket.read().unwrap().into_text().unwrap().to_string();
        let three = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(one.contains("\"he\""), "first delta streamed: {one}");
        assert!(two.contains("\"llo\""), "second delta streamed: {two}");
        assert!(three.contains("\"id\":1"), "the response last: {three}");
    }

    /// Purpose: Pins that a broken exchange is surfaced to the page as an event, not a hang.
    #[test]
    fn a_transport_failure_is_an_event_not_a_hang() {
        /// Purpose: A session that fails every exchange, to pin the failure notice.
        ///
        /// Serves: `a_transport_failure_is_an_event_not_a_hang`.
        /// Design: `docs/design/surfaces.md` (ADR-0009).
        /// Up: that test only.
        /// Down: `Session::exchange`.
        /// Verify: `cargo test -p loong-web`.
        struct Broken;
        impl Session for Broken {
            /// Purpose: Fail with a message the page can show.
            ///
            /// Serves: `a_transport_failure_is_an_event_not_a_hang`.
            /// Design: `docs/design/surfaces.md` (ADR-0009).
            /// Up: that test only.
            /// Down: nothing; the error is the output.
            /// Verify: `cargo test -p loong-web`.
            fn exchange(
                &mut self,
                _line: &str,
                _sink: &mut dyn FnMut(String) -> Result<(), String>,
            ) -> Result<(), String> {
                Err("the child died".to_string())
            }
        }

        let session = Arc::new(Mutex::new(Broken));
        let (addr, token) = start(session);
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc": "2.0", "id": 1, "method": "prompt.submit", "params": {}})
                    .to_string(),
            ))
            .unwrap();
        let line = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(line.contains("client.error"), "{line}");
        assert!(line.contains("the child died"), "{line}");
    }

    /// Purpose: Pins that a mounted surface's data routes are token-gated, and its page is not.
    #[test]
    fn a_mounted_surfaces_data_routes_carry_the_token() {
        /// Purpose: A study-surface stub that answers its own prefix with plain text.
        ///
        /// Serves: `a_mounted_surfaces_data_routes_carry_the_token`.
        /// Design: `docs/design/reverse.md` §4.
        /// Up: that test only.
        /// Down: `Router::answer`.
        /// Verify: `cargo test -p loong-web`.
        struct Stub;
        impl Router for Stub {
            /// Purpose: The stub owns `/study`.
            /// Contract: `crates/web/src/lib.rs`.
            fn prefix(&self) -> &str {
                "/study"
            }
            /// Purpose: Answer with a fixed body, like a study page would.
            /// Contract: `crates/web/src/lib.rs`.
            fn answer(&self, head: &str, stream: &mut TcpStream) -> Result<(), String> {
                let body = format!("stub answered: {}", head.lines().next().unwrap_or(""));
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .map_err(|e| e.to_string())
            }
        }

        /// Purpose: Run one GET and return the whole response, for the route assertions.
        ///
        /// Serves: the token-gating test.
        /// Design: `docs/design/reverse.md` §4.
        /// Up: that test only.
        /// Down: the TCP round trip.
        /// Verify: `cargo test -p loong-web`.
        fn get(addr: SocketAddr, request: &str) -> String {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut body = String::new();
            stream.read_to_string(&mut body).unwrap();
            body
        }

        let session = Arc::new(Mutex::new(Echo(vec![])));
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = random_token().unwrap();
        let for_server = token.clone();
        std::thread::spawn(move || serve_with(session, listener, for_server, Some(Arc::new(Stub))));

        // The surface's own page is public, like the chat page.
        let body = get(
            addr,
            "GET /study HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(body.starts_with("HTTP/1.1 200 OK"), "the page: {body}");

        // Its data routes are not: without the token, a refusal.
        let body = get(
            addr,
            "GET /study/tree HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(
            body.starts_with("HTTP/1.1 401"),
            "no token, no data: {body}"
        );

        // The token opens them, as a header or as a query parameter.
        let body = get(
            addr,
            &format!("GET /study/tree HTTP/1.1\r\nHost: x\r\nx-loong-token: {token}\r\nConnection: close\r\n\r\n"),
        );
        assert!(body.contains("stub answered"), "with the header: {body}");
        let body = get(
            addr,
            &format!(
                "GET /study/tree?token={token} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
            ),
        );
        assert!(body.contains("stub answered"), "with the query: {body}");

        // A wrong token is refused, not merely ignored.
        let body = get(
            addr,
            "GET /study/tree?token=deadbeef HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(body.starts_with("HTTP/1.1 401"), "wrong token: {body}");
    }

    /// Purpose: A mounted surface answers under its prefix while the portal keeps `/` and `/ws`.
    #[test]
    fn a_mounted_surface_answers_under_its_prefix() {
        /// Purpose: A study-surface stub that answers its own prefix with plain text.
        ///
        /// Serves: `a_mounted_surface_answers_under_its_prefix`.
        /// Design: `docs/design/reverse.md` §4.
        /// Up: that test only.
        /// Down: `Router::answer`.
        /// Verify: `cargo test -p loong-web`.
        struct Stub;
        impl Router for Stub {
            /// Purpose: The stub owns `/study`.
            /// Contract: `crates/web/src/lib.rs`.
            fn prefix(&self) -> &str {
                "/study"
            }
            /// Purpose: Answer with a fixed body, like a study page would.
            /// Contract: `crates/web/src/lib.rs`.
            fn answer(&self, head: &str, stream: &mut TcpStream) -> Result<(), String> {
                let body = format!("stub answered: {}", head.lines().next().unwrap_or(""));
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .map_err(|e| e.to_string())
            }
        }

        let session = Arc::new(Mutex::new(Echo(vec![])));
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = random_token().unwrap();
        let for_server = token.clone();
        std::thread::spawn(move || serve_with(session, listener, for_server, Some(Arc::new(Stub))));

        // The prefix goes to the surface (its data routes carry the token, §4.2).
        let mut stream = TcpStream::connect(addr).unwrap();
        let request = format!(
            "GET /study/tree HTTP/1.1\r\nHost: x\r\nx-loong-token: {token}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        assert!(body.contains("stub answered: GET /study/tree"), "{body}");

        // The chat page stays the portal's.
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        assert!(body.contains("loongagent"), "the portal page: {body}");

        // And the socket is still the portal's, token and all.
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket.send(Message::text("{}")).unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(answer.contains("accepted"), "{answer}");
    }

    /// Purpose: The workspace surface serves the page, its assets, and refuses the picker without a token.
    #[test]
    fn the_workspace_surface_serves_assets_and_gates_the_picker() {
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = random_token().unwrap();
        let for_server = token.clone();
        let home = std::env::temp_dir();
        std::thread::spawn(move || {
            serve_web(
                listener,
                for_server,
                home,
                Arc::new(|_: &std::path::Path, session: Option<&str>| {
                    let id = session.unwrap_or("echo").to_string();
                    Ok((
                        Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        id,
                    ))
                }),
            )
        });

        /// Purpose: Run one GET and return the whole response.
        ///
        /// Serves: the workspace-surface test's assertions.
        /// Design: `docs/design/web-surface.md` §5.
        /// Up: that test only.
        /// Down: the TCP round trip.
        /// Verify: `cargo test -p loong-web`.
        fn get(addr: SocketAddr, request: &str) -> String {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut body = String::new();
            stream.read_to_string(&mut body).unwrap();
            body
        }

        let page = get(
            addr,
            "GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(page.starts_with("HTTP/1.1 200 OK"), "the page: {page}");
        assert!(page.contains("loongagent"), "{page}");

        let js = get(
            addr,
            "GET /app.js HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(js.contains("application/javascript"), "{js}");
        let css = get(
            addr,
            "GET /style.css HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(css.contains("text/css"), "{css}");

        let refused = get(
            addr,
            "GET /api/browse HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(refused.starts_with("HTTP/1.1 401"), "no token: {refused}");
        let listed = get(
            addr,
            &format!(
                "GET /api/browse?token={token} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
            ),
        );
        assert!(
            listed.starts_with("HTTP/1.1 200 OK"),
            "with token: {listed}"
        );
        assert!(listed.contains("\"dirs\""), "{listed}");
    }
}
