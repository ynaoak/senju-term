//! `~/.ssh/known_hosts` as a whole file, for the UI to show and prune.
//!
//! The connect path only ever asks "is this key known for this host" (see
//! `sessions::ssh`); this module is for the person looking at the file: what
//! is in it, and removing an entry that no longer matches a server whose key
//! was legitimately replaced. Until now the app refused such a host (as it
//! must) and left the user to find and edit the line by hand.
//!
//! Lines are kept byte-for-byte except the ones removed: comments, blank
//! lines, hashed hosts and `@marker` entries all survive a prune untouched.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use russh::keys::{HashAlg, PublicKey};
use serde::Serialize;

/// One key line of the file.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct KnownHostEntry {
    /// 1-based line number — what `remove_lines` takes, and what the
    /// `HOST_KEY_MISMATCH` error reports.
    pub line: usize,
    /// The host field verbatim: `host`, `[host]:port`, `a,b` or a hash.
    pub hosts: String,
    /// `|1|salt|hash` form: the name cannot be shown, only matched.
    pub hashed: bool,
    /// `@cert-authority` / `@revoked`, or empty.
    pub marker: String,
    pub key_type: String,
    /// `SHA256:…`, or empty when the key did not parse (the line is still
    /// listed, so a corrupt entry can be removed rather than hidden).
    pub fingerprint: String,
    pub comment: String,
}

/// The file the app's own verification uses.
pub fn default_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

/// Every key line in the file. A missing file is an empty list, not an
/// error: nothing has been trusted yet.
pub fn list(path: &Path) -> io::Result<Vec<KnownHostEntry>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Parses the known_hosts format: `[@marker] hosts type base64 [comment]`.
pub fn parse(text: &str) -> Vec<KnownHostEntry> {
    text.lines()
        .enumerate()
        .filter_map(|(i, raw)| {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.split_whitespace();
            let mut first = parts.next()?;
            let mut marker = String::new();
            if first.starts_with('@') {
                marker = first.to_string();
                first = parts.next()?;
            }
            let hosts = first.to_string();
            let key_type = parts.next()?.to_string();
            let b64 = parts.next()?;
            let comment = parts.collect::<Vec<_>>().join(" ");
            let fingerprint = PublicKey::from_openssh(&format!("{key_type} {b64}"))
                .map(|k| k.fingerprint(HashAlg::Sha256).to_string())
                .unwrap_or_default();
            Some(KnownHostEntry {
                line: i + 1,
                hashed: hosts.starts_with("|1|"),
                hosts,
                marker,
                key_type,
                fingerprint,
                comment,
            })
        })
        .collect()
}

/// Deletes the given 1-based lines and writes the file back, every other
/// byte unchanged. Returns how many lines were removed. The write goes
/// through a sibling temp file and a rename, so a crash mid-write cannot
/// leave a half-empty known_hosts behind — which would silently un-trust
/// every host at once.
pub fn remove_lines(path: &Path, lines: &[usize]) -> io::Result<usize> {
    let text = fs::read_to_string(path)?;
    let mut out = String::with_capacity(text.len());
    let mut removed = 0;
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if lines.contains(&(i + 1)) {
            removed += 1;
        } else {
            out.push_str(l);
        }
    }
    if removed == 0 {
        return Ok(0);
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &out)?;
    // Keep the original mode: a known_hosts that is suddenly world-writable
    // is a different kind of problem.
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    fs::rename(&tmp, path)?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::{Algorithm, PrivateKey};

    fn real_key_line(hosts: &str) -> (String, String) {
        let key = PrivateKey::random(&mut rand::rngs::OsRng, Algorithm::Ed25519).unwrap();
        let pub_line = key.public_key().to_openssh().unwrap();
        let fp = key.public_key().fingerprint(HashAlg::Sha256).to_string();
        (format!("{hosts} {pub_line}"), fp)
    }

    #[test]
    fn parses_plain_hashed_and_marked_lines() {
        let (l1, fp1) = real_key_line("example.com,10.0.0.1");
        let (l2, fp2) = real_key_line("|1|abc=|def=");
        let (l3, fp3) = real_key_line("@cert-authority *.corp");
        let text = format!("# comment\n\n{l1}\n{l2}\n{l3} trailing comment\n");
        let got = parse(&text);
        assert_eq!(got.len(), 3);
        assert_eq!((got[0].line, got[0].hosts.as_str(), got[0].hashed), (3, "example.com,10.0.0.1", false));
        assert_eq!(got[0].fingerprint, fp1);
        assert_eq!(got[0].key_type, "ssh-ed25519");
        assert_eq!((got[1].line, got[1].hashed), (4, true));
        assert_eq!(got[1].fingerprint, fp2);
        assert_eq!((got[2].line, got[2].marker.as_str(), got[2].hosts.as_str()), (5, "@cert-authority", "*.corp"));
        assert_eq!(got[2].fingerprint, fp3);
        assert!(got[2].comment.ends_with("trailing comment"));
    }

    /// A line whose key is garbage is still listed — with no fingerprint —
    /// because the point of the list is to let the user remove it.
    #[test]
    fn unparseable_key_is_listed_without_fingerprint() {
        let got = parse("host ssh-ed25519 not-base64!!\n");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].fingerprint, "");
        assert_eq!(parse("only-two fields\n").len(), 0, "too few fields is not an entry");
    }

    #[test]
    fn missing_file_lists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list(&tmp.path().join("absent")).unwrap().is_empty());
    }

    #[test]
    fn remove_keeps_everything_else_byte_for_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("known_hosts");
        let (l1, _) = real_key_line("a");
        let (l2, _) = real_key_line("b");
        let (l3, _) = real_key_line("c");
        // CRLF on one line, a comment, a blank line and no trailing newline
        // at the end: all must survive untouched.
        let text = format!("# keep me\r\n{l1}\n\n{l2}\r\n{l3}");
        fs::write(&path, &text).unwrap();
        let removed = remove_lines(&path, &[4, 99]).unwrap();
        assert_eq!(removed, 1);
        let after = fs::read_to_string(&path).unwrap();
        assert_eq!(after, format!("# keep me\r\n{l1}\n\n{l3}"));
        assert_eq!(remove_lines(&path, &[42]).unwrap(), 0, "no such line: untouched");
        assert_eq!(fs::read_to_string(&path).unwrap(), after);
    }

    #[cfg(unix)]
    #[test]
    fn remove_preserves_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("known_hosts");
        let (l1, _) = real_key_line("a");
        fs::write(&path, format!("{l1}\n{l1}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        remove_lines(&path, &[1]).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
