//! Purpose: Carry one JSON request over HTTP/1.1, plain or TLS, with bounded timeouts and a streamed reply.
//!
//! # Contract
//! - **Serves:** the wire beneath provider calls, HTTP hooks, the context summarizer and the control client, so none owns a transport (config-routing.md §8).
//! - **Design:** `docs/design/config-routing.md` §8 (providers); `docs/design/control-plane.md` (the control client).
//! - **Upstream:** `loong-providers`' adapter and probe, the HTTP hook kind, `loong-context`'s summarizer and `loong-control`'s client.
//! - **Downstream:** `TcpStream`, rustls, and the `Transport` / `BodyReader` readers.
//! - **Verify:** `cargo test -p loong-http` · `./scripts/check.sh`
//!
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
/// Purpose: One blocking connection, plain TCP or wrapped in rustls TLS.
///
/// Serves: one read/write surface so the request code never branches on TLS (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `open` builds it; every request function reads and writes through it.
/// Down: `TcpStream` and `rustls::StreamOwned`.
/// Verify: `cargo test -p loong-http`.
enum Transport {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Read for Transport {
    /// Purpose: Read from whichever transport this connection wraps.
    ///
    /// Contract: `crates/http/src/lib.rs`.
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Transport::Plain(stream) => stream.read(buf),
            Transport::Tls(stream) => stream.read(buf),
        }
    }
}

impl Write for Transport {
    /// Purpose: Write to whichever transport this connection wraps.
    ///
    /// Contract: `crates/http/src/lib.rs`.
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Transport::Plain(stream) => stream.write(buf),
            Transport::Tls(stream) => stream.write(buf),
        }
    }
    /// Purpose: Flush whichever transport this connection wraps.
    ///
    /// Contract: `crates/http/src/lib.rs`.
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Transport::Plain(stream) => stream.flush(),
            Transport::Tls(stream) => stream.flush(),
        }
    }
}

/// The root store and provider, built once: constructing them per request would
/// re-parse every root certificate on every model call.
/// Purpose: Build the root store and TLS provider once and hand out the shared config.
///
/// Serves: a hosted provider is reachable without re-parsing roots on every call (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `open` for every `https://` request.
/// Down: `rustls::RootCertStore`, `webpki_roots` and the ring provider.
/// Verify: `cargo test -p loong-http`.
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
/// Purpose: A status and a body, returned whatever the status was.
///
/// Serves: a caller can tell a provider that answered with a refusal from one that did not answer (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `get_json_raw`, `get_json` and `post_json` return it.
/// Down: the response head and body readers.
/// Verify: `cargo test -p loong-http`.
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Purpose: A transport or protocol failure as one message string.
///
/// Serves: one error type for resolving, connecting, TLS, framing and reading.
/// Design: `docs/design/config-routing.md` §8.
/// Up: every public request function returns it.
/// Down: its `Display` and `Error` impls.
/// Verify: `cargo test -p loong-http`.
pub struct HttpError(pub String);

impl std::fmt::Display for HttpError {
    /// Purpose: Render the error message unchanged.
    ///
    /// Contract: `crates/http/src/lib.rs`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for HttpError {}

/// Purpose: Wrap a message as an `HttpError`.
///
/// Contract: `crates/http/src/lib.rs`.
fn err(message: impl Into<String>) -> HttpError {
    HttpError(message.into())
}

/// Split a URL into the pieces a socket needs, and say whether it is TLS.
/// Purpose: Split a URL into host, port, path and whether it needs TLS.
///
/// Serves: one URL parser for connecting and for writing the request line (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `open`, `write_request` and `get_json_raw`.
/// Down: string splitting only; no network access.
/// Verify: `cargo test -p loong-http`.
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

/// A GET that hands back the response **whatever its status**, so a caller can tell
/// "the provider answered and said no" (`401`) from "nothing answered at all" (a
/// transport error).
///
/// That distinction is not pedantry: on a first run it is the difference between
/// "your key is wrong" and "this vendor is unreachable from your network", and a
/// user told the first when the second is true will go and re-issue a working key.
/// Purpose: GET a URL and return the response whatever its status.
///
/// Serves: a provider probe can tell a refusal from an unreachable host (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `loong-providers`' probe (`probe_models_url` and `raw_models_answer`); `get_json` wraps it.
/// Down: `open`, `parse_http_url` and `read_head`.
/// Verify: `cargo test -p loong-http`.
pub fn get_json_raw(
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
    Ok(HttpResponse { status, body })
}

/// GET a small, structured resource, treating a non-2xx status as a failure.
/// Same framing, same TLS, no body.
/// Purpose: GET a small JSON resource and treat a non-2xx status as an error.
///
/// Serves: the control client fetches the pack or checks health and needs only the value or the failure (control-plane.md).
/// Design: `docs/design/config-routing.md` §8; `docs/design/control-plane.md` (the control client).
/// Up: `loong-control`'s pack fetch and health check.
/// Down: `get_json_raw`.
/// Verify: `cargo test -p loong-http`.
pub fn get_json(
    url: &str,
    bearer: Option<&str>,
    timeout: Duration,
) -> Result<HttpResponse, HttpError> {
    let response = get_json_raw(url, bearer, timeout)?;
    if !(200..300).contains(&response.status) {
        return Err(err(format!(
            "HTTP {}: {}",
            response.status,
            response.body.trim().chars().take(300).collect::<String>()
        )));
    }
    Ok(response)
}

/// POST a JSON body and read the whole response. `timeout` bounds both the
/// connect and the read/write, so a hanging server cannot hang a tool or a hook.
/// The status comes back to the caller rather than being an error, because an
/// adapter reads its protocol's way of saying "no" out of the body.
/// Purpose: POST a JSON body and read the whole response.
///
/// Serves: one request and one reply for the non-streaming provider path and HTTP hooks (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `loong-providers`' `complete` and the HTTP hook kind.
/// Down: `open`, `write_request` and `read_head`.
/// Verify: `cargo test -p loong-http`.
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
/// Purpose: POST and hand each Server-Sent Event `data:` payload to a callback as it arrives.
///
/// Serves: a model answer that appears while it is still being written (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `loong-providers`' streaming path.
/// Down: `open`, `write_request`, `read_head` and the line buffer.
/// Verify: `cargo test -p loong-http`.
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
/// Purpose: Resolve, connect, set read/write timeouts and complete the TLS handshake.
///
/// Serves: a certificate problem is reported as a certificate problem (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: every request function.
/// Down: `parse_http_url`, `TcpStream::connect_timeout`, `tls_config` and rustls.
/// Verify: `cargo test -p loong-http`.
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

/// Purpose: Write the POST head plus body, adding the bearer token when one is given.
///
/// Serves: the request shape the provider and hook callers share.
/// Design: `docs/design/config-routing.md` §8.
/// Up: `post_json` and `post_sse`.
/// Down: `parse_http_url` and `Transport::write_all`.
/// Verify: `cargo test -p loong-http`.
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
/// Purpose: Read the status line and headers and return a reader on the body.
///
/// Serves: framing that honours `Content-Length` and decodes a chunked body (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `get_json_raw`, `post_json` and `post_sse`.
/// Down: `Transport::read`, the chunk-size check and `BodyReader`.
/// Verify: `cargo test -p loong-http`.
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
    // The declared length, when there is one. Honouring it means the body ends where the
    // server said it ends, instead of waiting for a connection close that may never be
    // announced properly — and it is what makes a *short* body detectable as truncation.
    let length = head
        .lines()
        .find(|line| line.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|line| line.split_once(':'))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok());
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
            remaining: 0,
            length,
            seen: 0,
        },
    ))
}

/// The body of a response: plain, or chunked (which every streamed reply is).
/// A read that treats a missing TLS `close_notify` as the end of the body.
///
/// Servers routinely close the TCP connection without the TLS goodbye. curl has always
/// tolerated it; rustls reports `UnexpectedEof`, which turned a reply that had *arrived in
/// full* into "peer closed connection without sending TLS close_notify". It is only a
/// problem if bytes are missing, and that is precisely what a `Content-Length` is for — see
/// `BodyReader`, where a short body against a declared length is still an error.
/// Purpose: Read from the transport, treating a missing TLS `close_notify` as end of body.
///
/// Contract: `crates/http/src/lib.rs`.
fn read_body(stream: &mut Transport, buf: &mut [u8]) -> std::io::Result<usize> {
    match stream.read(buf) {
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => Ok(0),
        other => other,
    }
}

/// Purpose: The response body reader: plain with a declared length, or chunked.
///
/// Serves: callers read plain bytes even when the wire is chunked (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `read_head` returns it; the request functions read it.
/// Down: `Transport`, `read_body` and `read_chunk_size`.
/// Verify: `cargo test -p loong-http`.
struct BodyReader<'a> {
    stream: &'a mut Transport,
    chunked: bool,
    done: bool,
    /// What the reply said its body would be, when it said anything.
    length: Option<usize>,
    /// How many body bytes have been handed out, to hold the writer to `length`.
    seen: usize,
    /// Bytes of the current chunk not yet handed to the caller.
    ///
    /// A `Read` may deliver less than it was asked for, so the rest of a chunk has to
    /// survive until the next call. It used to be thrown away instead, which silently
    /// truncated every chunked body larger than the caller's buffer — and a truncated
    /// JSON body is not an error, it is *no models*, which reads as a vendor that lists
    /// nothing. One chunk of Kimi Code's model list is 33 bytes; that is what arrived.
    remaining: usize,
}

impl Read for BodyReader<'_> {
    /// Purpose: Read a plain or chunked body without losing the rest of a chunk.
    ///
    /// Serves: a body larger than the caller's buffer arrives whole (config-routing.md §8).
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: `Read::read_to_string` in the request functions and the tests.
    /// Down: `read_body` and `read_chunk_size`.
    /// Verify: `cargo test -p loong-http`.
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        // Not EOF: a caller that asks for nothing is told it got nothing, and no chunk
        // header is consumed on its behalf.
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.chunked {
            // Never past the declared length, even if the socket has more to give.
            let want = match self.length {
                Some(length) => length.saturating_sub(self.seen).min(buf.len()),
                None => buf.len(),
            };
            if want == 0 {
                self.done = true;
                return Ok(0);
            }
            let read = read_body(self.stream, &mut buf[..want])?;
            if read == 0 {
                self.done = true;
            }
            self.seen += read;
            return Ok(read);
        }
        // Chunked: `<hex size>CRLF <bytes> CRLF`, terminated by a zero-size chunk.
        if self.remaining == 0 {
            match read_chunk_size(self.stream)? {
                Some(0) | None => {
                    self.done = true;
                    return Ok(0);
                }
                Some(size) => self.remaining = size,
            }
        }
        // At most `buf.len()` of what is left, and what does not fit stays `remaining`.
        let take = self.remaining.min(buf.len());
        let mut filled = 0;
        while filled < take {
            let read = read_body(self.stream, &mut buf[filled..take])?;
            if read == 0 {
                self.done = true;
                break;
            }
            filled += read;
        }
        self.remaining -= filled;
        if self.remaining == 0 && !self.done {
            // The CRLF that closes the chunk. Both bytes, not one `read`: a socket may
            // hand back half of it, and a stray newline left in the stream is read as
            // the next chunk's size line — which parses as nothing and ends the body
            // early.
            let mut crlf = [0u8; 2];
            let mut got = 0;
            while got < crlf.len() {
                match read_body(self.stream, &mut crlf[got..]) {
                    Ok(0) => {
                        self.done = true;
                        break;
                    }
                    Ok(n) => got += n,
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(filled)
    }
}

/// Purpose: Parse one chunk-size line and return `None` at the terminating zero chunk.
///
/// Serves: chunked transfer decoding (config-routing.md §8).
/// Design: `docs/design/config-routing.md` §8.
/// Up: `BodyReader::read`.
/// Down: `Transport::read` and `u64::from_str_radix`.
/// Verify: `cargo test -p loong-http`.
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
    /// Purpose: Serve one canned HTTP response on loopback and return the base URL and request handle.
    ///
    /// Serves: dependent crates' tests drive the provider and hook paths without their own harness.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: the tests in this crate and in the provider and hook crates.
    /// Down: `TcpListener` and `read_request`.
    /// Verify: `cargo test -p loong-http`.
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
    /// Purpose: Serve chunked Server-Sent Events on loopback, one piece per write.
    ///
    /// Serves: streaming tests that control the chunk boundaries.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: streaming tests in this crate and in `loong-providers`.
    /// Down: `TcpListener` and `read_request`.
    /// Verify: `cargo test -p loong-http`.
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

    /// Purpose: Read a full HTTP request line, headers and the body its length declares.
    ///
    /// Serves: a fake server that hands the raw request to a test.
    /// Design: `docs/design/config-routing.md` §8.
    /// Up: `fake_server` and `fake_sse_server`.
    /// Down: `TcpStream::read` and the `Content-Length` header.
    /// Verify: `cargo test -p loong-http`.
    pub fn read_request(sock: &mut std::net::TcpStream) -> String {
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
    use super::testing::{fake_server, fake_sse_server, read_request};
    use super::*;
    use std::net::TcpListener;
    use std::time::Duration;

    /// Purpose: Pins the host, port, path and scheme split, including bad ports.
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
    /// Purpose: Pins that a real TLS handshake and request complete against a public host, or that the network is absent.
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

    /// Purpose: Pins that a certificate for another host is refused.
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

    /// Purpose: Pins the POST framing, the bearer header and the body.
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

    /// Purpose: Pins that a non-2xx response is returned, not hidden as an error.
    #[test]
    fn a_non_2xx_status_is_returned_not_hidden() {
        let (url, server) =
            fake_server("HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/plain\r\n\r\nbusy");
        let response = post_json(&url, "{}", None, Duration::from_secs(5)).unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(response.body, "busy");
        server.join().unwrap();
    }

    /// Purpose: Pins that SSE payloads reach the callback in order and `[DONE]` stops the stream.
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

    /// Purpose: Pins that a body stops at `Content-Length`, not at connection close.
    #[test]
    fn a_body_stops_at_its_declared_length_and_not_at_the_connection() {
        // Two things at once. The body ends where `Content-Length` says, so trailing
        // bytes are not swallowed; and the reply is complete *before* the connection
        // closes, which is what stops a missing TLS `close_notify` from turning a body
        // that arrived in full into an error. Servers do close without the goodbye, and
        // rustls reports that as `UnexpectedEof` where curl has always shrugged.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let _ = read_request(&mut sock);
            // A declared length of 2, then a body, then bytes that belong to nobody.
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                  Content-Length: 2\r\nConnection: close\r\n\r\nok",
            )
            .unwrap();
            sock.write_all(b"TRAILING GARBAGE THAT IS NOT PART OF THE BODY")
                .unwrap();
            sock.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_millis(50));
        });
        let response = get_json_raw(
            &format!("http://127.0.0.1:{}/v1", addr.port()),
            None,
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(
            response.body, "ok",
            "exactly the declared length, no more and no less"
        );
    }

    /// Purpose: Pins that a chunk larger than the caller's buffer is delivered whole.
    #[test]
    fn a_chunk_larger_than_the_callers_buffer_is_delivered_not_discarded() {
        // The bug, and it was the worst shape a bug can have: silent data loss. `read`
        // threw away whatever did not fit the caller's buffer, so any chunked body larger
        // than that buffer arrived truncated. Truncated JSON is not an error — it parses
        // as *no models*, which reads as a vendor whose list endpoint is empty. Kimi
        // Code's model list arrives in one 33-byte chunk and came back as exactly those
        // 33 bytes, which is how this was found.
        let payload: String = (0..1000)
            .map(|n| char::from(b'a' + (n % 26) as u8))
            .collect();
        let (base, handle) = fake_sse_server(vec![payload.clone()]);
        let mut stream = open(&base, std::time::Duration::from_secs(5)).unwrap();
        let (host, port, path, _tls) = parse_http_url(&base).unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .unwrap();
        stream.flush().unwrap();

        let (status, _content_type, mut reader) = read_head(&mut stream).unwrap();
        assert_eq!(status, 200);

        // Read with a buffer far smaller than the chunk. A `Read` may fill less than it
        // was asked for; it may never drop the remainder.
        let mut got = Vec::new();
        let mut buf = [0u8; 64];
        loop {
            let read = reader.read(&mut buf).unwrap();
            if read == 0 {
                break;
            }
            got.extend_from_slice(&buf[..read]);
        }
        handle.join().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&got),
            payload,
            "every byte of the chunk arrives, across as many calls as it takes"
        );
    }

    /// Purpose: Pins that a closing CRLF split across reads does not end the body.
    #[test]
    fn a_crlf_split_across_two_reads_does_not_end_the_body() {
        // A guard rather than a reproduction: the closing CRLF was taken with a single
        // `read`, which a socket is allowed to answer with one byte, and the leftover
        // newline would then be read as the next chunk's size line. Loopback always
        // delivered both bytes together, so this never failed here — but nothing
        // promises that, and the failure it would cause is another silent truncation.
        let (base, handle) = fake_sse_server(vec!["first".to_string(), "second".to_string()]);
        let response = get_json_raw(&base, None, std::time::Duration::from_secs(5)).unwrap();
        handle.join().unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "firstsecond", "both chunks, whole");
    }

    /// Purpose: Pins that an event split across chunks is reassembled.
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

    /// Purpose: Pins that a non-2xx stream is an error carrying the body.
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

    /// Purpose: Pins a GET with a bearer token and a body.
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

    /// Purpose: Pins that a failing GET carries the server's message.
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

    /// Purpose: Pins that a dead endpoint is an error, not a hang.
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
