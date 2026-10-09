//! Talking to the control plane: fetching the signed profile pack, verifying it,
//! caching it, and — for the control plane itself — serving it.
//!
//! ADR-0012 keeps the control plane small, and this crate keeps it to the one job
//! the local agent needs today: **pack distribution**. The client only ever reads;
//! publishing needs a separate admin token, so the token every install carries
//! cannot change policy.
//!
//! Offline is degraded, not dead (ADR-0012): the last verified pack is cached, and
//! the cache is only ever written *after* verification, so being disconnected can
//! never lower the policy standard.

use loong_pack::{key_id, Envelope, PackError, PROFILES_ARTIFACT, PROFILES_SCHEMA};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum ControlError {
    /// No control plane is configured yet: the first-run state, not a failure.
    NotConfigured,
    /// The address is not something we will send a token to.
    BadUrl(String),
    /// The control plane could not be reached, and nothing is cached either.
    Unreachable(String),
    /// The control plane answered, but has nothing to serve.
    NothingPublished(String),
    /// The pack did not verify. This is the important one, and it is never ignored
    /// in favour of a stale cache: a bad pack is not a reason to fall back.
    Rejected(PackError),
    Io(String),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::NotConfigured => write!(
                f,
                "no control plane is configured — run `loongagent setup` with your firm's \
                 control-plane address"
            ),
            ControlError::BadUrl(m) => write!(f, "control-plane address: {m}"),
            ControlError::Unreachable(m) => write!(f, "the control plane is unreachable: {m}"),
            ControlError::NothingPublished(m) => write!(f, "{m}"),
            ControlError::Rejected(e) => write!(f, "the pack was refused: {e}"),
            ControlError::Io(m) => write!(f, "io: {m}"),
        }
    }
}

impl std::error::Error for ControlError {}

/// What the client needs to know to fetch a pack, from the machine configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlConfig {
    pub base_url: String,
    /// The org's pinned public key.
    pub public_key: Vec<u8>,
    /// A reference to the client token (`env:NAME`, `file:PATH#KEY`), resolved by
    /// the caller: secrets never travel as literals in config (ADR-0008 §5).
    pub token: Option<String>,
    /// Which named profile this install runs (T-07: the one profile-related choice
    /// a user owns).
    pub profile: Option<String>,
}

impl ControlConfig {
    /// The origin, used to name the cache file: two control planes on one machine
    /// must not overwrite each other's packs.
    pub fn origin(&self) -> String {
        let trimmed = self
            .base_url
            .trim_start_matches("https://")
            .trim_start_matches("http://");
        trimmed.trim_end_matches('/').to_string()
    }

    /// Refuse to send a token in the clear to another machine. HTTPS is required
    /// everywhere except loopback, where the traffic cannot leave the machine
    /// (ADR-0016 §4).
    pub fn check_transport(&self) -> Result<(), ControlError> {
        let url = self.base_url.trim_end_matches('/');
        if let Some(rest) = url.strip_prefix("https://") {
            if rest.is_empty() {
                return Err(ControlError::BadUrl("no host".to_string()));
            }
            return Ok(());
        }
        if let Some(rest) = url.strip_prefix("http://") {
            let host = rest.split(['/', ':']).next().unwrap_or("");
            let loopback =
                host == "localhost" || host == "127.0.0.1" || host == "::1" || host == "[::1]";
            if loopback {
                return Ok(());
            }
            return Err(ControlError::BadUrl(format!(
                "{url} is plain HTTP to another machine; a pack decides where prompts go, so it \
                 must travel over TLS (use https://, or a loopback address for a local control \
                 plane)"
            )));
        }
        Err(ControlError::BadUrl(format!(
            "{url} must start with https:// (or http:// on loopback)"
        )))
    }

    /// The pack for this install, plus whose it is.
    pub fn pack_url(&self) -> String {
        format!("{}/v1/pack/profiles", self.base_url.trim_end_matches('/'))
    }
}

/// A pack that verified. The `from_cache` flag is part of the record: a session
/// should be able to say whether it ran on a live or last-known pack.
#[derive(Debug, Clone)]
pub struct Synced {
    pub envelope: Envelope,
    pub from_cache: bool,
    pub note: Option<String>,
}

/// The last verified pack, per control plane, under the state directory.
#[derive(Debug, Clone)]
pub struct Cache {
    path: PathBuf,
}

impl Cache {
    pub fn new(state_dir: impl AsRef<Path>, origin: &str) -> Self {
        let safe: String = origin
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Cache {
            path: state_dir
                .as_ref()
                .join("control")
                .join(safe)
                .join("profiles.json"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Option<Envelope> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        Envelope::from_json(&text).ok()
    }

    /// Written to a temporary file and renamed, so a crash mid-write cannot leave a
    /// half pack where the next run would read it.
    ///
    /// Two writers are normal, not exotic: a surface and the server it spawns both
    /// sync, and so do two terminals. The temporary name is therefore unique per
    /// *write* — with a fixed name they overwrite each other's half-written file and
    /// the next reader fails verification, which kills that session before it prints
    /// anything. A pack that is already there is not rewritten at all, which removes
    /// most of the window.
    pub fn store(&self, envelope: &Envelope) -> Result<(), ControlError> {
        let json = envelope.to_json().map_err(ControlError::Rejected)?;
        // Compare what the file *means*, not how it is spelled.
        if let Ok(on_disk) = std::fs::read_to_string(&self.path) {
            if Envelope::from_json(&on_disk).as_ref().ok() == Some(envelope) {
                return Ok(());
            }
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ControlError::Io(e.to_string()))?;
        }
        static WRITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let stamp = WRITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary = self
            .path
            .with_extension(format!("json.{}.{stamp}.tmp", std::process::id()));
        std::fs::write(&temporary, &json).map_err(|e| ControlError::Io(e.to_string()))?;
        match std::fs::rename(&temporary, &self.path) {
            Ok(()) => Ok(()),
            // Another writer got there first with a complete file: the outcome we
            // wanted anyway.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&temporary);
                Err(ControlError::Io(e.to_string()))
            }
        }
    }
}

/// Fetch the pack, verify it, cache it; fall back to the cache when the control
/// plane cannot be reached.
///
/// A pack that *verifies* badly is never replaced by the cache — a forged or broken
/// pack is a reason to stop, not to quietly run yesterday's policy.
pub fn sync(
    config: &ControlConfig,
    cache: &Cache,
    token: Option<&str>,
    timeout: Duration,
) -> Result<Synced, ControlError> {
    config.check_transport()?;
    match fetch(config, token, timeout) {
        Ok(envelope) => {
            envelope
                .verify(&config.public_key, &[PROFILES_SCHEMA])
                .map_err(ControlError::Rejected)?;
            cache.store(&envelope)?;
            Ok(Synced {
                envelope,
                from_cache: false,
                note: None,
            })
        }
        Err(ControlError::Rejected(e)) => Err(ControlError::Rejected(e)),
        Err(failure) => match cache.load() {
            Some(cached) => {
                cached
                    .verify(&config.public_key, &[PROFILES_SCHEMA])
                    .map_err(ControlError::Rejected)?;
                Ok(Synced {
                    envelope: cached,
                    from_cache: true,
                    note: Some(failure.to_string()),
                })
            }
            None => Err(failure),
        },
    }
}

fn fetch(
    config: &ControlConfig,
    token: Option<&str>,
    timeout: Duration,
) -> Result<Envelope, ControlError> {
    let response = loong_http::get_json(&config.pack_url(), token, timeout).map_err(|e| {
        let message = e.0;
        if message.contains("409") || message.contains("404") {
            ControlError::NothingPublished(format!(
                "the control plane has no profiles published yet — ask your profile administrator \
                 to publish a pack ({message})"
            ))
        } else {
            ControlError::Unreachable(message)
        }
    })?;
    Envelope::from_json(response.body.trim()).map_err(ControlError::Rejected)
}

// ------------------------------------------------------------------ the server

/// The control plane's side: a directory holding the published pack, a token every
/// install may use to read, and a separate token for publishing.
pub struct ControlPlane {
    dir: PathBuf,
    client_token: String,
    admin_token: String,
    /// The public key the served pack must verify under — derived from the signing
    /// key, or pinned alongside it.
    public_key: Vec<u8>,
    /// The signing key. **The control plane holds it** (owner decision, 2026-10-09):
    /// an administrator publishes a draft, and the control plane signs it, so the
    /// private key never has to be copied to an administrator's workstation. Absent
    /// means this control plane can distribute but not author.
    signing_key: Option<Vec<u8>>,
    issuer: String,
}

impl ControlPlane {
    pub fn new(
        dir: impl Into<PathBuf>,
        client_token: impl Into<String>,
        admin_token: impl Into<String>,
        public_key: Vec<u8>,
        issuer: impl Into<String>,
    ) -> Self {
        ControlPlane {
            dir: dir.into(),
            client_token: client_token.into(),
            admin_token: admin_token.into(),
            public_key,
            signing_key: None,
            issuer: issuer.into(),
        }
    }

    /// The same plane, holding a signing key: administrators publish drafts and the
    /// control plane signs them (owner decision, 2026-10-09).
    pub fn with_signing_key(mut self, key_pkcs8: Vec<u8>) -> Result<Self, ControlError> {
        let derived = loong_pack::public_of(&key_pkcs8).map_err(ControlError::Rejected)?;
        if derived != self.public_key {
            // A control plane signing with a key nobody pinned would publish packs
            // that every install refuses — better to refuse to start.
            return Err(ControlError::Rejected(loong_pack::PackError::WrongKey {
                expected: loong_pack::key_id(&self.public_key),
                found: loong_pack::key_id(&derived),
            }));
        }
        self.signing_key = Some(key_pkcs8);
        Ok(self)
    }

    pub fn can_sign(&self) -> bool {
        self.signing_key.is_some()
    }

    pub fn pack_path(&self) -> PathBuf {
        self.dir.join("profiles.json")
    }

    /// Sign and publish a **draft** payload.
    ///
    /// The administrator sends the profile TOML they wrote; validation happens here
    /// (`profiles_from_payload`) so a pack no install could resolve is refused at
    /// publish time, and the signature is made with the key this control plane holds.
    pub fn publish_draft(
        &self,
        payload: &str,
        version: &str,
        issuer: &str,
        scope: &str,
    ) -> Result<Envelope, ControlError> {
        let key = self.signing_key.as_ref().ok_or_else(|| {
            ControlError::Rejected(PackError::Malformed(
                "this control plane has no signing key — start it with --key so it can sign \
                 what you publish"
                    .to_string(),
            ))
        })?;
        // Refuse before signing anything an install would reject.
        loong_config::profiles_from_payload(payload).map_err(|e| {
            ControlError::Rejected(PackError::Malformed(format!(
                "the draft is not a usable profile pack: {e}"
            )))
        })?;
        let envelope = Envelope::sign(
            key,
            PROFILES_ARTIFACT,
            PROFILES_SCHEMA,
            version,
            issuer,
            scope,
            now_ms(),
            payload,
        )
        .map_err(ControlError::Rejected)?;
        self.store(&envelope)?;
        Ok(envelope)
    }

    /// Accept an already-signed envelope. Kept for an air-gapped administrator who
    /// signs elsewhere; it must verify under the key this control plane serves.
    pub fn publish(&self, json: &str) -> Result<Envelope, ControlError> {
        let envelope = Envelope::from_json(json).map_err(ControlError::Rejected)?;
        if envelope.artifact != PROFILES_ARTIFACT {
            return Err(ControlError::Rejected(PackError::Malformed(format!(
                "artifact is {:?}, expected {PROFILES_ARTIFACT:?}",
                envelope.artifact
            ))));
        }
        envelope
            .verify(&self.public_key, &[PROFILES_SCHEMA])
            .map_err(ControlError::Rejected)?;
        self.store(&envelope)?;
        Ok(envelope)
    }

    fn store(&self, envelope: &Envelope) -> Result<(), ControlError> {
        let json = envelope.to_json().map_err(ControlError::Rejected)?;
        std::fs::create_dir_all(&self.dir).map_err(|e| ControlError::Io(e.to_string()))?;
        let temporary = self.pack_path().with_extension("json.tmp");
        std::fs::write(&temporary, json).map_err(|e| ControlError::Io(e.to_string()))?;
        std::fs::rename(&temporary, self.pack_path())
            .map_err(|e| ControlError::Io(e.to_string()))?;
        Ok(())
    }

    pub fn has_pack(&self) -> bool {
        self.pack_path().exists()
    }

    /// Serve until the listener is dropped: health without a token, the pack with
    /// the client token, publishing with the admin token.
    pub fn serve(&self, listener: TcpListener) -> std::io::Result<()> {
        for stream in listener.incoming() {
            let mut stream = stream?;
            let _ = self.handle(&mut stream);
        }
        Ok(())
    }

    fn handle(&self, stream: &mut TcpStream) -> std::io::Result<()> {
        let mut reader = BufReader::new(stream.try_clone()?);
        let mut request_line = String::new();
        if reader.read_line(&mut request_line)? == 0 {
            return Ok(());
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_string();
        let path = parts.next().unwrap_or("").to_string();

        let mut token = None;
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header)? == 0 {
                break;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            let lowered = header.to_ascii_lowercase();
            if let Some(value) = lowered.strip_prefix("authorization: bearer ") {
                token = Some(value.trim().to_string());
            }
            if let Some(value) = lowered.strip_prefix("content-length: ") {
                length = value.trim().parse().unwrap_or(0);
            }
        }
        // The original spelling matters for tokens; re-read it case-sensitively.
        let token = token.or_else(|| None);

        let mut body = vec![0u8; length];
        if length > 0 {
            reader.read_exact(&mut body)?;
        }
        let body = String::from_utf8_lossy(&body).to_string();

        let (status, payload) = match (method.as_str(), path.as_str()) {
            ("GET", "/v1/health") => (200, self.health()),
            ("GET", "/v1/pack/profiles") => {
                if !self.authorised(token.as_deref(), &self.client_token) {
                    (401, json_error("a client token is required"))
                } else if !self.has_pack() {
                    (409, json_error("no profiles have been published yet"))
                } else {
                    match std::fs::read_to_string(self.pack_path()) {
                        Ok(text) => (200, text),
                        Err(e) => (500, json_error(&format!("reading the pack: {e}"))),
                    }
                }
            }
            ("POST", "/v1/admin/publish") => {
                if !self.authorised(token.as_deref(), &self.admin_token) {
                    (401, json_error("the admin token is required to publish"))
                } else {
                    match self.accept(&body) {
                        Ok((envelope, profiles)) => {
                            // Say it where the administrator will see it, every time.
                            let warnings = loong_config::profiles_from_payload(&envelope.payload)
                                .map(|pack| pack.warnings)
                                .unwrap_or_default();
                            for warning in &warnings {
                                eprintln!("published a pack with a warning: {warning}");
                            }
                            (
                                200,
                                serde_json::json!({
                                    "published": true,
                                    "version": envelope.version,
                                    "issuer": envelope.issuer,
                                    "key_id": envelope.key_id,
                                    "profiles": profiles,
                                    "warnings": warnings,
                                })
                                .to_string(),
                            )
                        }
                        Err(e) => (400, json_error(&e.to_string())),
                    }
                }
            }
            _ => (404, json_error("no such endpoint")),
        };
        write_response(stream, status, &payload)
    }

    /// What `/v1/admin/publish` accepts: a draft this plane signs, or an envelope
    /// signed elsewhere. Told apart by shape, and both are validated before storing.
    fn accept(&self, body: &str) -> Result<(Envelope, Vec<String>), ControlError> {
        let trimmed = body.trim();
        let envelope = if trimmed.starts_with('{') {
            let request: serde_json::Value = serde_json::from_str(trimmed)
                .map_err(|e| ControlError::Rejected(PackError::Json(e.to_string())))?;
            match request.get("payload").and_then(|p| p.as_str()) {
                Some(payload) => {
                    let version =
                        request
                            .get("version")
                            .and_then(|v| v.as_str())
                            .ok_or_else(|| {
                                ControlError::Rejected(PackError::Malformed(
                                    "a draft needs a version".to_string(),
                                ))
                            })?;
                    let issuer = request
                        .get("issuer")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&self.issuer)
                        .to_string();
                    let scope = request
                        .get("scope")
                        .and_then(|v| v.as_str())
                        .unwrap_or("org");
                    self.publish_draft(payload, version, &issuer, scope)?
                }
                // An envelope, signed elsewhere, is JSON too. Tell them apart by the
                // field that only an envelope has.
                None if request.get("signature").is_some() => self.publish(trimmed)?,
                None => {
                    return Err(ControlError::Rejected(PackError::Malformed(
                        "send either a draft (\"payload\", \"version\") or a signed envelope"
                            .to_string(),
                    )))
                }
            }
        } else if trimmed.starts_with("[[profiles]]") || trimmed.starts_with("default_profile") {
            // A bare TOML draft, for `curl --data-binary @profiles.toml`.
            let version = format!("{}", now_ms());
            self.publish_draft(trimmed, &version, &self.issuer.clone(), "org")?
        } else {
            return Err(ControlError::Rejected(PackError::Malformed(
                "send a draft as JSON ({\"payload\", \"version\"}) or as TOML".to_string(),
            )));
        };
        let names = loong_config::profiles_from_payload(&envelope.payload)
            .map(|pack| pack.profiles.into_iter().map(|p| p.name).collect())
            .unwrap_or_default();
        Ok((envelope, names))
    }

    fn health(&self) -> String {
        let mut health = serde_json::json!({
            "ok": true,
            "issuer": self.issuer,
            "key_id": key_id(&self.public_key),
            "has_pack": self.has_pack(),
            "can_sign": self.can_sign(),
        });
        if let Ok(text) = std::fs::read_to_string(self.pack_path()) {
            if let Ok(envelope) = Envelope::from_json(&text) {
                health["version"] = serde_json::json!(envelope.version);
                health["artifact"] = serde_json::json!(envelope.artifact);
                // Observable, not buried: how many credentials this plane is shipping
                // in the clear.
                if let Ok(pack) = loong_config::profiles_from_payload(&envelope.payload) {
                    health["plaintext_secrets"] = serde_json::json!(pack.warnings.len());
                }
            }
        }
        health.to_string()
    }

    fn authorised(&self, token: Option<&str>, expected: &str) -> bool {
        match token {
            Some(token) => token == expected,
            None => false,
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn json_error(message: &str) -> String {
    serde_json::json!({ "error": message }).to_string()
}

fn write_response(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Internal Server Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;
    stream.flush()
}

/// Bind a control plane. Kept here rather than in a binary so tests can start one
/// on an ephemeral port.
pub fn bind(addr: &str, port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind((addr, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use loong_pack::{generate_keypair, public_of, Envelope};

    fn pack(private: &[u8], version: &str, payload: &str) -> Envelope {
        Envelope::sign(
            private,
            PROFILES_ARTIFACT,
            PROFILES_SCHEMA,
            version,
            "acme-platform",
            "org",
            1_791_500_000_000,
            payload,
        )
        .unwrap()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "loong-control-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A control plane and a client, in one process, over a real socket.
    fn with_plane<T>(f: impl FnOnce(&ControlConfig, &Cache, &ControlPlane, TcpListener) -> T) -> T {
        let (private, public) = generate_keypair().unwrap();
        let dir = temp("plane");
        let plane = ControlPlane::new(&dir, "client-token", "admin-token", public.clone(), "acme");
        let listener = bind("127.0.0.1", 0).unwrap();
        let addr = listener.local_addr().unwrap();
        let config = ControlConfig {
            base_url: format!("http://127.0.0.1:{}/", addr.port()),
            public_key: public,
            token: Some("client-token".to_string()),
            profile: None,
        };
        let cache = Cache::new(temp("cache"), &config.origin());
        let serving = plane.clone_for_test();
        std::thread::spawn(move || {
            let _ = serving.serve(listener);
        });
        let envelope = pack(&private, "2026.10.09-1", "default_profile = \"default\"\n");
        plane.publish(&envelope.to_json().unwrap()).unwrap();
        // Re-bind a listener only for the caller's inspection: the serving thread owns
        // the real one, so hand back a fresh bind on the same port after shutdown is
        // not needed — the plane is reachable by the config above.
        let spare = bind("127.0.0.1", 0).unwrap();
        f(&config, &cache, &plane, spare)
    }

    impl ControlPlane {
        fn clone_for_test(&self) -> ControlPlane {
            ControlPlane {
                dir: self.dir.clone(),
                client_token: self.client_token.clone(),
                admin_token: self.admin_token.clone(),
                public_key: self.public_key.clone(),
                signing_key: self.signing_key.clone(),
                issuer: self.issuer.clone(),
            }
        }
    }

    #[test]
    fn a_client_fetches_verifies_and_caches_a_published_pack() {
        with_plane(|config, cache, _, _| {
            let synced = sync(config, cache, config.token.as_deref(), DEFAULT_TIMEOUT).unwrap();
            assert!(!synced.from_cache);
            assert_eq!(synced.envelope.version, "2026.10.09-1");
            assert!(synced.envelope.payload.contains("default_profile"));
            assert!(cache.path().exists(), "the pack is cached for offline use");
            assert_eq!(cache.load().unwrap(), synced.envelope);
        });
    }

    #[test]
    fn an_unreachable_control_plane_falls_back_to_the_last_verified_pack() {
        with_plane(|config, cache, _, _| {
            sync(config, cache, config.token.as_deref(), DEFAULT_TIMEOUT).unwrap();
            // Now point at a port nobody is listening on.
            let offline = ControlConfig {
                base_url: "http://127.0.0.1:9/".to_string(),
                ..config.clone()
            };
            let synced = sync(&offline, cache, None, Duration::from_secs(2)).unwrap();
            assert!(synced.from_cache, "offline is degraded, not dead");
            assert!(synced.note.unwrap().contains("unreachable"));
        });
    }

    #[test]
    fn a_pack_that_does_not_verify_is_never_used_and_never_cached() {
        with_plane(|config, cache, plane, _| {
            // A pack signed by somebody else, published by a control plane that does
            // not check — the case the client must survive.
            let (other_private, _) = generate_keypair().unwrap();
            let forged = pack(
                &other_private,
                "2026.10.09-9",
                "default_profile = \"evil\"\n",
            );
            std::fs::write(plane.pack_path(), forged.to_json().unwrap()).unwrap();

            let error = sync(config, cache, config.token.as_deref(), DEFAULT_TIMEOUT).unwrap_err();
            assert!(
                matches!(error, ControlError::Rejected(_)),
                "a forgery must not fall back to the cache: {error}"
            );
            assert!(!cache.path().exists(), "and nothing is cached from it");
        });
    }

    #[test]
    fn the_control_plane_signs_a_draft_with_the_key_it_holds() {
        let (private, public) = generate_keypair().unwrap();
        let dir = temp("signing");
        let plane = ControlPlane::new(&dir, "c", "a", public.clone(), "acme")
            .with_signing_key(private.clone())
            .unwrap();
        assert!(plane.can_sign());

        let draft = "default_profile = \"default\"\n[[profiles]]\nname = \"default\"\n\
                     description = \"balanced\"\n  [[profiles.models]]\n  name = \"m\"\n\
                     base_url = \"https://a.invalid\"\n  model = \"i\"\n";
        let envelope = plane
            .publish_draft(draft, "2026.10.09-2", "acme", "org")
            .unwrap();
        assert_eq!(envelope.version, "2026.10.09-2");
        assert_eq!(envelope.key_id, key_id(&public));
        // The pack it just signed is one every install can accept.
        envelope.verify(&public, &[PROFILES_SCHEMA]).unwrap();
        assert!(plane.has_pack());

        // A draft the resolver would reject never becomes a pack.
        let broken = "[[profiles]]\nname = \"x\"\n";
        let error = plane
            .publish_draft(broken, "v3", "acme", "org")
            .unwrap_err()
            .to_string();
        assert!(error.contains("not a usable profile pack"), "{error}");
        // …and the good pack is still the one being served.
        let served =
            Envelope::from_json(&std::fs::read_to_string(plane.pack_path()).unwrap()).unwrap();
        assert_eq!(served.version, "2026.10.09-2");
    }

    #[test]
    fn a_control_plane_without_a_signing_key_says_so_instead_of_guessing() {
        let (_, public) = generate_keypair().unwrap();
        let dir = temp("no-key");
        let plane = ControlPlane::new(&dir, "c", "a", public, "acme");
        assert!(!plane.can_sign());
        let error = plane
            .publish_draft("default_profile = \"d\"\n", "v1", "acme", "org")
            .unwrap_err()
            .to_string();
        assert!(error.contains("no signing key"), "{error}");
        assert!(
            error.contains("--key"),
            "and it says how to fix it: {error}"
        );
    }

    #[test]
    fn a_signing_key_that_does_not_match_the_served_public_key_refuses_to_start() {
        let (_, public) = generate_keypair().unwrap();
        let (other_private, _) = generate_keypair().unwrap();
        let dir = temp("mismatch");
        let plane = ControlPlane::new(&dir, "c", "a", public, "acme");
        match plane.with_signing_key(other_private) {
            Err(ControlError::Rejected(loong_pack::PackError::WrongKey { expected, found })) => {
                assert_ne!(expected, found);
            }
            Ok(plane) => panic!(
                "expected a key mismatch, but it started: can_sign={}",
                plane.can_sign()
            ),
            Err(other) => panic!("expected a key mismatch, got {other}"),
        }
    }

    #[test]
    fn the_publish_endpoint_takes_a_draft_and_signs_it() {
        let (private, public) = generate_keypair().unwrap();
        let dir = temp("endpoint-sign");
        let plane = ControlPlane::new(&dir, "client-token", "admin-token", public.clone(), "acme")
            .with_signing_key(private)
            .unwrap();
        let listener = bind("127.0.0.1", 0).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = plane.serve(listener);
        });
        let draft = "default_profile = \"default\"\n[[profiles]]\nname = \"default\"\n\
                     description = \"balanced\"\n  [[profiles.models]]\n  name = \"m\"\n\
                     base_url = \"https://a.invalid\"\n  model = \"i\"\n";
        let body = serde_json::json!({
            "payload": draft,
            "version": "2026.10.09-3",
            "issuer": "acme",
            "scope": "org",
        })
        .to_string();
        let response = loong_http::post_json(
            &format!("http://127.0.0.1:{}/v1/admin/publish", addr.port()),
            &body,
            Some("admin-token"),
            DEFAULT_TIMEOUT,
        )
        .unwrap();
        assert_eq!(response.status, 200, "{}", response.body);
        assert!(response.body.contains("2026.10.09-3"), "{}", response.body);
        assert!(response.body.contains("default"), "{}", response.body);

        // And a client reads back something it can verify.
        let fetched = loong_http::get_json(
            &format!("http://127.0.0.1:{}/v1/pack/profiles", addr.port()),
            Some("client-token"),
            DEFAULT_TIMEOUT,
        )
        .unwrap();
        let envelope = Envelope::from_json(fetched.body.trim()).unwrap();
        envelope.verify(&public, &[PROFILES_SCHEMA]).unwrap();
    }

    #[test]
    fn the_control_plane_refuses_to_serve_what_no_client_could_accept() {
        let (private, public) = generate_keypair().unwrap();
        let dir = temp("publish-check");
        let plane = ControlPlane::new(&dir, "c", "a", public, "acme");

        // Correctly signed: accepted.
        let good = pack(&private, "v1", "default_profile = \"default\"\n");
        plane.publish(&good.to_json().unwrap()).unwrap();
        assert!(plane.has_pack());

        // Signed by another key: refused, and the previous pack stays.
        let (other, _) = generate_keypair().unwrap();
        let bad = pack(&other, "v2", "default_profile = \"evil\"\n");
        assert!(matches!(
            plane.publish(&bad.to_json().unwrap()),
            Err(ControlError::Rejected(_))
        ));
        assert_eq!(
            Envelope::from_json(&std::fs::read_to_string(plane.pack_path()).unwrap())
                .unwrap()
                .version,
            "v1"
        );

        // The right signature over the wrong artifact id: refused.
        let wrong_artifact = Envelope::sign(
            &private,
            "loongagent.policy",
            PROFILES_SCHEMA,
            "v3",
            "acme",
            "org",
            0,
            "x",
        )
        .unwrap();
        assert!(plane.publish(&wrong_artifact.to_json().unwrap()).is_err());
    }

    #[test]
    fn tokens_are_required_and_separated() {
        let (private, public) = generate_keypair().unwrap();
        let dir = temp("tokens");
        let plane = ControlPlane::new(&dir, "client-token", "admin-token", public, "acme");
        plane
            .publish(
                &pack(&private, "v1", "default_profile = \"d\"\n")
                    .to_json()
                    .unwrap(),
            )
            .unwrap();
        let listener = bind("127.0.0.1", 0).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = plane.serve(listener);
        });

        // Health needs no token.
        let health = loong_http::get_json(
            &format!("http://127.0.0.1:{}/v1/health", addr.port()),
            None,
            DEFAULT_TIMEOUT,
        )
        .unwrap();
        assert!(health.body.contains("\"has_pack\":true"), "{}", health.body);

        // The pack needs the client token.
        let pack_url = format!("http://127.0.0.1:{}/v1/pack/profiles", addr.port());
        assert!(loong_http::get_json(&pack_url, None, DEFAULT_TIMEOUT).is_err());
        assert!(
            loong_http::get_json(&pack_url, Some("wrong"), DEFAULT_TIMEOUT).is_err(),
            "a wrong token is not a token"
        );
        assert!(loong_http::get_json(&pack_url, Some("client-token"), DEFAULT_TIMEOUT).is_ok());

        // The client token cannot publish. `post_json` hands back the status rather
        // than failing (the provider adapters read 4xx themselves), so the test reads
        // it too.
        let publish_url = format!("http://127.0.0.1:{}/v1/admin/publish", addr.port());
        let refused =
            loong_http::post_json(&publish_url, "{}", Some("client-token"), DEFAULT_TIMEOUT)
                .unwrap();
        assert_eq!(
            refused.status, 401,
            "reading is not publishing: {}",
            refused.body
        );
        assert!(refused.body.contains("admin token"), "{}", refused.body);

        // With the admin token it is authorised, and then the *content* is checked:
        // an unsigned body is refused, so the control plane never serves something no
        // client could accept.
        let unsigned =
            loong_http::post_json(&publish_url, "{}", Some("admin-token"), DEFAULT_TIMEOUT)
                .unwrap();
        assert_eq!(unsigned.status, 400, "{}", unsigned.body);
    }

    #[test]
    fn a_control_plane_with_nothing_published_says_so() {
        let (_, public) = generate_keypair().unwrap();
        let dir = temp("empty");
        let plane = ControlPlane::new(&dir, "c", "a", public, "acme");
        let listener = bind("127.0.0.1", 0).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let _ = plane.serve(listener);
        });
        let config = ControlConfig {
            base_url: format!("http://127.0.0.1:{}/", addr.port()),
            public_key: public_of(&generate_keypair().unwrap().0).unwrap(),
            token: Some("c".to_string()),
            profile: None,
        };
        let cache = Cache::new(temp("empty-cache"), &config.origin());
        let error = sync(&config, &cache, Some("c"), DEFAULT_TIMEOUT).unwrap_err();
        assert!(
            matches!(error, ControlError::NothingPublished(_)),
            "an empty control plane must say so, not look like a network failure: {error}"
        );
        assert!(
            error.to_string().contains("profile administrator"),
            "{error}"
        );
    }

    #[test]
    fn plain_http_is_refused_off_loopback() {
        let config = ControlConfig {
            base_url: "http://control.acme.internal".to_string(),
            public_key: vec![0; 32],
            token: None,
            profile: None,
        };
        let error = config.check_transport().unwrap_err().to_string();
        assert!(error.contains("TLS"), "{error}");
        assert!(ControlConfig {
            base_url: "https://control.acme.internal".to_string(),
            ..config.clone()
        }
        .check_transport()
        .is_ok());
        assert!(ControlConfig {
            base_url: "http://127.0.0.1:8738".to_string(),
            ..config
        }
        .check_transport()
        .is_ok());
    }

    #[test]
    fn several_writers_do_not_corrupt_the_cache() {
        // A surface and the server it spawns both sync; so do two terminals.
        let (private, public) = generate_keypair().unwrap();
        let state = temp("cache-race");
        let cache = std::sync::Arc::new(Cache::new(&state, "control.acme.internal"));
        let mut writers = Vec::new();
        for round in 0..8 {
            let cache = std::sync::Arc::clone(&cache);
            let envelope = pack(&private, &format!("v{round}"), "default_profile = \"d\"\n");
            writers.push(std::thread::spawn(move || {
                for _ in 0..25 {
                    cache.store(&envelope).unwrap();
                }
            }));
        }
        for writer in writers {
            writer.join().unwrap();
        }
        // Whatever won, the file is a whole, verifiable pack — never a half write.
        let stored = cache.load().expect("a readable pack");
        stored.verify(&public, &[PROFILES_SCHEMA]).unwrap();
        // And no scratch files are left behind.
        let leftovers: Vec<_> = std::fs::read_dir(cache.path().parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
    }

    #[test]
    fn an_unchanged_pack_is_not_written_again() {
        let (private, public) = generate_keypair().unwrap();
        let state = temp("cache-idempotent");
        let cache = Cache::new(&state, "control.acme.internal");
        let envelope = pack(&private, "v1", "default_profile = \"d\"\n");
        cache.store(&envelope).unwrap();
        let first = std::fs::metadata(cache.path()).unwrap().modified().unwrap();
        cache.store(&envelope).unwrap();
        let second = std::fs::metadata(cache.path()).unwrap().modified().unwrap();
        assert_eq!(first, second, "the same pack must not be rewritten");
        cache
            .load()
            .unwrap()
            .verify(&public, &[PROFILES_SCHEMA])
            .unwrap();
    }

    #[test]
    fn the_cache_is_named_after_the_control_plane() {
        let state = temp("cache-naming");
        let a = Cache::new(&state, "control.acme.internal");
        let b = Cache::new(&state, "127.0.0.1:8738");
        assert_ne!(
            a.path(),
            b.path(),
            "two control planes must not share a cache"
        );
        assert!(a.path().to_string_lossy().contains("control.acme.internal"));
    }
}
