//! Local SSH port-forward specs (`ssh -L` style), parsed from the strings a
//! user saves on an SSH host. Kept free of russh types so parsing is unit
//! testable; the actual listener/channel plumbing lives in `ssh.rs`.

/// One `-L` local forward: listen on 127.0.0.1:`local_port`, tunnel each
/// connection to `remote_host`:`remote_port` through the SSH session.
/// The bind address is always loopback — never 0.0.0.0 — so a forward can't
/// accidentally expose the tunnel to the local network.
#[derive(Debug, Clone, PartialEq)]
pub struct ForwardSpec {
    pub local_port: u16,
    pub remote_host: String,
    pub remote_port: u16,
}

impl std::fmt::Display for ForwardSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}:{}", self.local_port, self.remote_host, self.remote_port)
    }
}

/// Parses one `local_port:remote_host:remote_port` line (the common
/// `ssh -L` shape). Whitespace is trimmed; empty lines are the caller's
/// concern (skip before calling).
pub fn parse_forward_spec(s: &str) -> Result<ForwardSpec, String> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return Err(format!(
            "\"{s}\": ローカルポート:リモートホスト:リモートポート の形式で指定してください"
        ));
    }
    let local_port: u16 = parts[0]
        .trim()
        .parse()
        .map_err(|_| format!("\"{s}\": ローカルポート「{}」が不正です", parts[0]))?;
    let remote_host = parts[1].trim();
    if remote_host.is_empty() {
        return Err(format!("\"{s}\": リモートホストが空です"));
    }
    let remote_port: u16 = parts[2]
        .trim()
        .parse()
        .map_err(|_| format!("\"{s}\": リモートポート「{}」が不正です", parts[2]))?;
    if local_port == 0 || remote_port == 0 {
        return Err(format!("\"{s}\": ポート 0 は指定できません"));
    }
    Ok(ForwardSpec {
        local_port,
        remote_host: remote_host.to_string(),
        remote_port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_spec() {
        assert_eq!(
            parse_forward_spec("8080:localhost:80").unwrap(),
            ForwardSpec { local_port: 8080, remote_host: "localhost".into(), remote_port: 80 }
        );
        // Whitespace around parts is tolerated (hand-typed lines).
        assert_eq!(
            parse_forward_spec("  5433 : db.internal : 5432 ").unwrap(),
            ForwardSpec { local_port: 5433, remote_host: "db.internal".into(), remote_port: 5432 }
        );
    }

    #[test]
    fn rejects_malformed_specs() {
        assert!(parse_forward_spec("8080:localhost").is_err()); // missing part
        assert!(parse_forward_spec("8080:localhost:80:extra").is_err()); // too many
        assert!(parse_forward_spec("abc:localhost:80").is_err()); // bad local port
        assert!(parse_forward_spec("8080::80").is_err()); // empty host
        assert!(parse_forward_spec("8080:host:99999").is_err()); // port out of range
        assert!(parse_forward_spec("0:host:80").is_err()); // port 0
    }

    #[test]
    fn display_roundtrips() {
        let spec = parse_forward_spec("8080:localhost:80").unwrap();
        assert_eq!(parse_forward_spec(&spec.to_string()).unwrap(), spec);
    }
}
