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
    /// Purpose: Wrap a message the transport or a parse produced into the surface's error.
    /// Contract: the message is kept verbatim; no JSON-RPC code is decided here.
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
    /// `model.configured`, `model.providers` or `model.add` — answered on the socket, with
    /// the tab's children reloaded when a model was adopted (web-surface.md §6).
    Models,
    /// `permissions.set` reaching a running turn over its control channel — answered on the
    /// socket, and nothing to forward because the turn already has the ask (web-surface.md §4).
    Permissions,
    /// `session.replay` — answered from the session log on the socket, with no child in the
    /// way, so a busy session still shows its past (web-surface.md §5).
    Replay,
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
    /// the process behind the bridge, read once at creation: a Stop button has to end a
    /// turn that holds the bridge's lock, so the pid must not be asked for through it
    pub pid: Option<u32>,
    /// the private way into this child while a turn holds the bridge, if it has one: the
    /// permission channel of ADR-0019, kept apart from the bridge for the same reason as the
    /// pid — asking through the lock would wait for the turn it means to reach
    pub channel: Option<crate::PermissionChannel>,
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
    /// The slot order this tab has arranged, when ctrl+left / ctrl+right moved something.
    ///
    /// The session answers `model.list` in *declaration* order, and a child read its profile
    /// at startup, so a reorder has to be remembered here and laid over every later list —
    /// otherwise the next refresh would snap the bar back (web-surface.md §4). `None` means
    /// "never arranged", which is the declaration order and needs no interference.
    pub model_order: Option<Vec<String>>,
}

impl State {
    /// Purpose: An empty surface; the first `workspace.list` plants the home workspace.
    /// Serves: `serve_web`, once per listener.
    /// Design: `docs/design/web-surface.md` §1.
    /// Up: `serve_web`, which shares it across the tabs.
    /// Down: `restore_workspaces`, which fills it on first use.
    /// Verify: `cargo test -p loong-web` (`app::tests`).
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
    /// Up: `restore_workspaces`, `workspace.list` and `workspace.add`.
    /// Down: the canonicalised path stored in `State::workspaces`.
    /// Verify: `cargo test -p loong-web` (`many_workspaces_can_be_open`).
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
    /// Serves: the `workspace.close` call.
    /// Design: `docs/design/web-surface.md` §1 — removing never deletes history.
    /// Up: `workspace.close`.
    /// Down: `State::workspaces`, and the children map dropped with the entry.
    /// Verify: `cargo test -p loong-web` (`app::tests`).
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
/// Up: the two handlers that changed the list.
/// Down: `workspaces.json` under the client root.
/// Verify: `cargo test -p loong-web` (`workspaces_persist_across_a_restart`).
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
/// Up: every method that walks the workspace list.
/// Down: `State::ensure`, which appends the directories it reads back.
/// Verify: `cargo test -p loong-web` (`workspaces_persist_across_a_restart`).
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

/// The tab's outgoing lines, and the only way anything but the read loop reaches the socket.
///
/// A turn runs on a thread of its own (web-surface.md §3), so it cannot hold the socket:
/// it hands whole lines to this channel instead, and the read loop writes them. One writer
/// means one place that sends, and a turn that lasts a minute no longer stops the page from
/// opening a workspace or switching session while it runs.
/// Purpose: Hand a finished line to the thread that owns the socket.
///
/// Serves: `exchange_turn`, `dispatch_surface` and `send_error`, none of which hold the socket.
/// Design: `docs/design/web-surface.md` §3.
/// Up: `handle_ws` creates one per tab and hands clones to the turn threads.
/// Down: `drain`, the only reader, on the read loop's thread.
/// Verify: `cargo test -p loong-web` (`a_surface_answer_arrives_while_a_turn_is_running`).
#[derive(Clone)]
struct Outbox {
    /// Purpose: The lines waiting for the socket, in the order they were finished.
    tx: std::sync::mpsc::Sender<Message>,
}

impl Outbox {
    /// Purpose: Queue one line for the socket.
    ///
    /// Contract: `Err` means the tab is gone, which is how a turn learns to stop writing.
    fn text(&self, body: String) -> Result<(), String> {
        self.tx
            .send(Message::text(body))
            .map_err(|_| "the browser closed the socket".to_string())
    }
}

/// Purpose: Write everything the outbox is holding and return when it is empty.
///
/// Serves: `handle_ws`, once per read timeout and once per loop, so a turn's lines go out
/// while the read loop is idle between browser messages.
/// Design: `docs/design/web-surface.md` §3.
/// Up: `handle_ws`.
/// Down: `WebSocket::send` on the one thread allowed to touch the socket.
/// Verify: `cargo test -p loong-web`.
fn drain(
    socket: &mut tungstenite::WebSocket<TcpStream>,
    rx: &std::sync::mpsc::Receiver<Message>,
) -> Result<(), String> {
    loop {
        // An empty outbox and a closed one are the same thing here: nothing to write now.
        let Ok(message) = rx.try_recv() else {
            return Ok(());
        };
        socket
            .send(message)
            .and_then(|_| socket.flush())
            .map_err(|e| format!("sending to the browser: {e}"))?;
    }
}

/// Purpose: Say whether a failed read only means "nothing arrived before the timeout".
///
/// Contract: a timeout is not a closed tab, so the loop drains and reads again.
fn would_block(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
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
    start_session: &dyn Fn(&Path, Option<&str>) -> Result<crate::Started, String>,
    models: &dyn crate::ModelSetup,
) -> Result<(), String> {
    let mut tab = Tab::default();
    // One thread reads the socket and one thread writes it — this one. A turn runs
    // elsewhere and hands its lines to the outbox, so the loop is free between browser
    // messages; a read that waited forever would starve the outbox, hence the timeout.
    // `read` keeps a half-received frame across a timeout, so no message is lost.
    let (tx, rx) = std::sync::mpsc::channel();
    let out = Outbox { tx };
    socket
        .get_ref()
        .set_read_timeout(Some(std::time::Duration::from_millis(20)))
        .map_err(|e| format!("timing the browser socket: {e}"))?;
    loop {
        drain(socket, &rx)?;
        let message = match socket.read() {
            Ok(message) => message,
            Err(tungstenite::Error::Io(e)) if would_block(&e) => continue,
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
                        send_error(&out, Value::Null, &format!("not JSON-RPC: {e}"))?;
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
                    models,
                    &method,
                    &request,
                    &out,
                ) {
                    Ok(SurfaceCall::Forward) => {
                        if let Err(e) = forward(
                            state,
                            &mut tab,
                            home,
                            start_session,
                            &out,
                            line,
                            &method,
                            id.clone(),
                            request.get("params").unwrap_or(&Value::Null),
                        ) {
                            send_error(&out, id, &e)?;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => send_error(&out, id, &e.0)?,
                }
            }
            Message::Close(_) => {
                drain(socket, &rx)?;
                return Ok(());
            }
            Message::Ping(payload) => {
                socket
                    .send(Message::Pong(payload))
                    .and_then(|_| socket.flush())
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
    start_session: &dyn Fn(&Path, Option<&str>) -> Result<crate::Started, String>,
    models: &dyn crate::ModelSetup,
    method: &str,
    request: &Value,
    out: &Outbox,
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
                    // Newest added first: the launch workspace is index 0 and was there
                    // first, so walking back from the last index puts the workspace the
                    // user just added at the top. `index` still travels with the entry,
                    // so what the page shows changes but which workspace a click means
                    // does not.
                    .rev()
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
            reply(out, id, json!({"workspaces": list, "seat": tab.seat}))?;
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
            reply(out, id, json!({"index": index}))?;
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
            reply(out, id, json!({"index": index}))?;
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
            reply(out, id, json!({"closed": index}))?;
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
                    // Newest added first, as in `workspace.list` (web-surface.md §2).
                    .rev()
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
            reply(out, id, json!({"workspaces": tree, "seat": tab.seat}))?;
            Ok(SurfaceCall::Sessions)
        }
        "session.list" => {
            let dir = seat_dir(state, tab.seat)?;
            let entries = sessions::list(&sessions::client_root(), &dir);
            reply(out, id, json!({"sessions": entries}))?;
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
            // An arrangement belongs to the models of one session; another session's list
            // has its own, and carrying the old order over would reorder by accident.
            tab.model_order = None;
            reply(
                out,
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
            // asking the child (web-surface.md §2). An optional `workspace` index names
            // the workspace first, because the rail puts a `＋` on every workspace row
            // and not only on the one this tab is seated in (web-surface.md §4).
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
            let session_id = ensure_child(state, tab.seat, home, None, start_session)?;
            tab.attached = Some(session_id.clone());
            // A brand-new session starts on the declaration order, as a fresh tab does.
            tab.model_order = None;
            reply(
                out,
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
            reply(out, id, json!({"deleted": session_id}))?;
            Ok(SurfaceCall::DeleteSession(session_id.to_string()))
        }
        "model.reorder" => {
            // ctrl+left / ctrl+right: this moves the *current* model one slot, it does not
            // switch to another one. Told apart from `model.select` on purpose — they are
            // different acts and the bar has a key for each (web-surface.md §4).
            let delta = params.get("delta").and_then(Value::as_i64).unwrap_or(0);
            if delta != 1 && delta != -1 {
                return Err(Error(
                    "model.reorder takes delta: 1 (right) or -1 (left)".to_string(),
                ));
            }
            // A turn owns its child for the whole of it, so asking mid-turn would either
            // block this socket or lie. The page disables the keys; this catches the rest.
            if workspace_busy(state, tab)? {
                return Err(Error(
                    "a turn is running in this workspace; reorder the models when it ends"
                        .to_string(),
                ));
            }
            let session_id = params
                .get("session_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| tab.attached.clone())
                .ok_or_else(|| Error("model.reorder needs an open session".to_string()))?;
            // The order to move *in* is the one the session currently declares, read now:
            // a cached copy would be stale after `model.add`, and this is one cheap read.
            let list = ask_child(
                state,
                tab,
                home,
                start_session,
                &session_id,
                "model.list",
                &json!({"session_id": session_id}),
            )?;
            let names: Vec<String> = list["models"]
                .as_array()
                .map(|models| {
                    models
                        .iter()
                        .filter_map(|m| m["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let selected = list["selected"].as_str().unwrap_or("").to_string();
            // Walk on from what the bar is showing, not from the declaration: the session
            // keeps answering in declaration order, so a second press would otherwise undo
            // the first (web-surface.md §4).
            let shown = shown_order(tab.model_order.as_deref(), &names);
            let (order, from, to) = move_in_order(&shown, &selected, delta).map_err(Error)?;
            // The move itself never fails: the bar is this session's arrangement, and a file
            // that cannot be rewritten (or is not this run's to rewrite) does not unmake it.
            // Told apart so the answer can say which happened — the TUI makes the same split
            // and for the same reason (web-surface.md §4).
            let (persisted, warning) = match models.save_order(&order) {
                Ok(persisted) => (persisted, None),
                Err(error) => (false, Some(error)),
            };
            tab.model_order = Some(order.clone());
            let mut answer = list.clone();
            answer["models"] = offerings_in_order(&list["models"], &order);
            answer["moved"] = json!({"name": selected, "from": from + 1, "to": to + 1});
            answer["persisted"] = json!(persisted);
            if let Some(warning) = warning {
                answer["warning"] = json!(warning);
            }
            reply(out, id, answer)?;
            Ok(SurfaceCall::Models)
        }
        "model.configured" => {
            // Asked before any session exists: a new browser sits with no tab, no child
            // and no `model.list` to consult, and the empty-state warning has to be
            // right then (web-surface.md §6).
            reply(out, id, models.configured())?;
            Ok(SurfaceCall::Models)
        }
        "model.providers" => {
            reply(out, id, models.providers())?;
            Ok(SurfaceCall::Models)
        }
        "model.probe" => {
            let provider = params.get("provider").and_then(Value::as_str).unwrap_or("");
            let key = params.get("key").and_then(Value::as_str).unwrap_or("");
            // No busy guard and no reload: this call writes nothing, so a turn in progress
            // cannot be disturbed by it, and no child has to be told about anything
            // (web-surface.md §6).
            let offered = models.probe(provider, key).map_err(Error)?;
            reply(out, id, offered)?;
            Ok(SurfaceCall::Models)
        }
        "model.add" => {
            let provider = params
                .get("provider")
                .and_then(Value::as_str)
                .ok_or_else(|| Error("model.add needs a provider".to_string()))?;
            let key = params.get("key").and_then(Value::as_str).unwrap_or("");
            // The selection, and required: a page that forgot to send one must not be read
            // as "tick nothing", because that means "turn them all off" (web-surface.md §6).
            let chosen = selection(&params).map_err(Error)?;
            // A running child is mid-turn: killing it would drop that turn. The page
            // disables the panel while its own turn runs; this catches the other tabs.
            if workspace_busy(state, tab)? {
                return Err(Error(
                    "a turn is running in this workspace; change the models when it ends"
                        .to_string(),
                ));
            }
            let adopted = models.adopt(provider, key, &chosen).map_err(Error)?;
            // Config is read once, when a child starts, so every idle child is now stale.
            // Dropping one kills it and the next request spawns a fresh one — a child is
            // a reader of the session log, never its owner (web-surface.md §6).
            let (restarted, waiting) = {
                let mut st = lock(state)?;
                reload_children(&mut st)
            };
            let mut answer = adopted;
            if let Value::Object(map) = &mut answer {
                map.insert("restarted".to_string(), json!(restarted));
                map.insert("waiting".to_string(), json!(waiting));
            }
            // The file is authoritative again: whatever this tab had arranged is now
            // either written there or beside the point (web-surface.md §6).
            tab.model_order = None;
            reply(out, id, answer)?;
            Ok(SurfaceCall::Models)
        }
        "permissions.set" => {
            // A running turn owns the child's connections: a request written down the
            // ordinary path sits in the pipe until that turn ends, which is why this
            // switch used to be disabled for the length of one. The surface reaches a
            // running child over the private control channel instead — the same descriptor
            // 3 the TUI uses, out of reach of every tool (ADR-0019) — and answers at once,
            // because the ask is what this reply confirms; `permissions.relaxed` is the
            // record (ADR-0007 §5, web-surface.md §4).
            let on = params
                .get("auto_approve")
                .and_then(Value::as_bool)
                .ok_or_else(|| Error("permissions.set needs auto_approve".to_string()))?;
            let named = params
                .get("session_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| tab.attached.clone());
            let running = match &named {
                None => None,
                Some(id) => {
                    let st = lock(state)?;
                    st.workspaces
                        .get(tab.seat)
                        .and_then(|w| w.children.get(id))
                        .filter(|child| child.busy.is_some())
                        .and_then(|child| child.channel.clone())
                }
            };
            // Nothing to reach, or no way in: the request goes down the child as it always
            // did, and the change lands when the turn ends. Never a lie about a state this
            // surface cannot enforce (web-surface.md §4). The channel is taken from the
            // child and not through its bridge, which the running turn is holding.
            let reached = running.map(|channel| channel(on).is_ok()).unwrap_or(false);
            if !reached {
                return Ok(SurfaceCall::Forward);
            }
            reply(
                out,
                id,
                json!({
                    "session_id": named,
                    "auto_approve": on,
                    // The turn in flight applies it at its next boundary; the switch shows
                    // the ask, and the session's `permissions.relaxed` says it landed.
                    "queued": false,
                    "applies": "at the running turn's next step",
                }),
            )?;
            Ok(SurfaceCall::Permissions)
        }
        "session.replay" => {
            // The log is the record, and the page owns the rail that reads it for everything
            // else, so the transcript of a sitting comes from the file too — not from the
            // session's child. A child answers only when its turn ends, and the one moment a
            // reader most wants the past is the moment the session is busy writing the
            // present: switching back to a working session drew an empty chat until the turn
            // finished (web-surface.md §5). A session no seat has is refused, never guessed.
            let session_id = params
                .get("session_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| tab.attached.clone())
                .ok_or_else(|| Error("session.replay needs a session_id".to_string()))?;
            // The tab's own seat first, then the rest: `session.open` may have moved it, and
            // a `workspace` index in the request names the seat outright.
            let dirs = {
                let st = lock(state)?;
                let mut dirs: Vec<PathBuf> = Vec::with_capacity(st.workspaces.len());
                if let Some(seat) = st.workspaces.get(tab.seat) {
                    dirs.push(seat.dir.clone());
                }
                for (i, w) in st.workspaces.iter().enumerate() {
                    if i != tab.seat {
                        dirs.push(w.dir.clone());
                    }
                }
                dirs
            };
            let events = sessions::replay(&sessions::client_root(), &dirs, &session_id)?;
            reply(out, id, json!({"events": events, "session_id": session_id}))?;
            Ok(SurfaceCall::Replay)
        }
        _ => Ok(SurfaceCall::Forward),
    }
}

/// Purpose: The models a `model.add` names, as `(name, context window)` pairs.
/// Serves: `model.add`, so a selection is written as the person ticked it (web-surface.md §6).
/// Design: `docs/design/web-surface.md` §6.
/// Up: `dispatch_surface`.
/// Down: nothing; `ModelSetup::adopt` takes the pairs.
/// Verify: `cargo test -p loong-web` (`model_`).
/// Required, and an empty array is a real answer: it means every model of this provider was
/// unticked, which turns them off and keeps the key. A *missing* array is a broken page, and
/// reading it as "tick nothing" would delete models nobody meant to touch.
fn selection(params: &Value) -> Result<Vec<(String, Option<u64>)>, String> {
    let rows = params
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "model.add needs models: [{name, context_window}]".to_string())?;
    rows.iter()
        .map(|row| {
            let name = row
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| "every model in a selection needs a name".to_string())?;
            Ok((
                name.to_string(),
                row.get("context_window").and_then(Value::as_u64),
            ))
        })
        .collect()
}

/// Purpose: Whether any child in this tab's workspace is mid-turn.
/// Serves: the guards in `model.add` and `model.reorder`, the two calls that would otherwise
/// have to wait for a running turn's exclusive hold on its child.
/// Design: `docs/design/web-surface.md` §3 and §4.
/// Up: `dispatch_surface`.
/// Down: `Workspace::children` and `Child::busy`.
/// Verify: `cargo test -p loong-web` (`reorder_`).
/// Contract: a poisoned state answers "busy", because refusing is the safe side.
fn workspace_busy(state: &Arc<Mutex<State>>, tab: &Tab) -> Result<bool, Error> {
    let st = lock(state)?;
    Ok(st
        .workspaces
        .get(tab.seat)
        .is_some_and(|w| w.children.values().any(|c| c.busy.is_some())))
}

/// Purpose: Ask one session a question and give back its answer, for the surface's own calls.
/// Serves: `model.reorder`, which needs the current list before it can move anything in it.
/// Design: `docs/design/web-surface.md` §4.
/// Up: `dispatch_surface`.
/// Down: `ensure_child` and `Session::exchange`; the request carries a surface-only id so the
/// answer cannot be mistaken for one the browser is waiting for.
/// Verify: `cargo test -p loong-web` (`reorder_`).
/// Contract: lines that are not the answer (deltas, events) are dropped, not forwarded: this
/// read happens off the browser's own request, so nothing of it belongs on the page.
fn ask_child(
    state: &Arc<Mutex<State>>,
    tab: &mut Tab,
    home: &Path,
    start_session: &dyn Fn(&Path, Option<&str>) -> Result<crate::Started, String>,
    session_id: &str,
    method: &str,
    params: &Value,
) -> Result<Value, Error> {
    let child_id =
        ensure_child(state, tab.seat, home, Some(session_id), start_session).map_err(|e| e.0)?;
    tab.attached = Some(child_id.clone());
    let session = {
        let st = lock(state)?;
        st.workspaces
            .get(tab.seat)
            .and_then(|w| w.children.get(&child_id))
            .map(|c| c.session.clone())
            .ok_or_else(|| Error("the session has no live child".to_string()))?
    };
    let id = json!("surface");
    let request = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let mut answer = None;
    let outcome = match session.lock() {
        Ok(mut session) => session.exchange(&request.to_string(), &mut |line| {
            let parsed: Value = serde_json::from_str(&line).unwrap_or(Value::Null);
            if parsed.get("id") == Some(&id) {
                answer = Some(parsed);
            }
            Ok(())
        }),
        Err(_) => Err("the session is poisoned".to_string()),
    };
    outcome.map_err(Error)?;
    let answer = answer.ok_or_else(|| Error(format!("{method} got no answer from the session")))?;
    if let Some(error) = answer.get("error") {
        let message = error["message"].as_str().unwrap_or("the session refused");
        return Err(Error(message.to_string()));
    }
    // The caller wants the answer's body, not the envelope around it.
    Ok(answer.get("result").cloned().unwrap_or(Value::Null))
}

/// Purpose: The order the bar is currently showing, given what it arranged and what the session
/// declares — so a second move walks on from the first instead of starting over.
/// Serves: `model.reorder`; without it two ctrl+rights would oscillate between two arrangements.
/// Design: `docs/design/web-surface.md` §4.
/// Up: `dispatch_surface`'s `model.reorder` arm.
/// Down: nothing; the caller swaps one slot in the result.
/// Verify: `cargo test -p loong-web` (`reorder_`).
/// Contract: names the arrangement no longer covers are dropped, ones it has not seen are
/// appended in declaration order, and `None` simply means "the declaration is the order".
fn shown_order(arranged: Option<&[String]>, declared: &[String]) -> Vec<String> {
    let Some(arranged) = arranged else {
        return declared.to_vec();
    };
    let mut order: Vec<String> = arranged
        .iter()
        .filter(|name| declared.iter().any(|d| d == *name))
        .cloned()
        .collect();
    let rest: Vec<String> = declared
        .iter()
        .filter(|name| !order.iter().any(|o| o == *name))
        .cloned()
        .collect();
    order.extend(rest);
    order
}

/// Purpose: Swap the selected model one slot, or say which edge it is already at.
/// Serves: `model.reorder`.
/// Design: `docs/design/web-surface.md` §4.
/// Up: `dispatch_surface`'s `model.reorder` arm.
/// Down: nothing; the caller writes the order and reorders the offerings with it.
/// Verify: `cargo test -p loong-web` (`reorder_`).
/// Contract: returns the new order and the 0-based from/to slots.
fn move_in_order(
    names: &[String],
    selected: &str,
    delta: i64,
) -> Result<(Vec<String>, usize, usize), String> {
    if names.is_empty() {
        return Err("this session has no models to reorder".to_string());
    }
    let at = names
        .iter()
        .position(|name| name == selected)
        .ok_or_else(|| format!("{selected:?} is not in this session's model list"))?;
    let target = at as i64 + delta;
    if target < 0 {
        return Err(format!("{selected} is already in the first slot"));
    }
    if target as usize >= names.len() {
        return Err(format!("{selected} is already in the last slot"));
    }
    let mut order = names.to_vec();
    order.swap(at, target as usize);
    Ok((order, at, target as usize))
}

/// Purpose: Lay an arranged order over the offerings the session sent, keeping any name the
/// order does not mention at the end rather than dropping it.
/// Serves: `model.reorder`'s answer and `forward`, which applies the same order to later lists.
/// Design: `docs/design/web-surface.md` §4.
/// Up: `dispatch_surface` and `forward`.
/// Down: nothing beyond `serde_json::Value`.
/// Verify: `cargo test -p loong-web` (`reorder_`).
/// Contract: the two passes are what make a stale order harmless — a model added since the
/// arrangement was made still appears, and one removed since simply has no match.
fn offerings_in_order(models: &Value, order: &[String]) -> Value {
    let listed = models.as_array().cloned().unwrap_or_default();
    let named = |name: &String| {
        listed
            .iter()
            .any(|m| m["name"].as_str() == Some(name.as_str()))
    };
    let mut out = Vec::with_capacity(listed.len());
    for name in order.iter().filter(|name| named(name)) {
        if let Some(found) = listed
            .iter()
            .find(|m| m["name"].as_str() == Some(name.as_str()))
        {
            out.push(found.clone());
        }
    }
    out.extend(listed.into_iter().filter(|m| {
        m["name"]
            .as_str()
            .is_none_or(|n| !order.iter().any(|o| o == n))
    }));
    Value::Array(out)
}

/// Purpose: Rewrite a `model.list` line so it carries the order this tab arranged.
/// Serves: `forward`, so a later refresh does not snap the bar back to declaration order.
/// Design: `docs/design/web-surface.md` §4.
/// Up: `forward`'s sink closure.
/// Down: `offerings_in_order`.
/// Verify: `cargo test -p loong-web` (`reorder_`).
/// Contract: anything that is not a successful `model.list` answer goes through untouched,
/// including a line that is not JSON at all.
fn reorder_reply(line: &str, order: &[String]) -> String {
    let Ok(mut parsed) = serde_json::from_str::<Value>(line) else {
        return line.to_string();
    };
    let Some(models) = parsed.get("result").and_then(|r| r.get("models")) else {
        return line.to_string();
    };
    if !models.is_array() {
        return line.to_string();
    }
    let ordered = offerings_in_order(models, order);
    if let Some(result) = parsed.get_mut("result").and_then(Value::as_object_mut) {
        result.insert("models".to_string(), ordered);
    }
    parsed.to_string()
}

/// Purpose: Drop every idle session child so the next request starts one that has read the
/// config just written; leave the children mid-turn alone.
///
/// Serves: `model.add`, the one call that changes `local.toml` under a running surface.
/// Design: `docs/design/web-surface.md` §6 — a child is a reader of the log, so dropping one
/// loses nothing that replay cannot restore.
/// Up: `dispatch_surface`.
/// Down: `Workspace::children`, whose dropped bridges kill their processes on `Drop`.
/// Verify: `cargo test -p loong-web` (`model_add_`).
/// Contract: returns (restarted, still busy and therefore stale until the turn ends).
fn reload_children(st: &mut State) -> (usize, usize) {
    let mut restarted = 0;
    let mut waiting = 0;
    for workspace in st.workspaces.iter_mut() {
        let before = workspace.children.len();
        workspace.children.retain(|_, child| child.busy.is_some());
        restarted += before - workspace.children.len();
        waiting += workspace.children.len();
    }
    (restarted, waiting)
}

/// Purpose: Carry one line down the tab's workspace session and stream every answer line
/// back, on a thread of its own so the socket's read loop stays free.
/// Serves: `handle_ws`'s forwarding half — the browser speaks exactly what the TUI speaks.
/// Design: `docs/design/web-surface.md` §3.
/// Up: `handle_ws`.
/// Down: `exchange_turn` on its own thread, and `ensure_child` here on the loop.
/// Verify: `cargo test -p loong-web` (`forward_`).
/// Contract: the child, the session and the busy flag are settled before the thread starts,
/// so a refused request is still answered on the request's own turn of the loop; the turn
/// itself reports failure through the outbox as an event plus an error.
#[allow(clippy::too_many_arguments)]
fn forward(
    state: &Arc<Mutex<State>>,
    tab: &mut Tab,
    home: &Path,
    start_session: &dyn Fn(&Path, Option<&str>) -> Result<crate::Started, String>,
    out: &Outbox,
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
    // A prompt keeps this child busy until its response comes back: the page renders
    // that as "running", and `/api/interrupt` uses it to name the child to signal
    // (web-surface.md §3).
    let busy = method == "prompt.submit";
    if busy {
        set_busy(state, tab.seat, &child_id, id.as_u64());
    }
    // A `model.list` answer is the one line the surface edits: the tab may have arranged
    // the slots with ctrl+left / ctrl+right, and the session answers in declaration order
    // because its child read the profile at startup (web-surface.md §4).
    let turn = Turn {
        seat: tab.seat,
        child_id,
        session,
        line: line.to_string(),
        method: method.to_string(),
        arranged: tab.model_order.clone(),
        busy,
    };
    let state_for_turn = Arc::clone(state);
    let out = out.clone();
    // Named before the turn is moved into its thread: a thread that cannot start still
    // has to clear the busy flag it just set.
    let seat = turn.seat;
    let child = turn.child_id.clone();
    // A turn must not own the loop: the page keeps opening workspaces and switching
    // sessions while a model writes. One child still answers one request at a time —
    // its lock does that, on this thread and not on the socket's.
    std::thread::Builder::new()
        .name("loong-web-turn".to_string())
        .spawn(move || exchange_turn(&state_for_turn, &turn, &out, id))
        .map(|_| ())
        .map_err(|e| {
            if busy {
                set_busy(state, seat, &child, None);
            }
            format!("starting the turn: {e}")
        })
}

/// One request on its way to a child: everything `exchange_turn` needs, settled before the
/// thread starts so the loop holds no borrow of the tab.
/// Purpose: Describe the request a turn thread runs.
///
/// Serves: `forward`, which builds it on the loop and `exchange_turn`, which runs it.
/// Design: `docs/design/web-surface.md` §3.
/// Up: `forward`.
/// Down: `exchange_turn`.
/// Verify: `cargo test -p loong-web` (`forward_`).
struct Turn {
    /// Purpose: The workspace the child belongs to, for the busy flag when the turn ends.
    seat: usize,
    /// Purpose: The session's id, which every forwarded line carries back to the page.
    child_id: String,
    /// Purpose: The session bridge, one per (workspace, session).
    session: Arc<Mutex<dyn Session>>,
    /// Purpose: The request line, exactly as the browser wrote it.
    line: String,
    /// Purpose: The method, which decides the one reply the surface edits and the busy flag.
    method: String,
    /// Purpose: The tab's arrangement, for a `model.list` answer.
    arranged: Option<Vec<String>>,
    /// Purpose: Whether this request leaves the child busy, so the end of the turn clears it.
    busy: bool,
}

/// Purpose: Run one request against its session off the read loop, streaming every answer
/// line into the tab's outbox and settling the child's busy flag when it ends.
///
/// Serves: `forward`.
/// Design: `docs/design/web-surface.md` §3.
/// Up: the thread `forward` starts.
/// Down: `Session::exchange`, `tag_line` and `Outbox::text`.
/// Verify: `cargo test -p loong-web` (`a_turn_does_not_block_the_surface`).
fn exchange_turn(state: &Arc<Mutex<State>>, turn: &Turn, out: &Outbox, id: Value) {
    let mut failure = None;
    let outcome = match turn.session.lock() {
        Ok(mut session) => {
            let varied = &turn.arranged;
            let method = turn.method.as_str();
            session.exchange(&turn.line, &mut |answer| {
                // Straight through, marked with whose line it is so the page can leave
                // the chat of a session it has switched away from alone
                // (web-surface.md §3).
                let answer = match (varied, method) {
                    (Some(order), "model.list") => reorder_reply(&answer, order),
                    _ => answer,
                };
                out.text(tag_line(&answer, &turn.child_id))
            })
        }
        Err(_) => Err("the session is poisoned".to_string()),
    };
    if let Err(e) = outcome {
        failure = Some(e);
    }
    if turn.busy {
        set_busy(state, turn.seat, &turn.child_id, None);
    }
    let Some(e) = failure else {
        return;
    };
    // A transport failure is an event like any other, so the turn carries a line saying
    // why it did not run...
    let notice = json!({
        "jsonrpc": "2.0",
        "session_id": turn.child_id,
        "method": "event",
        "params": {
            "seq": 0, "id": "e-local", "parent_id": null,
            "kind": "client.error", "ignorable": true,
            "time_ms": 0, "data": {"message": e}
        }
    })
    .to_string();
    // ...and it is still an answer to the request: the page settles a request on the line
    // carrying its id, so an event alone would leave the turn's spinner turning over a
    // turn that nothing can end (web-composer.md §5).
    let _ = out.text(notice);
    let _ = send_error(out, id, &e);
}

/// Purpose: Mark one forwarded line with the session that wrote it.
///
/// Serves: the page, which drops a line whose `session_id` is not the session it shows, so a
/// turn left running in one session cannot paint itself into another's chat
/// (web-surface.md §3).
/// Design: `docs/design/web-surface.md` §3.
/// Up: `exchange_turn`.
/// Down: nothing; the line is the browser's to read.
/// Verify: `cargo test -p loong-web` (`forward_` and `tag_line_`).
/// Contract: a line that is not a JSON object is passed through untouched — the page already
/// knows to ignore what it cannot read, and refusing here would lose the only copy.
fn tag_line(line: &str, session: &str) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(mut fields)) => {
            fields.insert("session_id".to_string(), Value::String(session.to_string()));
            Value::Object(fields).to_string()
        }
        _ => line.to_string(),
    }
}

/// Purpose: Set or clear the child's busy marker, which the page reads as "running".
///
/// Serves: `forward` and `exchange_turn`.
/// Design: `docs/design/web-surface.md` §3.
/// Up: `forward` before the thread starts, `exchange_turn` when it ends.
/// Down: `State::workspaces[seat].children[id].busy`.
/// Verify: `cargo test -p loong-web` (`busy_`).
fn set_busy(state: &Arc<Mutex<State>>, seat: usize, child_id: &str, request: Option<u64>) {
    if let Ok(mut st) = state.lock() {
        if let Some(c) = st
            .workspaces
            .get_mut(seat)
            .and_then(|w| w.children.get_mut(child_id))
        {
            c.busy = request;
        }
    }
}

/// Purpose: Make sure the workspace's session `session` has a live child, starting one if
/// not, and return the id the child is bound to.
/// Serves: `session.open`, `session.new` and `forward`.
/// Design: `docs/design/web-surface.md` §3 — one child per (workspace, session), on demand.
/// Up: those three handlers.
/// Down: `start_session`, the factory that spawns `loongagent server --stdio`.
/// Verify: `cargo test -p loong-web` (`session_open_reuses_a_live_child`).
/// Contract: after `Ok`, `workspaces[seat].children[id]` is live; `Some(id)` reuses a live
/// child for that id, `None` starts a fresh one and returns its minted id.
fn ensure_child(
    state: &Arc<Mutex<State>>,
    seat: usize,
    home: &Path,
    session: Option<&str>,
    start_session: &dyn Fn(&Path, Option<&str>) -> Result<crate::Started, String>,
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
    let crate::Started {
        session: child,
        session_id: id,
        channel,
    } = start_session(&dir, session).map_err(Error)?;
    // Asked now, while nothing else can hold the bridge: `pid()` on a session whose turn
    // is running would wait for that turn to end, which is exactly the wait a Stop button
    // must not have (web-surface.md §4).
    let pid = child.lock().ok().and_then(|session| session.pid());
    st.workspaces[seat].children.insert(
        id.clone(),
        Child {
            session: child,
            busy: None,
            pid,
            channel,
        },
    );
    Ok(id)
}

/// Purpose: The child running a turn, and the pid a Stop button would signal.
///
/// Serves: `crate::interrupt` for `/api/interrupt` (web-surface.md §4).
/// Design: `docs/design/web-surface.md` §4.
/// Up: `crate::handle_web`'s route.
/// Down: `Child::busy`, `Child::pid`.
/// Verify: `cargo test -p loong-web` (`stop_`).
/// Contract: `None` means nothing is running — which the route reports as a refusal, not
/// as a failure. With no `session_id` the first busy child answers, so a page that lost its
/// session can still stop the turn it is watching.
pub fn busy_child(
    state: &Arc<Mutex<State>>,
    session_id: Option<&str>,
) -> Result<Option<(String, Option<u32>)>, String> {
    let st = state
        .lock()
        .map_err(|_| "the state is poisoned".to_string())?;
    for workspace in &st.workspaces {
        for (id, child) in &workspace.children {
            if child.busy.is_none() {
                continue;
            }
            if let Some(wanted) = session_id {
                if wanted != id {
                    continue;
                }
            }
            return Ok(Some((id.clone(), child.pid)));
        }
    }
    Ok(None)
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
/// Serves: `dispatch_surface`, which answers on the loop rather than holding the socket.
/// Design: `docs/design/web-surface.md` §1–§3.
/// Up: `dispatch_surface`'s arms.
/// Down: the tab's outbox.
/// Verify: `cargo test -p loong-web`.
/// Contract: the `id` echoes the request's; the result is the method's payload.
fn reply(out: &Outbox, id: Value, result: Value) -> Result<(), Error> {
    let body = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
    out.text(body).map_err(Error)
}

/// Purpose: One JSON-RPC error object down the tab's socket.
/// Serves: `handle_ws`, when a request is refused rather than answered.
/// Design: ADR-0002 — one error shape for every method.
/// Up: the refusal sites in `handle_ws`.
/// Down: the tab, as an `error` object rather than a dropped message.
/// Verify: `cargo test -p loong-web` (`a_websocket_carries_a_request_and_gets_the_answer_back`).
fn send_error(out: &Outbox, id: Value, message: &str) -> Result<(), String> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": -32602, "message": message}
    })
    .to_string();
    out.text(body)
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

    /// Purpose: a session factory that records the session id each child was started
    /// for, so the tests can assert two sessions become two children (web-surface.md §3).
    /// Serves: the `session.open`/`session.new` tests in this module.
    /// Design: `docs/design/web-surface.md` §3 — one child per session.
    /// Up: `ensure_child`, handed in as the factory.
    /// Down: the shared vector of ids the tests inspect.
    /// Verify: `cargo test -p loong-web` (`session_open_reuses_a_live_child`).
    fn recording_factory() -> (
        impl Fn(&Path, Option<&str>) -> Result<crate::Started, String>,
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
            Ok(crate::Started {
                session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                session_id: id,
                channel: None,
            })
        };
        (factory, seen)
    }

    /// Purpose: a full web surface on loopback with an echo bridge, for the ws tests.
    /// Serves: every socket test in this module that needs a running surface.
    /// Design: `docs/design/web-surface.md` §3.
    /// Up: those tests.
    /// Down: `serve_web`, on a private listener with a fresh token.
    /// Verify: `cargo test -p loong-web` (`app::tests`).
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
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
                }),
            )
        });
        (addr, token)
    }

    /// Purpose: a full web surface on loopback whose children the test chooses, for the tests
    /// that need a child which blocks, or one the surface has to reach out of band.
    /// Serves: `a_surface_answer_arrives_while_a_turn_is_running`, `yolo_reaches_a_running_child_out_of_band`.
    /// Design: `docs/design/web-surface.md` §3 and §4.
    /// Up: those two tests.
    /// Down: `serve_web`, on a private listener with a fresh token.
    /// Verify: `cargo test -p loong-web` (`app::tests`).
    fn start_with<F>(factory: F) -> (std::net::SocketAddr, String)
    where
        F: Fn(&Path, Option<&str>) -> Result<crate::Started, String> + Send + Sync + 'static,
    {
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = crate::random_token().unwrap();
        let for_server = token.clone();
        let home = std::env::temp_dir();
        std::thread::spawn(move || serve_web(listener, for_server, home, Arc::new(factory)));
        (addr, token)
    }

    /// Purpose: a full web surface on loopback **with** a model setup, for the panel tests.
    /// Serves: `model_panel_*`, the tests for the add-model panel (web-surface.md §6).
    /// Design: `docs/design/web-surface.md` §6.
    /// Up: those tests.
    /// Down: `serve_web_with_models`, on a private listener with a fresh token.
    /// Verify: `cargo test -p loong-web` (`model_panel`).
    fn start_with_models(models: Arc<dyn crate::ModelSetup>) -> (std::net::SocketAddr, String) {
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = crate::random_token().unwrap();
        let for_server = token.clone();
        let home = std::env::temp_dir();
        std::thread::spawn(move || {
            crate::serve_web_with_models(
                listener,
                for_server,
                home,
                Arc::new(|_: &Path, session: Option<&str>| {
                    let id = session.unwrap_or("echo").to_string();
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
                }),
                models,
                Arc::new(|_: u32| {}),
            )
        });
        (addr, token)
    }

    /// Purpose: the panel's stand-in: one provider, an empty profile, and a key it insists on.
    /// Serves: the `model_panel_*` tests, which must not touch a real config file.
    /// Design: `docs/design/web-surface.md` §6.
    /// Up: `start_with_models`.
    /// Down: `crate::ModelSetup`, whose three answers the surface relays.
    /// Verify: `cargo test -p loong-web` (`model_panel`).
    struct OneProvider;

    impl crate::ModelSetup for OneProvider {
        /// Purpose: An empty profile, the case the banner is for.
        /// Contract: `crate::ModelSetup::configured`, answered over the socket.
        fn configured(&self) -> Value {
            json!({"models": []})
        }

        /// Purpose: One hosted provider, which the panel must offer and mark as wanting a key.
        /// Contract: `crate::ModelSetup::providers`, relayed by `model.providers`.
        fn providers(&self) -> Value {
            json!({"providers": [
                {"id": "fake", "label": "Fake", "local": false,
                 "needs_key": true, "key_env": "FAKE_KEY"}
            ]})
        }

        /// Purpose: Write nothing, refuse a keyless probe, and name the models a key would unlock.
        /// Serves: the panel test, as the provider the page would have asked.
        /// Design: `docs/design/web-surface.md` §6.
        /// Up: `crate::ModelSetup::probe`, relayed by `model.probe`.
        /// Down: nothing; it answers from a constant.
        /// Verify: `cargo test -p loong-web` (`model_`).
        fn probe(&self, provider: &str, key: &str) -> Result<Value, String> {
            if key.trim().is_empty() {
                return Err("fake needs an API key".to_string());
            }
            Ok(json!({"provider": provider, "label": "Fake", "models": [
                {"name": "model", "context_window": 32768, "configured": false},
                {"name": "model-pro", "context_window": 131072, "configured": false},
            ]}))
        }

        /// Purpose: Write nothing, refuse a keyless add, and echo the selection it was given.
        /// Serves: the panel test, so the assertion is about what the page sent, not about a file.
        /// Design: `docs/design/web-surface.md` §6.
        /// Up: `crate::ModelSetup::adopt`, relayed by `model.add`.
        /// Down: nothing; it answers from its own arguments.
        /// Verify: `cargo test -p loong-web` (`model_`).
        fn adopt(
            &self,
            provider: &str,
            key: &str,
            models: &[(String, Option<u64>)],
        ) -> Result<Value, String> {
            if key.trim().is_empty() {
                return Err("fake needs an API key".to_string());
            }
            let rows: Vec<Value> = models
                .iter()
                .map(|(name, ctx)| json!({"name": name, "context_window": ctx}))
                .collect();
            Ok(json!({"provider": provider, "label": "Fake", "models": rows}))
        }
    }

    /// Purpose: A child that really answers `model.list`, so a reorder has a list to move in.
    /// Serves: the `reorder_` tests, and the proof that a later list still comes back arranged.
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: `start_with_models` in the reorder test.
    /// Down: nothing; it is a stand-in for `loongagent serve` on a three-model profile.
    /// Verify: `cargo test -p loong-web` (`reorder_`).
    struct Models {
        /// Purpose: The models this "profile" declares, in declaration order.
        names: Vec<String>,
        /// Purpose: Which one the session says is current.
        selected: String,
    }

    impl Models {
        /// Purpose: The `ModelList` shape, always in declaration order — which is the whole
        /// reason the surface has to remember an arrangement of its own.
        /// Contract: what `model.list` and `model.select` answer with.
        fn list(&self) -> Value {
            let models: Vec<Value> = self
                .names
                .iter()
                .map(|name| json!({"name": name, "model": name, "provider": "fake"}))
                .collect();
            json!({"models": models, "selected": self.selected})
        }
    }

    impl Session for Models {
        /// Purpose: Answer `model.list` and `model.select` in declaration order, ignoring order.
        /// Serves: the `reorder_` tests' stand-in child.
        /// Design: `docs/design/web-surface.md` §4.
        /// Up: `dispatch_surface` and `forward` through the seam.
        /// Down: nothing; `serde_json` only.
        /// Verify: `cargo test -p loong-web` (`reorder_`).
        fn exchange(
            &mut self,
            line: &str,
            sink: &mut dyn FnMut(String) -> Result<(), String>,
        ) -> Result<(), String> {
            let request: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let reply = match request["method"].as_str().unwrap_or("") {
                "model.list" => json!({"jsonrpc": "2.0", "id": id, "result": self.list()}),
                "model.select" => {
                    self.selected = request["params"]["name"].as_str().unwrap_or("").to_string();
                    json!({"jsonrpc": "2.0", "id": id, "result": self.list()})
                }
                _ => json!({"jsonrpc": "2.0", "id": id, "result": {"accepted": true}}),
            };
            sink(reply.to_string())
        }
    }

    /// Purpose: The setup that keeps the order it was handed, so the test can see what the
    /// surface asked it to write.
    /// Serves: `reorder_`, which has to tell "written" from "this session only".
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: the reorder test.
    /// Down: `crate::ModelSetup::save_order`.
    /// Verify: `cargo test -p loong-web` (`reorder_`).
    #[derive(Clone, Default)]
    struct OrderSink {
        /// Purpose: The last order the surface asked to be written; `None` until it asks.
        saved: Arc<Mutex<Option<Vec<String>>>>,
        /// Purpose: When set, `save_order` fails, the case of a file that refuses the write.
        refuses: bool,
    }

    impl crate::ModelSetup for OrderSink {
        /// Purpose: Record the order and claim it was written, or refuse it.
        /// Contract: `crate::ModelSetup::save_order`, relayed by `model.reorder`.
        fn save_order(&self, names: &[String]) -> Result<bool, String> {
            if self.refuses {
                return Err("the file disagrees with this list".to_string());
            }
            *self.saved.lock().unwrap() = Some(names.to_vec());
            Ok(true)
        }
    }

    /// Purpose: With three models and the middle one current, ctrl+right moves it one slot and
    /// keeps it lit; ctrl+left twice more walks it to the edge and stops there.
    /// Serves: the reorder keys, which are not the switch keys (web-surface.md §4).
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: `cargo test -p loong-web`.
    /// Down: `move_in_order`, `offerings_in_order` and `reorder_reply`.
    /// Verify: `cargo test -p loong-web` (`reorder_`).
    #[test]
    fn reorder_swaps_one_slot_and_stops_at_the_edge() {
        let names: Vec<String> = ["a", "b", "c"].iter().map(|n| n.to_string()).collect();

        let (order, from, to) = move_in_order(&names, "b", 1).expect("b can move right");
        assert_eq!(order, vec!["a", "c", "b"], "b swaps with c");
        assert_eq!(
            (from, to),
            (1, 2),
            "and the answer says which slots changed"
        );

        // The base a move starts from is the arrangement on screen, not the declaration:
        // otherwise a second ctrl+right would undo the first, since the session keeps
        // answering in declaration order.
        let declared: Vec<String> = ["a", "b", "c"].iter().map(|n| n.to_string()).collect();
        let arranged: Vec<String> = ["a", "c", "b"].iter().map(|n| n.to_string()).collect();
        let shown = shown_order(Some(&arranged), &declared);
        assert_eq!(
            shown, arranged,
            "an arrangement that still fits is kept as it is"
        );
        let (order, _, to) = move_in_order(&shown, "b", -1).expect("b walks back left");
        assert_eq!(
            order,
            vec!["a", "b", "c"],
            "from [a, c, b], left swaps b and c back"
        );
        assert_eq!(to, 1);

        // An arrangement made before a model was added keeps the new one, at the end.
        let stale: Vec<String> = ["c", "a"].iter().map(|n| n.to_string()).collect();
        assert_eq!(shown_order(Some(&stale), &declared), vec!["c", "a", "b"]);
        assert_eq!(
            shown_order(None, &declared),
            declared,
            "no arrangement, no interference"
        );

        let first = move_in_order(&names, "a", -1).expect_err("a is already first");
        assert!(first.contains("first slot"), "{first}");
        let last = move_in_order(&names, "c", 1).expect_err("c is already last");
        assert!(last.contains("last slot"), "{last}");
        let unknown = move_in_order(&names, "zz", 1).expect_err("zz is not in the list");
        assert!(unknown.contains("zz"), "{unknown}");
    }

    /// Purpose: An offering list is re-arranged by name, and a name the arrangement does not
    /// mention — one added since, or one removed — neither disappears nor blocks the rest.
    /// Serves: the reorder answer and the `model.list` rewrite in `forward`.
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: `cargo test -p loong-web`.
    /// Down: `offerings_in_order` and `reorder_reply`.
    /// Verify: `cargo test -p loong-web` (`reorder_`).
    #[test]
    fn reorder_lays_the_order_over_offerings_without_losing_any() {
        let models = json!([
            {"name": "a", "model": "a"},
            {"name": "b", "model": "b"},
            {"name": "c", "model": "c"}
        ]);
        let order: Vec<String> = ["c", "a"].iter().map(|n| n.to_string()).collect();
        let arranged = offerings_in_order(&models, &order);
        let names: Vec<&str> = arranged
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec!["c", "a", "b"],
            "named models come first in the arranged order, the rest follow as they were"
        );

        // The line the browser gets carries the arrangement; a line that is not a model list
        // — an event, a delta, or something that is not JSON — goes through untouched.
        let line =
            json!({"jsonrpc": "2.0", "id": 7, "result": {"models": models, "selected": "a"}});
        let rewritten = reorder_reply(&line.to_string(), &order);
        let parsed: Value = serde_json::from_str(&rewritten).unwrap();
        assert_eq!(parsed["result"]["models"][0]["name"], "c");
        assert_eq!(
            parsed["result"]["selected"], "a",
            "which model is current is not the order's business"
        );
        assert_eq!(reorder_reply("not json", &order), "not json");
        let event =
            json!({"jsonrpc": "2.0", "method": "model.selected", "params": {"selected": "a"}});
        assert_eq!(
            reorder_reply(&event.to_string(), &order),
            event.to_string(),
            "an event is not a list and must not be rewritten"
        );
    }

    /// Purpose: ctrl+right over the socket moves the current model, says which slots changed, and
    /// a later `model.list` still comes back arranged — the child answers in declaration order,
    /// so this is what proves the surface remembers the arrangement (web-surface.md §4).
    /// Serves: `model.reorder`, the reorder half of the keys.
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: `cargo test -p loong-web`.
    /// Down: `dispatch_surface`'s `model.reorder` arm, `ask_child` and `forward`'s rewrite.
    /// Verify: `cargo test -p loong-web` (`reorder_`).
    #[test]
    fn reorder_over_the_socket_moves_the_model_and_survives_a_refresh() {
        let _guard = HOME_LOCK.lock().unwrap();
        let sink = OrderSink::default();
        let written = Arc::clone(&sink.saved);
        let (addr, token) = {
            let (listener, addr) = bind("127.0.0.1", 0).unwrap();
            let token = crate::random_token().unwrap();
            let for_server = token.clone();
            let setup = sink.clone();
            std::thread::spawn(move || {
                crate::serve_web_with_models(
                    listener,
                    for_server,
                    std::env::temp_dir(),
                    Arc::new(|_: &Path, session: Option<&str>| {
                        let id = session.unwrap_or("models").to_string();
                        Ok(crate::Started {
                            session: Arc::new(Mutex::new(Models {
                                names: ["a", "b", "c"].iter().map(|n| n.to_string()).collect(),
                                selected: "b".to_string(),
                            })) as Arc<Mutex<dyn Session>>,
                            session_id: id,
                            channel: None,
                        })
                    }),
                    Arc::new(setup),
                    Arc::new(|_: u32| {}),
                )
            });
            (addr, token)
        };
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        let mut ask = |id: u64, method: &str, params: Value| -> Value {
            socket
                .send(Message::text(
                    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
                        .to_string(),
                ))
                .unwrap();
            let line = loop {
                let line = socket.read().unwrap().into_text().unwrap().to_string();
                // Rows about the workspace or the child arrive first; the answer is the one
                // carrying our id.
                let parsed: Value = serde_json::from_str(&line).unwrap();
                if parsed.get("id") == Some(&json!(id)) {
                    break parsed;
                }
            };
            line
        };

        ask(1, "workspace.list", json!({}));
        ask(2, "session.new", json!({}));

        let moved = ask(3, "model.reorder", json!({"delta": 1}));
        assert_eq!(
            moved["result"]["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["a", "c", "b"],
            "c is where b was and b took its slot: {moved}"
        );
        assert_eq!(
            moved["result"]["selected"], "b",
            "the move is not a switch: {moved}"
        );
        assert_eq!(
            moved["result"]["moved"],
            json!({"name": "b", "from": 2, "to": 3})
        );
        assert_eq!(moved["result"]["persisted"], json!(true), "{moved}");
        assert_eq!(
            written.lock().unwrap().clone(),
            Some(vec!["a".to_string(), "c".to_string(), "b".to_string()]),
            "the surface asked for exactly the order it drew"
        );

        // A refresh: the child answers ["a","b","c"] again, and the bar must not snap back.
        let refreshed = ask(4, "model.list", json!({"session_id": "models"}));
        assert_eq!(
            refreshed["result"]["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["name"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["a", "c", "b"],
            "the arrangement outlives a fresh model.list: {refreshed}"
        );

        // b is last now: ctrl+right has nowhere to go and says so instead of wrapping.
        let edge = ask(5, "model.reorder", json!({"delta": 1}));
        assert!(
            edge["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("last slot"),
            "the edge is an answer, not a wrap-around: {edge}"
        );
    }

    /// Purpose: A file that refuses the write does not unmake the move: the bar is arranged and
    /// the refusal is reported beside it (the same split the TUI makes).
    /// Serves: the `warning` half of `model.reorder`.
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: `cargo test -p loong-web`.
    /// Down: `dispatch_surface`'s `model.reorder` arm and its `save_order` result handling.
    /// Verify: `cargo test -p loong-web` (`reorder_`).
    #[test]
    fn reorder_keeps_the_arrangement_when_the_file_refuses() {
        let _guard = HOME_LOCK.lock().unwrap();
        let (addr, token) = {
            let (listener, addr) = bind("127.0.0.1", 0).unwrap();
            let token = crate::random_token().unwrap();
            let for_server = token.clone();
            std::thread::spawn(move || {
                crate::serve_web_with_models(
                    listener,
                    for_server,
                    std::env::temp_dir(),
                    Arc::new(|_: &Path, session: Option<&str>| {
                        let id = session.unwrap_or("models").to_string();
                        Ok(crate::Started {
                            session: Arc::new(Mutex::new(Models {
                                names: ["a", "b"].iter().map(|n| n.to_string()).collect(),
                                selected: "a".to_string(),
                            })) as Arc<Mutex<dyn Session>>,
                            session_id: id,
                            channel: None,
                        })
                    }),
                    Arc::new(OrderSink {
                        saved: Arc::new(Mutex::new(None)),
                        refuses: true,
                    }),
                    Arc::new(|_: u32| {}),
                )
            });
            (addr, token)
        };
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        let mut ask = |id: u64, method: &str, params: Value| -> Value {
            socket
                .send(Message::text(
                    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
                        .to_string(),
                ))
                .unwrap();
            loop {
                let line = socket.read().unwrap().into_text().unwrap().to_string();
                let parsed: Value = serde_json::from_str(&line).unwrap();
                if parsed.get("id") == Some(&json!(id)) {
                    break parsed;
                }
            }
        };
        ask(1, "workspace.list", json!({}));
        ask(2, "session.new", json!({}));
        let moved = ask(3, "model.reorder", json!({"delta": 1}));
        assert_eq!(
            moved["result"]["models"][0]["name"], "b",
            "the bar still rearranged: {moved}"
        );
        assert_eq!(
            moved["result"]["persisted"],
            json!(false),
            "nothing reached the file, so nothing may claim it did: {moved}"
        );
        assert_eq!(
            moved["result"]["warning"], "the file disagrees with this list",
            "and the refusal is handed back to be shown: {moved}"
        );
    }

    /// Purpose: the panel's three reads answer over the socket, and an adopt reloads the idle
    /// children so the next session starts on the config just written (web-surface.md §6).
    #[test]
    fn model_panel_answers_and_reloads_the_children() {
        let _guard = HOME_LOCK.lock().unwrap();
        let base = std::env::temp_dir().join(format!("loong-models-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        std::fs::create_dir_all(&home_dir).unwrap();
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        let (addr, token) = start_with_models(Arc::new(OneProvider));
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        let mut ask = |id: u64, method: &str, params: Value| -> Value {
            socket
                .send(Message::text(
                    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string(),
                ))
                .unwrap();
            let line = socket.read().unwrap().into_text().unwrap().to_string();
            serde_json::from_str(&line).unwrap()
        };

        // Seat the tab, then start a child: the adopt below has one to reload.
        let listed = ask(1, "workspace.list", json!({}));
        assert!(listed.get("result").is_some(), "{listed}");
        let opened = ask(2, "session.new", json!({}));
        assert!(opened.get("result").is_some(), "{opened}");

        let empty = ask(3, "model.configured", json!({}));
        assert_eq!(
            empty["result"]["models"],
            json!([]),
            "a profile with no model says so, which is what the banner reads: {empty}"
        );

        let catalogue = ask(4, "model.providers", json!({}));
        assert_eq!(
            catalogue["result"]["providers"][0]["id"], "fake",
            "the panel offers the catalogue: {catalogue}"
        );

        // The list arrives before anything is written, and asking for it without a key is
        // refused in the provider's own words: the panel has rows to tick before it has a
        // thing to write (web-surface.md §6).
        let offered = ask(
            5,
            "model.probe",
            json!({"provider": "fake", "key": "sk-test"}),
        );
        assert_eq!(
            offered["result"]["models"][1]["name"], "model-pro",
            "the provider's models are listed for ticking: {offered}"
        );
        let keyless = ask(6, "model.probe", json!({"provider": "fake", "key": ""}));
        assert_eq!(
            keyless["error"]["message"], "fake needs an API key",
            "a keyless probe is refused, not guessed: {keyless}"
        );

        // A key the setup refuses comes back as an error, and nothing is reloaded.
        let refused = ask(
            7,
            "model.add",
            json!({"provider": "fake", "key": "", "models": []}),
        );
        assert_eq!(
            refused["error"]["message"], "fake needs an API key",
            "a keyless add is refused in the provider's own words: {refused}"
        );

        // A page that sends no selection is a broken page, not a request to tick nothing:
        // "nothing ticked" deletes models, so it may never be assumed (web-surface.md §6).
        let unselected = ask(
            8,
            "model.add",
            json!({"provider": "fake", "key": "sk-test"}),
        );
        assert_eq!(
            unselected["error"]["message"], "model.add needs models: [{name, context_window}]",
            "the selection is required: {unselected}"
        );

        // One model ticked out of two: one block is written, not the whole catalogue.
        let added = ask(
            9,
            "model.add",
            json!({"provider": "fake", "key": "sk-test",
                   "models": [{"name": "model", "context_window": 32768}]}),
        );
        assert_eq!(
            added["result"]["models"],
            json!([{"name": "model", "context_window": 32768}]),
            "exactly the ticked model is written, and it is the whole set: {added}"
        );
        assert_eq!(
            added["result"]["restarted"],
            json!(1),
            "the live child is dropped so the next one reads the new config: {added}"
        );
        assert_eq!(
            added["result"]["waiting"],
            json!(0),
            "nothing was mid-turn, so nothing was left stale: {added}"
        );

        std::env::remove_var("LOONGAGENT_HOME");
        std::fs::remove_dir_all(&base).ok();
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
        // Every forwarded line names the session that wrote it, so a page showing another
        // session can leave it alone (web-surface.md §3).
        assert!(answer.contains("\"session_id\":\"s\""), "{answer}");
    }

    /// Purpose: a bridge that dies under a request still answers it — the page settles a
    /// request on the line carrying its id, so an event alone left the turn's spinner
    /// turning over a turn nothing could end (web-composer.md §5).
    #[test]
    fn a_dead_child_still_answers_the_request() {
        /// Purpose: A session that fails every exchange, to pin the failure answer.
        ///
        /// Serves: `a_dead_child_still_answers_the_request`.
        /// Design: `docs/design/web-composer.md` §5.
        /// Up: that test only.
        /// Down: `Session::exchange`.
        /// Verify: `cargo test -p loong-web`.
        struct Broken;
        impl Session for Broken {
            /// Purpose: Fail with a message the page can show.
            /// Contract: always `Err("the child died")`; the sink is never called.
            fn exchange(
                &mut self,
                _line: &str,
                _sink: &mut dyn FnMut(String) -> Result<(), String>,
            ) -> Result<(), String> {
                Err("the child died".to_string())
            }
        }

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
                    let id = session.unwrap_or("broken").to_string();
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Broken)) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
                }),
            )
        });

        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        // Seat the tab first, then speak the TUI's `prompt.submit`.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.list","params":{}}).to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":7,"method":"prompt.submit",
                       "params":{"session_id":"s","text":"hi"}})
                .to_string(),
            ))
            .unwrap();
        // The event that says why, then the answer that lets the page stop the turn.
        let mut answer = String::new();
        for _ in 0..2 {
            let line = socket.read().unwrap().into_text().unwrap().to_string();
            if line.contains("\"id\":7") {
                answer = line;
                break;
            }
            assert!(line.contains("client.error"), "{line}");
        }
        assert!(
            answer.contains("the child died"),
            "a failed bridge answers: {answer}"
        );
        assert!(answer.contains("\"error\""), "{answer}");
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
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
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
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
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

        // The page's next move is `session.attach`, asking the kernel what it still
        // waits for: a replayed question is only answerable if this child names it
        // (web-surface.md §4). It must reach the child rather than be refused by the
        // surface for naming no session.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":4,"method":"session.attach","params":{"cwd":"","client":"web"}}).to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&answer).unwrap();
        assert!(
            value.get("error").is_none(),
            "the tab is attached, so the child must answer the question the page asks \
             at boot: {answer}"
        );
        assert!(
            value.get("result").is_some(),
            "an attach answer, not a refusal: {answer}"
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

    /// Purpose: the rail lists the workspace added last first and the launch workspace
    /// last, so a workspace the user just added appears at the top of the list.
    #[test]
    fn session_tree_lists_the_newest_workspace_first() {
        let _guard = HOME_LOCK.lock().unwrap();
        let base = std::env::temp_dir().join(format!("loong-tree-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        let workspace = base.join("workspace");
        let first = base.join("first-added");
        let second = base.join("second-added");
        for d in [&workspace, &first, &second] {
            std::fs::create_dir_all(d).unwrap();
        }
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
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
                }),
            )
        });
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        for (id, dir) in [(1, &first), (2, &second)] {
            socket
                .send(Message::text(
                    json!({"jsonrpc":"2.0","id":id,"method":"workspace.add",
                           "params":{"dir":dir}})
                    .to_string(),
                ))
                .unwrap();
            let _ = socket.read().unwrap();
        }
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":3,"method":"session.tree","params":{}}).to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&answer).unwrap();
        let workspaces = value["result"]["workspaces"].as_array().unwrap();
        let dirs: Vec<String> = workspaces
            .iter()
            .map(|w| w["dir"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            dirs,
            vec![
                second.display().to_string(),
                first.display().to_string(),
                workspace.display().to_string(),
            ],
            "the last workspace added is listed first: {workspaces:?}"
        );
        // `index` still names the workspace for the calls that take one, so the order
        // shown on the page never changes which workspace a click means.
        assert_eq!(workspaces[0]["index"].as_u64(), Some(2));
        assert_eq!(workspaces[2]["index"].as_u64(), Some(0));
        assert_eq!(workspaces[2]["home"].as_bool(), Some(true));

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
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
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
        // The rail shows the newest workspace first, so find one by the `index` it
        // carries rather than by where it sits in the array.
        let at = |idx: u64| -> &Value {
            workspaces
                .iter()
                .find(|w| w["index"].as_u64() == Some(idx))
                .unwrap_or_else(|| panic!("no workspace of index {idx}: {workspaces:?}"))
        };
        // Home workspace carries s1; the other carries s9.
        let find = |idx: u64, id: &str| {
            at(idx)
                .get("sessions")
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .any(|s| s.get("id").and_then(Value::as_str) == Some(id))
        };
        assert!(find(0, "s1"), "home workspace has s1: {workspaces:?}");
        assert!(find(1, "s9"), "the other workspace has s9: {workspaces:?}");
        // Sessions carry running/busy flags.
        let s1 = &at(0)["sessions"]
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

    /// Purpose: `session.new {workspace}` starts the session in the workspace it names, not
    /// in the tab's seat, so the `＋` on any rail row adds to that row's workspace.
    /// Serves: the `session.new` branch of `dispatch_surface`.
    /// Design: `docs/design/web-surface.md` §2, §4.
    /// Up: the page's `newSession(w)` in `crates/web/src/app.js`.
    /// Down: `ensure_child`, `session.tree`.
    /// Verify: `cargo test -p loong-web` (`session_new_lands_in_the_workspace_it_names`).
    #[test]
    fn session_new_lands_in_the_workspace_it_names() {
        let _guard = HOME_LOCK.lock().unwrap();
        let base = std::env::temp_dir().join(format!("loong-new-seat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        let workspace = base.join("workspace");
        let other = base.join("other");
        for d in [&workspace, &other] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        // Every child the surface starts, by the directory it was started in: the fake
        // session writes no log, so the tree cannot show where the session landed and the
        // directory the factory was handed is the evidence instead.
        let started_in: Arc<Mutex<Vec<std::path::PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&started_in);
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = crate::random_token().unwrap();
        let for_server = token.clone();
        let home = workspace.clone();
        std::thread::spawn(move || {
            serve_web(
                listener,
                for_server,
                home,
                Arc::new(move |dir: &Path, session: Option<&str>| {
                    record.lock().unwrap().push(dir.to_path_buf());
                    let id = session.unwrap_or("echo").to_string();
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Echo(vec![]))) as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
                }),
            )
        });
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.add","params":{"dir":other}})
                    .to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();
        // The tab is seated on the home workspace (index 0); the `＋` is on the other row.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":2,"method":"session.new","params":{"workspace":1}})
                    .to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&answer).unwrap();
        assert!(
            value.get("result").is_some(),
            "a session, not an error: {value}"
        );
        let dirs: Vec<String> = started_in
            .lock()
            .unwrap()
            .iter()
            .map(|d| {
                d.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(
            dirs,
            vec!["other".to_string()],
            "the one child started is the one workspace the call named, not the seat"
        );
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":3,"method":"session.tree","params":{}}).to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(
            value["result"]["seat"].as_u64(),
            Some(1),
            "the tab moved to the workspace it added the session to: {value}"
        );
        // A workspace index the catalogue does not have is a refusal, not a quiet fallback
        // to the seat.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":4,"method":"session.new","params":{"workspace":9}})
                    .to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&answer).unwrap();
        assert!(
            value.get("error").is_some(),
            "no workspace 9 is an error: {value}"
        );

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

    /// Purpose: a session whose `prompt.submit` never answers until the test opens its gate,
    /// and whose pid is a number the test can recognise.
    /// Serves: `stop_ends_the_running_turn`.
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: the factory `start_for_stop` hands to `serve_web_with_models`.
    /// Down: `Echo`, for every method that is not a prompt.
    /// Verify: `cargo test -p loong-web` (`stop_`).
    /// Contract: only a prompt blocks; `session.new` must answer or the tab never learns
    /// its session id and never becomes busy.
    struct Stalled(Arc<(Mutex<bool>, std::sync::Condvar)>);

    impl crate::Session for Stalled {
        /// Purpose: Name the pid a Stop is expected to signal.
        /// Contract: 4242 is a number no real child has, so the recording closure proves
        /// the route took this session's pid and not something else.
        fn pid(&self) -> Option<u32> {
            Some(4242)
        }

        /// Purpose: Hold a prompt open until the gate opens, and answer every other method.
        /// Serves: `stop_ends_the_running_turn`, which needs a turn that is still running.
        /// Design: `docs/design/web-surface.md` §4.
        /// Up: `app::forward`, which marks the child busy and then blocks here.
        /// Down: `Echo`, for the methods that must answer.
        /// Verify: `cargo test -p loong-web stop_`.
        fn exchange(
            &mut self,
            line: &str,
            sink: &mut dyn FnMut(String) -> Result<(), String>,
        ) -> Result<(), String> {
            if line.contains("prompt.submit") {
                let (open, condvar) = &*self.0;
                let mut opened = open.lock().unwrap();
                while !*opened {
                    opened = condvar.wait(opened).unwrap();
                }
                drop(opened);
            }
            Echo(vec![]).exchange(line, sink)
        }
    }

    /// Purpose: a surface whose children stall on a prompt, recording every pid Stop signals.
    /// Serves: `stop_ends_the_running_turn`.
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: that test.
    /// Down: `serve_web_with_models`, with `Stalled` children and a recording `StopSignal`.
    /// Verify: `cargo test -p loong-web` (`stop_`).
    fn start_for_stop(
        gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
        signalled: Arc<Mutex<Vec<u32>>>,
    ) -> (std::net::SocketAddr, String) {
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = crate::random_token().unwrap();
        let for_server = token.clone();
        std::thread::spawn(move || {
            crate::serve_web_with_models(
                listener,
                for_server,
                std::env::temp_dir(),
                Arc::new(move |_: &Path, session: Option<&str>| {
                    let id = session.unwrap_or("stall").to_string();
                    Ok(crate::Started {
                        session: Arc::new(Mutex::new(Stalled(Arc::clone(&gate))))
                            as Arc<Mutex<dyn Session>>,
                        session_id: id,
                        channel: None,
                    })
                }),
                Arc::new(crate::NoModelSetup),
                Arc::new(move |pid: u32| signalled.lock().unwrap().push(pid)),
            )
        });
        (addr, token)
    }

    /// Purpose: POST `/api/interrupt` and hand back the status line and the body.
    /// Serves: `stop_ends_the_running_turn`.
    /// Design: `docs/design/web-surface.md` §4.
    /// Up: that test.
    /// Down: `TcpStream`, the route's answer.
    /// Verify: `cargo test -p loong-web` (`stop_`).
    fn interrupt_request(
        addr: std::net::SocketAddr,
        token: &str,
        session_id: &str,
    ) -> (String, String) {
        let body = json!({ "session_id": session_id }).to_string();
        let mut stream = TcpStream::connect(addr).unwrap();
        let request = format!(
            "POST /api/interrupt?token={token} HTTP/1.1\r\nHost: x\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        let status = answer.lines().next().unwrap_or_default().to_string();
        (status, answer)
    }

    /// Purpose: Stop ends the turn that is running, and refuses in words when none is
    /// (web-surface.md §4).
    #[test]
    fn stop_ends_the_running_turn() {
        let gate: Arc<(Mutex<bool>, std::sync::Condvar)> =
            Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let signalled: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
        let (addr, token) = start_for_stop(Arc::clone(&gate), Arc::clone(&signalled));

        // Nothing running: a refusal, not a signal. A signal sent to an idle child would
        // land on whatever it does next.
        let (status, answer) = interrupt_request(addr, &token, "s1");
        assert!(status.contains("409"), "{status}");
        assert!(answer.contains("no turn is running"), "{answer}");
        assert!(
            signalled.lock().unwrap().is_empty(),
            "signalled an idle child"
        );

        // A turn in flight: the gate holds the child inside `exchange`, which is exactly
        // the state a Stop has to break.
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        let mut ask = |id: u64, method: &str, params: Value| -> Value {
            socket
                .send(Message::text(
                    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
                        .to_string(),
                ))
                .unwrap();
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap()
        };
        // Seat the tab, then open a session: `session.new` is a surface method, so it
        // answers with the id the child was started under without asking the child.
        ask(1, "workspace.list", json!({}));
        let opened = ask(2, "session.new", json!({}));
        let session_id = opened["result"]["session_id"].as_str().unwrap().to_string();
        // Sent, not awaited: the child blocks on it, which is the point.
        socket
            .send(Message::text(
                json!({"jsonrpc": "2.0", "id": 3, "method": "prompt.submit",
                       "params": {"text": "hold the turn open"}})
                .to_string(),
            ))
            .unwrap();
        // The route reports a turn as running only once `forward` has marked it busy; retry
        // briefly rather than sleep a guessed number of milliseconds.
        let mut answered = (String::new(), String::new());
        for _ in 0..100 {
            answered = interrupt_request(addr, &token, &session_id);
            if answered.0.contains("200") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(answered.0.contains("200 OK"), "{}", answered.1);
        assert!(
            answered.1.contains("\"interrupted\":true"),
            "{}",
            answered.1
        );
        assert_eq!(
            *signalled.lock().unwrap(),
            vec![4242],
            "wrong pid signalled"
        );

        // And the turn ends because the child stopped, not because the socket closed: let
        // the gate open and read the answer the page would have seen.
        let (open, condvar) = &*gate;
        *open.lock().unwrap() = true;
        condvar.notify_all();
        let ended = socket.read().unwrap();
        let ended: Value = serde_json::from_str(ended.to_text().unwrap()).unwrap();
        assert_eq!(ended["id"], json!(3), "{ended}");
    }

    /// Purpose: prove a busy session still shows its past: the transcript comes from the log,
    /// answered at once, without waiting for the turn that holds the child.
    #[test]
    fn replay_answers_from_the_log_while_a_turn_is_running() {
        let _guard = HOME_LOCK.lock().unwrap();
        let base = std::env::temp_dir().join(format!("loong-replay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home_dir = base.join("home");
        let workspace = base.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let dir = sessions::session_dir(&home_dir, &workspace);
        std::fs::create_dir_all(&dir).unwrap();
        // Two events and a torn tail, which is what a log being written right now looks like.
        let one = json!({
            "kind": "message.appended", "seq": 1, "time_ms": 1, "id": "e1",
            "data": {"role": "user", "text": "what did we decide"}
        });
        let two = json!({
            "kind": "message.appended", "seq": 2, "time_ms": 2, "id": "e2",
            "data": {"role": "assistant", "text": "the small thing first"}
        });
        std::fs::write(
            dir.join("s1.jsonl"),
            format!("{one}\n{two}\n{{\"kind\":\"message.ap"),
        )
        .unwrap();
        std::env::set_var("LOONGAGENT_HOME", &home_dir);

        /// A child that never answers, standing in for a turn still writing.
        /// Purpose: Hold the session's bridge so the replay cannot come from the child.
        ///
        /// Serves: `replay_answers_from_the_log_while_a_turn_is_running`.
        /// Design: `docs/design/web-surface.md` §5.
        /// Up: the starter this test passes to `start_with`.
        /// Down: nothing.
        /// Verify: `cargo test -p loong-web` (`replay_answers_from_the_log_while_a_turn_is_running`).
        struct Silent;

        impl Session for Silent {
            /// Purpose: Never answer, so any line the page receives must have come from the log.
            ///
            /// Serves: `replay_answers_from_the_log_while_a_turn_is_running`.
            /// Design: `docs/design/web-surface.md` §5.
            /// Up: the turn `forward` starts on this session.
            /// Down: nothing.
            /// Verify: `cargo test -p loong-web` (`replay_answers_from_the_log_while_a_turn_is_running`).
            /// Contract: blocks until the socket is closed, then reports the turn as failed.
            fn exchange(
                &mut self,
                _line: &str,
                _sink: &mut dyn FnMut(String) -> Result<(), String>,
            ) -> Result<(), String> {
                std::thread::sleep(std::time::Duration::from_secs(5));
                Err("the test ended".to_string())
            }
        }

        let (addr, token) = start_with(|_: &Path, _: Option<&str>| {
            Ok(crate::Started {
                session: Arc::new(Mutex::new(Silent)) as Arc<Mutex<dyn Session>>,
                session_id: "s1".to_string(),
                channel: None,
            })
        });
        let (mut socket, _) = tungstenite::connect(
            format!("ws://{addr}/ws?token={token}")
                .into_client_request()
                .unwrap(),
        )
        .unwrap();
        // The planted directory becomes the tab's workspace; the rail does this before it
        // lists anything, and the seat is what names the log.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":1,"method":"workspace.add","params":{"dir":workspace}})
                    .to_string(),
            ))
            .unwrap();
        let _ = socket.read().unwrap();
        // Open the session, which starts a child that will never answer...
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":2,"method":"session.open","params":{"session_id":"","open":"s1"}}).to_string(),
            ))
            .unwrap();
        let opened = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(opened.contains("\"s1\""), "{opened}");
        // ...then ask for the transcript the page draws when a session is opened.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":3,"method":"session.replay","params":{"session_id":"s1","from_seq":0}})
                    .to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        let value: Value = serde_json::from_str(&answer).unwrap();
        assert_eq!(
            value["id"],
            json!(3),
            "the child answered, not the log: {answer}"
        );
        let events = value["result"]["events"]
            .as_array()
            .unwrap_or_else(|| panic!("a replay answer carries events: {answer}"));
        assert_eq!(
            events.len(),
            2,
            "the torn tail must be dropped, not fatal: {answer}"
        );
        assert_eq!(events[0]["seq"], json!(1), "{answer}");
        assert_eq!(
            events[1]["data"]["text"],
            json!("the small thing first"),
            "{answer}"
        );
        // A session no seat has is refused rather than guessed at.
        socket
            .send(Message::text(
                json!({"jsonrpc":"2.0","id":4,"method":"session.replay","params":{"session_id":"nope"}})
                    .to_string(),
            ))
            .unwrap();
        let answer = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(
            answer.contains("nope"),
            "an unknown session is refused: {answer}"
        );

        std::env::remove_var("LOONGAGENT_HOME");
        std::fs::remove_dir_all(&base).ok();
    }

    /// Purpose: prove a turn no longer owns the socket: a workspace request is answered while a
    /// model is still writing.
    #[test]
    fn a_surface_answer_arrives_while_a_turn_is_running() {
        /// A child whose turn takes long enough to watch.
        /// Purpose: Stand in for a model that is still writing.
        ///
        /// Serves: `a_surface_answer_arrives_while_a_turn_is_running`.
        /// Design: `docs/design/web-surface.md` §3.
        /// Up: the starter this test passes to `start_with`.
        /// Down: nothing; it answers every request itself.
        /// Verify: `cargo test -p loong-web` (`a_surface_answer_arrives_while_a_turn_is_running`).
        struct Working;

        impl Session for Working {
            /// Purpose: Hold the turn for 400 ms so the surface has to answer around it.
            /// Contract: answers with the request's own id; the delay is the whole point.
            fn exchange(
                &mut self,
                line: &str,
                sink: &mut dyn FnMut(String) -> Result<(), String>,
            ) -> Result<(), String> {
                let request: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                if request.get("method").and_then(Value::as_str) == Some("prompt.submit") {
                    std::thread::sleep(std::time::Duration::from_millis(400));
                }
                sink(json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}}).to_string())
            }
        }

        let (addr, token) = start_with(|_: &Path, _: Option<&str>| {
            Ok(crate::Started {
                session: Arc::new(Mutex::new(Working)) as Arc<Mutex<dyn Session>>,
                session_id: "s-1".to_string(),
                channel: None,
            })
        });
        let (mut socket, _) = tungstenite::connect(
            format!("ws://{addr}/ws?token={token}")
                .into_client_request()
                .unwrap(),
        )
        .unwrap();
        // The turn starts and holds its child's bridge...
        socket
            .send(Message::text(
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "prompt.submit",
                    "params": {"session_id": "s-1", "text": "hello"}
                })
                .to_string(),
            ))
            .unwrap();
        // ...and the page may still ask the surface things: this answer must arrive before
        // the turn's own, which is 400ms away.
        socket
            .send(Message::text(
                json!({"jsonrpc": "2.0", "id": 2, "method": "workspace.list"}).to_string(),
            ))
            .unwrap();
        let first: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(first["id"], json!(2), "a turn blocked the surface: {first}");
        let turn: Value = serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(turn["id"], json!(1), "{turn}");
    }

    /// Purpose: prove the YOLO switch reaches a running turn over the control channel instead
    /// of waiting for it, and that the surface answers at once rather than refusing.
    #[test]
    fn yolo_reaches_a_running_child_out_of_band() {
        /// A child that holds its bridge for the whole turn, the way a real one does.
        /// A child whose turn does not end until the test lets go.
        /// Purpose: Hold the bridge for as long as the test needs, so the YOLO switch has to
        /// be reached out of band.
        ///
        /// Serves: `yolo_reaches_a_running_child_out_of_band`.
        /// Design: `docs/design/web-surface.md` §4; ADR-0019.
        /// Up: the starter this test passes to `start_with`.
        /// Down: nothing; it answers only after `open` is released.
        /// Verify: `cargo test -p loong-web` (`yolo_reaches_a_running_child_out_of_band`).
        struct Busy {
            /// Purpose: The test's own release: the turn ends when the sender is dropped.
            open: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
        }

        impl Session for Busy {
            /// Purpose: Block a prompt until the test lets the turn end.
            ///
            /// Serves: `yolo_reaches_a_running_child_out_of_band`, which needs a turn that
            /// does not end on its own.
            /// Design: `docs/design/web-surface.md` §4.
            /// Up: the turn `forward` starts on this session.
            /// Down: nothing.
            /// Verify: `cargo test -p loong-web` (`yolo_reaches_a_running_child_out_of_band`).
            /// Contract: takes the request's id from its own line and waits only on
            /// `prompt.submit`, so the test can ask the surface other things meanwhile.
            fn exchange(
                &mut self,
                line: &str,
                sink: &mut dyn FnMut(String) -> Result<(), String>,
            ) -> Result<(), String> {
                let request: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                if request.get("method").and_then(Value::as_str) == Some("prompt.submit") {
                    // The turn does not end until the test lets go.
                    let _ = self.open.lock().unwrap().recv();
                }
                sink(json!({"jsonrpc": "2.0", "id": id, "result": {"ok": true}}).to_string())
            }
        }

        let (keep, open) = std::sync::mpsc::channel::<()>();
        let open = Arc::new(Mutex::new(open));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let asked_by_channel = Arc::clone(&asked);
        let (addr, token) = start_with(move |_: &Path, _: Option<&str>| {
            Ok(crate::Started {
                session: Arc::new(Mutex::new(Busy {
                    open: Arc::clone(&open),
                })) as Arc<Mutex<dyn Session>>,
                session_id: "s-1".to_string(),
                channel: Some(Arc::new({
                    let asked = Arc::clone(&asked_by_channel);
                    move |on: bool| {
                        asked.lock().unwrap().push(on);
                        Ok(())
                    }
                })),
            })
        });
        let (mut socket, _) = tungstenite::connect(
            format!("ws://{addr}/ws?token={token}")
                .into_client_request()
                .unwrap(),
        )
        .unwrap();
        socket
            .send(Message::text(
                json!({
                    "jsonrpc": "2.0", "id": 1, "method": "prompt.submit",
                    "params": {"session_id": "s-1", "text": "hello"}
                })
                .to_string(),
            ))
            .unwrap();
        // The turn is running and holds the bridge; the switch still has to work.
        socket
            .send(Message::text(
                json!({
                    "jsonrpc": "2.0", "id": 2, "method": "permissions.set",
                    "params": {"session_id": "s-1", "auto_approve": true}
                })
                .to_string(),
            ))
            .unwrap();
        let answer: Value =
            serde_json::from_str(socket.read().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(answer["id"], json!(2), "{answer}");
        assert_eq!(answer["result"]["auto_approve"], json!(true), "{answer}");
        assert_eq!(
            answer["result"]["queued"],
            json!(false),
            "the switch waited for the turn: {answer}"
        );
        assert_eq!(
            *asked.lock().unwrap(),
            vec![true],
            "the child was never asked"
        );
        drop(keep);
    }

    /// Purpose: prove the tab of a line is written down, so a page showing one session can drop
    /// another's events instead of painting them into the wrong chat.
    #[test]
    fn a_forwarded_line_says_which_session_wrote_it() {
        let tagged = tag_line(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#, "s-1");
        let read: Value = serde_json::from_str(&tagged).unwrap();
        assert_eq!(read["session_id"], json!("s-1"), "{tagged}");
        assert_eq!(read["id"], json!(1), "{tagged}");
        // A line the surface cannot read is passed through: losing it would lose the only copy.
        assert_eq!(tag_line("not json", "s-1"), "not json");
    }
}
