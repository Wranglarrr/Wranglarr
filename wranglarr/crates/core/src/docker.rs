// SPDX-License-Identifier: GPL-3.0-or-later
//! Just enough of the Docker Engine API (over the unix socket) for restores and
//! updates. Bodies stay raw JSON: recreating a container copies what inspect
//! returned, and a typed model would drop fields it doesn't know.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use regex::Regex;
use serde_json::{Value, json};
use tokio::net::UnixStream;

use crate::error::Error;

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Other(String),
}

impl From<DockerError> for Error {
    fn from(e: DockerError) -> Self {
        match e {
            DockerError::NotFound(m) => Error::NotFound(m),
            DockerError::Other(m) => Error::upstream(m),
        }
    }
}

pub type DResult<T> = Result<T, DockerError>;

/// Docker tags are one ASCII component, never a repository or digest.
pub fn valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 128
        && tag.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        && tag.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// A namespace-only recreation can use an immutable id while retaining the
/// original tracking reference in a label for the next registry check.
pub const IMAGE_REFERENCE_LABEL: &str = "io.wranglarr.image-reference";

pub fn image_reference(container: &Value) -> &str {
    container["Config"]["Labels"][IMAGE_REFERENCE_LABEL]
        .as_str()
        .or_else(|| container["Config"]["Image"].as_str())
        .unwrap_or("")
}

/// `lscr.io/linuxserver/radarr:latest` -> (repo, tag, digest). Digest wins over tag.
pub fn split_ref(reference: &str) -> (String, String, String) {
    let (r, digest) = match reference.split_once('@') {
        Some((r, d)) => (r, d.to_string()),
        None => (reference, String::new()),
    };
    let last = r.rsplit('/').next().unwrap_or(r);
    let (repo, tag) = if last.contains(':') {
        r.rsplit_once(':').map(|(a, b)| (a.to_string(), b.to_string())).unwrap_or((r.to_string(), String::new()))
    } else {
        (r.to_string(), String::new())
    };
    let tag = if !tag.is_empty() {
        tag
    } else if digest.is_empty() {
        "latest".into()
    } else {
        String::new()
    };
    (repo, tag, digest)
}

/// Percent-encode a query value (everything but unreserved characters).
pub fn q(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub struct Response {
    pub status: u16,
    pub body: Bytes,
}

impl Response {
    pub fn json(&self) -> DResult<Value> {
        serde_json::from_slice(&self.body).map_err(|e| DockerError::Other(format!("docker: bad JSON: {e}")))
    }
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// A request owns its connection driver, including on timeout or cancellation.
struct ConnectionTask(tokio::task::JoinHandle<()>);

impl Drop for ConnectionTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub struct Docker {
    pub socket_path: String,
    /// 0 = not probed yet, 1 = engine has /distribution, 2 = it doesn't (Podman)
    distribution: AtomicU8,
    /// reference -> (when, digest): what the registry answered a moment ago
    digests: Mutex<HashMap<String, (Instant, String)>>,
}

/// How long a registry's answer is reused: the check, auto-update and self-update
/// jobs and the checks after each update ask about the same tags within minutes.
const DIGEST_TTL: Duration = Duration::from_secs(10 * 60);
/// The waits before asking again after a registry's "too many requests" or a network blip.
#[cfg(not(test))]
const BACKOFF: [Duration; 3] = [Duration::from_secs(2), Duration::from_secs(10), Duration::from_secs(30)];
#[cfg(test)]
const BACKOFF: [Duration; 3] = [Duration::from_millis(5), Duration::from_millis(5), Duration::from_millis(5)];

static RETRY_AFTER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)retry-after:\s*([0-9.]+)\s*(µs|us|ms|s)?").expect("static regex"));

/// What a registry lookup can fail with for a moment: a registry's "slow down"
/// (GHCR's and Docker Hub's 429 toomanyrequests), a gateway error, or the network
/// (a DNS query dropped by a busy or rate-limiting resolver, a timeout).
const TRANSIENT: &[&str] = &[
    "toomanyrequests",
    "-> 429",
    "rate limited",
    "-> 502",
    "-> 503",
    "-> 504",
    "i/o timeout",
    "timed out",
    "temporary failure in name resolution",
    "server misbehaving",
    "connection reset",
    "tls handshake timeout",
    "error sending request",
    "dns error",
];

/// Whether `msg` is worth asking again for, and the wait the registry asks for, if it says.
fn transient(msg: &str) -> Option<Option<Duration>> {
    let m = msg.to_ascii_lowercase();
    if !TRANSIENT.iter().any(|t| m.contains(t)) {
        return None;
    }
    Some(RETRY_AFTER.captures(msg).and_then(|c| {
        let n: f64 = c[1].parse().ok()?;
        let secs = match c.get(2).map(|u| u.as_str()) {
            Some("µs" | "us") => n / 1e6,
            Some("ms") => n / 1e3,
            _ => n,
        };
        Some(Duration::from_secs_f64(secs.min(120.0)))
    }))
}

impl Docker {
    pub fn new(socket_path: &str) -> Docker {
        Docker {
            socket_path: socket_path.to_string(),
            distribution: AtomicU8::new(0),
            digests: Mutex::new(HashMap::new()),
        }
    }

    async fn raw(&self, method: Method, path: &str, body: Option<&Value>, timeout: Duration) -> DResult<Response> {
        let what = format!("docker {method} {path}");
        let fut = async {
            let stream =
                UnixStream::connect(&self.socket_path).await.map_err(|e| DockerError::Other(format!("{what}: {e}")))?;
            let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
                .await
                .map_err(|e| DockerError::Other(format!("{what}: {e}")))?;
            let _connection = ConnectionTask(tokio::spawn(async move {
                let _ = conn.await;
            }));
            let bytes = body.map(|b| Bytes::from(b.to_string())).unwrap_or_default();
            let mut req = Request::builder().method(method.clone()).uri(path).header("Host", "docker");
            if body.is_some() {
                req = req.header("Content-Type", "application/json");
            }
            let req = req.body(Full::new(bytes)).map_err(|e| DockerError::Other(format!("{what}: {e}")))?;
            let resp = sender.send_request(req).await.map_err(|e| DockerError::Other(format!("{what}: {e}")))?;
            let status = resp.status().as_u16();
            let body = Limited::new(resp.into_body(), crate::http::RESPONSE_LIMIT)
                .collect()
                .await
                .map_err(|e| DockerError::Other(format!("{what}: {e}")))?
                .to_bytes();
            Ok(Response { status, body })
        };
        tokio::time::timeout(timeout, fut).await.map_err(|_| DockerError::Other(format!("{what}: timed out")))?
    }

    async fn req(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        ok: &[u16],
        timeout: Duration,
    ) -> DResult<Response> {
        let m = method.to_string();
        let r = self.raw(method, path, body, timeout).await?;
        if r.status == 404 {
            return Err(DockerError::NotFound(format!("docker {path}: not found")));
        }
        if !ok.contains(&r.status) {
            let msg = r.json().ok().and_then(|j| j["message"].as_str().map(str::to_string)).unwrap_or_else(|| r.text());
            let msg: String = msg.chars().take(300).collect();
            return Err(DockerError::Other(format!("docker {m} {path} -> {}: {msg}", r.status)));
        }
        Ok(r)
    }

    const T: Duration = Duration::from_secs(60);

    pub async fn available(&self) -> bool {
        matches!(self.raw(Method::GET, "/_ping", None, Duration::from_secs(10)).await, Ok(r) if r.status == 200)
    }

    // ---- containers ----------------------------------------------------------
    pub async fn stop(&self, container: &str, timeout: u64) -> DResult<()> {
        tracing::info!("stopping container {container}");
        self.req(
            Method::POST,
            &format!("/containers/{container}/stop?t={timeout}"),
            None,
            &[204, 304],
            Duration::from_secs(timeout.saturating_add(30)),
        )
        .await?;
        Ok(())
    }

    pub async fn start(&self, container: &str) -> DResult<()> {
        tracing::info!("starting container {container}");
        self.req(Method::POST, &format!("/containers/{container}/start"), None, &[204, 304], Self::T).await?;
        Ok(())
    }

    pub async fn running(&self, container: &str) -> bool {
        self.inspect(container).await.is_ok_and(|j| j["State"]["Running"].as_bool().unwrap_or(false))
    }

    pub async fn inspect(&self, container: &str) -> DResult<Value> {
        self.req(Method::GET, &format!("/containers/{container}/json"), None, &[200], Self::T).await?.json()
    }

    /// Run a command in a running container; returns its exit code.
    pub async fn exec(&self, container: &str, cmd: &[&str], user: &str, timeout: Duration) -> DResult<i64> {
        let body = json!({"Cmd": cmd, "User": user, "AttachStdout": false, "AttachStderr": false});
        let r = self.req(Method::POST, &format!("/containers/{container}/exec"), Some(&body), &[201], Self::T).await?;
        let eid = r.json()?["Id"].as_str().unwrap_or_default().to_string();
        self.req(Method::POST, &format!("/exec/{eid}/start"), Some(&json!({"Detach": true})), &[200, 204], Self::T)
            .await?;
        let deadline = tokio::time::Instant::now() + timeout;
        while tokio::time::Instant::now() < deadline {
            let st = self.req(Method::GET, &format!("/exec/{eid}/json"), None, &[200], Self::T).await?.json()?;
            if !st["Running"].as_bool().unwrap_or(false) {
                return Ok(st["ExitCode"].as_i64().unwrap_or(0));
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Err(DockerError::Other(format!("docker exec in {container}: timed out")))
    }

    pub async fn containers(&self, all: bool) -> DResult<Vec<Value>> {
        let j = self
            .req(Method::GET, &format!("/containers/json?all={}", u8::from(all)), None, &[200], Self::T)
            .await?
            .json()?;
        Ok(j.as_array().cloned().unwrap_or_default())
    }

    pub async fn create(&self, name: &str, body: &Value) -> DResult<String> {
        let r = self
            .req(Method::POST, &format!("/containers/create?name={}", q(name)), Some(body), &[201], Self::T)
            .await?;
        Ok(r.json()?["Id"].as_str().unwrap_or_default().to_string())
    }

    pub async fn rename(&self, container: &str, new_name: &str) -> DResult<()> {
        self.req(
            Method::POST,
            &format!("/containers/{container}/rename?name={}", q(new_name)),
            None,
            &[204, 304],
            Self::T,
        )
        .await?;
        Ok(())
    }

    /// v=0: never delete the volumes a container used.
    pub async fn remove(&self, container: &str, force: bool) -> DResult<()> {
        self.req(
            Method::DELETE,
            &format!("/containers/{container}?force={}&v=0", u8::from(force)),
            None,
            &[204],
            Self::T,
        )
        .await?;
        Ok(())
    }

    pub async fn connect(&self, network: &str, container: &str, endpoint: &Value) -> DResult<()> {
        let body = json!({"Container": container, "EndpointConfig": endpoint});
        self.req(Method::POST, &format!("/networks/{network}/connect"), Some(&body), &[200], Self::T).await?;
        Ok(())
    }

    // ---- images --------------------------------------------------------------
    pub async fn image(&self, reference: &str) -> DResult<Value> {
        self.req(Method::GET, &format!("/images/{reference}/json"), None, &[200], Self::T).await?.json()
    }

    pub async fn tag(&self, image_id: &str, reference: &str) -> DResult<()> {
        let (repo, tag, _) = split_ref(reference);
        self.req(
            Method::POST,
            &format!("/images/{image_id}/tag?repo={}&tag={}", q(&repo), q(&tag)),
            None,
            &[200, 201],
            Self::T,
        )
        .await?;
        Ok(())
    }

    /// Digest the registry currently serves for `reference`, without pulling.
    /// An answer from the last few minutes is reused, and a registry's "too many
    /// requests" or a network blip is waited out and asked again (a few times)
    /// before it's an error.
    pub async fn remote_digest(&self, reference: &str) -> DResult<String> {
        if let Some((at, d)) = self.digests.lock().ok().and_then(|m| m.get(reference).cloned())
            && at.elapsed() < DIGEST_TTL
        {
            return Ok(d);
        }
        let mut waits = BACKOFF.iter();
        let digest = loop {
            let e = match self.lookup_digest(reference).await {
                Ok(d) => break d,
                Err(e) => e,
            };
            let (DockerError::Other(m), Some(wait)) = (&e, waits.next()) else { return Err(e) };
            let Some(asked) = transient(m) else { return Err(e) };
            let wait = asked.map_or(*wait, |a| a.max(*wait));
            tracing::info!("{reference}: {m}; asking again in {wait:?}");
            tokio::time::sleep(wait).await;
        };
        if let Ok(mut m) = self.digests.lock() {
            m.retain(|_, (at, _)| at.elapsed() < DIGEST_TTL);
            m.insert(reference.to_string(), (Instant::now(), digest.clone()));
        }
        Ok(digest)
    }

    /// Forget the registry answers kept, so the next lookup asks again.
    pub fn forget_digests(&self) {
        if let Ok(mut m) = self.digests.lock() {
            m.clear();
        }
    }

    /// Docker answers /distribution/{ref}/json (with the host's registry logins);
    /// Podman's compatible API doesn't, so ask the registry directly.
    async fn lookup_digest(&self, reference: &str) -> DResult<String> {
        if self.distribution.load(Ordering::Relaxed) != 2 {
            match self
                .req(Method::GET, &format!("/distribution/{reference}/json"), None, &[200], Duration::from_secs(30))
                .await
            {
                Ok(r) => {
                    self.distribution.store(1, Ordering::Relaxed);
                    let j = r.json()?;
                    return j["Descriptor"]["digest"]
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| DockerError::Other(format!("docker: no digest for {reference}")));
                }
                Err(DockerError::NotFound(m)) => {
                    if self.distribution.load(Ordering::Relaxed) == 1 {
                        return Err(DockerError::NotFound(m)); // the endpoint works, so the image really is unknown
                    }
                    self.distribution.store(2, Ordering::Relaxed);
                    tracing::info!("engine has no /distribution endpoint (Podman?); asking registries directly");
                }
                Err(e) => return Err(e),
            }
        }
        crate::registry::remote_digest(reference).await.map_err(|e| DockerError::Other(e.0))
    }

    pub async fn pull(&self, reference: &str) -> DResult<()> {
        let (repo, tag, digest) = split_ref(reference);
        let t = if digest.is_empty() { tag } else { digest };
        tracing::info!("pulling {reference}");
        let r = self
            .req(
                Method::POST,
                &format!("/images/create?fromImage={}&tag={}", q(&repo), q(&t)),
                None,
                &[200],
                Duration::from_secs(900),
            )
            .await?;
        for line in r.text().lines() {
            // progress stream; errors arrive in-band
            if let Ok(msg) = serde_json::from_str::<Value>(line)
                && let Some(err) = msg["error"].as_str()
            {
                return Err(DockerError::Other(format!("pull {reference}: {err}")));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::split_ref;

    async fn read_mock_request(stream: &mut tokio::net::UnixStream) {
        use tokio::io::AsyncReadExt;
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            header.push(stream.read_u8().await.expect("request byte"));
            assert!(header.len() <= 4096);
        }
    }

    #[test]
    fn refs() {
        let s = |r: &str| {
            let (a, b, c) = split_ref(r);
            format!("{a}|{b}|{c}")
        };
        assert_eq!(s("lscr.io/linuxserver/radarr:latest"), "lscr.io/linuxserver/radarr|latest|");
        assert_eq!(s("postgres"), "postgres|latest|");
        assert_eq!(s("localhost:5000/x"), "localhost:5000/x|latest|");
        assert_eq!(s("a/b@sha256:1"), "a/b||sha256:1");
        assert_eq!(s("a/b:2@sha256:1"), "a/b|2|sha256:1");
    }
    #[test]
    fn transient_errors() {
        use std::time::Duration;
        let ghcr = "docker GET /distribution/lscr.io/linuxserver/radarr:latest/json -> 429: \
                    toomanyrequests: retry-after: 323.116µs, allowed: 44000/minute";
        assert_eq!(super::transient(ghcr), Some(Some(Duration::from_nanos(323_116))));
        assert_eq!(super::transient("x -> 429: retry-after: 7s"), Some(Some(Duration::from_secs(7))));
        assert_eq!(super::transient("lscr.io: rate limited (toomanyrequests)"), Some(None));
        assert_eq!(super::transient("dial tcp: lookup ghcr.io on 127.0.0.53:53: i/o timeout"), Some(None));
        assert_eq!(super::transient("docker GET /distribution/x/json -> 401: unauthorized"), None);
        assert_eq!(super::transient("manifest unknown"), None);
    }

    /// One engine answer per connection: `answers` in order, then the test ends.
    async fn engine(answers: Vec<&'static str>) -> (tempfile::TempDir, super::Docker, tokio::task::JoinHandle<usize>) {
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().expect("tmp");
        let socket = dir.path().join("mock.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("mock listener");
        let peer = tokio::spawn(async move {
            let mut n = 0;
            for a in answers {
                let (mut stream, _) = listener.accept().await.expect("accept");
                read_mock_request(&mut stream).await;
                let (status, body) = a.split_once(' ').expect("status body");
                let head =
                    format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                stream.write_all(head.as_bytes()).await.expect("head");
                stream.write_all(body.as_bytes()).await.expect("body");
                n += 1;
            }
            n
        });
        let docker = super::Docker::new(socket.to_str().expect("path"));
        (dir, docker, peer)
    }

    #[tokio::test]
    async fn a_registry_slow_down_is_waited_out_and_the_answer_kept() {
        let ok = r#"200 {"Descriptor":{"digest":"sha256:ab"}}"#;
        let busy = r#"429 {"message":"toomanyrequests: retry-after: 323.116µs, allowed: 44000/minute"}"#;
        let (_dir, docker, peer) = engine(vec![busy, busy, ok]).await;
        assert_eq!(docker.remote_digest("lscr.io/linuxserver/radarr:latest").await.expect("digest"), "sha256:ab");
        // asked again within minutes (another job, the check after an update): not the registry
        assert_eq!(docker.remote_digest("lscr.io/linuxserver/radarr:latest").await.expect("kept"), "sha256:ab");
        assert_eq!(peer.await.expect("peer"), 3, "two slow-downs and one answer, then nothing");
    }

    #[tokio::test]
    async fn a_registry_that_keeps_saying_slow_down_is_an_error_in_the_end() {
        let busy = r#"429 {"message":"toomanyrequests: retry-after: 1µs"}"#;
        let (_dir, docker, peer) = engine(vec![busy; 4]).await;
        let e = docker.remote_digest("ghcr.io/a/b:latest").await.expect_err("gave up");
        assert!(e.to_string().contains("toomanyrequests"), "{e}");
        assert_eq!(peer.await.expect("peer"), 4, "asked once, then three more times");
        let (_dir, docker, peer) = engine(vec![r#"401 {"message":"unauthorized"}"#]).await;
        assert!(docker.remote_digest("ghcr.io/a/private:latest").await.is_err());
        assert_eq!(peer.await.expect("peer"), 1, "not worth asking again");
    }

    #[tokio::test]
    async fn cancelled_request_closes_connection_driver() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = tempfile::tempdir().expect("tmp");
        let socket = dir.path().join("mock.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("mock listener");
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            read_mock_request(&mut stream).await;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n").await.expect("headers");
            let mut buffer = [0; 1];
            stream.read(&mut buffer).await.expect("eof")
        });
        let docker = super::Docker::new(socket.to_str().expect("path"));
        assert!(docker.raw(hyper::Method::GET, "/mock", None, std::time::Duration::from_millis(50)).await.is_err());
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), peer).await.expect("driver stopped").expect("peer"),
            0
        );
    }

    #[tokio::test]
    async fn oversized_engine_response_is_rejected() {
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().expect("tmp");
        let socket = dir.path().join("mock.sock");
        let listener = tokio::net::UnixListener::bind(&socket).expect("mock listener");
        let peer = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            read_mock_request(&mut stream).await;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 16777217\r\n\r\n").await.expect("headers");
            let _ = stream.write_all(&vec![0; 16777217]).await;
        });
        let docker = super::Docker::new(socket.to_str().expect("path"));
        let err = docker
            .raw(hyper::Method::GET, "/mock", None, std::time::Duration::from_secs(5))
            .await
            .err()
            .expect("limit");
        assert!(err.to_string().contains("length limit exceeded"), "{err}");
        peer.await.expect("peer");
    }
}
