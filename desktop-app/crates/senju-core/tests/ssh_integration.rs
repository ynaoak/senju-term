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

/// Connects once with no `expected_fingerprint`, which must fail with
/// `SessionError::UnknownHostKey`, and returns the fingerprint it reported —
/// i.e. simulates "the UI showed the user this fingerprint and they looked
/// at it", the state a real approval would be based on.
async fn learn_unknown_host_fingerprint(mgr: &SessionManager, host: &SshHost, secrets: SshSecrets) -> String {
    let err = mgr
        .create_ssh(host, secrets, 80, 24, None)
        .await
        .expect_err("host must still be unknown at this point");
    match err {
        SessionError::UnknownHostKey { fingerprint, .. } => fingerprint,
        other => panic!("expected UnknownHostKey, got: {other}"),
    }
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
    // These tests exercise the shell roundtrip, not host key verification:
    // learn the real fingerprint via the expected first-contact rejection,
    // then reconnect approving exactly that fingerprint (the TOFU-approved
    // path), which records it and proceeds.
    let expected_fingerprint =
        learn_unknown_host_fingerprint(&mgr, &test_host(auth_method), secrets.clone()).await;
    let info = mgr
        .create_ssh(
            &test_host(auth_method),
            secrets,
            80,
            24,
            Some(expected_fingerprint),
        )
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

/// Combined public-key + password auth against a server that requires BOTH
/// (`AuthenticationMethods publickey,password`). Uses its own env vars because
/// it needs a differently-configured sshd than the other tests, and skips
/// cleanly when `SENJU_SSH_COMBINED_HOST` isn't set.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live sshd with AuthenticationMethods publickey,password"]
async fn combined_key_password_auth() {
    let Ok(host_addr) = std::env::var("SENJU_SSH_COMBINED_HOST") else {
        return;
    };
    let host = SshHost {
        id: "combo".into(),
        name: "combo".into(),
        host: host_addr,
        port: env("SENJU_SSH_COMBINED_PORT").parse().unwrap(),
        username: env("SENJU_SSH_COMBINED_USER"),
        auth_method: SshAuthMethod::KeyPassword,
        key_path: env("SENJU_SSH_COMBINED_KEY"),
    };
    let secrets = || SshSecrets {
        password: Some(env("SENJU_SSH_COMBINED_PASSWORD")),
        passphrase: None,
    };

    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");

    // The pre-save connection test succeeds when both secrets are supplied.
    // Learn the fingerprint via the first-contact rejection, then approve it.
    let sink = Arc::new(Capture::default());
    let mgr = SessionManager::with_known_hosts_path(sink, Some(known_hosts_path.clone()));
    let fp = learn_unknown_host_fingerprint(&mgr, &host, secrets()).await;
    let report = mgr
        .test_ssh(&host, secrets(), Some(fp.clone()))
        .await
        .expect("combined publickey+password auth should succeed");
    assert!(report.fingerprint.starts_with("SHA256:"), "{}", report.fingerprint);

    // Key alone must fail: the server also requires the password.
    let sink2 = Arc::new(Capture::default());
    let mgr2 = SessionManager::with_known_hosts_path(sink2, Some(known_hosts_path.clone()));
    let key_only = SshHost {
        auth_method: SshAuthMethod::Key,
        ..host.clone()
    };
    mgr2.test_ssh(&key_only, SshSecrets::default(), Some(fp))
        .await
        .expect_err("key-only auth must fail against a publickey,password server");

    // Full shell roundtrip over combined auth: learn the fingerprint via the
    // first-contact rejection, then connect approving it.
    let sink3 = Arc::new(Capture::default());
    let mgr3 =
        SessionManager::with_known_hosts_path(sink3.clone(), Some(known_hosts_path.clone()));
    let fingerprint = learn_unknown_host_fingerprint(&mgr3, &host, secrets()).await;
    let info = mgr3
        .create_ssh(&host, secrets(), 80, 24, Some(fingerprint))
        .await
        .expect("combined-auth connect");
    mgr3.write(&info.id, b"echo combo-$((1+1))\n").await.unwrap();
    assert!(
        wait_for(&sink3, "combo-2").await,
        "expected remote echo over combined auth"
    );
    mgr3.kill(&info.id);
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
    // Get past host-key verification first (with the real, approved
    // fingerprint) so the failure this test checks for is really about
    // authentication, not an incidental UnknownHostKey rejection.
    let expected_fingerprint = learn_unknown_host_fingerprint(
        &mgr,
        &test_host(SshAuthMethod::Password),
        SshSecrets {
            password: Some("definitely-wrong".into()),
            passphrase: None,
        },
    )
    .await;
    let err = mgr
        .create_ssh(
            &test_host(SshAuthMethod::Password),
            SshSecrets {
                password: Some("definitely-wrong".into()),
                passphrase: None,
            },
            80,
            24,
            Some(expected_fingerprint),
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
/// (a) first connection with `expected_fingerprint: None` fails with
///     `SessionError::UnknownHostKey`;
/// (b) retrying with `expected_fingerprint: Some(<the fingerprint that error
///     reported>)` succeeds and records the key;
/// (c) reconnecting with `expected_fingerprint: None` now succeeds, since the
///     key is recorded and matches (no approval needed for an already-known
///     host).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live sshd (see module docs)"]
async fn known_hosts_tofu_workflow() {
    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");
    let secrets = || SshSecrets {
        password: Some(env("SENJU_SSH_PASSWORD")),
        passphrase: None,
    };

    // (a) unknown host, no approved fingerprint -> rejected with
    // UnknownHostKey.
    let real_fingerprint = {
        let sink = Arc::new(Capture::default());
        let mgr =
            SessionManager::with_known_hosts_path(sink, Some(known_hosts_path.clone()));
        let err = mgr
            .create_ssh(
                &test_host(SshAuthMethod::Password),
                secrets(),
                80,
                24,
                None,
            )
            .await
            .expect_err("unknown host must be rejected");
        let fingerprint = match err {
            SessionError::UnknownHostKey { key_type, fingerprint } => {
                assert!(!key_type.is_empty());
                assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
                fingerprint
            }
            other => panic!("expected UnknownHostKey, got: {other}"),
        };
        assert!(
            !known_hosts_path.exists(),
            "known_hosts must not be written when no fingerprint has been approved"
        );
        fingerprint
    };

    // (b) unknown host, approving the SAME fingerprint the server actually
    // presented -> connects and records the key.
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
                Some(real_fingerprint.clone()),
            )
            .await
            .expect("approving the real fingerprint should succeed");
        mgr.kill(&info.id);
        assert!(
            known_hosts_path.exists(),
            "a matching expected_fingerprint should have recorded the host key"
        );
        let contents = std::fs::read_to_string(&known_hosts_path).unwrap();
        assert!(
            contents.contains("ssh-"),
            "known_hosts should contain the recorded key: {contents}"
        );
    }

    // (c) now-known host, no expected_fingerprint supplied -> still connects
    // (key matches the one already recorded in known_hosts).
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
                None,
            )
            .await
            .expect("previously-learned host key should verify");
        mgr.kill(&info.id);
    }
}

/// The pre-save connection test (`test_ssh`) against a live sshd:
/// - first contact with `expected_fingerprint: None` fails with
///   `UnknownHostKey` and — crucially — does so even with a WRONG password,
///   proving no credential was sent to the unverified host (SEC review S1);
/// - after approving that fingerprint the test authenticates, reports the key
///   as not-yet-known, and never writes known_hosts (a test must not trust);
/// - with the approved fingerprint but a wrong password it fails at auth.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live sshd (see module docs)"]
async fn test_connection_probes_without_learning() {
    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");
    let host = test_host(SshAuthMethod::Password);
    let good = || SshSecrets {
        password: Some(env("SENJU_SSH_PASSWORD")),
        passphrase: None,
    };

    // First contact with a DELIBERATELY WRONG password must still fail with
    // UnknownHostKey (not an auth error) — the password is never sent because
    // the host key isn't verified yet.
    let sink = Arc::new(Capture::default());
    let mgr = SessionManager::with_known_hosts_path(sink, Some(known_hosts_path.clone()));
    let fingerprint = match mgr
        .test_ssh(
            &host,
            SshSecrets {
                password: Some("definitely-wrong".into()),
                passphrase: None,
            },
            None,
        )
        .await
        .expect_err("first contact must be rejected before auth")
    {
        SessionError::UnknownHostKey { fingerprint, .. } => fingerprint,
        other => panic!("expected UnknownHostKey (no auth), got: {other}"),
    };
    assert!(
        !known_hosts_path.exists(),
        "a connection test must never write known_hosts"
    );

    // Approving that fingerprint lets the test authenticate — over a verified
    // key — and still leaves known_hosts untouched.
    let report = mgr
        .test_ssh(&host, good(), Some(fingerprint.clone()))
        .await
        .expect("approving the real fingerprint should let the test succeed");
    assert!(report.fingerprint.starts_with("SHA256:"), "{}", report.fingerprint);
    assert!(!report.key_type.is_empty());
    assert!(
        !report.host_key_known,
        "against a fresh known_hosts the key should be reported as new"
    );
    assert!(
        !known_hosts_path.exists(),
        "a connection test must never write known_hosts"
    );

    // Approved fingerprint + wrong password -> now it fails at auth.
    let err = mgr
        .test_ssh(
            &host,
            SshSecrets {
                password: Some("definitely-wrong".into()),
                passphrase: None,
            },
            Some(fingerprint),
        )
        .await
        .expect_err("wrong password over a verified key should fail at auth");
    let msg = err.to_string();
    assert!(
        msg.contains("auth") || msg.contains("reject"),
        "unexpected error: {msg}"
    );
    assert!(
        !known_hosts_path.exists(),
        "a failed connection test must not write known_hosts either"
    );
}

/// Regression test for the TOFU TOCTOU vulnerability: the fingerprint the
/// user approved (from a first, failed handshake) must be checked against
/// the key presented on the retried handshake, not accepted unconditionally.
///
/// (a) `expected_fingerprint: None` against an unknown host -> rejected with
///     `UnknownHostKey`, nothing written.
/// (b) `expected_fingerprint: Some(<the real fingerprint>)` -> connects,
///     known_hosts is written.
/// (c) `expected_fingerprint: Some("SHA256:" + wrong value)` against a still
///     -unknown host -> **fails closed** with `FingerprintMismatch`, and
///     known_hosts is NOT written. This is the exact scenario a MITM would
///     trigger by presenting a different key on the second handshake than
///     the one relayed (and approved) on the first.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a live sshd (see module docs)"]
async fn toctou_fingerprint_mismatch_is_rejected_and_not_learned() {
    let secrets = || SshSecrets {
        password: Some(env("SENJU_SSH_PASSWORD")),
        passphrase: None,
    };

    // (a) learn the real fingerprint via the expected first-contact
    // rejection, against a fresh known_hosts file.
    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");
    let real_fingerprint = {
        let sink = Arc::new(Capture::default());
        let mgr =
            SessionManager::with_known_hosts_path(sink, Some(known_hosts_path.clone()));
        learn_unknown_host_fingerprint(&mgr, &test_host(SshAuthMethod::Password), secrets()).await
    };
    assert!(
        !known_hosts_path.exists(),
        "known_hosts must not be written by the rejected first attempt"
    );

    // (c) reconnect against the SAME still-unknown host, but claim the user
    // approved a bogus fingerprint (simulating a MITM swapping the key
    // between the first and second handshake, or simply a stale/incorrect
    // approval). Must fail closed and must NOT touch known_hosts.
    {
        let sink = Arc::new(Capture::default());
        let mgr =
            SessionManager::with_known_hosts_path(sink, Some(known_hosts_path.clone()));
        let bogus_fingerprint =
            "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_string();
        assert_ne!(bogus_fingerprint, real_fingerprint);
        let err = mgr
            .create_ssh(
                &test_host(SshAuthMethod::Password),
                secrets(),
                80,
                24,
                Some(bogus_fingerprint.clone()),
            )
            .await
            .expect_err("a fingerprint that doesn't match the presented key must be rejected");
        match err {
            SessionError::FingerprintMismatch {
                expected, actual, ..
            } => {
                assert_eq!(expected, bogus_fingerprint);
                assert_eq!(actual, real_fingerprint);
            }
            other => panic!("expected FingerprintMismatch, got: {other}"),
        }
        assert!(
            !known_hosts_path.exists(),
            "known_hosts must NOT be written when the presented key doesn't match \
             the fingerprint the user approved"
        );
    }

    // (b) now reconnect approving the REAL fingerprint -> succeeds, and
    // known_hosts is written this time.
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
                Some(real_fingerprint),
            )
            .await
            .expect("approving the real fingerprint should succeed");
        mgr.kill(&info.id);
        assert!(
            known_hosts_path.exists(),
            "approving the correct fingerprint should have recorded the host key"
        );
    }
}

/// Regression test: an SSH session must be killable from a thread that is NOT
/// inside a Tokio runtime — exactly what Tauri's window `Destroyed` handler
/// does at app close via `SessionManager::kill_all`. Before the fix,
/// `SshSession::kill` called `tokio::spawn`, which panics with "there is no
/// reactor running" when invoked outside a runtime, crashing the app on exit
/// whenever an SSH session was open. `kill` now spawns onto the runtime handle
/// captured at connect time, so it is safe from any thread.
///
/// The manager is built and the session opened inside a runtime; the kill is
/// then issued from a fresh `std::thread` with no runtime, and the test fails
/// (panics propagate through the join) if that thread panics.
#[test]
#[ignore = "needs a live sshd (see module docs)"]
fn kill_all_from_non_runtime_thread_does_not_panic() {
    let known_hosts_dir = tempfile::tempdir().unwrap();
    let known_hosts_path = known_hosts_dir.path().join("known_hosts");
    let runtime = tokio::runtime::Runtime::new().unwrap();

    let mgr = runtime.block_on(async {
        let sink = Arc::new(Capture::default());
        let mgr = SessionManager::with_known_hosts_path(sink, Some(known_hosts_path.clone()));
        let secrets = SshSecrets {
            password: Some(env("SENJU_SSH_PASSWORD")),
            passphrase: None,
        };
        let fingerprint =
            learn_unknown_host_fingerprint(&mgr, &test_host(SshAuthMethod::Password), secrets.clone())
                .await;
        mgr.create_ssh(
            &test_host(SshAuthMethod::Password),
            secrets,
            80,
            24,
            Some(fingerprint),
        )
        .await
        .expect("ssh connect");
        mgr
    });

    // Tear down from a plain OS thread — no runtime in scope — mirroring the
    // GUI thread on window close. This must not panic.
    std::thread::spawn(move || {
        mgr.kill_all();
    })
    .join()
    .expect("kill_all must not panic when called outside a Tokio runtime");

    // Give the spawned disconnect a moment to run before the runtime is
    // dropped, so the teardown path is actually exercised.
    runtime.block_on(async {
        tokio::time::sleep(Duration::from_millis(200)).await;
    });
}
