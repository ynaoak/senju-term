//! Parsing and filling of placeholders in workflow command templates:
//!
//! - `{{name}}` — free text, asked for at run time
//! - `{{name:default}}` — free text with a prefilled default
//! - `{{name|a,b,c}}` — a choice: the run dialog shows a dropdown with
//!   exactly these options (the first is preselected)
//! - `{{name:b|a,b,c}}` — a choice with an explicit default
//!
//! The options list is what makes a placeholder a choice; `:` still
//! introduces the default in both forms.
//!
//! Names starting with `$` are *built-ins* — `{{$cwd}}`, `{{$host}}`,
//! `{{$date}}`, `{{$clipboard}}` and so on. They are never prompted for: the
//! UI knows the answer (which thread is focused, what the clock says) and
//! supplies it in the values map at run time. This module only has to keep
//! them out of the prompt list and say which ones a template uses, so the
//! UI reads the clipboard, say, only when a template actually asks for it.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Placeholder {
    pub name: String,
    pub default: Option<String>,
    /// Fixed choices for a `{{name|a,b,c}}` placeholder. Empty for free
    /// text. When non-empty and `default` is `None`, the first option is the
    /// effective default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

impl Placeholder {
    /// The value used when the user supplies none: the explicit default,
    /// else the first option of a choice, else nothing.
    pub fn effective_default(&self) -> Option<&str> {
        self.default
            .as_deref()
            .or_else(|| self.options.first().map(String::as_str))
    }
}

/// A `$`-prefixed name: filled in by the app, never asked of the user.
pub fn is_builtin(name: &str) -> bool {
    name.starts_with('$')
}

/// The built-in names a template references, each once, in order.
pub fn builtin_names(command: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in scan(command) {
        if is_builtin(&p.name) && !out.contains(&p.name) {
            out.push(p.name);
        }
    }
    out
}

/// Extracts the placeholders to prompt for, in order of first appearance.
/// Built-ins are left out. Duplicate names are reported once; the first
/// occurrence that carries a default wins, and the first occurrence that
/// carries options wins.
pub fn extract_placeholders(command: &str) -> Vec<Placeholder> {
    let mut out: Vec<Placeholder> = Vec::new();
    for p in scan(command) {
        if is_builtin(&p.name) {
            continue;
        }
        match out.iter_mut().find(|e| e.name == p.name) {
            Some(existing) => {
                if existing.default.is_none() {
                    existing.default = p.default;
                }
                if existing.options.is_empty() {
                    existing.options = p.options;
                }
            }
            None => out.push(p),
        }
    }
    out
}

/// Replaces every placeholder with `values[name]`, falling back to the
/// placeholder's inline default (for a choice: its first option), then to
/// an empty string.
pub fn fill_placeholders(command: &str, values: &HashMap<String, String>) -> String {
    let mut result = String::with_capacity(command.len());
    let mut rest = command;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let p = split_inner(&after[..end]);
                result.push_str(&rest[..start]);
                let value = values
                    .get(&p.name)
                    .map(String::as_str)
                    .or_else(|| p.effective_default())
                    .unwrap_or_default();
                result.push_str(value);
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    result.push_str(rest);
    result
}

fn scan(command: &str) -> Vec<Placeholder> {
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

/// `name`, `name:default`, `name|a,b`, `name:b|a,b` → a Placeholder. The
/// options part is split off first so a default may never contain `|`, and
/// a `:` inside an option stays part of that option. Blank options are
/// dropped; a `|` with no usable options is treated as free text.
fn split_inner(inner: &str) -> Placeholder {
    let (head, opts) = match inner.split_once('|') {
        Some((head, opts)) => (head, Some(opts)),
        None => (inner, None),
    };
    let (name, default) = match head.split_once(':') {
        Some((name, default)) => (name.trim().to_string(), Some(default.trim().to_string())),
        None => (head.trim().to_string(), None),
    };
    let options: Vec<String> = opts
        .map(|o| {
            o.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    Placeholder { name, default, options }
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

    fn free(name: &str, default: Option<&str>) -> Placeholder {
        Placeholder {
            name: name.into(),
            default: default.map(String::from),
            options: vec![],
        }
    }

    #[test]
    fn extracts_names_and_defaults() {
        let ps = extract_placeholders("git log -n {{count:20}} --author {{author}}");
        assert_eq!(ps, vec![free("count", Some("20")), free("author", None)]);
    }

    #[test]
    fn deduplicates_and_keeps_first_default() {
        let ps = extract_placeholders("{{x}} {{x:1}} {{x:2}}");
        assert_eq!(ps, vec![free("x", Some("1"))]);
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
    fn choice_placeholders_parse_options_and_defaults() {
        let ps = extract_placeholders("deploy --env {{env|dev, staging,prod}} --region {{region:eu|us,eu}}");
        assert_eq!(
            ps,
            vec![
                Placeholder {
                    name: "env".into(),
                    default: None,
                    options: vec!["dev".into(), "staging".into(), "prod".into()],
                },
                Placeholder {
                    name: "region".into(),
                    default: Some("eu".into()),
                    options: vec!["us".into(), "eu".into()],
                },
            ]
        );
        assert_eq!(ps[0].effective_default(), Some("dev"));
        assert_eq!(ps[1].effective_default(), Some("eu"));
    }

    #[test]
    fn choice_fill_uses_value_then_default_then_first_option() {
        let cmd = "{{env|dev,prod}} {{region:eu|us,eu}} {{x:1|}}";
        assert_eq!(fill_placeholders(cmd, &values(&[("env", "prod")])), "prod eu 1");
        assert_eq!(fill_placeholders(cmd, &HashMap::new()), "dev eu 1");
    }

    #[test]
    fn choice_options_may_contain_colons_and_blank_entries_are_dropped() {
        let ps = extract_placeholders("{{url|http://a,,http://b, }}");
        assert_eq!(ps[0].options, vec!["http://a".to_string(), "http://b".to_string()]);
        assert_eq!(ps[0].default, None);
        // A bare `|` degrades to free text rather than an empty dropdown.
        let ps = extract_placeholders("{{name|}}");
        assert!(ps[0].options.is_empty());
    }

    #[test]
    fn choice_merge_keeps_first_options_and_default() {
        let ps = extract_placeholders("{{e}} {{e|a,b}} {{e:b|x,y}}");
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].options, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(ps[0].default, Some("b".into()));
    }

    #[test]
    fn no_placeholders_is_identity() {
        let cmd = "ls -la";
        assert!(extract_placeholders(cmd).is_empty());
        assert_eq!(fill_placeholders(cmd, &HashMap::new()), cmd);
    }

    #[test]
    fn builtins_are_not_prompted_but_are_filled() {
        let t = "scp {{file}} {{$user}}@{{$host}}:{{$cwd}}/backup-{{$date}}.tgz";
        let prompts: Vec<_> = extract_placeholders(t).into_iter().map(|p| p.name).collect();
        assert_eq!(prompts, vec!["file".to_string()]);
        assert_eq!(builtin_names(t), vec!["$user", "$host", "$cwd", "$date"]);
        let mut v = HashMap::new();
        for (k, val) in [("file", "db.sql"), ("$user", "deploy"), ("$host", "web-01"), ("$cwd", "/srv"), ("$date", "2026-10-02")] {
            v.insert(k.to_string(), val.to_string());
        }
        assert_eq!(fill_placeholders(t, &v), "scp db.sql deploy@web-01:/srv/backup-2026-10-02.tgz");
    }

    #[test]
    fn builtin_names_are_unique_and_unknown_ones_fill_empty() {
        assert_eq!(builtin_names("{{$date}} {{$date}} {{x}}"), vec!["$date"]);
        assert!(builtin_names("echo {{x:1}}").is_empty());
        // A built-in the UI does not know stays empty rather than literal:
        // the same rule as any other missing value.
        assert_eq!(fill_placeholders("a{{$nope}}b", &HashMap::new()), "ab");
        // A default on a built-in still applies when the UI has no value.
        assert_eq!(fill_placeholders("{{$cwd:~}}", &HashMap::new()), "~");
    }

    #[test]
    fn builtins_with_options_are_still_builtins() {
        // A choice on a built-in makes no sense, but it must not turn the
        // built-in into a prompt either.
        assert!(extract_placeholders("{{$host|a,b}}").is_empty());
        assert_eq!(builtin_names("{{$host|a,b}}"), vec!["$host"]);
    }
}
