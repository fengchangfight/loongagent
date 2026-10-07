//! The smallest HTTP/1.1 client that can carry a JSON POST — shared by the
//! provider adapters and the HTTP hook kind, so neither owns a transport.
//!
//! **Transport scope, stated honestly:** HTTP only. An `https://` URL fails with
//! a clear message rather than pretending; wiring TLS is a dependency decision
//! (rustls or a platform TLS stack) that has not been made, and self-hosted
//! `http://` endpoints are the firm-internal case this targets.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError(pub String);

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for HttpError {}

fn err(message: impl Into<String>) -> HttpError {
    HttpError(message.into())
}

/// Split `http://host[:port]/path` into the pieces a socket needs.
pub fn parse_http_url(url: &str) -> Result<(String, u16, String), HttpError> {
    if url.starts_with("https://") {
        return Err(err(format!(
            "{url} uses https, and TLS is not wired in yet — point at an http:// endpoint, \
             or add a TLS backend"
        )));
    }
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| err(format!("{url} is not an http:// URL")))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>().map_err(|_| err(format!("bad port in {url}")))?,
        ),
        None => (authority, 80),
    };
    if host.is_empty() {
        return Err(err(format!("{url} has no host")));
    }
    Ok((host.to_string(), port, path.to_string()))
}

/// POST a JSON body and read the whole response. `timeout` bounds both the
/// connect and the read/write, so a hanging server cannot hang a tool or a hook.
pub fn post_json(
    url: &str,
    body: &str,
    bearer: Option<&str>,
    timeout: Duration,
) -> Result<HttpResponse, HttpError> {
    let (host, port, path) = parse_http_url(url)?;
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| err(format!("resolving {host}:{port}: {e}")))?
        .next()
        .ok_or_else(|| err(format!("no address for {host}:{port}")))?;

    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| err(format!("connecting to {host}:{port}: {e}")))?;
    stream.set_read_timeout(Some(timeout)).ok();
    stream.set_write_timeout(Some(timeout)).ok();

    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nAccept: application/json\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(token) = bearer {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("\r\n");

    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .and_then(|_| stream.flush())
        .map_err(|e| err(format!("sending the request: {e}")))?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| err(format!("reading the response: {e}")))?;
    let text = String::from_utf8_lossy(&raw).to_string();

    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| err("the response has no header/body separator"))?;
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| err("the response has no status line"))?;

    Ok(HttpResponse { status, body: body.to_string() })
}

/// A tiny HTTP/1.1 server that serves exactly one canned response. Exposed
/// (hidden) so dependent crates' tests can drive both the provider adapters and
/// the HTTP hook kind without each writing their own harness.
#[doc(hidden)]
pub mod testing {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread::JoinHandle;

    /// Returns `(base_url, join_handle)`; the handle yields the raw request the
    /// client sent, so a test can assert the wire shape.
    pub fn fake_server(response: &'static str) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = sock.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&raw).to_string();
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let want = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= want {
                        break;
                    }
                }
            }
            sock.write_all(response.as_bytes()).unwrap();
            sock.flush().unwrap();
            String::from_utf8_lossy(&raw).to_string()
        });
        (format!("http://127.0.0.1:{}/v1", addr.port()), handle)
    }
}

#[cfg(test)]
mod tests {
    use super::testing::fake_server;
    use super::*;
    use std::time::Duration;

    #[test]
    fn urls_are_split_into_host_port_and_path() {
        assert_eq!(
            parse_http_url("http://127.0.0.1:8000/v1/chat").unwrap(),
            ("127.0.0.1".to_string(), 8000, "/v1/chat".to_string())
        );
        assert_eq!(
            parse_http_url("http://gateway.internal").unwrap(),
            ("gateway.internal".to_string(), 80, "/".to_string())
        );
        assert!(parse_http_url("https://api.example.com/v1").unwrap_err().0.contains("TLS is not wired"));
        assert!(parse_http_url("ftp://x/y").is_err());
        assert!(parse_http_url("http://host:notaport/x").is_err());
    }

    #[test]
    fn a_post_round_trips_and_carries_the_bearer_token() {
        let (url, server) =
            fake_server("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}");
        let response = post_json(&url, "{\"hello\":1}", Some("secret"), Duration::from_secs(5)).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\":true}");

        let sent = server.join().unwrap();
        let (head, body) = sent.split_once("\r\n\r\n").unwrap();
        assert!(head.starts_with("POST /v1 HTTP/1.1"), "{head}");
        assert!(head.contains("Authorization: Bearer secret"), "{head}");
        assert!(head.contains("Content-Length: 11"), "{head}");
        assert_eq!(body, "{\"hello\":1}");
    }

    #[test]
    fn a_non_2xx_status_is_returned_not_hidden() {
        let (url, server) = fake_server(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\n\r\nbusy",
        );
        let response = post_json(&url, "{}", None, Duration::from_secs(5)).unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(response.body, "busy");
        server.join().unwrap();
    }

    #[test]
    fn a_dead_endpoint_is_an_error_not_a_hang() {
        let err = post_json("http://127.0.0.1:1/v1", "{}", None, Duration::from_millis(500))
            .unwrap_err();
        assert!(err.0.contains("connecting"), "{err}");
    }
}
