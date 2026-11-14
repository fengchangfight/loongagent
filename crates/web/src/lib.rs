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
mod files;
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

/// Purpose: Ask a child to turn its permission prompts off or on **now**, while a turn owns
/// its bridge.
///
/// Serves: `permissions.set` arriving mid-turn — the one thing a session cannot answer down
/// its own `exchange`, because a turn holds that bridge from the prompt to the last event.
/// Design: `docs/design/web-surface.md` §4; ADR-0019.
/// Up: `Started::channel`, kept by `Child` beside the pid for the same reason.
/// Down: the private control channel of the child (descriptor 3, out of every tool's reach).
/// Verify: `cargo test -p loong-web` (`yolo_`).
pub type PermissionChannel = std::sync::Arc<dyn Fn(bool) -> Result<(), String> + Send + Sync>;

/// What one starter hands back for one session.
/// Purpose: Bundle the three things a surface needs about a child it has just started: the
/// bridge, the id, and the way in that the bridge cannot provide.
///
/// Serves: `serve_web`'s starter, read by `ensure_child`.
/// Design: `docs/design/web-surface.md` §3.
/// Up: the starter the process owner passes in.
/// Down: `Child`'s three fields.
/// Verify: `cargo test -p loong-web` (`session_`).
pub struct Started {
    /// Purpose: The bridge every ordinary request goes down.
    pub session: Arc<Mutex<dyn Session>>,
    /// Purpose: The id this session answers to, which the surface keys its children by.
    pub session_id: String,
    /// Purpose: The way in while a turn holds the bridge; `None` when the child has none — a
    /// surface served in-process, or a platform with no control channel.
    pub channel: Option<PermissionChannel>,
}

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

    /// The process this session talks to, when it talks to one.
    ///
    /// Purpose: Name the child whose turn a Stop button may end.
    ///
    /// Serves: `/api/interrupt` (`app::stop_running_turn`), which must ask the operating
    /// system rather than the session: a running turn holds the session's lock, so a
    /// question asked through `exchange` would be answered only after the turn it was
    /// meant to end had already ended (web-surface.md §4).
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: `app::Child::pid`, read once when the child is created.
    /// Down: `ChildTransport`'s handle in `crates/cli`.
    /// Verify: `cargo test -p loong-web` (`stop_`).
    /// Contract: `None` means "not a process" — a test double, or a session served
    /// in-process. It is not an error, and the route says so in words.
    fn pid(&self) -> Option<u32> {
        None
    }
}

/// What the "add a model" panel needs: what is configured now, what a provider asks
/// for, what it offers, and the one act of adopting a selection.
///
/// The panel is the `local add` wizard without a terminal (web-surface.md §6). The
/// probing, the key file and the `[[models]]` blocks stay where they already live, in
/// the CLI, so this trait is the whole of what the surface knows about models: four
/// answers, carried as JSON, and no opinion about where they come from.
/// Purpose: What the surface needs to offer the model panel: what is configured, what a
/// provider asks for, what it offers, and the act of adopting a selection.
///
/// Serves: a browser can go from "no models at all" to a working one without leaving the
/// page, and can turn one off again later (web-surface.md §6, config-routing.md §6).
/// Design: `docs/design/web-surface.md` §6.
/// Up: `serve_web_with_models` holds one behind an `Arc`.
/// Down: `app::dispatch_surface`'s `model.*` arms.
/// Verify: `cargo test -p loong-web` (`model_`).
pub trait ModelSetup: Send + Sync {
    /// Purpose: The models the profile declares now, for the page's empty-state warning and
    /// for the panel's list of what can be turned off.
    ///
    /// Serves: `model.configured`, which the page asks at boot before any session exists and
    /// again after every write.
    /// Design: `docs/design/web-surface.md` §6.
    /// Up: `dispatch_surface`.
    /// Down: whatever the implementation reads, usually the resolved profile.
    /// Verify: `cargo test -p loong-web` (`model_`).
    fn configured(&self) -> Value {
        json!({"models": []})
    }

    /// Purpose: The providers the panel may offer, and whether each one needs a key.
    ///
    /// Serves: `model.providers`, which fills the panel's provider list.
    /// Design: `docs/design/web-surface.md` §6.
    /// Up: `dispatch_surface`.
    /// Down: the implementation's provider catalogue.
    /// Verify: `cargo test -p loong-web` (`model_`).
    fn providers(&self) -> Value {
        json!({"providers": []})
    }

    /// Purpose: Write a new model order where it belongs, and say whether it went anywhere.
    ///
    /// Serves: `model.reorder`, the ctrl+left / ctrl+right keys (web-surface.md §4).
    /// Design: `docs/design/web-surface.md` §4 and `docs/design/config-routing.md` §3.
    /// Up: `dispatch_surface`.
    /// Down: the implementation's config writer.
    /// Verify: `cargo test -p loong-web` (`reorder_`).
    /// `Ok(true)` means the order is now on disk and the next child will read it;
    /// `Ok(false)` means this surface has no file to rewrite, so the arrangement lives on the
    /// bar for this session only — the same split the TUI makes between a local profile and a
    /// managed one.
    fn save_order(&self, _names: &[String]) -> Result<bool, String> {
        Ok(false)
    }

    /// Purpose: List what one provider offers and which of it is already configured, without
    /// writing anything.
    ///
    /// Serves: `model.probe`, the panel's second step: the rows it draws as tick boxes.
    /// Design: `docs/design/web-surface.md` §6 and `docs/design/config-routing.md` §6.
    /// Up: `dispatch_surface`.
    /// Down: the implementation's probe and its read of the config.
    /// Verify: `cargo test -p loong-web` (`model_`).
    /// Nothing is written, and that is the point: pasting a key is a statement about the
    /// credentials, not about wanting every model the provider has ever published. An empty
    /// `key` means the secret is already on file.
    fn probe(&self, _provider: &str, _key: &str) -> Result<Value, String> {
        Err("this surface cannot list models".to_string())
    }

    /// Purpose: Write a provider's selection — the models ticked in the panel — and say which
    /// ones are now configured.
    ///
    /// Serves: `model.add`, the button's one action.
    /// Design: `docs/design/web-surface.md` §6 and `docs/design/config-routing.md` §6.
    /// Up: `dispatch_surface`.
    /// Down: the implementation's probe and config write.
    /// Verify: `cargo test -p loong-web` (`model_`).
    /// The list is written as given: the models in it are the models that end up on, and a
    /// model missing from it is a model that goes off. An empty `key` means the secret is on
    /// file, so nothing is probed before writing.
    fn adopt(
        &self,
        _provider: &str,
        _key: &str,
        _models: &[(String, Option<u64>)],
    ) -> Result<Value, String> {
        Err("this surface cannot add a model".to_string())
    }
}

/// The setup a surface without a model panel gets: it lists nothing and adds nothing.
/// Purpose: The empty model setup, for a surface that offers no panel and for tests that do not
/// exercise one.
///
/// Serves: `serve_web`, which wires no panel.
/// Design: `docs/design/web-surface.md` §6.
/// Up: `serve_web`.
/// Down: every `ModelSetup` default.
/// Verify: `cargo test -p loong-web`.
pub struct NoModelSetup;

impl ModelSetup for NoModelSetup {}

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
        dyn Fn(&std::path::Path, Option<&str>) -> Result<Started, String> + Send + Sync,
    >,
) {
    serve_web_with_models(
        listener,
        token,
        home,
        start_session,
        Arc::new(NoModelSetup),
        // No process was spawned by this call, so there is nothing for Stop to signal:
        // the route says as much instead of pretending (web-surface.md §4).
        Arc::new(|_: u32| {}),
    );
}

/// How the surface stops a running turn: signal the process behind a session.
///
/// It is injected rather than asked of the session for one reason: a running turn holds
/// the session's lock, so the only way to end one is to go around it — to the process
/// itself (`web-surface.md` §4). The CLI wires `loong_tui::interrupt` here, so the
/// browser's Stop and the terminal's esc are literally the same act, and this crate keeps
/// no second copy of the platform code. A surface with no child (`serve_web` in the tests)
/// passes a signal that does nothing.
/// Purpose: Signal the process a session talks to, ending the turn it is running.
///
/// Serves: `/api/interrupt`, the page's Stop button and its esc (web-surface.md §4).
/// Design: `docs/design/web-surface.md` §4.
/// Up: `serve_web_with_models`, which takes one from the surface that spawned the child.
/// Down: `handle_web`'s `interrupt` route.
/// Verify: `cargo test -p loong-web` (`stop_`) and `cargo test -p loongagent --test web_surface`.
pub type StopSignal = Arc<dyn Fn(u32) + Send + Sync>;

/// The same surface, with the model panel wired to a real setup.
///
/// The split exists so a surface with no panel — a test, or a build that cannot reach
/// the config files — says so once, in its call, instead of every call site carrying an
/// `Option` it will never fill.
/// Purpose: Serve the browser surface with the "add a model" panel wired to `models`.
///
/// Serves: `loongagent web`, whose panel adopts a provider into the user's own config
/// (web-surface.md §6).
/// Design: `docs/design/web-surface.md` §6.
/// Up: `crates/cli/src/web.rs`.
/// Down: `handle_web`, which passes `models` to the socket bridge.
/// Verify: `cargo test -p loong-web` (`model_`) and `cargo test -p loongagent --test web_surface`.
pub fn serve_web_with_models(
    listener: TcpListener,
    token: String,
    home: PathBuf,
    start_session: Arc<
        dyn Fn(&std::path::Path, Option<&str>) -> Result<Started, String> + Send + Sync,
    >,
    models: Arc<dyn ModelSetup>,
    stop: StopSignal,
) {
    let state = Arc::new(Mutex::new(app::State::new()));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let state = Arc::clone(&state);
        let token = token.clone();
        let home = home.clone();
        let start_session = Arc::clone(&start_session);
        let models = Arc::clone(&models);
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            if let Err(e) = handle_web(stream, state, token, home, start_session, models, stop) {
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
        dyn Fn(&std::path::Path, Option<&str>) -> Result<Started, String> + Send + Sync,
    >,
    models: Arc<dyn ModelSetup>,
    stop: StopSignal,
) -> Result<(), String> {
    // Peek (not read): the WebSocket handshake must see the request itself, while
    // a static request is ours to answer.
    let mut peek = [0u8; 4096];
    let peeked = stream.peek(&mut peek).map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&peek[..peeked]).to_string();
    let request_line = head.lines().next().unwrap_or_default().to_string();
    let is_upgrade = head.to_ascii_lowercase().contains("upgrade: websocket");
    if request_line.contains("/ws") || is_upgrade {
        return websocket_web(stream, state, token, home, start_session, models);
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
        "/api/files" => {
            // The composer's `@` picker asks for the workspace's files on every
            // keystroke; same token, same rules as the directory picker
            // (web-composer.md §2).
            if presented_token(&head).as_deref() != Some(token.as_str()) {
                return refuse_without_token(stream);
            }
            return files_under_workspace(stream, asked, &state);
        }
        "/api/interrupt" => {
            // The page's Stop button and its esc (web-composer.md §4). The token is the
            // same one every other data route takes: a signal is only as public as the
            // socket it answers.
            if presented_token(&head).as_deref() != Some(token.as_str()) {
                return refuse_without_token(stream);
            }
            return interrupt(stream, &state, &stop);
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

/// Purpose: End the turn a session is running: the page's Stop button and its esc.
///
/// Serves: `handle_web` for `/api/interrupt` (web-surface.md §4).
/// Design: `docs/design/web-surface.md` §4.
/// Up: `handle_web`, after the token check.
/// Down: `app::busy_child`, `read_request_body`, the injected `StopSignal`.
/// Verify: `cargo test -p loong-web` (`stop_`).
/// Contract: it answers with `interrupted` and, when false, a `reason` in words — never a
/// bare status — because the page shows that answer in its status line.
fn interrupt(
    mut stream: TcpStream,
    state: &Arc<Mutex<app::State>>,
    stop: &StopSignal,
) -> Result<(), String> {
    // The session id travels in the body, not the query: a signal is not something a
    // stray link or an image tag on another origin can fire at this surface.
    let asked = serde_json::from_str::<Value>(&read_request_body(&mut stream)?)
        .ok()
        .and_then(|body| {
            body.get("session_id")
                .and_then(Value::as_str)
                .map(|id| id.to_string())
        });
    let (status, body) = match app::busy_child(state, asked.as_deref()) {
        Err(e) => (
            "500 Internal Server Error",
            json!({ "interrupted": false, "reason": e }),
        ),
        // Nothing to stop is not an error of the caller's: the turn may have answered
        // between the click and this request.
        Ok(None) => (
            "409 Conflict",
            json!({ "interrupted": false, "reason": "no turn is running" }),
        ),
        Ok(Some((_, None))) => (
            "409 Conflict",
            json!({ "interrupted": false, "reason": "this session has no process to signal" }),
        ),
        Ok(Some((_, Some(pid)))) => {
            stop(pid);
            ("200 OK", json!({ "interrupted": true }))
        }
    };
    let body = body.to_string();
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

/// Purpose: Read a request's body, given that its head has only been peeked so far.
///
/// Serves: `interrupt`, the one route on this surface that takes a body.
/// Design: `docs/design/web-surface.md` §5.
/// Up: `interrupt`.
/// Down: the socket reads and the `Content-Length` header.
/// Verify: `cargo test -p loong-web` (`stop_`).
/// Contract: a request with no body answers the empty string, and a body over 64 KiB is
/// refused rather than buffered — this surface takes one field, not a payload.
fn read_request_body(stream: &mut TcpStream) -> Result<String, String> {
    let mut raw: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let split = raw.windows(4).position(|window| window == b"\r\n\r\n");
        let Some(split) = split else {
            let read = stream.read(&mut buffer).map_err(|e| e.to_string())?;
            if read == 0 {
                return Ok(String::new());
            }
            raw.extend_from_slice(&buffer[..read]);
            if raw.len() > 64 * 1024 {
                return Err("request too large".to_string());
            }
            continue;
        };
        let head = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
        let want = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let start = split + 4;
        if raw.len() - start >= want {
            return Ok(String::from_utf8_lossy(&raw[start..start + want]).to_string());
        }
        let read = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            // A client that closed mid-body: answer with what arrived, and let the JSON
            // parse decide whether it was enough.
            return Ok(String::from_utf8_lossy(&raw[start..]).to_string());
        }
        raw.extend_from_slice(&buffer[..read]);
    }
}

/// Purpose: One decoded value from a request's query string, by name.
///
/// Serves: `files_under_workspace` for `q=`, `limit=` and `seat=`.
/// Design: `docs/design/web-composer.md` §2.
/// Up: `files_under_workspace`.
/// Down: `sessions::percent_decode`.
/// Verify: `cargo test -p loong-web` (`files_answer_ranks_the_typed_query`).
/// Contract: a missing name is `None`, not the empty string; the caller decides the default.
fn query_value(asked: &str, name: &str) -> Option<String> {
    let query = asked.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        if key == name {
            Some(sessions::percent_decode(value))
        } else {
            None
        }
    })
}

/// Purpose: Answer the composer's `@` picker: the seat's workspace files, ranked for the typed query.
///
/// Serves: `handle_web` for `/api/files`.
/// Design: `docs/design/web-composer.md` §2.
/// Up: `handle_web`, after the token check.
/// Down: `files::answer` and the workspace seat in `app::State`.
/// Verify: `cargo test -p loong-web` (`files_answer_`).
fn files_under_workspace(
    mut stream: TcpStream,
    asked: &str,
    state: &Arc<Mutex<app::State>>,
) -> Result<(), String> {
    let mut buffer = [0u8; 4096];
    let _ = stream.read(&mut buffer);
    let query = query_value(asked, "q").unwrap_or_default();
    let limit: usize = query_value(asked, "limit")
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(10);
    // The seat is asked for by index, and resolved to a directory here: a page must
    // not be able to name a directory of its own choosing (web-composer.md §2).
    let dir = query_value(asked, "seat")
        .and_then(|raw| raw.parse::<usize>().ok())
        .and_then(|seat| {
            state
                .lock()
                .ok()
                .and_then(|st| st.workspaces.get(seat).map(|w| w.dir.clone()))
        });
    let (status, body) = match dir {
        Some(dir) => ("200 OK", files::answer(&dir, &query, limit)),
        None => (
            "400 Bad Request",
            json!({"error": "no such workspace seat"}).to_string(),
        ),
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

/// Purpose: Answer a request whose exchange failed, so the page can stop the turn it started.
///
/// Serves: `websocket`, on a failed `Session::exchange`.
/// Design: `docs/design/web-composer.md` §5 — nothing may show a turn running whose
/// only ending was a line that will never arrive.
/// Up: the failure branch in `websocket`.
/// Down: the socket write; `serde_json` for the request's id.
/// Verify: `cargo test -p loong-web` (`a_transport_failure_is_an_event_not_a_hang`).
/// Contract: sends one JSON-RPC error carrying the request's own id. A line with no id
/// (a malformed request) is answered with a null id rather than not answered at all.
fn answer_the_failure(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    request: &str,
    message: &str,
) -> Result<(), String> {
    let id = serde_json::from_str::<serde_json::Value>(request)
        .ok()
        .and_then(|request| request.get("id").cloned())
        .unwrap_or(serde_json::Value::Null);
    let answer = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32603, "message": message}
    })
    .to_string();
    socket
        .send(Message::text(answer))
        .and_then(|_| socket.flush())
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
                        // A transport failure is an event like any other, so the turn
                        // carries a line saying why it did not run...
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
                        // ...and it is still an answer to the request, because the page
                        // settles that request on the line carrying its id: an event
                        // alone leaves the turn's spinner turning over a turn that
                        // nothing can end (web-composer.md §5).
                        answer_the_failure(&mut socket, text.as_str(), &e)?;
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
        dyn Fn(&std::path::Path, Option<&str>) -> Result<Started, String> + Send + Sync,
    >,
    models: Arc<dyn ModelSetup>,
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

    app::handle_ws(&mut socket, &state, &home, &*start_session, &*models)
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
    use loong_schema::generated as gen;
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

    /// Purpose: Pin that the words that streamed in raw are re-rendered before the turn
    /// closes — the fix for the transcript that showed markdown source: the final
    /// `message.appended` only re-renders while the draft is still open, so a close
    /// that drops the draft without rendering leaves the raw text on screen forever.
    #[test]
    fn a_closing_turn_renders_its_draft() {
        // The close itself renders: nothing may null the draft out from under it.
        let close = APP_JS
            .split("function closeTurn() {")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("closeTurn is the one place a turn ends");
        let render = close
            .find("finishDraft(")
            .expect("closeTurn must call finishDraft(), or a turn ends on raw markdown");
        let drop = close
            .find("state.draft = null")
            .expect("closeTurn clears the draft it leaves behind");
        assert!(
            render < drop,
            "the draft must be rendered before it is dropped: {close}"
        );
        // The finish renders what streamed: the streamed text, not the finished
        // message, is the fallback when the final event never arrives.
        let finish = APP_JS
            .split("function finishDraft(")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("finishDraft is the one place the draft is rendered");
        assert!(
            finish.contains("state.draftRaw"),
            "the fallback renders what streamed: {finish}"
        );
        assert!(
            finish.contains("renderWords("),
            "the draft is rendered, not left raw: {finish}"
        );
    }

    /// Purpose: Pin that a turn ends when the connection does — the socket's close and
    /// error handlers must fail every request in flight and stop the spinner, because a
    /// request that can never be answered leaves its promise open forever: the page then
    /// keeps a `Working…` spinner turning over a turn that already ended, and only a
    /// reload (which throws that state away) showed the truth.
    #[test]
    fn a_lost_connection_ends_the_turn() {
        let handler = APP_JS
            .split("function linkLost(")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("linkLost is the one place a dead wire is answered");
        // The requests first, or the turn's own code never learns its answer is gone.
        // The map is copied before it is emptied, so clearing cannot lose a request and
        // a rejecting handler cannot mutate what is being walked.
        let snapshot = handler
            .find("[...state.pending.values()]")
            .expect("the requests in flight are collected before the map is emptied");
        let clear = handler
            .find("state.pending.clear()")
            .expect("the map is emptied so a second close rejects nothing twice");
        let fail = handler
            .find("request.reject(")
            .expect("every request in flight is failed: their promises can never settle");
        assert!(
            snapshot < clear && clear < fail,
            "collect, then clear, then fail each one: {handler}"
        );
        assert!(
            handler.contains("for (const request of inFlight)"),
            "every collected request is failed, not just the first: {handler}"
        );
        // And the turn's own indicators with it.
        assert!(
            handler.contains("setThinking(false)"),
            "a dead wire stops the spinner: {handler}"
        );
        assert!(
            handler.contains("closeTurn()"),
            "a dead wire closes the turn it leaves open: {handler}"
        );
        // Both ways a socket dies take that path, so neither can be forgotten alone.
        for (event, message) in [
            ("ws.onclose", "服务器关闭了连接"),
            ("ws.onerror", "连不上服务器"),
        ] {
            let body = APP_JS
                .split(&format!("{event} = () => {{"))
                .nth(1)
                .and_then(|rest| rest.split("\n  };").next())
                .unwrap_or_else(|| panic!("{event} is handled"));
            assert!(
                body.contains("linkLost("),
                "{event} must end the turn through linkLost: {body}"
            );
            assert!(
                body.contains(message),
                "{event} still says in words what happened: {body}"
            );
        }
        // The close must not be gated on the very flag it clears: a turn asked to close
        // while `state.running` is still true is this turn ending, not the next starting.
        let close = APP_JS
            .split("function closeTurn() {")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("closeTurn is the one place a turn ends");
        assert!(
            !close.contains("state.running"),
            "closeTurn may not refuse to close a running turn: {close}"
        );
    }

    /// Purpose: Pin that the page only switches on event kinds the schema defines, and
    /// only reads fields those events carry — the page once handled `tool.called`,
    /// `decision.made` and `turn.completed`, none of which the protocol ever sends, so
    /// tool calls and policy decisions were silently dropped.
    #[test]
    fn the_page_speaks_the_schemas_event_vocabulary() {
        let kinds: Vec<&str> = gen::EVENT_KINDS.iter().map(|(kind, _)| *kind).collect();
        // The kinds the surface itself puts on the wire, beside the schema's: a bad
        // request is answered as an event so the page can show it in the transcript.
        let surface_local = ["client.error"];
        let handled: Vec<&str> = APP_JS
            .match_indices("case \"")
            .filter_map(|(at, marker)| {
                let rest = &APP_JS[at + marker.len()..];
                rest.find('"').map(|end| &rest[..end])
            })
            .collect();
        assert!(
            !handled.is_empty(),
            "the event switch was not found in app.js"
        );
        for kind in &handled {
            assert!(
                kinds.contains(kind) || surface_local.contains(kind),
                "app.js switches on `{kind}`, which is not an event kind — \
                 the schema's kinds are {kinds:?}"
            );
        }
        // The fields the client reads off each event, by kind. A name here that the
        // schema does not carry is a field the page would read as undefined.
        let fields: &[(&str, &[&str])] = &[
            ("tool.call", &["call_id", "tool", "args", "action_class"]),
            ("tool.result", &["call_id", "ok", "output"]),
            ("policy.decided", &["action_class", "verdict", "rule"]),
            (
                "approval.requested",
                &["request_id", "action_class", "reason"],
            ),
            ("message.appended", &["role", "text"]),
            (
                "compaction.applied",
                &["tokens_before", "tokens_after", "summary"],
            ),
            (
                "skill.invoked",
                &["name", "source", "args", "tokens", "error"],
            ),
            ("cost.recorded", &["tokens_in", "tokens_out", "usd_micros"]),
            ("permissions.relaxed", &["auto_approve"]),
        ];
        for (kind, names) in fields {
            for name in *names {
                assert!(
                    APP_JS.contains(&format!("data.{name}")),
                    "app.js never reads `data.{name}`, which `{kind}` carries"
                );
            }
        }
        // The names the page invented, gone for good: kinds checked as quoted strings,
        // fields as the `data.` reads they would be.
        for wrong in [
            "tool.called",
            "decision.made",
            "decision.synthesized",
            "tool.intercepted",
            "turn.started",
            "turn.completed",
            "model.switched",
            "session.resumed",
        ] {
            assert!(
                !APP_JS.contains(&format!("\"{wrong}\"")),
                "app.js still switches on `{wrong}`, which the protocol does not send"
            );
        }
        for wrong in [
            "data.cached_tokens",
            "data.context_window",
            "data.usd",
            "data.lines",
            "data.status",
            "data.command",
            "data.risk",
        ] {
            // `data.usd` must not match `data.usd_micros`, so the match has to end where
            // the identifier ends.
            let used = APP_JS.match_indices(wrong).any(|(at, found)| {
                !APP_JS[at + found.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
            });
            assert!(
                !used,
                "app.js still reads `{wrong}`, which no event carries"
            );
        }
    }

    /// Purpose: Pins that a question is drawn held and only a decision greys it, so a
    /// refresh can neither invent an answer nor lose a live question.
    #[test]
    fn an_approval_goes_grey_by_its_decision() {
        let approval = APP_JS
            .split("function showApproval(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("showApproval is the one place an approval renders");
        let signature = approval.lines().next().unwrap_or("");
        assert_eq!(
            format!("function showApproval({signature}"),
            "function showApproval(data, replayed) {",
            "showApproval takes the replay flag, because a replayed box is drawn held"
        );
        assert!(
            approval.contains("if (replayed) hold(box);")
                && approval.contains("else openBox(box);"),
            "a question out of the log is drawn out of reach; one the stream just delivered is \
             already the kernel's to answer: {approval}"
        );
        assert!(
            approval.contains("box.dataset.request = data.request_id"),
            "the box carries the id its decisions quote, so the transcript can be walked by it: \
             {approval}"
        );
        let decided = APP_JS
            .split("function decideBox(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("decideBox is the one place a decision is drawn");
        assert!(
            decided.contains("answered") && decided.contains("disabled = true"),
            "a decided question is a record and takes its buttons out of reach: {decided}"
        );
        let find = APP_JS
            .split("function decideQuestions(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("decideQuestions is how a decision finds its box");
        assert!(
            find.contains("#chat .approval") && find.contains("box.dataset.request === requestId"),
            "a decision finds its question in the transcript, by the id it answers: {find}"
        );
        let on = APP_JS
            .split("case \"approval.decided\": {")
            .nth(1)
            .and_then(|rest| rest.split("      case ").next())
            .expect("approval.decided is drawn in onEvent");
        assert!(
            on.contains("decideQuestions(data.request_id"),
            "the decision must settle the box it answers: {on}"
        );
        // And the turn holding a live question stays unfolded: a replayed turn folds shut
        // as a record, but folding the one with an answerable question would hide the
        // buttons a refresh has just made reachable.
        let close = APP_JS
            .split("function closeTurn() {")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("closeTurn is the one place a turn ends");
        assert!(
            close.contains(".approval:not(.answered):not(.expired)"),
            "a turn holding a live question must not fold shut: {close}"
        );
    }

    /// Purpose: Pins that only the kernel can make a question answerable — one pass after
    /// the log is drawn, `session.attach`'s `pending_approval`, read from the transcript.
    /// Without it the page guesses, and it guessed wrong in both directions: every
    /// replayed question clickable and doomed, then every replayed question greyed and a
    /// live one unanswerable.
    #[test]
    fn a_question_only_the_kernel_holds_can_be_answered() {
        let replay = APP_JS
            .split("async function replay(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("replay is where the log comes back into the page");
        assert!(
            replay.contains("session.attach"),
            "the kernel's word on what it waits for is `session.attach`'s \
             `pending_approval`, and it is a read: {replay}"
        );
        assert!(
            replay.contains("pending_approval"),
            "replay must ask the kernel which question is still live: {replay}"
        );
        assert!(
            replay.contains("if (known) retireQuestions(live);"),
            "no word from the kernel means no question is retired: a held box is already out \
             of reach, and a guess here is the bug: {replay}"
        );
        let ask = APP_JS
            .split("function retireQuestions(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("retireQuestions is what replay calls");
        assert!(
            ask.contains("#chat .approval") && ask.contains("box.dataset.request"),
            "the pass walks the transcript by id, so no second register can drift: {ask}"
        );
        assert!(
            ask.contains("openBox(box)"),
            "the question the kernel still waits for is the one made answerable: {ask}"
        );
        assert!(
            ask.contains("retireBox(box, RETIRED)"),
            "every other open question is put away: {ask}"
        );
        let retire = APP_JS
            .split("function retireBox(")
            .nth(1)
            .and_then(|rest| rest.split("\n}\n").next())
            .expect("retireBox is the one place a dead question is put away");
        assert!(
            retire.contains("expired") && retire.contains("disabled"),
            "a retired question is greyed out of reach: {retire}"
        );
        assert!(
            !retire.contains("answered"),
            "nobody answered it, so it must not be drawn answered: {retire}"
        );
        let why = APP_JS
            .split("const RETIRED = ")
            .nth(1)
            .and_then(|rest| rest.split(";\n").next())
            .expect("RETIRED is the one line a dead question carries");
        assert!(
            why.contains("重新发一次提问"),
            "the transcript says what to do instead, like the TUI's repl: {why}"
        );
        // An answer the kernel refuses is the same fact arriving by the other road: the
        // box becomes the record it always was.
        let answer = APP_JS
            .split("const answer = (granted, scope) => {")
            .nth(1)
            .and_then(|rest| rest.split("\n  };").next())
            .expect("answer is where a click leaves the page");
        assert!(
            answer.contains("no approval is pending") && answer.contains("retireBox(box, RETIRED)"),
            "a refused answer must show a record, not a decision that never happened: {answer}"
        );
        assert!(
            answer.contains("openBox(box)"),
            "any other failure leaves the question answerable, so the person can retry: {answer}"
        );
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

    /// Purpose: Pins the model bar as a row of slots with keys and the YOLO switch as a
    /// control that reads the session's own record — the banner once offered a dropdown,
    /// and a region's shape is what a later refactor silently undoes (web-surface.md §4).
    #[test]
    fn the_banner_offers_every_model_at_once() {
        assert!(
            INDEX.contains("id=\"model-slots\""),
            "the models are slots on one row, not a menu"
        );
        // The model list is a row, not a menu. A `<select>` elsewhere is fine — the
        // add-model panel picks a *provider* from one, which is a catalogue, not a switch.
        let bar = INDEX
            .split("id=\"modelbar\"")
            .nth(1)
            .and_then(|rest| rest.split("</nav>").next())
            .expect("the model bar is in the page");
        assert!(
            !bar.contains("<select"),
            "no dropdown in the bar: every model is on screen (web-surface.md §4)"
        );
        assert!(
            INDEX.contains("id=\"add-model\""),
            "the bar carries the add-model button, and `id=\"add-model-dialog\"` the panel"
        );
        assert!(
            INDEX.contains("id=\"no-models\""),
            "an install with no model gets the warning before it gets anything else (web-surface.md §6)"
        );
        assert!(
            !bar.contains("id=\"model-now\""),
            "the selected model is said once, by its lit slot: a chip repeating the name \
             beside the row says the same thing twice"
        );
        assert!(
            APP_JS.contains("aria-current") && STYLE_CSS.contains(".slot.current"),
            "the lit slot must answer in more than colour — `aria-current` and a bold face"
        );
        assert!(
            INDEX.contains("id=\"yolo\""),
            "the banner carries the session's approval switch"
        );
        for wanted in [
            "ctrlKey",
            "ArrowRight",
            "ArrowLeft",
            "model.select",
            "model.reorder",
            "permissions.set",
            "permissions.relaxed",
            "model.configured",
            "model.providers",
            "model.add",
        ] {
            assert!(
                APP_JS.contains(wanted),
                "app.js does not carry `{wanted}`, so the bar has no key or the switch no state"
            );
        }
        // The chords the browser leaves alone: the letters on Alt, the arrows on Ctrl.
        // Ctrl+N ("new window") and Ctrl+P ("print") are the browser's, and Alt+←/→ are its
        // back/forward, so a page that binds them either loses the key or steals the action.
        for wanted in ["alt+n", "alt+p", "ctrl+←"] {
            assert!(
                INDEX.contains(wanted),
                "the bar does not print `{wanted}`, so the chord is only discoverable by trying"
            );
        }
        // Two chords, two acts: the letters switch, the arrows move the current model one
        // slot. Getting these mixed up is the bug this pins — Ctrl+arrow is not a switch.
        let action = APP_JS
            .split("function modelActionForKey(")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("app.js reads the keys in modelActionForKey");
        let letters = action
            .find("act: \"switch\"")
            .expect("alt+n / alt+p switch");
        let arrows = action.find("act: \"order\"").expect("ctrl+arrows reorder");
        let alt_guard = action.find("if (e.altKey").expect("the letters are on Alt");
        let ctrl_guard = action
            .find("if (e.ctrlKey")
            .expect("the arrows are on Ctrl");
        assert!(
            alt_guard < letters && letters < ctrl_guard,
            "switching belongs to the Alt branch: a letter key inside the Ctrl branch would \
             take ctrl+n/ctrl+p from the browser"
        );
        assert!(
            ctrl_guard < arrows,
            "reordering belongs to the Ctrl branch, not to the letters"
        );
        assert!(
            APP_JS.contains("moveModel("),
            "the arrow chord must reach model.reorder, not stepModel: moving is not switching"
        );
        for wrong in ["ctrl+n", "ctrl+p", "alt+←", "alt+→"] {
            assert!(
                !INDEX.contains(wrong) && !APP_JS.contains(wrong),
                "`{wrong}` is bindable only by taking it from the browser"
            );
        }
        assert!(
            APP_JS.contains("e.altKey") && APP_JS.contains("e.ctrlKey"),
            "modelStepForKey must tell the two families apart, or one steals the other's keys"
        );
    }

    /// Purpose: Pins the banner's name and the inscription beside it: the wordmark is set
    /// in capitals, and the saying is the terminal's own — the two surfaces introduce
    /// themselves with the same words (web-surface.md §4, `surfaces.md` §3).
    #[test]
    fn the_banner_says_the_name_and_the_inscription() {
        assert!(
            INDEX.contains("<span class=\"wordmark\">LOONGAGENT</span>"),
            "the wordmark is a wordmark: capitals, not the command's spelling"
        );
        // The same words as `MARK_SUBLINE` in `crates/tui-app/src/lib.rs`: two halves and
        // the diamond between them. The runes are drawn as text, so a font that lacks them
        // shows boxes — but a page that drops them says half the saying.
        for half in ["兴云吐雾", "隐介藏形"] {
            assert!(
                INDEX.contains(half),
                "the inscription is missing `{half}`, so the banner says only one of the two"
            );
        }
        assert!(
            INDEX.contains("class=\"orn sep\">⋄"),
            "the halves are divided by the terminal's diamond"
        );
        assert!(
            STYLE_CSS.contains("#banner .mark .wordmark")
                && STYLE_CSS.contains("#banner .mark .subline"),
            "the name and the inscription are drawn by the banner's own rules"
        );
        assert!(
            STYLE_CSS.contains("#banner .mark .subline .orn"),
            "the runes take the frame's colour, not the text's: they frame the saying"
        );
    }

    /// Purpose: Pins the one thing that left the bar lighting nothing: the current model is
    /// `ModelList.selected`, not a field on each offering.
    /// Serves: the model bar's lit slot (web-surface.md §4).
    /// Design: `docs/design/web-surface.md` §4 and the schema's `ModelList`/`ModelOffering`.
    /// Up: `cargo test -p loong-web`.
    /// Down: `APP_JS`'s `markSelected`, checked against the wire shape the schema generates.
    /// Verify: `cargo test -p loong-web` (`the_model_bar_reads`).
    #[test]
    fn the_model_bar_reads_the_selected_name_the_schema_sends() {
        // The wire shape, built from the generated types rather than from a hand-written
        // object: a page that agrees with this test agrees with the server.
        let list = gen::ModelList {
            models: vec![gen::ModelOffering {
                name: "a".to_string(),
                model: "a-1".to_string(),
                provider: Some("fake".to_string()),
                context_window: Some(1000),
            }],
            selected: "a".to_string(),
        };
        let wire = serde_json::to_value(&list).expect("ModelList serialises");
        assert!(
            wire.get("selected").is_some(),
            "ModelList carries `selected`; the page must read it there"
        );
        let offering = wire["models"][0]
            .as_object()
            .expect("an offering is an object");
        assert!(
            !offering.contains_key("active"),
            "an offering carries no `active`: which one is current is `ModelList.selected`"
        );
        let keys: Vec<&str> = offering.keys().map(String::as_str).collect();

        // The page derives it, from that field, in one place.
        let derive = APP_JS
            .split("function markSelected(")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("app.js derives the current model in markSelected");
        assert!(
            derive.contains("list.selected"),
            "markSelected must read `list.selected`, the field the schema sends"
        );
        assert!(
            derive.contains("m.active ="),
            "markSelected must write the derived `active` back onto each offering"
        );

        // And it is the *only* way `state.models` is filled from the wire: a bar that
        // stores the offerings raw has nothing to light, whatever the derivation says.
        let apply = APP_JS
            .split("function applyModelList(")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("app.js folds a ModelList in applyModelList");
        assert!(
            apply.contains("markSelected(list)"),
            "applyModelList must go through markSelected, or the row lights nothing"
        );

        // And it reads nothing off an offering that the schema does not send. `active` is
        // the derived flag; everything else has to be a field of `ModelOffering`.
        let bar = APP_JS
            .split("function renderModels(")
            .nth(1)
            .and_then(|rest| rest.split("function stepModel(").next())
            .expect("app.js draws the bar in renderModels");
        let mut reads: Vec<&str> = bar
            .match_indices("m.")
            .filter_map(|(at, found)| {
                // Only a bare `m.` counts, so `dom.`-prefixed handles never match.
                let before = bar[..at].chars().next_back();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                    return None;
                }
                let rest = &bar[at + found.len()..];
                let end = rest
                    .find(|c: char| !c.is_alphanumeric() && c != '_')
                    .unwrap_or(rest.len());
                (end > 0).then(|| &rest[..end])
            })
            .collect();
        reads.sort_unstable();
        reads.dedup();
        for name in reads {
            assert!(
                keys.contains(&name) || name == "active",
                "the bar reads `m.{name}`, which `ModelOffering` does not carry (it has {keys:?})"
            );
        }
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

    /// Purpose: Pins that a broken exchange is surfaced to the page as an event, not a hang,
    /// and that the request still gets an answer: the page's promise settles only on the
    /// answer carrying its id, so an event alone left the turn's spinner turning forever
    /// over a turn nothing could end.
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
        // The answer to the request itself: without it the page waits forever.
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(
            answer.contains("\"id\":1"),
            "the failure answers the request: {answer}"
        );
        assert!(
            answer.contains("\"error\""),
            "as an error, so the page can stop the turn: {answer}"
        );
        assert!(answer.contains("the child died"), "{answer}");
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
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
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

    /// Purpose: Pins the `@` picker's route: token-gated, seat-addressed, ranked, and closed to an unknown seat.
    /// Serves: the composer's file list (`web-composer.md` §2).
    /// Design: `docs/design/web-composer.md` §2.
    /// Up: `cargo test -p loong-web`.
    /// Down: `serve_web`, `handle_web` and `files::answer`.
    /// Verify: `cargo test -p loong-web`.
    #[test]
    fn the_file_route_ranks_the_typed_query_and_refuses_an_unknown_seat() {
        // A workspace of its own: seat 0 is the home directory the surface is opened on.
        let home = std::env::temp_dir().join(format!("loong-web-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(home.join("src")).unwrap();
        std::fs::write(home.join("src/main.rs"), b"fn main() {}").unwrap();
        std::fs::write(home.join("docs/notes.md"), b"x").ok();

        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = random_token().unwrap();
        let for_server = token.clone();
        let served_dir = home.clone();
        std::thread::spawn(move || {
            serve_web(
                listener,
                for_server,
                served_dir,
                Arc::new(|_: &std::path::Path, session: Option<&str>| {
                    let id = session.unwrap_or("echo").to_string();
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
                }),
            )
        });

        /// Purpose: Run one GET and return the whole response.
        /// Contract: the connection is closed by the server, so reading to the end is the body.
        fn get(addr: SocketAddr, request: &str) -> String {
            let mut stream = TcpStream::connect(addr).unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            let mut body = String::new();
            stream.read_to_string(&mut body).unwrap();
            body
        }

        let refused = get(
            addr,
            "GET /api/files?seat=0&q=main HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n",
        );
        assert!(refused.starts_with("HTTP/1.1 401"), "no token: {refused}");

        let unknown = get(
            addr,
            &format!(
                "GET /api/files?token={token}&seat=9&q=main HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
            ),
        );
        assert!(
            unknown.starts_with("HTTP/1.1 400"),
            "a page must not name a directory by seat it does not have: {unknown}"
        );

        // Seat 0 is a workspace the page opened, as the real page does before any
        // session exists: the seat is resolved on the server, never named by the page.
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc": "2.0", "id": 1, "method": "workspace.add",
                       "params": {"dir": home.to_string_lossy()}})
                .to_string(),
            ))
            .unwrap();
        loop {
            let line = socket.read().unwrap().into_text().unwrap().to_string();
            let parsed: Value = serde_json::from_str(&line).unwrap();
            if parsed.get("id") == Some(&json!(1)) {
                break;
            }
        }

        let ranked = get(
            addr,
            &format!(
                "GET /api/files?token={token}&seat=0&q=main HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
            ),
        );
        std::fs::remove_dir_all(&home).unwrap();

        assert!(ranked.starts_with("HTTP/1.1 200 OK"), "{ranked}");
        assert!(ranked.contains("\"src/main.rs\""), "{ranked}");
    }

    /// Purpose: Pins the composer's `@` picker: the markup, the route it asks, and the word-boundary rule.
    /// Serves: typing `@` to name a workspace file (`web-composer.md` §2).
    /// Design: `docs/design/web-composer.md` §2.
    /// Up: `cargo test -p loong-web`.
    /// Down: `INDEX`, `APP_JS`, `STYLE_CSS` and `files::answer`'s route.
    /// Verify: `cargo test -p loong-web` (`the_composer_names`).
    #[test]
    fn the_composer_names_workspace_files_with_at() {
        assert!(
            INDEX.contains("id=\"file-picker\""),
            "the composer needs a place to draw the `@` list"
        );
        assert!(
            STYLE_CSS.contains("#file-picker"),
            "the list must be drawn somewhere, and above the input it would cover it"
        );
        assert!(
            APP_JS.contains("/api/files?token="),
            "the picker asks the route that holds the workspace's files"
        );
        // The seat, not a path: a page must not be able to list a directory of its
        // own choosing through this route.
        assert!(
            APP_JS.contains("seat=${state.seat}"),
            "the picker names the workspace it sits on, and the server resolves it"
        );
        // `@` opens the list only at a word boundary, so `a@b` stays a plain address.
        let rule = APP_JS
            .split("function mentionAtCaret(")
            .nth(1)
            .and_then(|rest| rest.split("\n}").next())
            .expect("mentionAtCaret is the one place the mention is read");
        assert!(
            rule.contains("boundary"),
            "a mention begins at a word boundary: {rule}"
        );
        assert!(
            rule.contains("/\\s/"),
            "the word before `@` must be whitespace or the start of the message: {rule}"
        );
        // Choosing a row inserts the path and closes the list; it does not send.
        assert!(
            APP_JS.contains("function takeFile(") && APP_JS.contains("closeFilePicker();"),
            "taking a row leaves `@path ` in the message and closes the list"
        );
    }
}
