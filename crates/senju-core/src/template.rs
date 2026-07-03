//! Parsing and filling of `{{name}}` / `{{name:default}}` placeholders in
//! workflow command templates.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Placeholder {
    pub name: String,
    pub default: Option<String>,
}

/// Extracts placeholders in order of first appearance. Duplicate names are
/// reported once; the first occurrence that carries a default wins.
pub fn extract_placeholders(command: &str) -> Vec<Placeholder> {
    let mut out: Vec<Placeholder> = Vec::new();
    for (name, default) in scan(command) {
        match out.iter_mut().find(|p| p.name == name) {
            Some(existing) => {
                if existing.default.is_none() {
                    existing.default = default;
                }
            }
            None => out.push(Placeholder { name, default }),
        }
    }
    out
}

/// Replaces every placeholder with `values[name]`, falling back to the
/// placeholder's inline default, then to an empty string.
pub fn fill_placeholders(command: &str, values: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(command.len());
    let mut rest = command;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let inner = &after[..end];
                let (name, default) = split_inner(inner);
                result.push_str(&rest[..start]);
                let value = values
                    .get(&name)
                    .cloned()
                    .or(default)
                    .unwrap_or_default();
                result.push_str(&value);
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    result.push_str(rest);
    result
}

fn scan(command: &str) -> Vec<(String, Option<String>)> {
    let mut found = Vec::new();
    let mut rest = command;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                found.push(split_inner(&after[..end]));
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    found
}

fn split_inner(inner: &str) -> (String, Option<String>) {
    match inner.split_once(':') {
        Some((name, default)) => (name.trim().to_string(), Some(default.trim().to_string())),
        None => (inner.trim().to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn extracts_names_and_defaults() {
        let ps = extract_placeholders("git log -n {{count:20}} --author {{author}}");
        assert_eq!(
            ps,
            vec![
                Placeholder {
                    name: "count".into(),
                    default: Some("20".into())
                },
                Placeholder {
                    name: "author".into(),
                    default: None
                },
            ]
        );
    }

    #[test]
    fn deduplicates_and_keeps_first_default() {
        let ps = extract_placeholders("{{x}} {{x:1}} {{x:2}}");
        assert_eq!(
            ps,
            vec![Placeholder {
                name: "x".into(),
                default: Some("1".into())
            }]
        );
    }

    #[test]
    fn fills_values_defaults_and_missing() {
        let cmd = "du -ah {{path:.}} | head -n {{count}} # {{missing}}";
        let filled = fill_placeholders(cmd, &values(&[("count", "5")]));
        assert_eq!(filled, "du -ah . | head -n 5 # ");
    }

    #[test]
    fn ignores_unclosed_braces() {
        assert!(extract_placeholders("echo {{oops").is_empty());
        assert_eq!(fill_placeholders("echo {{oops", &HashMap::new()), "echo {{oops");
    }

    #[test]
    fn no_placeholders_is_identity() {
        let cmd = "ls -la";
        assert!(extract_placeholders(cmd).is_empty());
        assert_eq!(fill_placeholders(cmd, &HashMap::new()), cmd);
    }
}
