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
use senju_core::sessions::{EventSink, SessionManager, SshSecrets};

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
    let mgr = SessionManager::new(sink.clone());
    let info = mgr
        .create_ssh(&test_host(auth_method), secrets, 80, 24)
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
    let mgr = SessionManager::new(sink);
    let err = mgr
        .create_ssh(
            &test_host(SshAuthMethod::Password),
            SshSecrets {
                password: Some("definitely-wrong".into()),
                passphrase: None,
            },
            80,
            24,
        )
        .await
        .expect_err("auth should fail");
    let msg = err.to_string();
    assert!(
        msg.contains("auth") || msg.contains("reject"),
        "unexpected error: {msg}"
    );
}
