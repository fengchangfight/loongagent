//! Purpose: The worker thread: own the client, run each blocking command, and send the turn's events back to the screen.
//!
//! # Contract
//! - **Serves:** `surface::run` and the screen's `ToWorker`/`FromWorker` channels.
//! - **Design:** `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §1 and §5 for the new-session and approval-resume paths.
//! - **Upstream:** `surface::run` calls `spawn_worker`; the screen sends `ToWorker` commands.
//! - **Downstream:** `loong_tui::Client`'s methods and `Renderer`, and `FromWorker`, which `App::drain` reads.
//! - **Verify:** `cargo test -p loong-tui-app` · `./scripts/check.sh`
use super::*;

/// Purpose: Start the client thread and return its command and event channels, numbering folds from the replayed transcript.
/// Serves: `surface::run` and the worker tests.
/// Design: `docs/design/surfaces.md` §3; `docs/design/sessions-lineage.md` §1 and §5.
/// Up: `surface::run`; the tests.
/// Down: `ToWorker` commands to `Client::{submit_with, respond, set_auto_approve, expand_last_tool_result, expand_tool_result, models, select_model, new_session, context, pending_decision, pending_approval}`, and `FromWorker` back to `App::drain`.
/// Verify: `cargo test -p loong-tui-app`.
/// Own the client on a worker thread so a blocking turn never blocks the screen.
///
/// `folded` is how many folds the transcript already shows, so the worker can number the
/// ones it streams next: they are the same list the keyboard walks, and a fold's number has
/// to mean the same thing on both sides of the channel.
pub fn spawn_worker<T: Transport + Send + 'static>(
    mut client: Client<T>,
    folded: usize,
) -> (Sender<ToWorker>, Receiver<FromWorker>) {
    let (to_worker, commands) = channel::<ToWorker>();
    let (events, from_worker) = channel::<FromWorker>();
    std::thread::spawn(move || {
        // Where the next fold the turn writes will sit among the ones already on screen.
        let mut folds = folded;
        // The decision whose detail has been sent, so re-stating the same one does not
        // re-send it: the panel would lose the highlighted answer on every state change.
        let mut asked: Option<String> = None;
        while let Ok(command) = commands.recv() {
            let mut refreshed = false;
            let result = match command {
                ToWorker::Submit(text) => {
                    // Deltas go out as they arrive; the finished lines follow.
                    let _ = events.send(FromWorker::Lines(Vec::new()));
                    let mut renderer = loong_tui::Renderer::default();
                    // What the turn is doing is named by the call events themselves. The
                    // detail of a decision is assembled by the client and sent after every
                    // command, so it covers the resume path too.
                    let outcome = client.submit_with(&text, &mut |method, params| {
                        if method == loong_schema::generated::notification::MODEL_DELTA {
                            if let Some(piece) = params.get("text").and_then(|t| t.as_str()) {
                                let _ = events.send(FromWorker::Delta(piece.to_string()));
                            }
                            return;
                        }
                        // The work itself, the moment it is recorded — this is the
                        // difference between watching an agent and waiting for one.
                        // The person's own line is already on screen — it was pushed the
                        // moment they pressed enter, so it appears instantly. The event
                        // that comes back is that same line, and rendering it again is how
                        // a prompt appeared twice. History replay is a different path and
                        // still renders it (see `Client::replay`).
                        if kind_of(params) == loong_schema::generated::event::MESSAGE_APPENDED
                            && params
                                .get("data")
                                .and_then(|data| data.get("role"))
                                .and_then(|role| role.as_str())
                                == Some("user")
                        {
                            return;
                        }
                        if let Some(line) = renderer.render(params) {
                            let kind = params.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                            // The summary is what the transcript keeps and what this line
                            // is; the whole of it is what the keyboard may ask for while
                            // the turn is still running, and it is on this very message.
                            // Sending it now is what makes opening a fold a local act.
                            if kind == loong_schema::generated::event::TOOL_RESULT
                                && loong_tui::folded_output(&line)
                            {
                                if let Some(full) = loong_tui::render_event_full(params) {
                                    let _ = events.send(FromWorker::Fold {
                                        at: folds,
                                        lines: full.split('\n').map(|l| l.to_string()).collect(),
                                    });
                                }
                                folds += 1;
                            }
                            if kind == loong_schema::generated::event::TOOL_CALL {
                                let _ = events.send(FromWorker::Working(line.clone()));
                            }
                            let _ = events.send(FromWorker::Lines(vec![line]));
                        }
                    });
                    // Everything above was already sent as it happened; the returned
                    // lines are the same ones, and sending them again would double them.
                    outcome.map(|_| Vec::new())
                }
                ToWorker::Respond {
                    request_id,
                    granted,
                    scope,
                } => client.respond(&request_id, granted, scope, None),
                ToWorker::Yolo(on) => match client.set_auto_approve(on) {
                    Ok(state) => {
                        let _ = events.send(FromWorker::Yolo(state));
                        Ok(Vec::new())
                    }
                    Err(e) => Err(e),
                },
                ToWorker::Expand => {
                    // The session's own log holds the untouched output; the transcript
                    // holds only the summary. Asking the log is what makes `/expand`
                    // honest rather than a cache of what was already printed.
                    match client.expand_last_tool_result() {
                        Ok(lines) if lines.is_empty() => {
                            let _ = events.send(FromWorker::Lines(vec![
                                "· no tool result to expand yet".to_string(),
                            ]));
                        }
                        Ok(lines) => {
                            let _ = events.send(FromWorker::Lines(lines));
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
                ToWorker::ExpandAt { at } => {
                    // The reader picked a specific summary line off the screen; the log is
                    // asked for that same one, by the number both sides count it by.
                    match client.expand_tool_result(at) {
                        Ok(lines) => {
                            let _ = events.send(FromWorker::Expanded { at, lines });
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
                ToWorker::RefreshModels => {
                    match client.models() {
                        Ok(list) => {
                            let _ = events.send(FromWorker::Models(Box::new(list)));
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
                ToWorker::SelectModel(name) => {
                    // The switch is recorded by the kernel, so the log says who chose
                    // it; the reply is the state afterwards, which is what the bar draws.
                    match client.select_model(&name) {
                        Ok(list) => {
                            let _ = events.send(FromWorker::Models(Box::new(list)));
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
                ToWorker::NewSession => {
                    match client.new_session() {
                        Ok(id) => {
                            let _ = events.send(FromWorker::SessionStarted(id));
                            // The model list belongs to the session too — a new one may
                            // have been configured in between, and the bar should say what
                            // is true now rather than what was true of the old log.
                            if let Ok(list) = client.models() {
                                let _ = events.send(FromWorker::Models(Box::new(list)));
                            }
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
                ToWorker::RefreshContext => {
                    match client.context() {
                        Ok(explained) => {
                            let _ = events.send(FromWorker::Context(Box::new(explained)));
                        }
                        Err(e) => {
                            let _ = events.send(FromWorker::Failed(e));
                        }
                    }
                    refreshed = true;
                    Ok(Vec::new())
                }
            };
            match result {
                Ok(lines) => {
                    // An empty list is "nothing to add", not "clear the screen": the
                    // submit path uses it to drop the streaming preview.
                    let _ = events.send(FromWorker::Lines(lines));
                }
                Err(e) => {
                    let _ = events.send(FromWorker::Failed(e));
                }
            }
            // Keep the context view current after every command, so opening the panel
            // mid-session shows the turn that just happened rather than an empty box.
            if !refreshed {
                if let Ok(explained) = client.context() {
                    let _ = events.send(FromWorker::Context(Box::new(explained)));
                }
            }
            // The panel's detail comes from here, after the command, rather than from inside
            // the submit callback. A question is met in two places — a submit that stops for
            // it, and a resume after the previous answer, which is where the next one usually
            // appears — and only the first of those ran that callback. That is why the second
            // question used to arrive with no detail at all, while the first was fine.
            let pending = client.pending_decision().cloned();
            let asking = pending.as_ref().map(|decision| decision.request_id.clone());
            if asking != asked {
                asked = asking;
                if let Some(decision) = pending {
                    let _ = events.send(FromWorker::Approval(Box::new(ApprovalInfo {
                        request_id: decision.request_id,
                        class: decision.class,
                        reason: decision.reason,
                        tool: decision.tool,
                        args: decision.args,
                    })));
                }
            }
            let _ = events.send(FromWorker::State {
                pending: client.pending_approval().map(|s| s.to_string()),
            });
        }
    });
    (to_worker, from_worker)
}
