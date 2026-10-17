//! SSH key pairs: generating one, and listing the public halves on disk.
//!
//! Ed25519 only. It is what `ssh-keygen` defaults to, every server from the
//! last decade accepts it, and offering RSA sizes and ECDSA curves would be a
//! menu of ways to pick something worse. The private key is written in the
//! OpenSSH format `ssh` itself reads, so the pair is usable outside this app.

use std::fs;
use std::path::{Path, PathBuf};

use russh::keys::ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey, PublicKey};
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct GeneratedKey {
    pub private_path: String,
    pub public_path: String,
    /// The `.pub` line, ready to paste into an `authorized_keys`.
    pub public_key: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PublicKeyInfo {
    pub path: String,
    pub key_type: String,
    pub fingerprint: String,
    pub comment: String,
}

/// `~/.ssh`, where both `ssh` and the saved hosts' `key_path` expect keys.
pub fn default_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".ssh"))
}

/// Creates `<dir>/<name>` and `<dir>/<name>.pub`. Refuses to overwrite:
/// a private key that gets replaced is a lockout from every server that
/// trusts the old one, which is not something a typo should be able to do.
/// `passphrase` encrypts the private key when non-empty.
pub fn generate_ed25519(
    dir: &Path,
    name: &str,
    comment: &str,
    passphrase: Option<&str>,
) -> Result<GeneratedKey, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("鍵のファイル名を入力してください".into());
    }
    // A plain file name only. A path here would write the key wherever the
    // string said, and `..` would let a saved host's key_path escape ~/.ssh.
    if name.contains(['/', '\\']) || name.starts_with('.') || name.chars().any(char::is_whitespace) {
        return Err("ファイル名に使えるのは英数字・-・_・. です (先頭の . と区切り文字は不可)".into());
    }
    let private_path = dir.join(name);
    let public_path = dir.join(format!("{name}.pub"));
    if private_path.exists() || public_path.exists() {
        return Err(format!("{} は既に存在します。別の名前にしてください", private_path.display()));
    }
    fs::create_dir_all(dir).map_err(|e| format!("{} を作成できません: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }

    let mut key = PrivateKey::random(&mut rand::rngs::OsRng, Algorithm::Ed25519)
        .map_err(|e| format!("鍵の生成に失敗しました: {e}"))?;
    key.set_comment(comment.trim());
    let public = key.public_key().clone();
    let fingerprint = public.fingerprint(HashAlg::Sha256).to_string();
    let public_key = public.to_openssh().map_err(|e| e.to_string())?;

    let to_write = match passphrase.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => key
            .encrypt(&mut rand::rngs::OsRng, p)
            .map_err(|e| format!("パスフレーズでの暗号化に失敗しました: {e}"))?,
        None => key,
    };
    // ssh-key writes the private half with mode 0600 on Unix, as ssh demands.
    to_write
        .write_openssh_file(&private_path, LineEnding::LF)
        .map_err(|e| format!("{} に書き込めません: {e}", private_path.display()))?;
    public
        .write_openssh_file(&public_path)
        .map_err(|e| format!("{} に書き込めません: {e}", public_path.display()))?;

    Ok(GeneratedKey {
        private_path: private_path.to_string_lossy().into_owned(),
        public_path: public_path.to_string_lossy().into_owned(),
        public_key,
        fingerprint,
    })
}

/// Checks that `line` is one OpenSSH public key (`type base64 [comment]`),
/// so a stray file is rejected before it is appended to a server's
/// `authorized_keys` — as a line sshd ignores, or worse, honours.
pub fn validate_public_key_line(line: &str) -> Result<(), String> {
    PublicKey::from_openssh(line.trim())
        .map(|_| ())
        .map_err(|_| "OpenSSH 形式の公開鍵ではありません".to_string())
}

/// Every parseable `*.pub` in `dir`, for the "install this key" picker.
pub fn list_public_keys(dir: &Path) -> Vec<PublicKeyInfo> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PublicKeyInfo> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "pub"))
        .filter_map(|e| {
            let path = e.path();
            let key = PublicKey::read_openssh_file(&path).ok()?;
            Some(PublicKeyInfo {
                path: path.to_string_lossy().into_owned(),
                key_type: key.algorithm().to_string(),
                fingerprint: key.fingerprint(HashAlg::Sha256).to_string(),
                comment: key.comment().to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_a_usable_pair() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".ssh");
        let g = generate_ed25519(&dir, "id_test", "me@box", None).unwrap();
        assert!(Path::new(&g.private_path).is_file() && Path::new(&g.public_path).is_file());
        assert!(g.public_key.starts_with("ssh-ed25519 "), "{}", g.public_key);
        assert!(g.public_key.ends_with(" me@box"), "{}", g.public_key);
        assert!(g.fingerprint.starts_with("SHA256:"));
        // The private file is what ssh will read back, and it matches.
        let back = PrivateKey::read_openssh_file(Path::new(&g.private_path)).unwrap();
        assert!(!back.is_encrypted());
        assert_eq!(back.public_key().fingerprint(HashAlg::Sha256).to_string(), g.fingerprint);
        assert_eq!(fs::read_to_string(&g.public_path).unwrap().trim(), g.public_key);
        let listed = list_public_keys(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!((listed[0].fingerprint.as_str(), listed[0].comment.as_str()), (g.fingerprint.as_str(), "me@box"));
    }

    #[cfg(unix)]
    #[test]
    fn private_key_and_dir_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(".ssh");
        let g = generate_ed25519(&dir, "k", "", None).unwrap();
        assert_eq!(fs::metadata(&g.private_path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    }

    #[test]
    fn passphrase_encrypts_and_decrypts() {
        let tmp = tempfile::tempdir().unwrap();
        let g = generate_ed25519(tmp.path(), "enc", "c", Some("correct horse")).unwrap();
        let back = PrivateKey::read_openssh_file(Path::new(&g.private_path)).unwrap();
        assert!(back.is_encrypted());
        assert!(back.decrypt("wrong").is_err());
        let open = back.decrypt("correct horse").unwrap();
        assert_eq!(open.public_key().fingerprint(HashAlg::Sha256).to_string(), g.fingerprint);
        // A blank passphrase means none, not an empty-string password.
        let g2 = generate_ed25519(tmp.path(), "plain", "c", Some("  ")).unwrap();
        assert!(!PrivateKey::read_openssh_file(Path::new(&g2.private_path)).unwrap().is_encrypted());
    }

    #[test]
    fn never_overwrites_and_rejects_paths() {
        let tmp = tempfile::tempdir().unwrap();
        generate_ed25519(tmp.path(), "id", "", None).unwrap();
        let before = fs::read(tmp.path().join("id")).unwrap();
        assert!(generate_ed25519(tmp.path(), "id", "", None).unwrap_err().contains("既に存在"));
        assert_eq!(fs::read(tmp.path().join("id")).unwrap(), before, "untouched");
        for bad in ["", "a/b", "a\\b", ".hidden", "has space", "../escape"] {
            assert!(generate_ed25519(tmp.path(), bad, "", None).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn listing_skips_non_keys() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("junk.pub"), "not a key\n").unwrap();
        fs::write(tmp.path().join("notes.txt"), "ssh-ed25519 AAAA\n").unwrap();
        assert!(list_public_keys(tmp.path()).is_empty());
        assert!(list_public_keys(&tmp.path().join("missing")).is_empty());
    }
}
