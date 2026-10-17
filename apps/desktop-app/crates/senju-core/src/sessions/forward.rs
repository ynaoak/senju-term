//! SSH port-forward specs parsed from the strings a user saves on an SSH
//! host: local (`ssh -L`), remote (`ssh -R`) and dynamic/SOCKS (`ssh -D`).
//! Kept free of russh types so parsing is unit testable; the actual
//! listener/channel plumbing lives in `ssh.rs`.
//!
//! Every listener described here binds loopback, on whichever side of the
//! tunnel it lives:
//!
//! - `-L` and `-D` listen on `127.0.0.1` locally,
//! - `-R` asks the server to listen on **its** `127.0.0.1`, matching
//!   OpenSSH's default (`GatewayPorts no`).
//!
//! That is deliberate and not configurable: a forward bound to `0.0.0.0`
//! hands everyone on that network an unauthenticated hole through the
//! tunnel, and whoever asked for the forward is rarely the one who notices.

/// One forward. The three variants mirror `ssh -L` / `-R` / `-D`.
#[derive(Debug, Clone, PartialEq)]
pub enum ForwardSpec {
    /// `-L`: listen on 127.0.0.1:`local_port`, tunnel each connection to
    /// `remote_host`:`remote_port` through the SSH session.
    Local {
        local_port: u16,
        remote_host: String,
        remote_port: u16,
    },
    /// `-R`: ask the server to listen on its own 127.0.0.1:`remote_port` and
    /// forward what arrives there to `local_host`:`local_port` on this
    /// machine.
    Remote {
        remote_port: u16,
        local_host: String,
        local_port: u16,
    },
    /// `-D`: listen on 127.0.0.1:`local_port` as a SOCKS5 proxy; each CONNECT
    /// becomes its own tunnel to whatever the client asked for.
    Dynamic { local_port: u16 },
}

impl ForwardSpec {
    /// The port this forward occupies on **this** machine, if any. `-R` takes
    /// none — its listener lives on the server — so local port-collision
    /// checks must skip it rather than compare the wrong number.
    pub fn local_bind_port(&self) -> Option<u16> {
        match self {
            ForwardSpec::Local { local_port, .. } | ForwardSpec::Dynamic { local_port } => {
                Some(*local_port)
            }
            ForwardSpec::Remote { .. } => None,
        }
    }
}

impl std::fmt::Display for ForwardSpec {
    /// Round-trips through [`parse_forward_spec`]. `-L` prints bare rather
    /// than prefixed, so lines saved before the other kinds existed keep
    /// their exact text.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ForwardSpec::Local {
                local_port,
                remote_host,
                remote_port,
            } => write!(f, "{local_port}:{remote_host}:{remote_port}"),
            ForwardSpec::Remote {
                remote_port,
                local_host,
                local_port,
            } => write!(f, "-R {remote_port}:{local_host}:{local_port}"),
            ForwardSpec::Dynamic { local_port } => write!(f, "-D {local_port}"),
        }
    }
}

/// Splits an optional leading `-L` / `-R` / `-D` (or a bare `L` / `R` / `D`)
/// off the line, returning the kind letter and the rest.
///
/// A line with no prefix is `-L`: that is the only shape this field held
/// before the other two kinds existed, and saved hosts are full of them.
/// Anything that is not exactly one of the three letters is left alone, so a
/// typo fails as a malformed `-L` line instead of being silently accepted as
/// a fourth kind of forward.
fn split_kind(s: &str) -> (char, &str) {
    let Some((head, rest)) = s.split_once(char::is_whitespace) else {
        return ('L', s);
    };
    let letter = head.strip_prefix('-').unwrap_or(head);
    let mut chars = letter.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if matches!(c.to_ascii_uppercase(), 'L' | 'R' | 'D') => {
            (c.to_ascii_uppercase(), rest.trim_start())
        }
        _ => ('L', s),
    }
}

fn port(value: &str, line: &str, label: &str) -> Result<u16, String> {
    let n: u16 = value
        .trim()
        .parse()
        .map_err(|_| format!("\"{line}\": {label}「{}」が不正です", value.trim()))?;
    if n == 0 {
        return Err(format!("\"{line}\": ポート 0 は指定できません"));
    }
    Ok(n)
}

/// Parses one saved forward line.
///
/// Accepted shapes (the `-L` / `-R` / `-D` prefix may also be written without
/// the dash, and is case-insensitive):
///
/// ```text
///   8080:localhost:80        local forward  (no prefix — the original shape)
///   -L 8080:localhost:80     the same, written explicitly
///   -R 9000:localhost:3000   remote forward
///   -D 1080                  dynamic (SOCKS5) forward
/// ```
///
/// Whitespace is trimmed; empty lines are the caller's concern (skip before
/// calling).
pub fn parse_forward_spec(s: &str) -> Result<ForwardSpec, String> {
    let line = s.trim();
    let (kind, body) = split_kind(line);
    let body = body.trim();

    if kind == 'D' {
        // A bare port. Anything with a colon is a three-part spec that was
        // given the wrong prefix, and saying so beats "port is invalid".
        if body.contains(':') {
            return Err(format!(
                "\"{line}\": 動的フォワード (-D) はローカルポートのみを指定します(例: -D 1080)"
            ));
        }
        return Ok(ForwardSpec::Dynamic {
            local_port: port(body, line, "ローカルポート")?,
        });
    }

    let parts: Vec<&str> = body.split(':').collect();
    if parts.len() != 3 {
        return Err(match kind {
            'R' => format!(
                "\"{line}\": リモートポート:ローカルホスト:ローカルポート の形式で指定してください"
            ),
            _ => format!(
                "\"{line}\": ローカルポート:リモートホスト:リモートポート の形式で指定してください"
            ),
        });
    }
    let host = parts[1].trim();
    if host.is_empty() {
        return Err(format!(
            "\"{line}\": {}が空です",
            if kind == 'R' { "ローカルホスト" } else { "リモートホスト" }
        ));
    }

    if kind == 'R' {
        Ok(ForwardSpec::Remote {
            remote_port: port(parts[0], line, "リモートポート")?,
            local_host: host.to_string(),
            local_port: port(parts[2], line, "ローカルポート")?,
        })
    } else {
        Ok(ForwardSpec::Local {
            local_port: port(parts[0], line, "ローカルポート")?,
            remote_host: host.to_string(),
            remote_port: port(parts[2], line, "リモートポート")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(p: u16, h: &str, rp: u16) -> ForwardSpec {
        ForwardSpec::Local { local_port: p, remote_host: h.into(), remote_port: rp }
    }

    #[test]
    fn bare_lines_stay_local_forwards() {
        // The only shape this field held before -R/-D existed. Saved hosts
        // are full of these, so they must keep parsing untouched.
        assert_eq!(parse_forward_spec("8080:localhost:80").unwrap(), local(8080, "localhost", 80));
        // Whitespace around parts is tolerated (hand-typed lines).
        assert_eq!(
            parse_forward_spec("  5433 : db.internal : 5432 ").unwrap(),
            local(5433, "db.internal", 5432)
        );
    }

    #[test]
    fn parses_each_kind_with_and_without_the_dash() {
        for line in ["-L 8080:localhost:80", "L 8080:localhost:80", "l  8080:localhost:80"] {
            assert_eq!(parse_forward_spec(line).unwrap(), local(8080, "localhost", 80), "{line}");
        }
        for line in ["-R 9000:localhost:3000", "R 9000:localhost:3000"] {
            assert_eq!(
                parse_forward_spec(line).unwrap(),
                ForwardSpec::Remote {
                    remote_port: 9000,
                    local_host: "localhost".into(),
                    local_port: 3000,
                },
                "{line}"
            );
        }
        for line in ["-D 1080", "D 1080", "-d 1080"] {
            assert_eq!(
                parse_forward_spec(line).unwrap(),
                ForwardSpec::Dynamic { local_port: 1080 },
                "{line}"
            );
        }
    }

    #[test]
    fn remote_reads_its_ports_in_ssh_order() {
        // -R is the mirror of -L: the FIRST port is the one the server binds,
        // the last is ours. Reversing them silently tunnels the wrong way and
        // looks like a server problem, so pin the order.
        let spec = parse_forward_spec("-R 9000:127.0.0.1:3000").unwrap();
        assert_eq!(
            spec,
            ForwardSpec::Remote {
                remote_port: 9000,
                local_host: "127.0.0.1".into(),
                local_port: 3000,
            }
        );
        // ...and it occupies no port on this machine, unlike the other two.
        assert_eq!(spec.local_bind_port(), None);
        assert_eq!(parse_forward_spec("-L 8080:h:80").unwrap().local_bind_port(), Some(8080));
        assert_eq!(parse_forward_spec("-D 1080").unwrap().local_bind_port(), Some(1080));
    }

    #[test]
    fn rejects_malformed_specs() {
        assert!(parse_forward_spec("8080:localhost").is_err()); // missing part
        assert!(parse_forward_spec("8080:localhost:80:extra").is_err()); // too many
        assert!(parse_forward_spec("abc:localhost:80").is_err()); // bad local port
        assert!(parse_forward_spec("8080::80").is_err()); // empty host
        assert!(parse_forward_spec("8080:host:99999").is_err()); // port out of range
        assert!(parse_forward_spec("0:host:80").is_err()); // port 0
        assert!(parse_forward_spec("-R 9000:localhost").is_err()); // -R needs three parts
        assert!(parse_forward_spec("-R 9000:localhost:0").is_err()); // port 0
        assert!(parse_forward_spec("-D").is_err()); // -D needs a port
        assert!(parse_forward_spec("-D 0").is_err()); // port 0
        assert!(parse_forward_spec("-D 1080:localhost:80").is_err()); // -D takes only a port
    }

    #[test]
    fn an_unknown_leading_word_is_not_swallowed_as_a_kind() {
        // These must fail as malformed -L lines rather than quietly becoming
        // some fourth kind of forward.
        assert!(parse_forward_spec("X 8080:localhost:80").is_err());
        assert!(parse_forward_spec("-X 8080:localhost:80").is_err());
        assert!(parse_forward_spec("LR 8080:localhost:80").is_err());
    }

    #[test]
    fn display_roundtrips_every_kind() {
        for line in ["8080:localhost:80", "-R 9000:localhost:3000", "-D 1080"] {
            let spec = parse_forward_spec(line).unwrap();
            assert_eq!(spec.to_string(), line, "display of {line}");
            assert_eq!(parse_forward_spec(&spec.to_string()).unwrap(), spec);
        }
    }
}
