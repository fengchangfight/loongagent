//! The web surface (ADR-0009): the same session, in a browser.
//!
//! This layer is deliberately **ignorant of the protocol**: it carries JSON-RPC
//! lines between a WebSocket and whatever session is behind it, and serves one
//! self-contained page. That is what the "one brain, many faces" split buys — the
//! browser speaks exactly what the TUI speaks, the kernel does not know a browser
//! exists, and there is no second implementation of the loop to keep in step.
//!
//! Two things are local to this surface:
//!
//! * **A token**, required on the WebSocket handshake. Any page in any tab can
//!   open `ws://127.0.0.1:<port>`, so a loopback bind is not authorisation on its
//!   own. The token travels in the URL *fragment* (which browsers never send to a
//!   server) and is presented as a query parameter on the connection only.
//! * **The static page**, embedded in the binary so a demo needs no asset server
//!   and no bundler.

use serde_json::{json, Value};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tungstenite::{accept_hdr, Message};

/// The page, compiled in: one file, no build step, no CDN.
pub const INDEX: &str = include_str!("index.html");

/// What the web layer needs from a session: a line in, lines out.
///
/// The lines are exactly what the stdio server writes — events first, then the
/// response — so a WebSocket message maps one-to-one onto a `handle_line` call.
pub trait Session: Send {
    /// Send one line, and hand back each line of the answer **as it arrives**.
    ///
    /// The sink exists for one reason: a model writes its answer over seconds, and
    /// making the browser wait for the last byte would waste the whole point of the
    /// event stream. A session that only produces its lines at the end is still
    /// correct — it simply calls the sink late.
    fn exchange(
        &mut self,
        line: &str,
        sink: &mut dyn FnMut(String) -> Result<(), String>,
    ) -> Result<(), String>;
}

/// A token no other page can guess, read from the OS entropy pool.
pub fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .map_err(|e| format!("reading /dev/urandom: {e}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn bind(host: &str, port: u16) -> Result<(TcpListener, SocketAddr), String> {
    let listener =
        TcpListener::bind((host, port)).map_err(|e| format!("binding {host}:{port}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    Ok((listener, addr))
}

/// Accept connections until the process ends. Each connection gets a thread; the
/// session is behind a mutex, so two browsers cannot interleave one request.
pub fn serve(session: Arc<Mutex<dyn Session>>, listener: TcpListener, token: String) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let session = Arc::clone(&session);
        let token = token.clone();
        std::thread::spawn(move || {
            if let Err(e) = handle(stream, session, token) {
                eprintln!("[web] {e}");
            }
        });
    }
}

fn handle(
    stream: TcpStream,
    session: Arc<Mutex<dyn Session>>,
    token: String,
) -> Result<(), String> {
    // Peek (not read): the WebSocket handshake must see the request itself, while
    // a static request is ours to answer.
    let mut peek = [0u8; 4096];
    let peeked = stream.peek(&mut peek).map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&peek[..peeked]);
    let request_line = head.lines().next().unwrap_or_default();
    let is_upgrade = head.to_ascii_lowercase().contains("upgrade: websocket");
    if request_line.contains("/ws") || is_upgrade {
        websocket(stream, session, token)
    } else {
        static_page(stream)
    }
}

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

/// A session that answers from a script: the web layer's own tests, and a way to
/// demo the page without a model behind it.
pub struct Echo(pub Vec<String>);

impl Session for Echo {
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

    fn start(session: Arc<Mutex<dyn Session>>) -> (SocketAddr, String) {
        let (listener, addr) = bind("127.0.0.1", 0).unwrap();
        let token = random_token().unwrap();
        let for_server = token.clone();
        std::thread::spawn(move || serve(session, listener, for_server));
        (addr, token)
    }

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

    #[test]
    fn lines_reach_the_browser_as_the_session_produces_them() {
        struct Trickle;
        impl Session for Trickle {
            fn exchange(
                &mut self,
                _line: &str,
                sink: &mut dyn FnMut(String) -> Result<(), String>,
            ) -> Result<(), String> {
                for piece in ["one", "two"] {
                    sink(
                        json!({"jsonrpc": "2.0", "method": "model.delta",
                                "params": {"turn_id": 1, "text": piece}})
                        .to_string(),
                    )?;
                }
                sink(json!({"jsonrpc": "2.0", "id": 1, "result": {"accepted": true}}).to_string())
            }
        }
        let (addr, token) = start(Arc::new(Mutex::new(Trickle)));
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket.send(Message::text("{}")).unwrap();
        let first = socket.read().unwrap().into_text().unwrap().to_string();
        let second = socket.read().unwrap().into_text().unwrap().to_string();
        let third = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(
            first.contains("model.delta") && first.contains("one"),
            "{first}"
        );
        assert!(second.contains("two"), "{second}");
        assert!(
            third.contains("accepted"),
            "the response still comes last: {third}"
        );
    }

    #[test]
    fn a_session_failure_becomes_an_event_instead_of_a_dead_socket() {
        struct Broken;
        impl Session for Broken {
            fn exchange(
                &mut self,
                _line: &str,
                _sink: &mut dyn FnMut(String) -> Result<(), String>,
            ) -> Result<(), String> {
                Err("the server closed the connection".to_string())
            }
        }
        let (addr, token) = start(Arc::new(Mutex::new(Broken)));
        let request = format!("ws://{addr}/ws?token={token}")
            .into_client_request()
            .unwrap();
        let (mut socket, _) = tungstenite::connect(request).unwrap();
        socket.send(Message::text("{}")).unwrap();
        let notice = socket.read().unwrap().into_text().unwrap().to_string();
        assert!(notice.contains("client.error"), "{notice}");
        assert!(
            notice.contains("the server closed the connection"),
            "{notice}"
        );
    }

    #[test]
    fn tokens_are_random_hex_of_a_usable_length() {
        let a = random_token().unwrap();
        let b = random_token().unwrap();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "two tokens must not collide");
    }

    #[test]
    fn the_embedded_page_talks_the_protocol() {
        // The page is the other half of the contract: it must speak JSON-RPC and
        // render the kinds the kernel emits.
        for needle in [
            "session.attach",
            "prompt.submit",
            "approval.respond",
            "message.appended",
            "tool.call",
            "approval.requested",
            "ws",
        ] {
            assert!(INDEX.contains(needle), "the page handles {needle}");
        }
    }
}
