//! Purpose: The web surface's own half: workspaces, the session catalogue, and the tab bridge.
//!
//! # Contract
//! - **Serves:** `crate::serve_web` hands every upgraded socket to `handle_ws`.
//! - **Design:** `docs/design/web-surface.md` §1–§3.
//! - **Upstream:** `crates/cli/src/web.rs` passes the launch directory and a session starter in.
//! - **Downstream:** `crate::sessions` (the catalogue), and the `Session` the starter builds.
//! - **Verify:** `cargo test -p loong-web`
//!
//! The surface the session protocol does not cover. The chat socket itself is
//! what it always was — one WebSocket bridging JSON-RPC lines to a session
//! bridge — but a browser workspace has two concerns a terminal never had: it
//! can sit on several directories (one session bridge each), and it can list,
//! open and delete sessions in them. Those two are this module. Everything
//! else — the models, the prompts, the approvals — is forwarded untouched to
//! the bridge of the tab's current workspace, and the bridge's lines stream
//! back down.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tungstenite::Message;

use crate::sessions;
use crate::Session;

/// Purpose: The surface-side error a tab receives as a JSON-RPC error object.
/// Serves: the `web.*`-style surface methods and the workspace/session guards.
/// Design: ADR-0002 — one error shape for every method.
/// Up: `handle_ws`'s dispatch.
/// Down: the `json!` error object it renders to.
/// Verify: `cargo test -p loong-web`.
#[derive(Debug)]
pub struct Error(pub String);

impl From<String> for Error {
    fn from(message: String) -> Self {
        Error(message)
    }
}

/// Purpose: What one tab asks the surface itself to do, before anything touches a session bridge.
/// Serves: `dispatch_surface`, the router between surface methods and forwarded ones.
/// Design: `docs/design/web-surface.md` §1–§2.
/// Up: `handle_ws`.
/// Down: every variant's handler.
/// Verify: `cargo test -p loong-web` (`dispatch_`).
pub enum SurfaceCall {
    /// `workspace.list` — answered on the socket; the variant carries the seat the tab got.
    Workspaces,
    /// `workspace.add` — the directory that was added and seated.
    AddWorkspace(String),
    /// `workspace.switch` — the workspace index the tab moved to.
    SwitchWorkspace(usize),
    /// `workspace.close` — the workspace index that was dropped.
    CloseWorkspace(usize),
    /// `session.list` — the catalogue was answered on the socket.
    Sessions,
    /// `session.delete` — the session id that was removed.
    DeleteSession(String),
    /// anything else: straight to the workspace's session bridge.
    Forward,
}

/// Purpose: One live session child: its bridge and the prompt it is running, if any.
/// Serves: `Workspace`.
/// Design: `docs/design/web-surface.md` §3 — one child per session, busy per child.
/// Up: `Workspace::children`.
/// Down: the `Session` the starter built for it.
/// Verify: `cargo test -p loong-web`.
pub struct Child {
    /// the bridge to this session's server child
    pub session: Arc<Mutex<dyn Session>>,
    /// the request id of a prompt in flight on this child, if one is running
    pub busy: Option<u64>,
}

/// Purpose: One workspace: its directory and the live session children running there.
/// Serves: `State`.
/// Design: `docs/design/web-surface.md` §1, §3 — one child per (workspace, session).
/// Up: `State::workspaces`.
/// Down: `Child`.
/// Verify: `cargo test -p loong-web` (`workspace_`, `two_sessions_`).
pub struct Workspace {
    /// the canonical directory the workspace's children run in
    pub dir: PathBuf,
    /// the live children, keyed by the session id each is bound to; empty until first needed
    pub children: std::collections::HashMap<String, Child>,
}

/// Purpose: What the surface remembers between requests: the workspaces, in the order opened.
/// Serves: `crate::serve_web`, which shares one across every tab.
/// Design: `docs/design/web-surface.md` §1.
/// Up: `serve_web`.
/// Down: `Workspace`.
/// Verify: `cargo test -p loong-web`.
pub struct State {
    /// the workspaces this surface has opened; index 0 is the home directory
    pub workspaces: Vec<Workspace>,
    /// whether the persisted catalogue has been read back yet this run
    restored: bool,
}

/// Purpose: What one tab is attached to: a workspace seat and the session it is on.
/// Serves: `handle_ws`'s loop.
/// Design: `docs/design/web-surface.md` §3 — a tab's active session is its attachment.
/// Up: `handle_ws`.
/// Down: `Workspace::children` via the `attached` id.
/// Verify: `cargo test -p loong-web`.
#[derive(Default)]
pub struct Tab {
    /// the workspace index the tab is seated in; defaults to the home workspace
    pub seat: usize,
    /// the session id the tab is attached to; `None` until it opens or creates one
    pub attached: Option<String>,
}

impl State {
    /// Purpose: An empty surface; the first `workspace.list` plants the home workspace.
    /// Contract: `home` is canonicalised by the caller (`crates/cli/src/web.rs`).
    pub fn new() -> Self {
        State {
            workspaces: Vec::new(),
            restored: false,
        }
    }
    /// Purpose: The index of the workspace for `dir`, adding it when unknown.
    /// Serves: the `workspace.list` and `workspace.add` calls.
    /// Design: `docs/design/web-surface.md` §1 — the list is never destroyed by switching,
    /// and many workspaces may be open at once.
    /// Contract: paths are canonicalised; a known dir returns its index.
    pub fn ensure(&mut self, dir: &Path) -> Result<usize, Error> {
        let dir = std::fs::canonicalize(dir)
            .map_err(|_| Error(format!("no such directory: {}", dir.display())))?;
        if let Some(i) = self.workspaces.iter().position(|w| w.dir == dir) {
            return Ok(i);
        }
        self.workspaces.push(Workspace {
            dir,
            children: std::collections::HashMap::new(),
        });
        Ok(self.workspaces.len() - 1)
    }

    /// Purpose: Remove a workspace that is not the home one.
    /// Contract: index 0 is the home workspace and stays; a live bridge is dropped with it.
    pub fn close(&mut self, index: usize) -> Result<(), Error> {
        if index == 0 {
            return Err(Error("the home workspace stays open".to_string()));
        }
        if index >= self.workspaces.len() {
            return Err(Error(format!("no workspace {index}")));
        }
        self.workspaces.remove(index);
        Ok(())
    }
}

/// Purpose: The workspace catalogue file: every non-home workspace directory, in order.
/// Serves: `persist_workspaces` and `restore_workspaces`.
/// Design: `docs/design/web-surface.md` §1 — workspaces are many and remembered across restarts.
/// Contract: lives at `$LOONGAGENT_HOME/workspaces.json`; missing or unreadable means "none yet".
fn workspaces_file() -> PathBuf {
    sessions::client_root().join("workspaces.json")
}

/// Purpose: Write the surface's workspaces to disk, skipping home (index 0), so a restart
/// brings them back.
/// Serves: `workspace.add` and `workspace.close`.
/// Design: `docs/design/web-surface.md` §1.
/// Contract: best-effort and atomic enough for a catalogue; a write failure loses the edit,
/// never the live state.
fn persist_workspaces(st: &State) -> Result<(), Error> {
    let dirs: Vec<&Path> = st
        .workspaces
        .iter()
        .skip(1)
        .map(|w| w.dir.as_path())
        .collect();
    let body = serde_json::to_string_pretty(&dirs).map_err(|e| Error(e.to_string()))?;
    let file = workspaces_file();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error(e.to_string()))?;
    }
    std::fs::write(&file, body).map_err(|e| Error(e.to_string()))
}

/// Purpose: Plant home at index 0, then re-add every workspace the file remembers.
/// Serves: the first `workspace.list`/`workspace.add` of a fresh surface.
/// Design: `docs/design/web-surface.md` §1 — the catalogue survives a restart.
/// Contract: idempotent — a second call is a no-op; a missing or unreadable file is fine.
fn restore_workspaces(st: &mut State, home: &Path) -> Result<(), Error> {
    if st.restored {
        return Ok(());
    }
    st.restored = true;
    st.ensure(home)?;
    let file = workspaces_file();
    let body = match std::fs::read_to_string(&file) {
        Ok(body) => body,
        Err(_) => return Ok(()),
    };
    let dirs: Vec<String> = match serde_json::from_str(&body) {
        Ok(dirs) => dirs,
        Err(_) => return Ok(()),
    };
    for dir in dirs {
        // A directory that vanished since the last run is simply not restored.
        let _ = st.ensure(Path::new(&dir));
    }
    Ok(())
}

/// Purpose: Serve one browser tab until it closes: dispatch its messages to the surface or
/// forward them down its workspace's session bridge.
/// Serves: `crate::websocket_web`.
/// Design: `docs/design/web-surface.md` §3.
/// Up: `websocket_web`.
/// Down: `dispatch_surface` for the surface's own methods, `forward` for the rest.
/// Verify: `cargo test -p loong-web` and `cargo test -p loongagent --test web_surface`.
pub fn handle_ws(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    state: &Arc<Mutex<State>>,
    home: &Path,
    start_session: &dyn Fn(
        &Path,
        Option<&str>,
    ) -> Result<(Arc<Mutex<dyn Session>>, String), String>,
) -> Result<(), String> {
    let mut tab = Tab::default();
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
                let line = text.as_str();
                let request: Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(e) => {
                        send_error(socket, Value::Null, &format!("not JSON-RPC: {e}"))?;
                        continue;
                    }
                };
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                let method = request
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match dispatch_surface(
                    state,
                    &mut tab,
                    home,
                    start_session,
                    &method,
                    &request,
                    socket,
                ) {
                    Ok(SurfaceCall::Forward) => {
                        if let Err(e) = forward(
                            state,
                            &mut tab,
                            home,
                            start_session,
                            socket,
                            line,
                            &method,
                            id.clone(),
                            request.get("params").unwrap_or(&Value::Null),
                        ) {
                            send_error(socket, id, &e)?;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => send_error(socket, id, &e.0)?,
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

/// Purpose: Route one request to the surface's own methods, answering it on the socket, or
/// say "forward".
/// Serves: `handle_ws`.
/// Design: `docs/design/web-surface.md` §1–§2.
/// Up: `handle_ws`.
/// Down: `State`, `sessions::{list, known, delete}` and the workspace guards.
/// Verify: `cargo test -p loong-web` (`dispatch_`).
#[allow(clippy::too_many_arguments)]
fn dispatch_surface(
    state: &Arc<Mutex<State>>,
    tab: &mut Tab,
    home: &Path,
    start_session: &dyn Fn(
        &Path,
        Option<&str>,
    ) -> Result<(Arc<Mutex<dyn Session>>, String), String>,
    method: &str,
    request: &Value,
    socket: &mut tungstenite::WebSocket<TcpStream>,
) -> Result<SurfaceCall, Error> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "workspace.list" => {
            let list = {
                let mut st = lock(state)?;
                restore_workspaces(&mut st, home)?;
                st.workspaces
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        json!({
                            "index": i,
                            "dir": w.dir,
                            "running": !w.children.is_empty(),
                            "busy": w.children.values().any(|c| c.busy.is_some()),
                            "home": i == 0,
                        })
                    })
                    .collect::<Vec<_>>()
            };
            reply(socket, id, json!({"workspaces": list, "seat": tab.seat}))?;
            Ok(SurfaceCall::Workspaces)
        }
        "workspace.add" => {
            let dir = params
                .get("dir")
                .and_then(Value::as_str)
                .ok_or_else(|| Error("workspace.add needs a dir".to_string()))?;
            let index = {
                let mut st = lock(state)?;
                restore_workspaces(&mut st, home)?;
                let index = st.ensure(Path::new(dir))?;
                persist_workspaces(&st)?;
                index
            };
            tab.seat = index;
            tab.attached = None;
            reply(socket, id, json!({"index": index}))?;
            Ok(SurfaceCall::AddWorkspace(dir.to_string()))
        }
        "workspace.switch" => {
            let index = params
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error("workspace.switch needs an index".to_string()))?
                as usize;
            {
                let st = lock(state)?;
                if index >= st.workspaces.len() {
                    return Err(Error(format!("no workspace {index}")));
                }
            }
            if tab.seat != index {
                tab.seat = index;
                tab.attached = None;
            }
            reply(socket, id, json!({"index": index}))?;
            Ok(SurfaceCall::SwitchWorkspace(index))
        }
        "workspace.close" => {
            let index = params
                .get("index")
                .and_then(Value::as_u64)
                .ok_or_else(|| Error("workspace.close needs an index".to_string()))?
                as usize;
            {
                let mut st = lock(state)?;
                restore_workspaces(&mut st, home)?;
                st.close(index)?;
                persist_workspaces(&st)?;
            }
            if tab.seat == index {
                tab.seat = 0;
                tab.attached = None;
            }
            reply(socket, id, json!({"closed": index}))?;
            Ok(SurfaceCall::CloseWorkspace(index))
        }
        "session.tree" => {
            // The two-level rail in one call: every workspace with its sessions, each
            // session flagged live/busy from the children map (web-surface.md §2).
            let tree = {
                let mut st = lock(state)?;
                restore_workspaces(&mut st, home)?;
                st.workspaces
                    .iter()
                    .enumerate()
                    .map(|(i, w)| {
                        let root = sessions::client_root();
                        let mut sessions = sessions::list(&root, &w.dir);
                        for entry in &mut sessions {
                            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                                let live = w.children.get(id);
                                entry["running"] = json!(live.is_some());
                                entry["busy"] = json!(live.and_then(|c| c.busy).is_some());
                            }
                        }
                        json!({
                            "index": i,
                            "dir": w.dir,
                            "name": w.dir.file_name().map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| w.dir.display().to_string()),
                            "home": i == 0,
                            "sessions": sessions,
                        })
                    })
                    .collect::<Vec<_>>()
            };
            reply(socket, id, json!({"workspaces": tree, "seat": tab.seat}))?;
            Ok(SurfaceCall::Sessions)
        }
        "session.list" => {
            let dir = seat_dir(state, tab.seat)?;
            let entries = sessions::list(&sessions::client_root(), &dir);
            reply(socket, id, json!({"sessions": entries}))?;
            Ok(SurfaceCall::Sessions)
        }
        "session.open" => {
            // `open` names the session to move to; an optional `workspace` index moves the
            // tab to that workspace first (the two-level rail lets a tab open a session in
            // a workspace it is not seated in). The surface binds a child to that session
            // (starting one if none is live) and attaches the tab to it; it never forwards
            // a bare session.open across a shared child, which is what produced
            // `unknown session ""` against a child holding "default" (web-surface.md §2, §3).
            let open = params
                .get("open")
                .and_then(Value::as_str)
                .ok_or_else(|| Error("session.open needs an open id".to_string()))?;
            if let Some(index) = params.get("workspace").and_then(Value::as_u64) {
                let index = index as usize;
                {
                    let st = lock(state)?;
                    if index >= st.workspaces.len() {
                        return Err(Error(format!("no workspace {index}")));
                    }
                }
                tab.seat = index;
            }
            let dir = seat_dir(state, tab.seat)?;
            sessions::known(&sessions::client_root(), &dir, open)?;
            let session_id = ensure_child(state, tab.seat, home, Some(open), start_session)?;
            tab.attached = Some(session_id.clone());
            reply(
                socket,
                id,
                json!({
                    "session_id": session_id,
                    "session": {"id": session_id, "title": Value::Null},
                    "attached": true,
                    "events": [],
                }),
            )?;
            Ok(SurfaceCall::Sessions)
        }
        "session.new" => {
            // A fresh child is started for a freshly minted id; the factory returns
            // that id, so the tab attaches to the child's real session without
            // asking the child (web-surface.md §2).
            let session_id = ensure_child(state, tab.seat, home, None, start_session)?;
            tab.attached = Some(session_id.clone());
            reply(
                socket,
                id,
                json!({
                    "session_id": session_id,
                    "session": {"id": session_id, "title": Value::Null},
                    "attached": true,
                    "events": [],
                }),
            )?;
            Ok(SurfaceCall::Sessions)
        }
        "session.delete" => {
            let session_id = params
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error("session.delete needs a session_id".to_string()))?;
            let dir = seat_dir(state, tab.seat)?;
            sessions::delete(&sessions::client_root(), &dir, session_id)?;
            // Drop the deleted session's child, if one was live, and detach any tab on it.
            {
                let mut st = lock(state)?;
                if let Some(w) = st.workspaces.get_mut(tab.seat) {
                    w.children.remove(session_id);
                }
            }
            if tab.attached.as_deref() == Some(session_id) {
                tab.attached = None;
            }
            reply(socket, id, json!({"deleted": session_id}))?;
            Ok(SurfaceCall::DeleteSession(session_id.to_string()))
        }
        _ => Ok(SurfaceCall::Forward),
    }
}

/// Purpose: Carry one line down the tab's workspace session and stream every answer line back.
/// Serves: `handle_ws`'s forwarding half — the browser speaks exactly what the TUI speaks.
/// Design: `docs/design/web-surface.md` §3.
/// Up: `handle_ws`.
/// Down: `Workspace.session`'s `exchange`.
/// Verify: `cargo test -p loong-web` (`forward_`).
#[allow(clippy::too_many_arguments)]
fn forward(
    state: &Arc<Mutex<State>>,
    tab: &mut Tab,
    home: &Path,
    start_session: &dyn Fn(
        &Path,
        Option<&str>,
    ) -> Result<(Arc<Mutex<dyn Session>>, String), String>,
    socket: &mut tungstenite::WebSocket<TcpStream>,
    line: &str,
    method: &str,
    id: Value,
    params: &Value,
) -> Result<(), String> {
    // Which session is this request for? An explicit `session_id` wins (a tab may
    // address a session it has open without making it the active one); otherwise
    // the tab's attachment. A tab with neither can still `session.attach` (the
    // page's boot handshake) or `session.new`: both stand up a fresh child and
    // attach the tab to it. Anything else with no session is a readable error.
    let requested = params
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .or_else(|| tab.attached.clone());
    let child_id = match requested {
        Some(session_id) => ensure_child(state, tab.seat, home, Some(&session_id), start_session)
            .map_err(|e| e.0)?,
        None if method == "session.attach" || method == "session.new" => {
            ensure_child(state, tab.seat, home, None, start_session).map_err(|e| e.0)?
        }
        None => {
            return Err("no session yet: open or create one in the session rail first".to_string())
        }
    };
    tab.attached = Some(child_id.clone());
    let session = {
        let st = state
            .lock()
            .map_err(|_| "the state is poisoned".to_string())?;
        st.workspaces
            .get(tab.seat)
            .and_then(|w| w.children.get(&child_id))
            .map(|c| c.session.clone())
            .ok_or_else(|| "the session has no live child".to_string())?
    };
    // A prompt keeps this child busy until its response comes back: the page
    // renders that as "running" and holds further prompts (web-surface.md §3).
    if method == "prompt.submit" {
        if let Some(rid) = id.as_u64() {
            if let Ok(mut st) = state.lock() {
                if let Some(c) = st
                    .workspaces
                    .get_mut(tab.seat)
                    .and_then(|w| w.children.get_mut(&child_id))
                {
                    c.busy = Some(rid);
                }
            }
        }
    }
    // Held for the whole exchange: one request at a time reaches the session,
    // while each line goes out the moment it exists.
    let mut failure = None;
    {
        let socket_ref = &mut *socket;
        let outcome = match session.lock() {
            Ok(mut session) => session.exchange(line, &mut |answer| {
                socket_ref
                    .send(Message::text(answer))
                    .and_then(|_| socket_ref.flush())
                    .map_err(|e| format!("sending to the browser: {e}"))
            }),
            Err(_) => Err("the session is poisoned".to_string()),
        };
        if let Err(e) = outcome {
            failure = Some(e);
        }
    }
    if method == "prompt.submit" {
        if let Ok(mut st) = state.lock() {
            if let Some(c) = st
                .workspaces
                .get_mut(tab.seat)
                .and_then(|w| w.children.get_mut(&child_id))
            {
                c.busy = None;
            }
        }
    }
    match failure {
        None => Ok(()),
        Some(e) => {
            // A transport failure is an event like any other: the page shows it
            // instead of freezing with a dead socket.
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
            Ok(())
        }
    }
}

/// Purpose: Make sure the workspace's session `session` has a live child, starting one if
/// not, and return the id the child is bound to.
/// Serves: `session.open`, `session.new` and `forward`.
/// Design: `docs/design/web-surface.md` §3 — one child per (workspace, session), on demand.
/// Contract: after `Ok`, `workspaces[seat].children[id]` is live; `Some(id)` reuses a live
/// child for that id, `None` starts a fresh one and returns its minted id.
fn ensure_child(
    state: &Arc<Mutex<State>>,
    seat: usize,
    home: &Path,
    session: Option<&str>,
    start_session: &dyn Fn(
        &Path,
        Option<&str>,
    ) -> Result<(Arc<Mutex<dyn Session>>, String), String>,
) -> Result<String, Error> {
    let mut st = lock(state)?;
    if !st.restored {
        restore_workspaces(&mut st, home)?;
    }
    let dir = st
        .workspaces
        .get(seat)
        .map(|w| w.dir.clone())
        .ok_or_else(|| Error(format!("no workspace {seat}")))?;
    // A live child for the named session is reused; only a fresh start (`None`) or a
    // session with no child yet spawns one.
    if let Some(id) = session {
        if st.workspaces[seat].children.contains_key(id) {
            return Ok(id.to_string());
        }
    }
    let (child, id) = start_session(&dir, session).map_err(Error)?;
    st.workspaces[seat].children.insert(
        id.clone(),
        Child {
            session: child,
            busy: None,
        },
    );
    Ok(id)
}

/// Purpose: The directory of the tab's seat, planting home when nothing was chosen yet.
/// Contract: a seat never outlives its workspace; `workspace.close` reseats the tab first.
fn seat_dir(state: &Arc<Mutex<State>>, seat: usize) -> Result<PathBuf, Error> {
    let st = lock(state)?;
    st.workspaces
        .get(seat)
        .map(|w| w.dir.clone())
        .ok_or_else(|| Error("no workspace yet: call workspace.list first".to_string()))
}

/// Purpose: One JSON-RPC result down the tab's socket, answering a surface method.
/// Contract: the `id` echoes the request's; the result is the method's payload.
fn reply(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    id: Value,
    result: Value,
) -> Result<(), Error> {
    let body = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
    socket
        .send(Message::text(body))
        .and_then(|_| socket.flush())
        .map_err(|e| Error(e.to_string()))
}

/// Purpose: One JSON-RPC error object down the tab's socket.
/// Contract: a surface refusal is an error, never a dropped message.
fn send_error(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    id: Value,
    message: &str,
) -> Result<(), String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32602, "message": message}
    })
    .to_string();
    socket
        .send(Message::text(body))
        .and_then(|_| socket.flush())
        .map_err(|e| e.to_string())
}

/// Purpose: Lock the shared state or fail the request cleanly.
/// Contract: a poisoned lock is an error message, never a panic in a handler thread.
fn lock(state: &Arc<Mutex<State>>) -> Result<std::sync::MutexGuard<'_, State>, Error> {
    state
        .lock()
        .map_err(|_| Error("the state is poisoned".to_string()))
}

#[cfg(test)]
mod tests {
    //! Purpose: The surface half's gates: workspace bookkeeping, dispatch answers, and the
    //! forwarding bridge, all over a scripted `Echo` session.
    //!
    //! # Contract
    //! - **Serves:** `cargo test -p loong-web`.
    //! - **Design:** `docs/design/web-surface.md` §1–§3.
    //! - **Upstream:** `scripts/check.sh`.
    //! - **Downstream:** `crate::app::{State, dispatch_surface, forward}` and `crate::Echo`.
    //! - **Verify:** `cargo test -p loong-web`.

    use super::*;
    use crate::{bind, serve_web, Echo};
    use std::io::{Read, Write};
    use tungstenite::client::IntoClientRequest;

    /// Purpose: serialize the tests that set the process-wide `LOONGAGENT_HOME`, so two
    /// of them never race the shared variable.
    /// Contract: take the lock before `set_var("LOONGAGENT_HOME", …)`, hold it to the end.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Purpose: a scripted session factory for the dispatch tests; the id the surface
    /// asked for is handed straight back as the child's id.
    fn echo_factory(
    ) -> impl Fn(&Path, Option<&str>) -> Result<(Arc<Mutex<dyn Session>>, String), String> {
        |_: &Path, session: Option<&str>| {
            let id = session.unwrap_or("echo").to_string();
            Ok((
                Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                id,
            ))
        }
    }

    /// Purpose: a session factory that records the session id each child was started
    /// for, so the tests can assert two sessions become two children (web-surface.md §3).
    fn recording_factory() -> (
        impl Fn(&Path, Option<&str>) -> Result<(Arc<Mutex<dyn Session>>, String), String>,
        Arc<Mutex<Vec<Option<String>>>>,
    ) {
        let seen: Arc<Mutex<Vec<Option<String>>>> = Arc::new(Mutex::new(Vec::new()));
        let for_factory = Arc::clone(&seen);
        let factory = move |_: &Path, session: Option<&str>| {
            for_factory
                .lock()
                .unwrap()
                .push(session.map(|s| s.to_string()));
            let id = session.unwrap_or("echo").to_string();
            Ok((
                Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                id,
            ))
        };
        (factory, seen)
    }

    /// Purpose: a full web surface on loopback with an echo bridge, for the ws tests.
    fn start() -> (std::net::SocketAddr, String) {
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = crate::random_token().unwrap();
        let for_server = token.clone();
        let home = std::env::temp_dir();
        std::thread::spawn(move || {
            serve_web(
                listener,
                for_server,
                home,
                Arc::new(|_: &Path, session: Option<&str>| {
                    let id = session.unwrap_or("echo").to_string();
                    Ok((
                        Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        id,
                    ))
                }),
            )
        });
        (addr, token)
    }

    /// Purpose: home is planted on the first `workspace.list`, and the tab is seated there.
    #[test]
    fn dispatch_workspace_list_plants_home() {
        let _guard = HOME_LOCK.lock().unwrap();
        // Isolate the catalogue so a developer's real workspaces.json is not restored here.
        let base = std::env::temp_dir().join(format!("loong-plant-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        std::fs::create_dir_all(&home_dir).unwrap();
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        let (addr, token) = start();
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.list","params":{}}).to_string(),
            ))
            .unwrap();
        let line = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&line).unwrap();
        let result = value.get("result").expect("a result, not an error");
        assert_eq!(result.get("seat").and_then(Value::as_u64), Some(0));
        let workspaces = result.get("workspaces").and_then(Value::as_array).unwrap();
        assert_eq!(workspaces.len(), 1, "{workspaces:?}");
        assert_eq!(
            workspaces[0].get("home").and_then(Value::as_bool),
            Some(true)
        );

        std::env::remove_var("LOONGAGENT_HOME");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Purpose: a forwarded request rides the workspace's bridge and comes back with its events.
    #[test]
    fn forward_rides_the_workspace_bridge() {
        let (addr, token) = start();
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        // Seat the tab first.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.list","params":{}}).to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();
        // Then speak exactly what the TUI speaks.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":7,"method":"prompt.submit","params":{"session_id":"s","text":"hi"}}).to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(answer.contains("\"id\":7"), "{answer}");
        assert!(answer.contains("accepted"), "{answer}");
    }

    /// Purpose: the surface lets many workspaces be open at once; there is no cap.
    #[test]
    fn many_workspaces_can_be_open() {
        let _guard = HOME_LOCK.lock().unwrap();
        // Isolate the catalogue: a private client root so the persistence file is clean.
        let base = std::env::temp_dir().join(format!("loong-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        std::fs::create_dir_all(&home_dir).unwrap();
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        let (addr, token) = start();
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.list","params":{}}).to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();
        // Three extra workspaces on top of home: all accepted, none refused.
        let mut last = String::new();
        for (i, name) in ["a", "b", "c"].iter().enumerate() {
            let dir = base.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            socket
                .send(Message::text(
                    json!({"jsonrpc":"2.0","id":10+i,"method":"workspace.add","params":{"dir":dir}}).to_string(),
                ))
                .unwrap();
            last = socket.read().unwrap().into_text().unwrap().to_string();
        }
        let value: Value = serde_json::from_str(&last).unwrap();
        assert!(
            value.get("error").is_none(),
            "a third workspace is accepted, not refused: {value}"
        );
        std::env::remove_var("LOONGAGENT_HOME");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Purpose: the catalogue methods answer over the socket against a planted home.
    #[test]
    fn session_list_and_delete_over_the_socket() {
        let _guard = HOME_LOCK.lock().unwrap();
        // Plant a home with one session, and point the client's root at it.
        let base = std::env::temp_dir().join(format!("loong-wsdl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let dir = sessions::session_dir(&home_dir, &workspace);
        std::fs::create_dir_all(&dir).unwrap();
        let line = json!({
            "kind": "message.appended", "seq": 1, "time_ms": 1, "id": "e1",
            "data": {"role": "user", "text": "a planted session"}
        });
        std::fs::write(dir.join("s1.jsonl"), format!("{line}\n")).unwrap();
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = crate::random_token().unwrap();
        let for_server = token.clone();
        let home = workspace.clone();
        std::thread::spawn(move || {
            serve_web(
                listener,
                for_server,
                home,
                Arc::new(|_: &Path, session: Option<&str>| {
                    let id = session.unwrap_or("echo").to_string();
                    Ok((
                        Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        id,
                    ))
                }),
            )
        });
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.list","params":{}}).to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();

        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":2,"method":"session.list","params":{}}).to_string(),
            ))
            .unwrap();
        let line = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&line).unwrap();
        let sessions = value
            .get("result")
            .and_then(|r| r.get("sessions"))
            .and_then(Value::as_array)
            .expect("a session list");
        assert!(
            sessions
                .iter()
                .any(|s| s.get("id").and_then(Value::as_str) == Some("s1")),
            "{sessions:?}"
        );

        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":3,"method":"session.delete","params":{"session_id":"s1"}}).to_string(),
            ))
            .unwrap();
        let line = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(line.contains("\"deleted\":\"s1\""), "{line}");
        assert!(!dir.join("s1.jsonl").exists(), "the log is gone");

        std::env::remove_var("LOONGAGENT_HOME");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Purpose: a tab with no current session can still open one: `session_id` may be
    /// empty and the guard checks the `open` target, not the session being left.
    #[test]
    fn session_open_guards_the_target_not_the_current() {
        let _guard = HOME_LOCK.lock().unwrap();
        // Plant a home with one session, and point the client's root at it.
        let base = std::env::temp_dir().join(format!("loong-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let dir = sessions::session_dir(&home_dir, &workspace);
        std::fs::create_dir_all(&dir).unwrap();
        let line = json!({
            "kind": "message.appended", "seq": 1, "time_ms": 1, "id": "e1",
            "data": {"role": "user", "text": "a planted session"}
        });
        std::fs::write(dir.join("s1.jsonl"), format!("{line}\n")).unwrap();
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = crate::random_token().unwrap();
        let for_server = token.clone();
        let home = workspace.clone();
        std::thread::spawn(move || {
            serve_web(
                listener,
                for_server,
                home,
                Arc::new(|_: &Path, session: Option<&str>| {
                    let id = session.unwrap_or("echo").to_string();
                    Ok((
                        Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        id,
                    ))
                }),
            )
        });
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.list","params":{}}).to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();

        // A fresh tab: no current session (`session_id` ""), opening "s1". The
        // surface binds a child to "s1" and attaches; it must NOT forward a bare
        // session.open across a shared child (which is what produced the
        // `unknown session ""` error against a child holding "default").
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":2,"method":"session.open","params":{"session_id":"","open":"s1"}}).to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&answer).unwrap();
        assert!(
            value.get("error").is_none(),
            "opening a planted session must not error: {answer}"
        );
        let result = value
            .get("result")
            .expect("an attach result, not a forward");
        assert_eq!(
            result.get("session_id").and_then(Value::as_str),
            Some("s1"),
            "the tab is attached to the session it opened: {result}"
        );

        // A target that does not exist is refused with a readable error.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":3,"method":"session.open","params":{"session_id":"","open":"nope"}}).to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&answer).unwrap();
        let error = value.get("error").expect("an unknown target is refused");
        assert!(
            error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("")
                .contains("nope"),
            "{error}"
        );

        std::env::remove_var("LOONGAGENT_HOME");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Purpose: two sessions in one workspace run as two children, each bound to its
    /// own session, and switching between them never kills the first
    /// (web-surface.md §3 — the property a per-workspace child could not give).
    #[test]
    fn two_sessions_are_two_children() {
        let _guard = HOME_LOCK.lock().unwrap();
        // Plant a home with two sessions.
        let base = std::env::temp_dir().join(format!("loong-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let dir = sessions::session_dir(&home_dir, &workspace);
        std::fs::create_dir_all(&dir).unwrap();
        let line = json!({
            "kind": "message.appended", "seq": 1, "time_ms": 1, "id": "e1",
            "data": {"role": "user", "text": "a planted session"}
        });
        std::fs::write(dir.join("s1.jsonl"), format!("{line}\n")).unwrap();
        std::fs::write(dir.join("s2.jsonl"), format!("{line}\n")).unwrap();
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = crate::random_token().unwrap();
        let for_server = token.clone();
        let home = workspace.clone();
        let (factory, seen) = recording_factory();
        std::thread::spawn(move || serve_web(listener, for_server, home, Arc::new(factory)));
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.list","params":{}}).to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();

        // Open s1, then open s2 on the same tab: two distinct children, each bound
        // to the session it was opened for, and neither was started twice.
        for (i, session) in ["s1", "s2"].iter().enumerate() {
            socket
                .send(Message::text(
                    json!({"jsonrpc":"2.0","id":10+i,"method":"session.open","params":{"session_id":"","open":session}}).to_string(),
                ))
                .unwrap();
            let _ = socket.read().unwrap();
        }
        let started = seen.lock().unwrap().clone();
        assert_eq!(
            started,
            vec![Some("s1".to_string()), Some("s2".to_string())],
            "each session got its own child bound to its own id: {started:?}"
        );

        // Re-opening s1 reuses the existing child instead of starting a third.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":12,"method":"session.open","params":{"session_id":"","open":"s1"}}).to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();
        let started = seen.lock().unwrap().clone();
        assert_eq!(
            started.len(),
            2,
            "re-opening a live session reuses its child: {started:?}"
        );

        std::env::remove_var("LOONGAGENT_HOME");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Purpose: workspaces the user added survive a restart: they are read back from
    /// `workspaces.json` under the client root, and home stays index 0.
    #[test]
    fn workspaces_persist_across_a_restart() {
        let _guard = HOME_LOCK.lock().unwrap();
        // A client root and two extra workspace directories on disk.
        let base = std::env::temp_dir().join(format!("loong-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        let workspace = base.join("workspace");
        let extra_a = base.join("extra-a");
        let extra_b = base.join("extra-b");
        for d in [&workspace, &extra_a, &extra_b] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        // First run: home plus two added workspaces — more than the old cap of two.
        {
            let mut st = State::new();
            st.ensure(&workspace).unwrap();
            st.ensure(&extra_a).unwrap();
            st.ensure(&extra_b).unwrap();
            persist_workspaces(&st).unwrap();
        }
        // A restart: a fresh State restores the extra workspaces from the file, home first.
        let mut st = State::new();
        restore_workspaces(&mut st, &workspace).unwrap();
        let dirs: Vec<_> = st.workspaces.iter().map(|w| w.dir.clone()).collect();
        assert_eq!(
            dirs,
            vec![
                std::fs::canonicalize(&workspace).unwrap(),
                std::fs::canonicalize(&extra_a).unwrap(),
                std::fs::canonicalize(&extra_b).unwrap(),
            ],
            "home first, then the persisted extras in order: {dirs:?}"
        );

        std::env::remove_var("LOONGAGENT_HOME");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Purpose: `session.tree` returns every workspace with its sessions nested, flagged
    /// live/busy, so the rail can render the two-level tree in one call (web-surface.md §2).
    #[test]
    fn session_tree_nests_sessions_under_workspaces() {
        let _guard = HOME_LOCK.lock().unwrap();
        // Home workspace with one session, and a second workspace with another.
        let base = std::env::temp_dir().join(format!("loong-tree-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        let workspace = base.join("workspace");
        let other = base.join("other");
        for d in [&workspace, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        let line = json!({
            "kind": "message.appended", "seq": 1, "time_ms": 1, "id": "e1",
            "data": {"role": "user", "text": "a planted session"}
        });
        let home_sessions = sessions::session_dir(&home_dir, &workspace);
        let other_sessions = sessions::session_dir(&home_dir, &other);
        std::fs::create_dir_all(&home_sessions).unwrap();
        std::fs::create_dir_all(&other_sessions).unwrap();
        std::fs::write(home_sessions.join("s1.jsonl"), format!("{line}\n")).unwrap();
        std::fs::write(other_sessions.join("s9.jsonl"), format!("{line}\n")).unwrap();
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = crate::random_token().unwrap();
        let for_server = token.clone();
        let home = workspace.clone();
        std::thread::spawn(move || {
            serve_web(
                listener,
                for_server,
                home,
                Arc::new(|_: &Path, session: Option<&str>| {
                    let id = session.unwrap_or("echo").to_string();
                    Ok((
                        Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        id,
                    ))
                }),
            )
        });
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        // Add the second workspace, then ask for the whole tree.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.add","params":{"dir":other}})
                    .to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":2,"method":"session.tree","params":{}}).to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&answer).unwrap();
        let result = value.get("result").expect("a result, not an error");
        let workspaces = result
            .get("workspaces")
            .and_then(Value::as_array)
            .expect("a workspaces array");
        assert_eq!(workspaces.len(), 2, "{workspaces:?}");
        // Home workspace carries s1; the other carries s9.
        let find = |idx: usize, id: &str| {
            workspaces[idx]
                .get("sessions")
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .any(|s| s.get("id").and_then(Value::as_str) == Some(id))
        };
        assert!(find(0, "s1"), "home workspace has s1: {workspaces:?}");
        assert!(find(1, "s9"), "the other workspace has s9: {workspaces:?}");
        // Sessions carry running/busy flags.
        let s1 = &workspaces[0]["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s.get("id").and_then(Value::as_str) == Some("s1"))
            .unwrap()
            .clone();
        assert_eq!(s1.get("running").and_then(Value::as_bool), Some(false));
        assert_eq!(s1.get("busy").and_then(Value::as_bool), Some(false));

        std::env::remove_var("LOONGAGENT_HOME");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Purpose: the picker route still answers on the workspace surface (smoke for `handle_web`).
    #[test]
    fn the_picker_is_token_gated() {
        let (addr, token) = start();
        // Without the token: a refusal.
        let mut stream = TcpStream::connect(addr).unwrap();
        stream
            .write_all(b"GET /api/browse HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        assert!(body.starts_with("HTTP/1.1 401"), "{body}");
        // With it: a listing.
        let mut stream = TcpStream::connect(addr).unwrap();
        let request = format!(
            "GET /api/browse?token={token} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut body = String::new();
        stream.read_to_string(&mut body).unwrap();
        assert!(body.starts_with("HTTP/1.1 200 OK"), "{body}");
        assert!(body.contains("\"dirs\""), "{body}");
    }
}
