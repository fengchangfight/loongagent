//! The smallest HTTP/1.1 client that can carry a JSON POST — shared by the
//! provider adapters and the HTTP hook kind, so neither owns a transport.
//!
//! `http://` for a local or intranet endpoint, `https://` for everything else:
//! TLS is rustls with the `ring` backend and the webpki root store, so a hosted
//! provider (DeepSeek, OpenAI, Moonshot, …) is reachable without a platform TLS
//! stack. Streaming responses are not implemented: one request, one reply.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// One connection, plain or wrapped in TLS. Both are blocking sockets with the
/// same read/write timeouts, so the calling code does not branch again.
enum Transport {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Transport {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Transport::Plain(stream) => stream.read(buf),
            Transport::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for Transport {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Transport::Plain(stream) => stream.write(buf),
            Transport::Tls(stream) => stream.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Transport::Plain(stream) => stream.flush(),
            Transport::Tls(stream) => stream.flush(),
        }
    }
}

/// The root store and provider, built once: constructing them per request would
/// re-parse every root certificate on every model call.
fn tls_config() -> Arc<rustls::ClientConfig> {
    static CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .expect("the ring provider supports the default protocol versions")
                .with_root_certificates(roots)
                .with_no_client_auth();
            Arc::new(config)
        })
        .clone()
}

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

/// Split a URL into the pieces a socket needs, and say whether it is TLS.
pub fn parse_http_url(url: &str) -> Result<(String, u16, String, bool), HttpError> {
    let (rest, tls, default_port) = if let Some(rest) = url.strip_prefix("https://") {
        (rest, true, 443)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (rest, false, 80)
    } else {
        return Err(err(format!("{url} is not an http:// or https:// URL")));
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h,
            p.parse::<u16>()
                .map_err(|_| err(format!("bad port in {url}")))?,
        ),
        None => (authority, default_port),
    };
    if host.is_empty() {
        return Err(err(format!("{url} has no host")));
    }
    Ok((host.to_string(), port, path.to_string(), tls))
}

/// POST a JSON body and read the whole response. `timeout` bounds both the
/// connect and the read/write, so a hanging server cannot hang a tool or a hook.
/// A GET, for reading something small and structured (the pack a control plane
/// serves). Same framing, same TLS, no body.
pub fn get_json(
    url: &str,
    bearer: Option<&str>,
    timeout: Duration,
) -> Result<HttpResponse, HttpError> {
    let mut stream = open(url, timeout)?;
    let (host, port, path, tls) = parse_http_url(url)?;
    let authority = if (tls && port == 443) || (!tls && port == 80) {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    let mut head = format!(
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\n\
         Connection: close\r\n"
    );
    if let Some(token) = bearer {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .map_err(|e| err(format!("sending the request: {e}")))?;
    stream
        .flush()
        .map_err(|e| err(format!("sending the request: {e}")))?;

    let (status, _content_type, mut reader) = read_head(&mut stream)?;
    let mut body = String::new();
    reader
        .read_to_string(&mut body)
        .map_err(|e| err(format!("reading the response: {e}")))?;
    if !(200..300).contains(&status) {
        return Err(err(format!(
            "HTTP {status}: {}",
            body.trim().chars().take(300).collect::<String>()
        )));
    }
    Ok(HttpResponse { status, body })
}

pub fn post_json(
    url: &str,
    body: &str,
    bearer: Option<&str>,
    timeout: Duration,
) -> Result<HttpResponse, HttpError> {
    let mut stream = open(url, timeout)?;
    write_request(&mut stream, url, body, bearer)?;
    let (status, _content_type, mut reader) = read_head(&mut stream)?;
    let mut raw = String::new();
    reader
        .read_to_string(&mut raw)
        .map_err(|e| err(format!("reading the response: {e}")))?;
    Ok(HttpResponse { status, body: raw })
}

/// POST and hand each Server-Sent Event `data:` payload to `on_data` as it
/// arrives. Returns the status once the server closes the stream.
///
/// This is what lets a model's answer appear while it is still being written. Two
/// details matter: a streamed HTTP/1.1 reply uses **chunked** transfer encoding
/// (there is no `Content-Length` to wait for), and a chunk boundary has nothing to
/// do with an event boundary, so the reader buffers *lines*, not chunks.
pub fn post_sse(
    url: &str,
    body: &str,
    bearer: Option<&str>,
    timeout: Duration,
    on_data: &mut dyn FnMut(&str),
) -> Result<u16, HttpError> {
    let mut stream = open(url, timeout)?;
    write_request(&mut stream, url, body, bearer)?;
    let (status, content_type, mut reader) = read_head(&mut stream)?;
    if !(200..300).contains(&status) {
        // An error reply is ordinary JSON, and the caller wants to see it.
        let mut raw = String::new();
        let _ = reader.read_to_string(&mut raw);
        return Err(err(format!(
            "HTTP {status}: {}",
            raw.chars().take(500).collect::<String>()
        )));
    }

    // A server (or a proxy in front of one) may ignore `stream` and answer with an
    // ordinary JSON body. Handing that body over in one piece keeps the caller
    // working instead of returning an empty answer.
    if !content_type
        .to_ascii_lowercase()
        .contains("text/event-stream")
    {
        let mut whole = String::new();
        reader
            .read_to_string(&mut whole)
            .map_err(|e| err(format!("reading the response: {e}")))?;
        if !whole.trim().is_empty() {
            on_data(whole.trim());
        }
        return Ok(status);
    }

    let mut pending = String::new();
    let mut buffer = [0u8; 8192];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| err(format!("reading the stream: {e}")))?;
        if read == 0 {
            break;
        }
        pending.push_str(&String::from_utf8_lossy(&buffer[..read]));
        // Only complete lines are interpreted; a partial line waits for the rest.
        while let Some(index) = pending.find('\n') {
            let line = pending[..index].trim_end_matches('\r').to_string();
            pending.drain(..=index);
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim_start();
                if data == "[DONE]" {
                    return Ok(status);
                }
                if !data.is_empty() {
                    on_data(data);
                }
            }
            // `event:`, `id:`, `retry:` and comment lines are not needed here.
        }
    }
    Ok(status)
}

/// Connect, with TLS when the URL asks for it. The handshake completes here, so a
/// certificate problem is reported as a certificate problem.
fn open(url: &str, timeout: Duration) -> Result<Transport, HttpError> {
    let (host, port, _path, tls) = parse_http_url(url)?;
    let addr = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| err(format!("resolving {host}:{port}: {e}")))?
        .next()
        .ok_or_else(|| err(format!("no address for {host}:{port}")))?;
    let socket = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|e| err(format!("connecting to {host}:{port}: {e}")))?;
    socket.set_read_timeout(Some(timeout)).ok();
    socket.set_write_timeout(Some(timeout)).ok();
    if !tls {
        return Ok(Transport::Plain(socket));
    }
    let name = rustls::pki_types::ServerName::try_from(host.clone())
        .map_err(|e| err(format!("{host} is not a valid certificate name: {e}")))?;
    let connection = rustls::ClientConnection::new(tls_config(), name)
        .map_err(|e| err(format!("starting TLS with {host}: {e}")))?;
    let mut stream = rustls::StreamOwned::new(connection, socket);
    stream
        .conn
        .complete_io(&mut stream.sock)
        .map_err(|e| err(format!("TLS handshake with {host} failed: {e}")))?;
    Ok(Transport::Tls(Box::new(stream)))
}

fn write_request(
    stream: &mut Transport,
    url: &str,
    body: &str,
    bearer: Option<&str>,
) -> Result<(), HttpError> {
    let (host, port, path, tls) = parse_http_url(url)?;
    let authority = if (tls && port == 443) || (!tls && port == 80) {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    let mut head = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nAccept: text/event-stream, application/json\r\n\
         Connection: close\r\n",
        body.len()
    );
    if let Some(token) = bearer {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("User-Agent: loongagent\r\n\r\n");
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(body.as_bytes()))
        .and_then(|_| stream.flush())
        .map_err(|e| err(format!("sending the request: {e}")))
}

/// Read the status line and headers, and hand back a reader on the body.
///
/// A **chunked** body is decoded here so callers see plain bytes: SSE arrives in
/// chunks whose boundaries have nothing to do with event boundaries.
fn read_head(stream: &mut Transport) -> Result<(u16, String, BodyReader<'_>), HttpError> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        let read = stream
            .read(&mut byte)
            .map_err(|e| err(format!("reading the response head: {e}")))?;
        if read == 0 {
            return Err(err("the response ended before its headers did"));
        }
        head.push(byte[0]);
        if head.len() > 64 * 1024 {
            return Err(err("the response headers are implausibly large"));
        }
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| err("the response has no status line"))?;
    let lowered = head.to_ascii_lowercase();
    let chunked = lowered.contains("transfer-encoding: chunked");
    let content_type = head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("content-type:"))
        .map(|line| {
            line.split_once(':')
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    Ok((
        status,
        content_type,
        BodyReader {
            stream,
            chunked,
            done: false,
        },
    ))
}

/// The body of a response: plain, or chunked (which every streamed reply is).
struct BodyReader<'a> {
    stream: &'a mut Transport,
    chunked: bool,
    done: bool,
}

impl Read for BodyReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if !self.chunked {
            let read = self.stream.read(buf)?;
            if read == 0 {
                self.done = true;
            }
            return Ok(read);
        }
        // Chunked: `<hex size>CRLF <bytes> CRLF`, terminated by a zero-size chunk.
        let size = match read_chunk_size(self.stream)? {
            Some(0) | None => {
                self.done = true;
                return Ok(0);
            }
            Some(size) => size,
        };
        let take = size.min(buf.len());
        let mut filled = 0;
        while filled < take {
            let read = self.stream.read(&mut buf[filled..take])?;
            if read == 0 {
                self.done = true;
                return Ok(filled);
            }
            filled += read;
        }
        // Whatever did not fit is discarded, so the next call starts on a boundary.
        let mut leftover = size - filled;
        let mut sink = [0u8; 1024];
        while leftover > 0 {
            let want = leftover.min(sink.len());
            let read = self.stream.read(&mut sink[..want])?;
            if read == 0 {
                self.done = true;
                break;
            }
            leftover -= read;
        }
        let mut crlf = [0u8; 2];
        let _ = self.stream.read(&mut crlf);
        Ok(filled)
    }
}

fn read_chunk_size(stream: &mut Transport) -> std::io::Result<Option<usize>> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let read = stream.read(&mut byte)?;
        if read == 0 {
            return Ok(None);
        }
        if byte[0] == b'\n' {
            break;
        }
        if byte[0] != b'\r' {
            line.push(byte[0]);
        }
        if line.len() > 64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "chunk size line is implausibly long",
            ));
        }
    }
    let text = String::from_utf8_lossy(&line);
    let size = text.split(';').next().unwrap_or("").trim();
    usize::from_str_radix(size, 16).map(Some).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("chunk size {text:?}"),
        )
    })
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
            let request = read_request(&mut sock);
            sock.write_all(response.as_bytes()).unwrap();
            sock.flush().unwrap();
            request
        });
        (format!("http://127.0.0.1:{}/v1", addr.port()), handle)
    }

    /// A server that answers with **chunked** Server-Sent Events, writing each
    /// piece separately: the shape a streaming model actually uses, including the
    /// case where one event is split across two chunks.
    ///
    /// `pieces` are written as they are, so a test controls the framing exactly.
    pub fn fake_sse_server(pieces: Vec<String>) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let request = read_request(&mut sock);
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                  Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .unwrap();
            for piece in pieces {
                let framed = format!("{:x}\r\n{}\r\n", piece.len(), piece);
                sock.write_all(framed.as_bytes()).unwrap();
                sock.flush().unwrap();
            }
            sock.write_all(b"0\r\n\r\n").unwrap();
            sock.flush().unwrap();
            request
        });
        (
            format!("http://127.0.0.1:{}/v1/chat/completions", addr.port()),
            handle,
        )
    }

    fn read_request(sock: &mut std::net::TcpStream) -> String {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let Ok(n) = sock.read(&mut buf) else { break };
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
        String::from_utf8_lossy(&raw).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{fake_server, fake_sse_server};
    use super::*;
    use std::time::Duration;

    #[test]
    fn urls_are_split_into_host_port_path_and_scheme() {
        assert_eq!(
            parse_http_url("http://127.0.0.1:8000/v1/chat").unwrap(),
            ("127.0.0.1".to_string(), 8000, "/v1/chat".to_string(), false)
        );
        assert_eq!(
            parse_http_url("http://gateway.internal").unwrap(),
            ("gateway.internal".to_string(), 80, "/".to_string(), false)
        );
        assert_eq!(
            parse_http_url("https://api.deepseek.com/v1").unwrap(),
            ("api.deepseek.com".to_string(), 443, "/v1".to_string(), true),
            "a hosted provider is https on 443"
        );
        assert_eq!(
            parse_http_url("https://host:8443/x").unwrap(),
            ("host".to_string(), 8443, "/x".to_string(), true)
        );
        assert!(parse_http_url("ftp://x/y").is_err());
        assert!(parse_http_url("http://host:notaport/x").is_err());
    }

    /// A real TLS handshake against a real host. Skipped only when this machine has
    /// no route out; a TLS failure is a failure, not a skip.
    #[test]
    fn https_really_works_against_a_public_host() {
        let response = post_json(
            "https://mirrors.tuna.tsinghua.edu.cn/",
            "{}",
            None,
            Duration::from_secs(20),
        );
        match response {
            Ok(response) => {
                assert!(
                    (100..600).contains(&response.status),
                    "an HTTP status means the handshake and the request completed: {}",
                    response.status
                );
            }
            Err(HttpError(message))
                if message.contains("connecting") || message.contains("resolving") =>
            {
                eprintln!("skipping: no route to a public host ({message})");
            }
            Err(e) => panic!("TLS itself failed: {e}"),
        }
    }

    #[test]
    fn a_certificate_that_does_not_match_the_host_is_refused() {
        // `wrong.host.badssl.com` presents a certificate for another name; if DNS or
        // the network is unavailable the test says so instead of pretending.
        let response = post_json(
            "https://wrong.host.badssl.com/",
            "{}",
            None,
            Duration::from_secs(20),
        );
        match response {
            Ok(response) => panic!(
                "a mismatched certificate must not be accepted: {}",
                response.status
            ),
            Err(e) => {
                let message = e.0;
                let network = message.contains("resolving") || message.contains("connecting");
                assert!(
                    !network || message.contains("TLS"),
                    "either a TLS refusal or no network, got: {message}"
                );
            }
        }
    }

    #[test]
    fn a_post_round_trips_and_carries_the_bearer_token() {
        let (url, server) =
            fake_server("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}");
        let response = post_json(
            &url,
            "{\"hello\":1}",
            Some("secret"),
            Duration::from_secs(5),
        )
        .unwrap();
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
        let (url, server) =
            fake_server("HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\n\r\nbusy");
        let response = post_json(&url, "{}", None, Duration::from_secs(5)).unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(response.body, "busy");
        server.join().unwrap();
    }

    #[test]
    fn sse_events_arrive_in_order_as_they_are_written() {
        let (url, server) = fake_sse_server(vec![
            "data: {\"n\":1}\n\n".to_string(),
            "data: {\"n\":2}\n\n".to_string(),
            "data: [DONE]\n\n".to_string(),
        ]);
        let mut seen = Vec::new();
        let status = post_sse(&url, "{}", None, Duration::from_secs(5), &mut |data| {
            seen.push(data.to_string());
        })
        .unwrap();
        assert_eq!(status, 200);
        assert_eq!(seen, vec!["{\"n\":1}".to_string(), "{\"n\":2}".to_string()]);
        let sent = server.join().unwrap();
        assert!(
            sent.contains("Accept: text/event-stream"),
            "we ask for a stream: {sent}"
        );
    }

    #[test]
    fn a_chunk_boundary_does_not_split_an_event() {
        // One event, delivered in three pieces: the reader buffers lines, not chunks.
        let (url, server) = fake_sse_server(vec![
            "data: {\"te".to_string(),
            "xt\":\"hello\"}".to_string(),
            "\n\ndata: [DONE]\n\n".to_string(),
        ]);
        let mut seen = Vec::new();
        post_sse(&url, "{}", None, Duration::from_secs(5), &mut |data| {
            seen.push(data.to_string());
        })
        .unwrap();
        assert_eq!(
            seen,
            vec!["{\"text\":\"hello\"}".to_string()],
            "one event, reassembled"
        );
        server.join().unwrap();
    }

    #[test]
    fn a_non_2xx_stream_is_an_error_with_the_body() {
        let (url, server) = fake_server(
            "HTTP/1.1 402 Payment Required\r\nContent-Length: 12\r\nConnection: close\r\n\r\nno credit :(",
        );
        let mut seen = Vec::new();
        let err = post_sse(&url, "{}", None, Duration::from_secs(5), &mut |data| {
            seen.push(data.to_string());
        })
        .unwrap_err();
        assert!(err.0.contains("402"), "{err}");
        assert!(err.0.contains("no credit"), "{err}");
        assert!(seen.is_empty(), "nothing is reported as a delta");
        server.join().unwrap();
    }

    #[test]
    fn a_get_reads_a_body_and_a_bearer_token() {
        let (url, server) = fake_server(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 11\r\n\
             Connection: close\r\n\r\n{\"ok\":true}",
        );
        let response = get_json(&url, Some("s3cret"), Duration::from_secs(5)).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\":true}");
        let sent = server.join().unwrap();
        assert!(sent.starts_with("GET "), "GET, not POST: {sent}");
        assert!(sent.contains("Authorization: Bearer s3cret"));
    }

    #[test]
    fn a_get_that_fails_carries_what_the_server_said() {
        let (url, server) = fake_server(
            "HTTP/1.1 409 Conflict\r\nContent-Length: 28\r\nConnection: close\r\n\r\n\
             {\"error\":\"nothing published\"}",
        );
        let error = get_json(&url, None, Duration::from_secs(5)).unwrap_err();
        assert!(error.0.contains("409"), "{error}");
        assert!(error.0.contains("nothing published"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn a_dead_endpoint_is_an_error_not_a_hang() {
        let err = post_json(
            "http://127.0.0.1:1/v1",
            "{}",
            None,
            Duration::from_millis(500),
        )
        .unwrap_err();
        assert!(err.0.contains("connecting"), "{err}");
    }
}
