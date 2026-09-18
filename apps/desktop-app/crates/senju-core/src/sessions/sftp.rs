//! SFTP file transfer over an established SSH session (russh-sftp, pure
//! Rust). The SFTP subsystem runs on its own channel of the *same* SSH
//! connection the shell uses, so it needs no second authentication and works
//! unchanged through a multi-hop (ProxyJump) chain: the target handle already
//! sits on top of the jump tunnels.
//!
//! One `SftpSession` is opened lazily per SSH session and cached; a failed
//! call drops the cache so the next request reopens the subsystem (e.g. after
//! the server closed the channel on idle).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::OpenFlags;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;

use super::ssh::{Client, SshWriter};
use super::SessionError;
use russh::client::Handle;

/// Chunk size for streaming reads/writes. Large enough to keep the SFTP
/// pipeline busy, small enough for smooth progress updates.
const CHUNK: usize = 256 * 1024;

/// Cached SFTP subsystem for one SSH session (see module docs).
pub(crate) type SftpCache = Arc<AsyncMutex<Option<Arc<SftpSession>>>>;

/// One entry of a remote directory listing, shaped for the UI's browser.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteEntry {
    pub name: String,
    /// Full remote path (`dir/name`), ready to pass back to a download call.
    pub path: String,
    pub is_dir: bool,
    /// True when the entry is a symlink (to a dir or file — `is_dir` then
    /// reflects the link target when the server resolved it).
    pub is_symlink: bool,
    pub size: u64,
    /// Modification time as Unix seconds, 0 when unknown.
    pub modified: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct RemoteDirListing {
    /// Canonical absolute path of the listed directory.
    pub path: String,
    pub entries: Vec<RemoteEntry>,
}

/// Result of an upload: how many files and bytes landed.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TransferSummary {
    pub files: u64,
    pub bytes: u64,
}

/// Progress callback: `(bytes_done, bytes_total)`. `bytes_total` is 0 when
/// unknown (a remote file whose size the server did not report).
pub type ProgressFn = Arc<dyn Fn(u64, u64) + Send + Sync>;

fn sftp_err(e: impl std::fmt::Display) -> SessionError {
    SessionError::Ssh(format!("SFTP: {e}"))
}

/// Opens the SFTP subsystem on a fresh channel of `handle`.
async fn open_sftp(handle: &Handle<Client>) -> Result<SftpSession, SessionError> {
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| sftp_err(format!("channel open failed: {e}")))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| sftp_err(format!("subsystem request failed: {e}")))?;
    SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| sftp_err(format!("init failed: {e}")))
}

/// Returns the cached SFTP session for `writer`'s connection, opening it on
/// first use. The writer lock is held only while the subsystem channel is
/// being opened (once per session) — never during a transfer — so shell
/// input keeps flowing while files copy.
pub(crate) async fn get_sftp(
    writer: &AsyncMutex<SshWriter>,
    cache: &SftpCache,
) -> Result<Arc<SftpSession>, SessionError> {
    let mut slot = cache.lock().await;
    if let Some(s) = slot.as_ref() {
        return Ok(s.clone());
    }
    let session = {
        let guard = writer.lock().await;
        Arc::new(open_sftp(&guard.handle).await?)
    };
    *slot = Some(session.clone());
    Ok(session)
}

/// Forgets the cached session so the next call reopens the subsystem. Called
/// after any failure — the channel may be gone.
pub(crate) async fn reset_sftp(cache: &SftpCache) {
    cache.lock().await.take();
}

/// Joins a remote directory and a file name with a single `/`.
pub fn join_remote(dir: &str, name: &str) -> String {
    if dir.is_empty() || dir == "." {
        return name.to_string();
    }
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Lists `path` (empty or "." = the SFTP start directory, normally the login
/// home). Directories are sorted first, then case-insensitively by name.
pub(crate) async fn list_dir(sftp: &SftpSession, path: &str) -> Result<RemoteDirListing, SessionError> {
    let want = if path.trim().is_empty() { "." } else { path.trim() };
    let canonical = sftp.canonicalize(want).await.map_err(sftp_err)?;
    let mut entries = Vec::new();
    for entry in sftp.read_dir(canonical.clone()).await.map_err(sftp_err)? {
        let name = entry.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let meta = entry.metadata();
        let ft = entry.file_type();
        let is_symlink = ft.is_symlink();
        let full = join_remote(&canonical, &name);
        // For symlinks ask what they point at so a linked directory browses
        // as a directory. A dangling link just stays a plain (non-dir) entry.
        let is_dir = if is_symlink {
            sftp.metadata(full.clone()).await.map(|m| m.is_dir()).unwrap_or(false)
        } else {
            ft.is_dir()
        };
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        entries.push(RemoteEntry {
            name,
            path: full,
            is_dir,
            is_symlink,
            size: meta.len(),
            modified,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(RemoteDirListing {
        path: canonical,
        entries,
    })
}

/// Total bytes under `local` (a file's size, or the sum over a directory
/// tree) — computed up front so progress for a folder upload is meaningful.
fn local_total_bytes(local: &Path) -> u64 {
    if local.is_dir() {
        std::fs::read_dir(local)
            .map(|rd| {
                rd.flatten()
                    .map(|e| local_total_bytes(&e.path()))
                    .sum()
            })
            .unwrap_or(0)
    } else {
        std::fs::metadata(local).map(|m| m.len()).unwrap_or(0)
    }
}

/// Uploads a local file or directory tree into `remote_dir`. A directory is
/// recreated remotely under its own name (existing directories are reused,
/// existing files overwritten). Progress is cumulative across the tree.
pub(crate) async fn upload(
    sftp: &SftpSession,
    local: &Path,
    remote_dir: &str,
    progress: ProgressFn,
) -> Result<TransferSummary, SessionError> {
    let name = local
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| sftp_err("ローカルパスのファイル名を取得できません"))?
        .to_string();
    let total = local_total_bytes(local);
    let mut summary = TransferSummary::default();
    let mut done = 0u64;
    progress(0, total);
    let dest = join_remote(remote_dir, &name);
    upload_entry(sftp, local, &dest, total, &mut done, &mut summary, &progress).await?;
    Ok(summary)
}

fn upload_entry<'a>(
    sftp: &'a SftpSession,
    local: &'a Path,
    remote: &'a str,
    total: u64,
    done: &'a mut u64,
    summary: &'a mut TransferSummary,
    progress: &'a ProgressFn,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SessionError>> + Send + 'a>> {
    Box::pin(async move {
        if local.is_dir() {
            // Reuse an existing directory; only a real failure to create is
            // reported (e.g. a file already occupying the name).
            if !sftp.try_exists(remote.to_string()).await.map_err(sftp_err)? {
                sftp.create_dir(remote.to_string()).await.map_err(sftp_err)?;
            }
            let mut children: Vec<PathBuf> = std::fs::read_dir(local)
                .map_err(|e| sftp_err(format!("{}: {e}", local.display())))?
                .flatten()
                .map(|e| e.path())
                .collect();
            children.sort();
            for child in children {
                let child_name = child
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_default();
                if child_name.is_empty() {
                    continue;
                }
                let child_remote = join_remote(remote, &child_name);
                upload_entry(sftp, &child, &child_remote, total, done, summary, progress).await?;
            }
            return Ok(());
        }
        let mut src = tokio::fs::File::open(local)
            .await
            .map_err(|e| sftp_err(format!("{}: {e}", local.display())))?;
        let mut dst = sftp
            .open_with_flags(
                remote.to_string(),
                OpenFlags::CREATE | OpenFlags::WRITE | OpenFlags::TRUNCATE,
            )
            .await
            .map_err(|e| sftp_err(format!("{remote}: {e}")))?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = src.read(&mut buf).await.map_err(sftp_err)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])
                .await
                .map_err(|e| sftp_err(format!("{remote}: {e}")))?;
            *done += n as u64;
            summary.bytes += n as u64;
            progress(*done, total);
        }
        dst.shutdown().await.map_err(sftp_err)?;
        summary.files += 1;
        Ok(())
    })
}

/// Downloads one remote *file* to `local` (overwritten). Directories are
/// refused — the browser only offers files for download.
pub(crate) async fn download(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    progress: ProgressFn,
) -> Result<u64, SessionError> {
    let meta = sftp.metadata(remote.to_string()).await.map_err(|e| sftp_err(format!("{remote}: {e}")))?;
    if meta.is_dir() {
        return Err(sftp_err("ディレクトリはダウンロードできません(ファイルを選択してください)"));
    }
    let total = meta.len();
    let mut src = sftp
        .open(remote.to_string())
        .await
        .map_err(|e| sftp_err(format!("{remote}: {e}")))?;
    let mut dst = tokio::fs::File::create(local)
        .await
        .map_err(|e| sftp_err(format!("{}: {e}", local.display())))?;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    progress(0, total);
    loop {
        let n = src.read(&mut buf).await.map_err(|e| sftp_err(format!("{remote}: {e}")))?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await.map_err(sftp_err)?;
        done += n as u64;
        progress(done, total);
    }
    dst.flush().await.map_err(sftp_err)?;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_remote_handles_slashes_and_home() {
        assert_eq!(join_remote("/home/u", "a.txt"), "/home/u/a.txt");
        assert_eq!(join_remote("/home/u/", "a.txt"), "/home/u/a.txt");
        assert_eq!(join_remote("/", "a.txt"), "/a.txt");
        assert_eq!(join_remote("", "a.txt"), "a.txt");
        assert_eq!(join_remote(".", "a.txt"), "a.txt");
    }

    #[test]
    fn local_total_bytes_sums_a_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), b"12345").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b"), b"123").unwrap();
        assert_eq!(local_total_bytes(dir.path()), 8);
        assert_eq!(local_total_bytes(&dir.path().join("a")), 5);
        assert_eq!(local_total_bytes(&dir.path().join("missing")), 0);
    }
}
