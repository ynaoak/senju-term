//! A static file server with hot reload, for whichever directory you are
//! standing in.
//!
//! The gap this fills is small but constant: previewing a built site, a
//! coverage report or a bundle of docs needs a server (`file://` breaks module
//! imports, `fetch`, and absolute paths), so people keep a one-liner around —
//! `python3 -m http.server`, `npx serve` — that occupies a terminal, has no
//! reload, and has to be remembered per machine. This is that one-liner as a
//! managed thing: a port you chose, a directory you picked, and a browser that
//! refreshes itself when the files change.
//!
//! It is a *preview* server and is written as one. Three properties are not
//! negotiable, because a dev server is a program that hands the contents of a
//! directory to anything that asks:
//!
//! **It listens on 127.0.0.1 and nothing else.** Never `0.0.0.0`. Serving a
//! working directory to the local network — a café, an office VLAN — is not a
//! thing to do by accident, and no UI toggle offers it.
//!
//! **A request can only reach files under the served root.** Every path is
//! decoded, rejected if it contains a `..` or a Windows path fragment, then
//! resolved on disk and checked to still be inside the root — so a symlink
//! pointing out of the tree does not become an exfiltration route either.
//!
//! **Dotfiles are not served.** A project directory is exactly where `.env` and
//! `.git/config` live, and they are never part of the site being previewed.
//!
//! **There is no directory listing.** A directory is reachable only through its
//! own `index.html`; without one it answers 404, exactly as a path that does not
//! exist does. A listing enumerates a working directory to anything that can
//! reach the port, which is a different and much larger disclosure than serving
//! one file somebody already knew to ask for. The redirect that adds a trailing
//! slash is withheld for the same reason unless there is an index to serve —
//! otherwise it would confirm the directory by itself.
//!
//! What it deliberately is not: no HTTPS, no keep-alive (every response closes
//! its connection), no `Range` requests (so seeking in a `<video>` will not
//! work), no compression, no HTTP/2. Those matter for a production server, and
//! this must never be mistaken for one.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, watch};

/// Longest request head accepted. A browser sends well under 2 KB; anything
/// past this is a client that will not be served correctly anyway, and reading
/// it unbounded is how a socket turns into unbounded memory.
const MAX_HEAD: usize = 8 * 1024;
/// A connection that opens and then says nothing holds a task forever without
/// this.
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);
/// One reload per quiet period. Editors write, rename and chmod a single save,
/// and a build touches hundreds of files — reloading per event would reload a
/// page mid-build, repeatedly.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(120);
/// Keeps the reload stream's socket honest: a browser tab that was closed is
/// only discovered by writing to it.
const SSE_HEARTBEAT: Duration = Duration::from_secs(30);
/// Namespaced so it cannot collide with a real path in the served directory.
const RELOAD_PATH: &str = "/__senju__/reload";
/// Largest file read into memory to serve. Responses are built in one buffer
/// rather than streamed; this bounds that.
const MAX_SERVE: u64 = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum DevServerError {
    #[error("not a directory: {0}")]
    NotADirectory(String),
    #[error("port {0} is already in use")]
    PortInUse(u16),
    #[error("cannot listen on port {port}: {source}")]
    Listen {
        port: u16,
        #[source]
        source: std::io::Error,
    },
}

/// What to serve, and how.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DevServerSpec {
    /// Directory to serve. Must already exist.
    pub root: String,
    /// 0 asks the OS for a free port, which is then reported back in
    /// [`DevServerInfo::port`] — the only way to start a server without first
    /// guessing what is free.
    #[serde(default)]
    pub port: u16,
    /// Serve `index.html` for paths that match no file, so a client-side router
    /// owns the URL space. Off by default: with it on, every typo is a 200 and
    /// a genuinely missing asset is invisible.
    #[serde(default)]
    pub spa: bool,
    #[serde(default)]
    pub hot_reload: bool,
}

/// A server that is currently listening.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevServerInfo {
    pub id: String,
    /// Canonicalized, so what is shown is what is actually being served.
    pub root: String,
    pub port: u16,
    pub url: String,
    pub spa: bool,
    pub hot_reload: bool,
    /// Unix seconds, for an uptime readout.
    pub started_at: u64,
}

/// One served request, for the panel's access log.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessLog {
    pub server_id: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub at: u64,
}

/// How the server reports what it is doing. Implemented by the Tauri layer,
/// which forwards to the webview; the core does not know about events.
pub trait DevServerSink: Send + Sync + 'static {
    fn access(&self, log: AccessLog);
    /// A file change triggered a browser reload. `path` is relative to the root.
    fn reloaded(&self, server_id: &str, path: String);
    /// Hot reload could not be set up, or stopped working. The server keeps
    /// serving: losing reload is a degraded feature, not a reason to take the
    /// preview away.
    fn watch_failed(&self, server_id: &str, message: String);
    /// The tree was larger than [`MAX_WATCHED_DIRS`], so reload covers only the
    /// `watched` directories that were registered. Distinct from
    /// [`Self::watch_failed`] because the outcomes differ for the person
    /// looking at the page: there, nothing reloads; here, most things do and a
    /// few quietly will not.
    fn watch_limited(&self, server_id: &str, watched: usize);
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/* ---------------- manager ---------------- */

struct Running {
    info: DevServerInfo,
    /// Sending `true` unblocks the accept loop, the watcher and every open
    /// reload stream at once.
    shutdown: watch::Sender<bool>,
}

pub struct DevServerManager {
    sink: Arc<dyn DevServerSink>,
    running: Mutex<HashMap<String, Running>>,
}

impl DevServerManager {
    pub fn new(sink: Arc<dyn DevServerSink>) -> Self {
        Self { sink, running: Mutex::new(HashMap::new()) }
    }

    /// Binds and starts serving. Returns once the socket is listening, so a
    /// port clash is an error at the button press rather than a failure that
    /// happens invisibly a moment later.
    pub async fn start(&self, spec: DevServerSpec) -> Result<DevServerInfo, DevServerError> {
        let root = std::fs::canonicalize(&spec.root)
            .map_err(|_| DevServerError::NotADirectory(spec.root.clone()))?;
        if !root.is_dir() {
            return Err(DevServerError::NotADirectory(spec.root.clone()));
        }

        // 127.0.0.1, never 0.0.0.0 — see the module docs. `port` may be 0, in
        // which case the OS picks and `local_addr` reports what it picked.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, spec.port)).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                DevServerError::PortInUse(spec.port)
            } else {
                DevServerError::Listen { port: spec.port, source: e }
            }
        })?;
        let port = listener
            .local_addr()
            .map_err(|e| DevServerError::Listen { port: spec.port, source: e })?
            .port();

        let id = uuid::Uuid::new_v4().to_string();
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (reload, _) = broadcast::channel::<()>(8);

        let ctx = Arc::new(ServerCtx {
            id: id.clone(),
            root: root.clone(),
            spa: spec.spa,
            hot_reload: spec.hot_reload,
            reload: reload.clone(),
            sink: self.sink.clone(),
        });

        if spec.hot_reload {
            spawn_watcher(ctx.clone(), reload, shutdown_rx.clone());
        }

        let info = DevServerInfo {
            id: id.clone(),
            root: root.to_string_lossy().into_owned(),
            port,
            url: format!("http://127.0.0.1:{port}/"),
            spa: spec.spa,
            hot_reload: spec.hot_reload,
            started_at: now(),
        };

        tokio::spawn(accept_loop(listener, ctx, shutdown_rx));
        self.lock().insert(id, Running { info: info.clone(), shutdown });
        Ok(info)
    }

    /// Stops one server. Returns false if that id was not running, which the
    /// caller can treat as already-stopped rather than an error.
    pub fn stop(&self, id: &str) -> bool {
        let Some(running) = self.lock().remove(id) else { return false };
        let _ = running.shutdown.send(true);
        true
    }

    /// Stops everything. Called on app exit: these are child listeners of a
    /// desktop app, and leaving one bound after the window closes would keep
    /// the port taken with nothing to stop it.
    pub fn stop_all(&self) {
        for (_, running) in self.lock().drain() {
            let _ = running.shutdown.send(true);
        }
    }

    pub fn list(&self) -> Vec<DevServerInfo> {
        let mut out: Vec<DevServerInfo> =
            self.lock().values().map(|r| r.info.clone()).collect();
        out.sort_by_key(|i| i.started_at);
        out
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Running>> {
        // Poison-safe like the rest of the crate: a panicked task must not
        // make every later start/stop fail.
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }
}

struct ServerCtx {
    id: String,
    /// Canonical, and the prefix every resolved path is checked against.
    root: PathBuf,
    spa: bool,
    hot_reload: bool,
    reload: broadcast::Sender<()>,
    sink: Arc<dyn DevServerSink>,
}

/* ---------------- accept loop ---------------- */

async fn accept_loop(listener: TcpListener, ctx: Arc<ServerCtx>, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            // Also fires if the sender is dropped, which is the manager going
            // away — either way there is nothing left to serve for.
            _ = shutdown.changed() => return,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let ctx = ctx.clone();
                    let shutdown = shutdown.clone();
                    tokio::spawn(async move {
                        let _ = serve(stream, ctx, shutdown).await;
                    });
                }
                // Out of file descriptors, mostly. Retrying immediately would
                // spin a core; the pause gives whatever is holding them a
                // chance to release.
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            },
        }
    }
}

/* ---------------- request handling ---------------- */

struct Request {
    method: String,
    target: String,
}

/// Reads the request head. `None` means the client hung up, sent nothing, or
/// sent something that is not a request line — all of which are handled by
/// closing the connection.
async fn read_head(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let read = tokio::time::timeout(HEAD_TIMEOUT, stream.read(&mut chunk)).await.ok()?.ok()?;
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let line = head.lines().next()?;
    let mut parts = line.split(' ');
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    (!method.is_empty() && target.starts_with('/')).then_some(Request { method, target })
}

async fn serve(mut stream: TcpStream, ctx: Arc<ServerCtx>, shutdown: watch::Receiver<bool>) -> std::io::Result<()> {
    let Some(req) = read_head(&mut stream).await else { return Ok(()) };
    let path = req.target.split(['?', '#']).next().unwrap_or("/").to_string();

    if ctx.hot_reload && path == RELOAD_PATH {
        return reload_stream(stream, &ctx, shutdown).await;
    }

    let head_only = req.method == "HEAD";
    if req.method != "GET" && !head_only {
        // A preview server has nothing to accept; saying so plainly beats
        // letting a stray POST look like a routing problem.
        ctx.log(&req.method, &path, 405);
        return send(&mut stream, 405, "text/plain; charset=utf-8", b"405 method not allowed", false, None).await;
    }

    let status = respond(&mut stream, &ctx, &path, head_only).await?;
    ctx.log(&req.method, &path, status);
    Ok(())
}

impl ServerCtx {
    fn log(&self, method: &str, path: &str, status: u16) {
        self.sink.access(AccessLog {
            server_id: self.id.clone(),
            method: method.to_string(),
            path: path.to_string(),
            status,
            at: now(),
        });
    }
}

/// Writes the response for `url_path` and reports the status it sent, so the
/// caller can log it without deciding it twice.
async fn respond(stream: &mut TcpStream, ctx: &ServerCtx, url_path: &str, head_only: bool) -> std::io::Result<u16> {
    let Some(joined) = safe_join(&ctx.root, url_path) else {
        send(stream, 403, "text/plain; charset=utf-8", b"403 forbidden", head_only, None).await?;
        return Ok(403);
    };

    // A directory is only ever reachable through its index file — there is no
    // directory listing (see the module docs).
    let target = if joined.is_dir() {
        match dir_index(&ctx.root, &joined) {
            Some(index) => {
                // Redirect so relative links on the page resolve against the
                // directory instead of its parent. Deliberately only once there
                // *is* something to serve: redirecting for a directory with no
                // index would confirm the directory exists, which is the thing
                // dropping the listing is meant to stop.
                if !url_path.ends_with('/') {
                    let location = format!("{url_path}/");
                    send(stream, 301, "text/html; charset=utf-8", b"", head_only, Some(&location))
                        .await?;
                    return Ok(301);
                }
                Some(index)
            }
            // Falls through to the SPA fallback and then to 404, so an indexless
            // directory answers exactly like a path that does not exist.
            None => None,
        }
    } else {
        within(&ctx.root, &joined)
    };

    if let Some(real) = target {
        return send_file(stream, ctx, &real, head_only).await;
    }

    // Client-side routing: an unknown path is a route, not a missing file.
    if ctx.spa {
        let index = ctx.root.join("index.html");
        if let Some(real) = within(&ctx.root, &index) {
            return send_file(stream, ctx, &real, head_only).await;
        }
    }

    send(stream, 404, "text/html; charset=utf-8", NOT_FOUND.as_bytes(), head_only, None).await?;
    Ok(404)
}

/// The index file a directory is served through, if it has one.
fn dir_index(root: &Path, dir: &Path) -> Option<PathBuf> {
    ["index.html", "index.htm"].iter().find_map(|name| within(root, &dir.join(name)))
}

async fn send_file(stream: &mut TcpStream, ctx: &ServerCtx, path: &Path, head_only: bool) -> std::io::Result<u16> {
    let meta = match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() && m.len() <= MAX_SERVE => m,
        _ => {
            send(stream, 404, "text/html; charset=utf-8", NOT_FOUND.as_bytes(), head_only, None).await?;
            return Ok(404);
        }
    };
    let mime = content_type(path);
    let html = mime.starts_with("text/html");

    // HEAD on a large file still needs the right Content-Length, but not the
    // read. The injected snippet changes the length, so an HTML HEAD is sized
    // from the snippet too rather than from the file on disk.
    if head_only && !html {
        return send_headers(stream, 200, mime, meta.len() as usize, None).await.map(|_| 200);
    }

    let Ok(body) = tokio::fs::read(path).await else {
        send(stream, 404, "text/html; charset=utf-8", NOT_FOUND.as_bytes(), head_only, None).await?;
        return Ok(404);
    };
    let body = if html && ctx.hot_reload { inject_reload(body) } else { body };
    send(stream, 200, mime, &body, head_only, None).await?;
    Ok(200)
}

/* ---------------- path safety ---------------- */

/// Maps a URL path onto a path under `root`, or `None` if it must not be
/// served. This is the first of the two checks; [`within`] is the second, and
/// both have to pass.
fn safe_join(root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path)?;
    if !decoded.starts_with('/') {
        return None;
    }
    let mut out = root.to_path_buf();
    for seg in decoded.split('/') {
        if seg.is_empty() || seg == "." {
            continue;
        }
        // `..` is the traversal itself. A backslash or a colon is a Windows
        // path fragment (`..\`, `C:`) that `Path::push` would honour — on
        // Windows, pushing `C:\x` *replaces* the root entirely.
        if seg == ".." || seg.contains('\\') || seg.contains(':') || seg.contains('\0') {
            return None;
        }
        // Dotfiles are never part of a previewed site, and are exactly where a
        // project keeps its secrets.
        if seg.starts_with('.') {
            return None;
        }
        out.push(seg);
    }
    Some(out)
}

/// Resolves symlinks and confirms the result is still inside `root`. Without
/// this, a symlink in the served directory pointing at `/etc` or at the user's
/// home would be followed and served.
fn within(root: &Path, path: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(path).ok()?;
    real.starts_with(root).then_some(real)
}

/// Decodes `%XX`. Returns `None` for a malformed escape or for bytes that are
/// not UTF-8 — a path that cannot be read exactly is not one to guess at.
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let hex = std::str::from_utf8(hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/* ---------------- responses ---------------- */

const NOT_FOUND: &str =
    "<!doctype html><meta charset=\"utf-8\"><title>404</title><h1>404</h1><p>Not found.</p>";

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        301 => "Moved Permanently",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Error",
    }
}

async fn send_headers(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    len: usize,
    location: Option<&str>,
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {len}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n",
        reason(status)
    );
    if let Some(location) = location {
        head.push_str("Location: ");
        head.push_str(location);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.flush().await
}

async fn send(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head_only: bool,
    location: Option<&str>,
) -> std::io::Result<()> {
    send_headers(stream, status, content_type, body.len(), location).await?;
    if !head_only {
        stream.write_all(body).await?;
        stream.flush().await?;
    }
    Ok(())
}

fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "xml" => "application/xml; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        // Anything unrecognized downloads rather than renders, which is the
        // safe direction: a guessed `text/html` would execute.
        _ => "application/octet-stream",
    }
}

/* ---------------- hot reload ---------------- */

const RELOAD_SNIPPET: &str = r#"<script>
/* Injected by Senju Term's built-in dev server. Not part of your files. */
(function () {
  try {
    var es = new EventSource("/__senju__/reload");
    es.onmessage = function () { location.reload(); };
  } catch (e) { /* reload is a convenience; never break the page over it */ }
})();
</script>"#;

/// Puts the reload client just before `</body>`, or at the end when there is
/// none. The position barely matters to a browser — the point is to not land
/// inside a `<pre>` or between `<head>` elements a page is picky about.
fn inject_reload(body: Vec<u8>) -> Vec<u8> {
    let at = rfind_ascii_ci(&body, b"</body>").unwrap_or(body.len());
    let mut out = Vec::with_capacity(body.len() + RELOAD_SNIPPET.len());
    out.extend_from_slice(&body[..at]);
    out.extend_from_slice(RELOAD_SNIPPET.as_bytes());
    out.extend_from_slice(&body[at..]);
    out
}

fn rfind_ascii_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .rev()
        .find(|&i| haystack[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

async fn reload_stream(
    mut stream: TcpStream,
    ctx: &ServerCtx,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let mut rx = ctx.reload.subscribe();
    stream
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
              Cache-Control: no-store\r\nConnection: close\r\n\r\nretry: 1000\n\n",
        )
        .await?;
    stream.flush().await?;
    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            received = rx.recv() => match received {
                // Lagged means changes outran this client; it still needs to
                // reload, and one reload covers every change it missed.
                Ok(()) | Err(broadcast::error::RecvError::Lagged(_)) => {
                    stream.write_all(b"data: reload\n\n").await?;
                    stream.flush().await?;
                }
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
            // A closed tab is only noticed by writing to its socket. Without
            // this the task would live as long as the server.
            _ = tokio::time::sleep(SSE_HEARTBEAT) => {
                stream.write_all(b": ping\n\n").await?;
                stream.flush().await?;
            }
        }
    }
}

/// Churn that has nothing to do with the page being previewed. Checked against
/// the path *relative to the root*, so serving a directory that happens to live
/// under `~/.local` does not filter out every one of its own files.
/// Directory names the watcher never descends into.
///
/// These are package-manager and build trees. They hold tens of thousands of
/// files that churn on every install and every build, none of which is the page
/// being previewed — and watching them is how a tree exhausts the OS limit on
/// watches (`fs.inotify.max_user_watches` on Linux), at which point hot reload
/// stops working for *everything*, not just for them. So this is a guard
/// against losing reload, not only noise suppression.
///
/// Only names that do not already start with a dot are listed: a leading dot is
/// excluded separately, which covers `.git`, `.venv`, `.yarn`, `.pnpm-store`,
/// `.next`, `.gradle`, `.m2`, `.tox` and the rest of that family at once.
///
/// **Serving is not affected.** A page asking for `/node_modules/x/x.js` still
/// gets it; this list decides only what is watched.
const UNWATCHED_DIRS: &[&str] = &[
    "node_modules",     // npm / pnpm / yarn / bun
    "bower_components", // bower
    "vendor",           // composer, `go mod vendor`, bundler
    "Pods",             // CocoaPods
    "target",           // cargo, maven, sbt
    "venv",             // python virtualenv kept undotted
    "__pycache__",      // python bytecode
];

/// Ceiling on how many directories one server watches.
///
/// The exclusions above remove the usual offenders, but a large enough source
/// tree can still reach the OS limit on its own. Stopping here leaves reload
/// working for the part that was registered — and, crucially, leaves watches for
/// the rest of the machine — instead of failing the registration outright.
const MAX_WATCHED_DIRS: usize = 4096;

fn is_unwatched_dir(name: &str) -> bool {
    name.starts_with('.') || UNWATCHED_DIRS.contains(&name)
}

fn is_noise(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return true; // outside the tree we were asked to watch
    };
    // Events can still arrive for an excluded tree — the walk skips it, but a
    // watch on its parent reports the directory itself changing — so the same
    // list filters here too.
    let noisy_dir = rel.components().any(|c| {
        let Component::Normal(name) = c else { return false };
        let Some(name) = name.to_str() else { return false };
        is_unwatched_dir(name)
    });
    let scratch = rel
        .file_name()
        .and_then(|n| n.to_str())
        // Editor scratch files: `main.css~`, `#main.css#`, vim's `.swp`.
        .is_some_and(|n| n.ends_with('~') || n.ends_with(".swp") || n.ends_with(".tmp"));
    noisy_dir || scratch
}

/// Registers a watch on `root` and on every directory below it, skipping the
/// trees named in [`UNWATCHED_DIRS`]. Returns how many directories were
/// registered, or the error from the root — the one that cannot be recovered
/// from, since without it there is no watch at all.
///
/// The walk exists because `RecursiveMode::Recursive` cannot express an
/// exclusion: it registers a watch for every directory under the root,
/// `node_modules` included, *before* any event reaches the filter. Filtering
/// events makes the reloads quiet; only walking the tree makes the guard real.
///
/// Symlinked directories are not followed. `read_dir` reports the link itself
/// rather than its target, so they never look like directories here — which
/// also means a symlink cycle cannot spin this loop forever.
fn watch_tree(watcher: &mut notify::RecommendedWatcher, root: &Path) -> notify::Result<usize> {
    use notify::{RecursiveMode, Watcher};

    watcher.watch(root, RecursiveMode::NonRecursive)?;
    let mut watched = 1usize;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if watched >= MAX_WATCHED_DIRS {
                return Ok(watched);
            }
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            if name.to_str().is_none_or(is_unwatched_dir) {
                continue;
            }
            let path = entry.path();
            // A directory that vanished between the listing and the watch is
            // ordinary churn, not a reason to abandon the rest of the tree.
            if watcher.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                watched += 1;
                stack.push(path);
            }
        }
    }
    Ok(watched)
}

fn spawn_watcher(ctx: Arc<ServerCtx>, reload: broadcast::Sender<()>, mut shutdown: watch::Receiver<bool>) {
    use notify::EventKind;

    // `created` travels with the path because the tree is watched directory by
    // directory: a directory made after the walk inherits nothing and needs its
    // own watch, and only the event kind can tell it apart from an existing
    // directory merely being touched.
    let (tx, mut rx) = mpsc::unbounded_channel::<(PathBuf, bool)>();
    let root = ctx.root.clone();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else { return };
        // Reads are not changes; on some backends they arrive constantly.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        let created = matches!(event.kind, EventKind::Create(_));
        for path in event.paths {
            if !is_noise(&root, &path) {
                let _ = tx.send((path, created));
            }
        }
    });
    let mut watcher = match watcher {
        Ok(w) => w,
        Err(e) => {
            ctx.sink.watch_failed(&ctx.id, e.to_string());
            return;
        }
    };
    match watch_tree(&mut watcher, &ctx.root) {
        // The tree is larger than the cap, so changes below the part that was
        // registered will not reload. Saying so beats a preview that silently
        // stops refreshing for some files.
        Ok(n) if n >= MAX_WATCHED_DIRS => ctx.sink.watch_limited(&ctx.id, n),
        Ok(_) => {}
        Err(e) => {
            // The server stays up without reload rather than refusing to start:
            // losing reload is a degraded feature, not a dead preview.
            ctx.sink.watch_failed(&ctx.id, e.to_string());
            return;
        }
    }

    tokio::spawn(async move {
        // Dropping the watcher stops the notifications, so it lives here — and
        // being here is also what lets the burst below extend it.
        let mut watcher = watcher;
        loop {
            let first = tokio::select! {
                _ = shutdown.changed() => return,
                path = rx.recv() => match path { Some(p) => p, None => return },
            };
            // Collapse the burst a single save or a build produces into one
            // reload, and report the last path — the one that finished.
            let mut fresh: Vec<PathBuf> = Vec::new();
            let mut note = |(path, created): &(PathBuf, bool)| {
                if *created && path.is_dir() {
                    fresh.push(path.clone());
                }
            };
            note(&first);
            let mut last = first.0;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(RELOAD_DEBOUNCE) => break,
                    path = rx.recv() => match path {
                        Some(p) => { note(&p); last = p.0; }
                        None => break,
                    },
                }
            }
            // Only directories that were *created* are walked, so this is not a
            // re-walk of the root every time a file inside it changes.
            for dir in fresh {
                let _ = watch_tree(&mut watcher, &dir);
            }
            let shown = last
                .strip_prefix(&ctx.root)
                .unwrap_or(&last)
                .to_string_lossy()
                .into_owned();
            // `send` fails only when nothing is subscribed — no browser has the
            // page open, which is not a problem worth reporting.
            let _ = reload.send(());
            ctx.sink.reloaded(&ctx.id, shown);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collects what the server reports so tests can assert on behaviour the
    /// HTTP responses do not show (a reload actually firing).
    #[derive(Default)]
    struct Recorder {
        access: Mutex<Vec<AccessLog>>,
        reloads: Mutex<Vec<String>>,
        failures: Mutex<Vec<String>>,
    }

    impl DevServerSink for Recorder {
        fn access(&self, log: AccessLog) {
            self.access.lock().unwrap_or_else(|e| e.into_inner()).push(log);
        }
        fn reloaded(&self, _id: &str, path: String) {
            self.reloads.lock().unwrap_or_else(|e| e.into_inner()).push(path);
        }
        fn watch_failed(&self, _id: &str, message: String) {
            self.failures.lock().unwrap_or_else(|e| e.into_inner()).push(message);
        }
        fn watch_limited(&self, _id: &str, watched: usize) {
            self.failures
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("limited to {watched}"));
        }
    }

    fn write(dir: &Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// One request/response over a real socket. The point of these tests is
    /// that the server is a server; a unit test of `respond` would not catch a
    /// malformed status line or a missing Content-Length.
    async fn get(port: u16, target: &str) -> (String, Vec<u8>) {
        request(port, &format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")).await
    }

    async fn request(port: u16, raw: &str) -> (String, Vec<u8>) {
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await.unwrap();
        stream.write_all(raw.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut buf = Vec::new();
        // The server closes after every response, so read-to-end terminates.
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
            .await
            .expect("server did not close the connection")
            .unwrap();
        let split = buf.windows(4).position(|w| w == b"\r\n\r\n").expect("no header terminator");
        (String::from_utf8_lossy(&buf[..split]).into_owned(), buf[split + 4..].to_vec())
    }

    fn status(head: &str) -> u16 {
        head.lines().next().unwrap().split(' ').nth(1).unwrap().parse().unwrap()
    }

    async fn start(dir: &Path, spa: bool, hot_reload: bool) -> (Arc<Recorder>, DevServerManager, DevServerInfo) {
        let sink = Arc::new(Recorder::default());
        let mgr = DevServerManager::new(sink.clone());
        let info = mgr
            .start(DevServerSpec {
                root: dir.to_string_lossy().into_owned(),
                // Port 0: tests must never fight each other (or the developer's
                // own servers) over a hardcoded number.
                port: 0,
                spa,
                hot_reload,
            })
            .await
            .unwrap();
        (sink, mgr, info)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn serves_a_file_and_reports_it() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "<h1>hi</h1>");
        write(dir.path(), "app.css", "body{}");
        let (sink, mgr, info) = start(dir.path(), false, false).await;

        let (head, body) = get(info.port, "/").await;
        assert_eq!(status(&head), 200);
        assert!(head.contains("Content-Type: text/html; charset=utf-8"), "{head}");
        // Without this a browser caches the file and hot reload shows stale
        // content, which looks exactly like the server being broken.
        assert!(head.contains("Cache-Control: no-store"), "{head}");
        assert_eq!(String::from_utf8_lossy(&body), "<h1>hi</h1>");

        let (head, _) = get(info.port, "/app.css").await;
        assert!(head.contains("Content-Type: text/css"), "{head}");

        let logged = sink.access.lock().unwrap();
        assert_eq!(logged.len(), 2);
        assert_eq!((logged[0].status, logged[0].path.as_str()), (200, "/"));
        mgr.stop(&info.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_request_cannot_escape_the_served_directory() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "password").unwrap();
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "ok");
        write(dir.path(), ".env", "TOKEN=1");
        // A symlink is the case a purely textual path check misses: nothing in
        // the URL looks like traversal.
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();

        let (_, mgr, info) = start(dir.path(), false, false).await;
        for target in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/a/../../secret.txt",
            "/.env",
            "/.git/config",
        ] {
            let (head, body) = get(info.port, target).await;
            assert!(
                status(&head) >= 400,
                "{target} was served: {head} {}",
                String::from_utf8_lossy(&body)
            );
            assert!(!String::from_utf8_lossy(&body).contains("password"));
            assert!(!String::from_utf8_lossy(&body).contains("TOKEN"));
        }
        #[cfg(unix)]
        {
            let (head, body) = get(info.port, "/escape/secret.txt").await;
            assert_eq!(status(&head), 404, "a symlink out of the tree was followed");
            assert!(!String::from_utf8_lossy(&body).contains("password"));
        }
        mgr.stop(&info.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hot_reload_injects_a_client_and_pushes_on_change() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "<html><body><p>v1</p></body></html>");
        let (sink, mgr, info) = start(dir.path(), false, true).await;

        let (_, body) = get(info.port, "/").await;
        let body = String::from_utf8_lossy(&body);
        assert!(body.contains(RELOAD_PATH), "reload client not injected: {body}");
        // Injected before </body>, and the page's own markup is untouched.
        assert!(body.contains("<p>v1</p>"));
        assert!(body.find(RELOAD_PATH).unwrap() < body.find("</body>").unwrap());

        // Hold the event stream open, then change a file and expect a push.
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, info.port)).await.unwrap();
        stream
            .write_all(format!("GET {RELOAD_PATH} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        stream.flush().await.unwrap();
        let mut head = [0u8; 256];
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut head))
            .await
            .unwrap()
            .unwrap();
        assert!(String::from_utf8_lossy(&head[..n]).contains("text/event-stream"));

        // The watcher is registered before start() returns, but give the OS a
        // moment before the write it must notice.
        tokio::time::sleep(Duration::from_millis(200)).await;
        write(dir.path(), "index.html", "<html><body><p>v2</p></body></html>");

        let mut got = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !String::from_utf8_lossy(&got).contains("data: reload") {
            let mut chunk = [0u8; 256];
            let n = tokio::time::timeout_at(deadline, stream.read(&mut chunk))
                .await
                .expect("no reload was pushed within 10s")
                .unwrap();
            assert_ne!(n, 0, "reload stream closed before pushing");
            got.extend_from_slice(&chunk[..n]);
        }
        assert_eq!(sink.reloads.lock().unwrap().as_slice(), ["index.html"]);
        mgr.stop(&info.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_directory_created_after_startup_is_watched_too() {
        // The tree is walked once at startup and watched directory by
        // directory, so a directory made afterwards inherits nothing. Before
        // the guard this came free from RecursiveMode::Recursive; it is the
        // thing most likely to have silently regressed.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "<body>x</body>");
        let (sink, mgr, info) = start(dir.path(), false, true).await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        std::fs::create_dir(dir.path().join("added")).unwrap();
        // Let the create event land and the new directory get its own watch.
        tokio::time::sleep(Duration::from_millis(600)).await;
        write(dir.path(), "added/page.html", "<body>new</body>");

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while sink.reloads.lock().unwrap().iter().all(|p| !p.contains("page.html")) {
            assert!(std::time::Instant::now() < deadline, "no reload for a file in a new directory");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        mgr.stop(&info.id);
    }

    #[test]
    fn the_walk_does_not_descend_into_package_directories() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // A realistic mix: real source, every guarded name, and a dot-directory
        // (covered by the leading-dot rule rather than the list).
        for sub in [
            "src/components",
            "assets",
            "node_modules/react/cjs",
            "node_modules/.bin",
            "vendor/composer",
            "target/debug/build",
            "__pycache__",
            "venv/lib",
            "Pods/Firebase",
            "bower_components/jquery",
            ".git/objects",
        ] {
            std::fs::create_dir_all(root.join(sub)).unwrap();
        }

        let (tx, _rx) = mpsc::unbounded_channel::<(PathBuf, bool)>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                for path in event.paths {
                    let _ = tx.send((path, false));
                }
            }
        })
        .unwrap();

        let watched = watch_tree(&mut watcher, root).unwrap();
        // root + src + src/components + assets. Everything else is guarded, so
        // a count is enough to say the walk never entered those trees — no
        // watch was spent on them at all, which is the point.
        assert_eq!(watched, 4, "walked into a directory it should have skipped");
    }

    #[test]
    fn the_guard_covers_the_package_managers_it_claims_to() {
        for name in [
            "node_modules",
            "bower_components",
            "vendor",
            "Pods",
            "target",
            "venv",
            "__pycache__",
            // Dotted names are excluded by the leading-dot rule, which is why
            // they are deliberately absent from UNWATCHED_DIRS.
            ".git",
            ".venv",
            ".yarn",
            ".pnpm-store",
            ".next",
            ".gradle",
        ] {
            assert!(is_unwatched_dir(name), "{name} would be watched");
        }
        for name in ["src", "assets", "dist", "public", "docs", "build"] {
            assert!(!is_unwatched_dir(name), "{name} would be skipped");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn noise_does_not_trigger_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "<body>x</body>");
        let (sink, mgr, info) = start(dir.path(), false, true).await;
        tokio::time::sleep(Duration::from_millis(200)).await;

        // A git operation, every guarded package directory, and an editor swap
        // file — none of which is the page.
        write(dir.path(), ".git/index", "junk");
        write(dir.path(), "node_modules/pkg/index.js", "junk");
        write(dir.path(), "vendor/composer/autoload.php", "junk");
        write(dir.path(), "target/debug/app", "junk");
        write(dir.path(), "__pycache__/mod.pyc", "junk");
        write(dir.path(), "venv/lib/site.py", "junk");
        write(dir.path(), "Pods/Firebase/Core.m", "junk");
        write(dir.path(), "bower_components/jquery/jquery.js", "junk");
        write(dir.path(), "index.html~", "junk");
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(
            sink.reloads.lock().unwrap().is_empty(),
            "reloaded for: {:?}",
            sink.reloads.lock().unwrap()
        );
        mgr.stop(&info.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn spa_mode_serves_the_index_for_unknown_routes_only_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "<body>app</body>");

        let (_, mgr, info) = start(dir.path(), false, false).await;
        assert_eq!(status(&get(info.port, "/users/42").await.0), 404);
        mgr.stop(&info.id);

        let (_, mgr, info) = start(dir.path(), true, false).await;
        let (head, body) = get(info.port, "/users/42").await;
        assert_eq!(status(&head), 200);
        assert!(String::from_utf8_lossy(&body).contains("app"));
        // Traversal is still refused in SPA mode: the fallback must not become
        // a way to turn any rejected path into a 200.
        assert_eq!(status(&get(info.port, "/../secret").await.0), 403);
        mgr.stop(&info.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_directory_is_reachable_only_through_its_index() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "docs/index.html", "<p>docs</p>");
        write(dir.path(), "files/report.pdf", "%PDF-");
        write(dir.path(), "files/secret-plan.txt", "x");
        let (_, mgr, info) = start(dir.path(), false, false).await;

        // With an index: redirect so relative links resolve against the
        // directory, then serve the index.
        let (head, _) = get(info.port, "/docs").await;
        assert_eq!(status(&head), 301);
        assert!(head.contains("Location: /docs/"), "{head}");
        let (head, body) = get(info.port, "/docs/").await;
        assert_eq!(status(&head), 200);
        assert!(String::from_utf8_lossy(&body).contains("docs"));

        // Without one: no listing, and no hint that the directory is there.
        for target in ["/files/", "/files"] {
            let (head, body) = get(info.port, target).await;
            let body = String::from_utf8_lossy(&body);
            assert_eq!(status(&head), 404, "{target} was not a 404: {head}");
            assert!(!body.contains("report.pdf"), "{target} listed its contents");
            assert!(!body.contains("secret-plan"), "{target} listed its contents");
        }
        // A directory with no index has to be indistinguishable from a path
        // that was never there — otherwise the response itself is the listing,
        // one probe at a time.
        let (absent_head, absent_body) = get(info.port, "/nope/").await;
        let (dir_head, dir_body) = get(info.port, "/files/").await;
        assert_eq!(status(&absent_head), status(&dir_head));
        assert_eq!(absent_body, dir_body);

        // Files inside it are still served: dropping the listing hides the
        // index of the directory, not the content of the site.
        let (head, _) = get(info.port, "/files/report.pdf").await;
        assert_eq!(status(&head), 200);
        mgr.stop(&info.id);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stopped_server_releases_its_port() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "x");
        let (_, mgr, info) = start(dir.path(), false, true).await;
        assert_eq!(mgr.list().len(), 1);
        assert!(mgr.stop(&info.id));
        assert!(mgr.list().is_empty());
        assert!(!mgr.stop(&info.id), "stopping twice should report not-running");

        // The port must actually be free again, not just absent from the list.
        let mut rebound = None;
        for _ in 0..50 {
            match TcpListener::bind((Ipv4Addr::LOCALHOST, info.port)).await {
                Ok(l) => {
                    rebound = Some(l);
                    break;
                }
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert!(rebound.is_some(), "port {} still bound after stop", info.port);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_taken_port_is_reported_at_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();
        let mgr = DevServerManager::new(Arc::new(Recorder::default()));
        let err = mgr
            .start(DevServerSpec {
                root: dir.path().to_string_lossy().into_owned(),
                port,
                spa: false,
                hot_reload: false,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, DevServerError::PortInUse(p) if p == port), "{err:?}");
        assert!(mgr.list().is_empty(), "a failed start must leave nothing running");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_get_and_head_are_served() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "index.html", "<body>hello</body>");
        let (_, mgr, info) = start(dir.path(), false, false).await;

        let (head, body) = request(info.port, "HEAD /index.html HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert_eq!(status(&head), 200);
        assert!(body.is_empty(), "HEAD returned a body");
        // The length has to describe the body a GET would return, including the
        // injected snippet when there is one — here there is not.
        assert!(head.contains("Content-Length: 18"), "{head}");

        let (head, _) = request(info.port, "POST /x HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert_eq!(status(&head), 405);
        mgr.stop(&info.id);
    }

    #[test]
    fn paths_that_must_never_resolve() {
        let root = Path::new("/srv/site");
        assert_eq!(safe_join(root, "/a/b.txt"), Some(PathBuf::from("/srv/site/a/b.txt")));
        assert_eq!(safe_join(root, "/"), Some(root.to_path_buf()));
        assert_eq!(safe_join(root, "/a/./b"), Some(PathBuf::from("/srv/site/a/b")));
        for bad in ["/..", "/a/../..", "/.env", "/a/.git/config", "/C:/x", "/a\\b", "/%zz"] {
            assert_eq!(safe_join(root, bad), None, "{bad} resolved");
        }
        // Percent-encoded traversal has to be rejected *after* decoding, which
        // is the bug this check exists for.
        assert_eq!(safe_join(root, "/%2e%2e/etc"), None);
        // A space is a legal file name, so decoding must still happen.
        assert_eq!(safe_join(root, "/my%20file.txt"), Some(PathBuf::from("/srv/site/my file.txt")));
    }

    #[test]
    fn the_reload_client_goes_before_the_closing_body_tag() {
        let out = inject_reload(b"<html><BODY>x</BODY></html>".to_vec());
        let out = String::from_utf8(out).unwrap();
        // Case-insensitive: HTML tags are not case-sensitive and real pages
        // are written both ways.
        assert!(out.find("EventSource").unwrap() < out.find("</BODY>").unwrap());
        // No </body> at all is normal in fragments; the script still lands.
        let out = String::from_utf8(inject_reload(b"<p>x</p>".to_vec())).unwrap();
        assert!(out.starts_with("<p>x</p>"));
        assert!(out.contains("EventSource"));
    }

}
