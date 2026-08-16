//! Minimal `~/.ssh/config` reader used by the "import from ssh config"
//! feature: it extracts concrete `Host` blocks as [`SshHost`] candidates for
//! the user to review in the UI — nothing is saved here.
//!
//! Deliberately NOT a full ssh_config implementation. Supported: `Host`
//! blocks with `HostName` / `User` / `Port` / `IdentityFile`, both
//! whitespace and `=` separators, quoted values, and `#` comments.
//! Skipped: wildcard host patterns (`*`, `?`, `!`), `Match` blocks, and
//! `Include` directives — those describe rules, not importable endpoints.

use crate::models::{SshAuthMethod, SshHost};

/// One `Host` block: its concrete aliases, and the `key`/`value` options
/// declared under it — both in appearance order.
type HostBlock = (Vec<String>, Vec<(String, String)>);

/// Parses ssh_config text into import candidates. Aliases without an
/// explicit `User` default to `default_user` (ssh itself defaults to the
/// local login name). A `Host` line with several aliases yields one
/// candidate per concrete alias.
pub fn parse_ssh_config(content: &str, default_user: &str) -> Vec<SshHost> {
    // Blocks in appearance order.
    let mut blocks: Vec<HostBlock> = Vec::new();
    let mut in_skipped_section = false; // inside a Match block

    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = split_directive(line) else {
            continue;
        };
        let key_lc = key.to_ascii_lowercase();
        match key_lc.as_str() {
            "match" => in_skipped_section = true,
            "host" => {
                in_skipped_section = false;
                let aliases: Vec<String> = value
                    .split_whitespace()
                    .filter(|a| !a.contains(['*', '?']) && !a.starts_with('!'))
                    .map(unquote)
                    .collect();
                // Wildcard-only Host blocks still open a block so their
                // options don't leak into the previous concrete block.
                blocks.push((aliases, Vec::new()));
            }
            _ if in_skipped_section => {}
            _ => {
                if let Some((_, opts)) = blocks.last_mut() {
                    opts.push((key_lc, unquote(value)));
                }
                // Options before any Host line (global defaults) are ignored:
                // applying them correctly would need full pattern matching.
            }
        }
    }

    let mut out = Vec::new();
    for (aliases, opts) in blocks {
        let get = |name: &str| opts.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        for alias in aliases {
            let host = get("hostname").unwrap_or_else(|| alias.clone());
            if host.is_empty() {
                continue;
            }
            let key_path = get("identityfile").unwrap_or_default();
            out.push(SshHost {
                id: String::new(),
                name: alias,
                host,
                port: get("port").and_then(|p| p.parse().ok()).unwrap_or(22),
                username: get("user").unwrap_or_else(|| default_user.to_string()),
                auth_method: if key_path.is_empty() {
                    SshAuthMethod::Password
                } else {
                    SshAuthMethod::Key
                },
                key_path,
                forwards: Vec::new(),
                jump_hosts: Vec::new(),
            });
        }
    }
    out
}

/// Splits "Key Value", "Key=Value" or "Key = Value" into (key, rest).
fn split_directive(line: &str) -> Option<(&str, &str)> {
    let sep = line.find(|c: char| c.is_whitespace() || c == '=')?;
    let key = &line[..sep];
    let rest = line[sep..].trim_start_matches(|c: char| c.is_whitespace() || c == '=');
    (!key.is_empty() && !rest.is_empty()).then_some((key, rest))
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(s)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_basic_block() {
        let hosts = parse_ssh_config(
            "Host web\n  HostName 10.0.0.5\n  User deploy\n  Port 2222\n",
            "local",
        );
        assert_eq!(hosts.len(), 1);
        let h = &hosts[0];
        assert_eq!((h.name.as_str(), h.host.as_str(), h.port, h.username.as_str()),
                   ("web", "10.0.0.5", 2222, "deploy"));
        assert_eq!(h.auth_method, SshAuthMethod::Password);
    }

    #[test]
    fn identityfile_selects_key_auth() {
        let hosts = parse_ssh_config(
            "Host box\n  HostName b.example.com\n  IdentityFile ~/.ssh/id_ed25519\n",
            "me",
        );
        assert_eq!(hosts[0].auth_method, SshAuthMethod::Key);
        assert_eq!(hosts[0].key_path, "~/.ssh/id_ed25519");
        // No User line: ssh defaults to the local login name; so do we.
        assert_eq!(hosts[0].username, "me");
    }

    #[test]
    fn multiple_aliases_yield_one_candidate_each() {
        let hosts = parse_ssh_config("Host a b\n  User u\n  HostName same.host\n", "x");
        assert_eq!(hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert!(hosts.iter().all(|h| h.host == "same.host"));
    }

    #[test]
    fn wildcards_and_match_blocks_are_skipped() {
        let hosts = parse_ssh_config(
            concat!(
                "Host *\n  User everyone\n",           // wildcard-only: no candidate
                "Host prod-*\n  Port 2200\n",          // pattern: no candidate
                "Match user deploy\n  Port 9\n",       // Match section ignored
                "Host real\n  HostName r.example.com\n",
            ),
            "x",
        );
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].name, "real");
        // Options from skipped sections must not leak into `real`.
        assert_eq!(hosts[0].port, 22);
        assert_eq!(hosts[0].username, "x");
    }

    #[test]
    fn equals_separator_and_quotes() {
        let hosts = parse_ssh_config(
            "Host q\n  HostName=q.example.com\n  IdentityFile = \"/path/with space/key\"\n",
            "x",
        );
        assert_eq!(hosts[0].host, "q.example.com");
        assert_eq!(hosts[0].key_path, "/path/with space/key");
    }

    #[test]
    fn alias_without_hostname_uses_the_alias() {
        let hosts = parse_ssh_config("Host bare.example.com\n  User u\n", "x");
        assert_eq!(hosts[0].host, "bare.example.com");
    }
}
