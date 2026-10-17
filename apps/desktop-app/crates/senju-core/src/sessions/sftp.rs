//! File transfer over an SSH session's SFTP subsystem.
//!
//! The transfers run on the connection the user already has open, which is
//! the whole point: no second authentication, no separate host key to trust,
//! and a jump-host chain is followed for free because the channel is opened
//! on the far end of it.
//!
//! Both directions stream in fixed chunks rather than reading a file into
//! memory. A terminal is exactly the tool people use to move a database dump
//! or a core file around, and `read_to_end` on one of those is how an app
//! gets killed by the OOM killer while claiming to be transferring.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};

use russh_sftp::client::SftpSession;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::SessionError;

/// Chunk size for both directions. Large enough that the per-packet SFTP
/// round trip is not the bottleneck, small enough that cancelling and the
/// progress bar both feel immediate on a slow link.
const CHUNK: usize = 32 * 1024;

/// Reports bytes moved so far and the total when it is known. `total` is 0
/// when the size could not be determined (a remote `stat` the server refused,
/// a stream with no length) — a caller showing a bar should fall back to an
/// indeterminate one rather than dividing by zero.
pub type Progress<'a> = dyn Fn(u64, u64) + Send + Sync + 'a;

fn sftp_err(context: &str, e: impl std::fmt::Display) -> SessionError {
    SessionError::Ssh(format!("{context}: {e}"))
}

/// Cancelled transfers are an error, not a silent success: the file at the
/// destination is a partial one, and a caller that treated this as "done"
/// would leave it there looking complete.
fn cancelled() -> SessionError {
    SessionError::Ssh("転送を中止しました".into())
}

/// Sends a local file to `remote`, overwriting it. Returns the byte count.
pub async fn upload(
    sftp: &SftpSession,
    local: &Path,
    remote: &str,
    cancel: &AtomicBool,
    progress: &Progress<'_>,
) -> Result<u64, SessionError> {
    let mut src = tokio::fs::File::open(local)
        .await
        .map_err(|e| sftp_err(&format!("ローカルファイルを開けません ({})", local.display()), e))?;
    let total = src.metadata().await.map(|m| m.len()).unwrap_or(0);

    let mut dst = sftp
        .create(remote)
        .await
        .map_err(|e| sftp_err(&format!("リモートに書き込めません ({remote})"), e))?;

    let mut buf = vec![0u8; CHUNK];
    let mut moved = 0u64;
    progress(0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let n = src.read(&mut buf).await.map_err(|e| sftp_err("読み込みに失敗", e))?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await.map_err(|e| sftp_err("送信に失敗", e))?;
        moved += n as u64;
        progress(moved, total);
    }
    // Without this the last chunks can still be buffered when the handle is
    // dropped, and a "finished" transfer arrives truncated.
    dst.flush().await.map_err(|e| sftp_err("送信の完了処理に失敗", e))?;
    dst.shutdown().await.map_err(|e| sftp_err("送信の完了処理に失敗", e))?;
    Ok(moved)
}

/// Fetches `remote` into a local path, overwriting it. Returns the byte count.
pub async fn download(
    sftp: &SftpSession,
    remote: &str,
    local: &Path,
    cancel: &AtomicBool,
    progress: &Progress<'_>,
) -> Result<u64, SessionError> {
    // Ask for the size first so the progress bar is a bar rather than a
    // spinner. A server that refuses `stat` is not a reason to refuse the
    // transfer, so a failure here just means "total unknown".
    let total = sftp.metadata(remote).await.ok().and_then(|m| m.size).unwrap_or(0);

    let mut src = sftp
        .open(remote)
        .await
        .map_err(|e| sftp_err(&format!("リモートファイルを開けません ({remote})"), e))?;
    let mut dst = tokio::fs::File::create(local)
        .await
        .map_err(|e| sftp_err(&format!("ローカルに書き込めません ({})", local.display()), e))?;

    let mut buf = vec![0u8; CHUNK];
    let mut moved = 0u64;
    progress(0, total);
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        let n = src.read(&mut buf).await.map_err(|e| sftp_err("受信に失敗", e))?;
        if n == 0 {
            break;
        }
        dst.write_all(&buf[..n]).await.map_err(|e| sftp_err("書き込みに失敗", e))?;
        moved += n as u64;
        progress(moved, total);
    }
    dst.flush().await.map_err(|e| sftp_err("書き込みの完了処理に失敗", e))?;
    Ok(moved)
}

/// One entry in a remote directory listing.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteEntry {
    pub name: String,
    /// Full path, so the caller never has to re-join and get separators wrong.
    pub path: String,
    /// Directories are navigable and files are transferable, and the UI needs
    /// to sort and label by that before anything else.
    pub is_dir: bool,
    /// A symlink is reported as such *and* by what it points at (`is_dir`
    /// follows the link), because "can I open this" is the question the
    /// browser is actually asking.
    pub is_symlink: bool,
    /// `None` for entries whose size the server did not report.
    pub size: Option<u64>,
    /// Unix mtime in seconds, when reported.
    pub mtime: Option<u32>,
}

/// Lists a remote directory, resolved on the server so `~` and relative paths
/// behave as they would in the shell.
///
/// `.` and `..` are dropped by the underlying iterator; the caller supplies
/// its own "up" affordance from the resolved path, which is more reliable
/// than trusting a server to list a parent entry.
///
/// Entries are sorted directories-first then by name, case-insensitively.
/// Servers return them in whatever order the filesystem hands over, which for
/// a large directory is effectively random — unusable to look through.
pub async fn read_dir(
    sftp: &SftpSession,
    path: &str,
) -> Result<(String, Vec<RemoteEntry>), SessionError> {
    let resolved = canonicalize(sftp, path).await?;
    let dir = sftp
        .read_dir(resolved.clone())
        .await
        .map_err(|e| sftp_err(&format!("ディレクトリを読めません ({resolved})"), e))?;

    let mut out: Vec<RemoteEntry> = dir
        .map(|entry| {
            let meta = entry.metadata();
            RemoteEntry {
                name: entry.file_name(),
                path: entry.path(),
                is_dir: meta.is_dir(),
                is_symlink: meta.is_symlink(),
                size: meta.size,
                mtime: meta.mtime,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok((resolved, out))
}

/// Resolves `path` on the server the way the shell would: `~` and relative
/// paths become absolute, which is what makes a remote path typed from memory
/// ("~/dumps/db.sql") land where the user expects.
pub async fn canonicalize(sftp: &SftpSession, path: &str) -> Result<String, SessionError> {
    sftp.canonicalize(path)
        .await
        .map_err(|e| sftp_err(&format!("リモートパスを解決できません ({path})"), e))
}

/* ---------------- drop / folder upload ---------------- */

/// Joins a remote directory and a name with a single `/`. An empty or `.`
/// directory means "relative to the SFTP start directory", so the name is
/// returned bare and the server resolves it.
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

/// Total bytes under `local`: a file's size, or the sum over a directory
/// tree. Computed up front so the progress bar of a folder upload counts
/// toward the whole folder rather than restarting for every file.
pub fn local_total_bytes(local: &Path) -> u64 {
    if local.is_dir() {
        std::fs::read_dir(local)
            .map(|rd| rd.flatten().map(|e| local_total_bytes(&e.path())).sum())
            .unwrap_or(0)
    } else {
        std::fs::metadata(local).map(|m| m.len()).unwrap_or(0)
    }
}

/// Uploads a local file or directory tree *into* `remote_dir`, under its own
/// name: `/tmp/report.pdf` into `/srv` lands at `/srv/report.pdf`, and a
/// folder is recreated remotely (existing directories are reused, existing
/// files overwritten). Returns the remote path it created and the bytes
/// moved. Progress is cumulative across the tree. This is what a drop or an
/// "upload here" wants, as opposed to [`upload`], which takes the exact
/// destination path.
pub async fn upload_into(
    sftp: &SftpSession,
    local: &Path,
    remote_dir: &str,
    cancel: &AtomicBool,
    progress: &Progress<'_>,
) -> Result<(String, u64), SessionError> {
    let name = local
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| sftp_err("ローカルパスの名前を取得できません", local.display()))?;
    let dest = join_remote(remote_dir, name);
    let total = local_total_bytes(local);
    let mut done = 0u64;
    progress(0, total);
    upload_tree(sftp, local, &dest, total, &mut done, cancel, progress).await?;
    Ok((dest, done))
}

/// Creates `path` on the server unless a directory is already there. A
/// *file* of that name is an error rather than something to overwrite: the
/// user dropped a folder, and silently replacing a file with it is not what
/// "upload" means.
async fn ensure_remote_dir(sftp: &SftpSession, path: &str) -> Result<(), SessionError> {
    match sftp.metadata(path).await {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(sftp_err(
            &format!("同名のファイルがあるためディレクトリを作れません ({path})"),
            "exists",
        )),
        Err(_) => sftp
            .create_dir(path)
            .await
            .map_err(|e| sftp_err(&format!("ディレクトリを作れません ({path})"), e)),
    }
}

/// Recursive worker for [`upload_into`]. Boxed because async recursion needs
/// a known-size future. Children are visited in name order so the progress
/// (and any error) is reproducible.
fn upload_tree<'a>(
    sftp: &'a SftpSession,
    local: &'a Path,
    remote: &'a str,
    total: u64,
    done: &'a mut u64,
    cancel: &'a AtomicBool,
    progress: &'a Progress<'a>,
) -> Pin<Box<dyn Future<Output = Result<(), SessionError>> + Send + 'a>> {
    Box::pin(async move {
        if cancel.load(Ordering::Relaxed) {
            return Err(cancelled());
        }
        if !local.is_dir() {
            let base = *done;
            let moved = upload(sftp, local, remote, cancel, &|moved, _| progress(base + moved, total)).await?;
            *done += moved;
            return Ok(());
        }
        ensure_remote_dir(sftp, remote).await?;
        let mut children: Vec<PathBuf> = std::fs::read_dir(local)
            .map_err(|e| sftp_err(&format!("ローカルのディレクトリを読めません ({})", local.display()), e))?
            .flatten()
            .map(|e| e.path())
            .collect();
        children.sort();
        for child in children {
            let Some(name) = child.file_name().and_then(|n| n.to_str()).filter(|n| !n.is_empty()) else {
                continue;
            };
            let child_remote = join_remote(remote, name);
            upload_tree(sftp, &child, &child_remote, total, done, cancel, progress).await?;
        }
        Ok(())
    })
}

/* ---------------- authorized_keys ---------------- */

/// Adds `line` (one `type base64 [comment]` public key) to the contents of an
/// `authorized_keys`, unless the same key is already there. Keys are compared
/// by type and key material only: a comment is a label, and the same key
/// with a different label is still the same key. Returns the new contents
/// and whether anything was added.
pub fn merge_authorized_keys(existing: &str, line: &str) -> (String, bool) {
    let identity = |l: &str| -> Option<(String, String)> {
        let mut it = l.split_whitespace();
        // `authorized_keys` lines may start with options (`no-pty,from="…"`);
        // skip ahead to the first field that looks like a key type.
        let mut t = it.next()?;
        while !t.starts_with("ssh-") && !t.starts_with("ecdsa-") && !t.starts_with("sk-") {
            t = it.next()?;
        }
        Some((t.to_string(), it.next()?.to_string()))
    };
    let Some(wanted) = identity(line.trim()) else {
        return (existing.to_string(), false);
    };
    let present = existing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .any(|l| identity(l).as_ref() == Some(&wanted));
    if present {
        return (existing.to_string(), false);
    }
    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(line.trim());
    out.push('\n');
    (out, true)
}

/// Installs a public key into the session user's `~/.ssh/authorized_keys`
/// over SFTP — what `ssh-copy-id` does, without needing it on the client.
/// Creates `~/.ssh` (0700) and the file (0600) if missing. Returns whether
/// the key was added (false = it was already there).
pub async fn install_authorized_key(sftp: &SftpSession, pubkey_line: &str) -> Result<bool, SessionError> {
    use russh_sftp::protocol::FileAttributes;
    let home = canonicalize(sftp, ".").await?;
    let ssh_dir = format!("{home}/.ssh");
    let ak = format!("{ssh_dir}/authorized_keys");

    let dir_exists = sftp.try_exists(&ssh_dir).await.map_err(|e| sftp_err("~/.ssh の確認に失敗", e))?;
    if !dir_exists {
        sftp.create_dir(&ssh_dir).await.map_err(|e| sftp_err("~/.ssh を作成できません", e))?;
        // sshd refuses keys from a directory others can write to, so the
        // mode is part of installing the key, not a nicety.
        let attrs = FileAttributes { permissions: Some(0o700), ..Default::default() };
        let _ = sftp.set_metadata(&ssh_dir, attrs).await;
    }

    let existing = match sftp.open(&ak).await {
        Ok(mut f) => {
            let mut buf = String::new();
            f.read_to_string(&mut buf).await.map_err(|e| sftp_err("authorized_keys を読めません", e))?;
            buf
        }
        Err(_) => String::new(),
    };
    let (merged, added) = merge_authorized_keys(&existing, pubkey_line);
    if !added {
        return Ok(false);
    }
    let mut dst = sftp.create(&ak).await.map_err(|e| sftp_err("authorized_keys に書き込めません", e))?;
    dst.write_all(merged.as_bytes()).await.map_err(|e| sftp_err("送信に失敗", e))?;
    dst.flush().await.map_err(|e| sftp_err("送信の完了処理に失敗", e))?;
    dst.shutdown().await.map_err(|e| sftp_err("送信の完了処理に失敗", e))?;
    let attrs = FileAttributes { permissions: Some(0o600), ..Default::default() };
    let _ = sftp.set_metadata(&ak, attrs).await;
    Ok(true)
}

#[cfg(test)]
mod authorized_keys_tests {
    use super::merge_authorized_keys;

    const K1: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA me@a";
    const K2: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB me@b";

    #[test]
    fn appends_with_newline_handling() {
        assert_eq!(merge_authorized_keys("", K1), (format!("{K1}\n"), true));
        // No trailing newline on the existing file: one is added before ours,
        // or the two keys would end up on one line.
        assert_eq!(merge_authorized_keys(K2, K1), (format!("{K2}\n{K1}\n"), true));
        assert_eq!(merge_authorized_keys(&format!("{K2}\n"), K1), (format!("{K2}\n{K1}\n"), true));
    }

    #[test]
    fn same_key_is_not_added_twice_even_with_another_comment() {
        let existing = format!("# header\n{K2}\n{K1}\n");
        let relabeled = K1.replace("me@a", "other label");
        assert_eq!(merge_authorized_keys(&existing, &relabeled), (existing.clone(), false));
        assert_eq!(merge_authorized_keys(&existing, K1), (existing, false));
    }

    #[test]
    fn options_prefix_on_existing_lines_is_seen_through() {
        let existing = format!("no-pty,from=\"10.0.0.0/8\" {K1}\n");
        assert!(!merge_authorized_keys(&existing, K1).1);
        assert!(merge_authorized_keys(&existing, K2).1);
    }

    #[test]
    fn garbage_is_not_installed() {
        assert_eq!(merge_authorized_keys("", "not a key"), (String::new(), false));
    }
}

#[cfg(test)]
mod drop_upload_tests {
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
