//! End-to-end SSH session tests against a real sshd. Ignored by default —
//! they need a reachable server described by environment variables:
//!
//! ```sh
//! SENJU_SSH_HOST=127.0.0.1 SENJU_SSH_PORT=2222 SENJU_SSH_USER=smoke \
//! SENJU_SSH_PASSWORD=... SENJU_SSH_KEY=/path/to/key \
//! cargo test -p senju-core --test ssh_integration -- --ignored
//! ```

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use senju_core::models::{SshAuthMethod, SshHost};
use senju_core::sessions::{EventSink, SessionError, SessionManager, SshSecrets};

#[derive(Default)]
struct Capture {
    data: Mutex<Vec<u8>>,
    exited: Mutex<Option<i32>>,
}

impl EventSink for Capture {
    fn data(&self, _id: &str, data: &[u8]) {
        self.data.lock().unwrap().extend_from_slice(data);
    }
    fn exit(&self, _id: &str, code: i32) {
        *self.exited.lock().unwrap() = Some(code);
    }
}

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for this test"))
}

fn test_host(auth_method: SshAuthMethod) -> SshHost {
    SshHost {
        id: "test".into(),
        name: "test".into(),
        host: env("SENJU_SSH_HOST"),
        port: env("SENJU_SSH_PORT").parse().unwrap(),
        username: env("SENJU_SSH_USER"),
        auth_method,
        key_path: std::env::var("SENJU_SSH_KEY").unwrap_or_default(),
    }
}

async fn wait_for(capture: &Capture, needle: &str) -> bool {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if String::from_utf8_lossy(&capture.data.lock().unwrap()).contains(needle) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

async fn run_shell_roundtrip(auth_method: SshAuthMethod, secrets: SshSecrets) {
    let sink = Arc::new(Capture::default());
    // Each test gets its own known_hosts file (not the real
    // `~/.ssh/known_hosts`) so these roundtrip tests never depend on, or
    // race with, the dedicated known_hosts tests below.
    let known_hosts_dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::with_known_hosts_path(
        sink.clone(),
        Some(known_hosts_dir.path().join("known_hosts")),
    );
    let info = mgr
        // trust_host: true — these tests exercise the shell roundtrip, not
        // host key verification, so the first connection should just work.
        .create_ssh(&test_host(auth_method), secrets, 80, 24, true)
        .await
        .expect("ssh connect");
    assert_eq!(info.kind, "ssh");

    mgr.write(&info.id, b"echo remote-$((40+2))\n").await.unwrap();
    assert!(
        wait_for(&sink, "remote-42").await,
        "expected remote echo, got: {}",
        String::from_utf8_lossy(&sink.data.lock().unwrap())
    );

    mgr.resize(&info.id, 120, 40).await.unwrap();
    mgr.write(&info.id, b"exit\n").await.unwrap();

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if sink.exited.lock().unwrap().is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("session did not exit");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live sshd (see module docs)"]
async fn password_auth_shell_roundtrip() {
    run_shell_roundtrip(
        SshAuthMethod::Password,
        SshSecrets {
            password: Some(env("SENJU_SSH_PASSWORD")),
            passphrase: None,
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live sshd (see module docs)"]
async fn key_auth_shell_roundtrip() {
    run_shell_roundtrip(SshAuthMethod::Key, SshSecrets::default()).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live sshd (see module docs)"]
async fn wrong_password_is_rejected() {
    let sink = Arc::new(Capture::default());
    let known_hosts_dir = tempfile::tempdir().unwrap();
    let mgr = SessionManager::with_known_hosts_path(
        sink,
        Some(known_hosts_dir.path().join("known_hosts")),
    );
    let err = mgr
        .create_ssh(
            &test_host(SshAuthMethod::Password),
            SshSecrets {
                password: Some("definitely-wrong".into()),
                passphrase: None,
            },
            80,
            24,
            true,
        )
        .await
        .expect_err("auth should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("auth") || msg.contains("reject"),
        "unexpected error: {msg}"
    );
}

/// Known_hosts TOFU workflow against a real sshd:
/// (a) first connection with `trust_host: false` fails with
///     `SessionError::UnknownHostKey`;
/// (b) retrying with `trust_host: true` succeeds and records the key;
/// (c) reconnecting with `trust_host: false` now succeeds, since the key is
///     recorded and matches.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live sshd (see module docs)"]
async fn known_hosts_tofu_workflow() {
    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");
    let secrets = || SshSecrets {
        password: Some(env("SENJU_SSH_PASSWORD")),
        passphrase: None,
    };

    // (a) unknown host, not trusted -> rejected with UnknownHostKey.
    {
        let sink = Arc::new(Capture::default());
        let mgr =
            SessionManager::with_known_hosts_path(sink, Some(known_hosts_path.clone()));
        let err = mgr
            .create_ssh(
                &test_host(SshAuthMethod::Password),
                secrets(),
                80,
                24,
                false,
            )
            .await
            .expect_err("unknown host must be rejected");
        match err {
            SessionError::UnknownHostKey { key_type, fingerprint } => {
                assert!(!key_type.is_empty());
                assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
            }
            other => panic!("expected UnknownHostKey, got: {other}"),
        }
        assert!(
            !known_hosts_path.exists(),
            "known_hosts must not be written when trust_host is false"
        );
    }

    // (b) unknown host, trusted -> connects and records the key.
    {
        let sink = Arc::new(Capture::default());
        let mgr =
            SessionManager::with_known_hosts_path(sink.clone(), Some(known_hosts_path.clone()));
        let info = mgr
            .create_ssh(
                &test_host(SshAuthMethod::Password),
                secrets(),
                80,
                24,
                true,
            )
            .await
            .expect("trusted first connection should succeed");
        mgr.kill(&info.id);
        assert!(
            known_hosts_path.exists(),
            "trust_host: true should have recorded the host key"
        );
        let contents = std::fs::read_to_string(&known_hosts_path).unwrap();
        assert!(
            contents.contains("ssh-"),
            "known_hosts should contain the recorded key: {contents}"
        );
    }

    // (c) now-known host, not trusted -> still connects (key matches).
    {
        let sink = Arc::new(Capture::default());
        let mgr =
            SessionManager::with_known_hosts_path(sink, Some(known_hosts_path.clone()));
        let info = mgr
            .create_ssh(
                &test_host(SshAuthMethod::Password),
                secrets(),
                80,
                24,
                false,
            )
            .await
            .expect("previously-learned host key should verify");
        mgr.kill(&info.id);
    }
}
