//! Detects the task runners configured in a directory and turns them into a
//! flat list of runnable commands.
//!
//! The point is the moment you `cd` into an unfamiliar repository. The commands
//! that actually build and test it are sitting right there in `package.json` or
//! `Cargo.toml`, but reading them means stopping, opening the file, and
//! translating a script name into whatever invocation its package manager
//! wants. This module does that translation once so the UI can offer the
//! result as buttons.
//!
//! Two rules shape the parsers.
//!
//! **A wrong command is worse than a missing one.** Every task here is emitted
//! only when the file says so plainly; nothing is guessed from a directory
//! listing alone. A button that runs the wrong thing in someone's repository is
//! a much bigger problem than a task the user types by hand.
//!
//! **Detection picks the package manager the repository already uses.** Running
//! `npm run build` in a pnpm workspace can rewrite the lockfile and produce a
//! different dependency tree, so the lockfile — not a default — decides.

use std::path::Path;

use serde::Serialize;

/// Largest file this module will read. A `Makefile` that big is generated, and
/// parsing it would stall the UI for no benefit.
const MAX_FILE: u64 = 1024 * 1024;

/// One runnable command.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Task {
    /// Short label — the script/target/recipe name as written in the file.
    pub name: String,
    /// The exact shell command to run. Complete as-is; the caller does not
    /// prepend or append anything.
    pub command: String,
    /// What the task actually does, when the file says: the script body, a
    /// `## comment` on a make target. Empty when there is nothing useful to
    /// show — the UI omits the line rather than repeating the name.
    pub detail: String,
}

/// The tasks found in one file.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskGroup {
    /// File name the tasks came from, for display (`package.json`).
    pub file: String,
    /// The tool that runs them (`pnpm`, `cargo`, `make`). Shown as the group's
    /// badge so a repository with several runners stays readable.
    pub runner: String,
    pub tasks: Vec<Task>,
}

fn task(name: &str, command: String, detail: &str) -> Task {
    Task { name: name.to_string(), command, detail: detail.to_string() }
}

/// Reads a file, or `None` if it is absent, unreadable, oversized or not UTF-8.
/// Every caller treats all of those the same way — the file contributes no
/// tasks — so they are not worth distinguishing.
fn read(dir: &Path, name: &str) -> Option<String> {
    let path = dir.join(name);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE {
        return None;
    }
    std::fs::read_to_string(&path).ok()
}

fn exists(dir: &Path, name: &str) -> bool {
    dir.join(name).exists()
}

/// Finds the first of `names` that exists, so callers can express the
/// case-variants a format allows (`Makefile` / `makefile` / `GNUmakefile`).
fn first_present<'a>(dir: &Path, names: &[&'a str]) -> Option<(&'a str, String)> {
    names.iter().find_map(|n| read(dir, n).map(|body| (*n, body)))
}

/// Every task in `dir`, grouped by the file that declared it.
///
/// Groups come back in a fixed order rather than filesystem order so the panel
/// does not reshuffle itself between directories.
pub fn scan(dir: &Path) -> Vec<TaskGroup> {
    let mut out = Vec::new();
    out.extend(node(dir));
    out.extend(deno(dir));
    out.extend(cargo(dir));
    out.extend(python(dir));
    out.extend(go(dir));
    out.extend(composer(dir));
    out.extend(make(dir));
    out.extend(just(dir));
    out.extend(taskfile(dir));
    out.extend(compose(dir));
    out.extend(dockerfile(dir));
    out.retain(|g| !g.tasks.is_empty());
    out
}

/* ---------------- Node ---------------- */

/// Picks the package manager this project is already using.
///
/// `packageManager` (Corepack) is authoritative when present — the project
/// pinned it deliberately. Otherwise the lockfile decides: running the wrong
/// manager regenerates the lockfile and can resolve a different dependency
/// tree, which is a real way to break someone's checkout by pressing a button.
fn node_runner(dir: &Path, pkg: &serde_json::Value) -> &'static str {
    if let Some(pm) = pkg.get("packageManager").and_then(|v| v.as_str()) {
        let name = pm.split('@').next().unwrap_or("");
        match name {
            "pnpm" => return "pnpm",
            "yarn" => return "yarn",
            "bun" => return "bun",
            "npm" => return "npm",
            _ => {}
        }
    }
    for (lock, pm) in [
        ("bun.lock", "bun"),
        ("bun.lockb", "bun"),
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("package-lock.json", "npm"),
    ] {
        if exists(dir, lock) {
            return pm;
        }
    }
    "npm"
}

fn node(dir: &Path) -> Option<TaskGroup> {
    let body = read(dir, "package.json")?;
    let pkg: serde_json::Value = serde_json::from_str(&body).ok()?;
    let runner = node_runner(dir, &pkg);
    // serde_json's default map is sorted, so the buttons come out alphabetically
    // rather than in the order the author wrote them. Source order would read
    // better (dev, build, test), but recovering it means either a global
    // `preserve_order` feature — which would also reorder every settings file
    // this crate writes — or re-scanning the raw text. Alphabetical is at least
    // stable, which is what a list of buttons actually needs.
    let scripts = pkg.get("scripts")?.as_object()?;
    let tasks = scripts
        .iter()
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, body)| {
            task(
                name,
                format!("{runner} run {name}"),
                body.as_str().unwrap_or_default(),
            )
        })
        .collect();
    Some(TaskGroup { file: "package.json".into(), runner: runner.into(), tasks })
}

fn deno(dir: &Path) -> Option<TaskGroup> {
    let (file, body) = first_present(dir, &["deno.json", "deno.jsonc"])?;
    // `.jsonc` may carry comments that serde_json rejects. Rather than write a
    // comment stripper that has to know when it is inside a string, we let the
    // parse fail and contribute nothing — a wrong strip could silently corrupt
    // a task command, which is worse than the task not appearing.
    let cfg: serde_json::Value = serde_json::from_str(&body).ok()?;
    let tasks = cfg.get("tasks")?.as_object()?;
    let tasks = tasks
        .iter()
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, body)| {
            // A task may be a plain string or `{ "command": "…" }`.
            let detail = body
                .as_str()
                .or_else(|| body.get("command").and_then(|c| c.as_str()))
                .unwrap_or_default();
            task(name, format!("deno task {name}"), detail)
        })
        .collect();
    Some(TaskGroup { file: file.into(), runner: "deno".into(), tasks })
}

/* ---------------- Rust ---------------- */

fn cargo(dir: &Path) -> Option<TaskGroup> {
    let body = read(dir, "Cargo.toml")?;
    let manifest: toml::Value = toml::from_str(&body).ok()?;
    let is_workspace = manifest.get("workspace").is_some();
    let has_package = manifest.get("package").is_some();

    // A virtual manifest (workspace, no package) has nothing to build on its
    // own, so every command needs `--workspace` to mean anything.
    let scope = if is_workspace && !has_package { " --workspace" } else { "" };

    let mut tasks = vec![
        task("build", format!("cargo build{scope}"), ""),
        task("test", format!("cargo test{scope}"), ""),
        task("check", format!("cargo check{scope}"), ""),
        task("clippy", format!("cargo clippy{scope} --all-targets"), ""),
        task("fmt", "cargo fmt --all".into(), ""),
    ];

    let bins = array_names(&manifest, "bin");
    // `cargo run` is only offered when something is actually runnable.
    // Suggesting it for a library crate produces an error, not a build.
    if has_package && (!bins.is_empty() || dir.join("src/main.rs").is_file()) {
        tasks.push(task("run", "cargo run".into(), ""));
    }
    // Naming the target only matters once there is a choice to make; with a
    // single bin, `cargo run` above already covers it.
    if bins.len() > 1 {
        for b in &bins {
            tasks.push(task(b, format!("cargo run --bin {b}"), "bin"));
        }
    }
    for e in array_names(&manifest, "example") {
        tasks.push(task(&e, format!("cargo run --example {e}"), "example"));
    }
    Some(TaskGroup { file: "Cargo.toml".into(), runner: "cargo".into(), tasks })
}

/// `name` of every entry in an array-of-tables such as `[[bin]]`.
fn array_names(manifest: &toml::Value, key: &str) -> Vec<String> {
    manifest
        .get(key)
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .filter_map(|e| e.get("name")?.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/* ---------------- Python ---------------- */

fn python(dir: &Path) -> Option<TaskGroup> {
    let body = read(dir, "pyproject.toml")?;
    let cfg: toml::Value = toml::from_str(&body).ok()?;
    let tool = cfg.get("tool");
    let poetry = tool.and_then(|t| t.get("poetry"));
    let pdm = tool.and_then(|t| t.get("pdm"));

    // The runner decides how a console script is invoked, so it has to be
    // settled before any task is emitted.
    let runner = if exists(dir, "uv.lock") || tool.and_then(|t| t.get("uv")).is_some() {
        "uv"
    } else if poetry.is_some() {
        "poetry"
    } else if pdm.is_some() {
        "pdm"
    } else {
        return None; // a bare pyproject says nothing about how to run anything
    };

    let mut tasks = Vec::new();
    match runner {
        "uv" => tasks.push(task("sync", "uv sync".into(), "")),
        "poetry" => tasks.push(task("install", "poetry install".into(), "")),
        _ => tasks.push(task("install", "pdm install".into(), "")),
    }
    // pdm's own script table is the closest thing Python has to npm scripts.
    if let Some(scripts) = pdm.and_then(|p| p.get("scripts")).and_then(|s| s.as_table()) {
        for (name, body) in scripts {
            tasks.push(task(name, format!("pdm run {name}"), detail_of(body)));
        }
    }
    // Console entry points: installed into the environment, so they run through
    // the resolved runner rather than directly.
    let entry_points = cfg
        .get("project")
        .and_then(|p| p.get("scripts"))
        .and_then(|s| s.as_table())
        .into_iter()
        .chain(poetry.and_then(|p| p.get("scripts")).and_then(|s| s.as_table()));
    for table in entry_points {
        for (name, body) in table {
            tasks.push(task(name, format!("{runner} run {name}"), detail_of(body)));
        }
    }
    Some(TaskGroup { file: "pyproject.toml".into(), runner: runner.into(), tasks })
}

/// Best-effort one-line summary of a TOML value, for the task subtitle.
fn detail_of(v: &toml::Value) -> &str {
    v.as_str()
        .or_else(|| v.get("cmd").and_then(|c| c.as_str()))
        .or_else(|| v.get("call").and_then(|c| c.as_str()))
        .unwrap_or_default()
}

/* ---------------- Go ---------------- */

fn go(dir: &Path) -> Option<TaskGroup> {
    read(dir, "go.mod")?;
    Some(TaskGroup {
        file: "go.mod".into(),
        runner: "go".into(),
        tasks: vec![
            task("build", "go build ./...".into(), ""),
            task("test", "go test ./...".into(), ""),
            task("run", "go run .".into(), ""),
            task("tidy", "go mod tidy".into(), ""),
        ],
    })
}

/* ---------------- PHP ---------------- */

fn composer(dir: &Path) -> Option<TaskGroup> {
    let body = read(dir, "composer.json")?;
    let cfg: serde_json::Value = serde_json::from_str(&body).ok()?;
    let scripts = cfg.get("scripts")?.as_object()?;
    let tasks = scripts
        .iter()
        .filter(|(name, _)| !name.is_empty())
        .map(|(name, body)| {
            // A composer script is a string or an array of strings.
            let detail = body
                .as_str()
                .map(str::to_string)
                .or_else(|| {
                    body.as_array()
                        .map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join(" && "))
                })
                .unwrap_or_default();
            task(name, format!("composer run {name}"), &detail)
        })
        .collect();
    Some(TaskGroup { file: "composer.json".into(), runner: "composer".into(), tasks })
}

/* ---------------- make ---------------- */

fn make(dir: &Path) -> Option<TaskGroup> {
    let (file, body) = first_present(dir, &["Makefile", "makefile", "GNUmakefile"])?;
    let mut tasks: Vec<Task> = Vec::new();
    for line in body.lines() {
        let Some(name) = make_target(line) else { continue };
        // The `target: deps ## description` convention is widespread enough
        // that picking it up costs one split and makes the list self-describing.
        let detail = line.split_once("##").map(|(_, c)| c.trim()).unwrap_or_default();
        if tasks.iter().any(|t| t.name == name) {
            continue; // a target may be declared more than once
        }
        tasks.push(task(name, format!("make {name}"), detail));
    }
    Some(TaskGroup { file: file.into(), runner: "make".into(), tasks })
}

/// The target name on a makefile rule line, or `None` if the line is not one.
///
/// Deliberately conservative: a false positive becomes a button that fails with
/// "No rule to make target", so anything ambiguous is skipped.
fn make_target(line: &str) -> Option<&str> {
    // Recipe bodies are tab-indented, and an indented line is never a rule.
    if line.starts_with('\t') || line.starts_with(' ') || line.starts_with('#') {
        return None;
    }
    let (head, rest) = line.split_once(':')?;
    // `VAR := value` and `VAR ::= value` are assignments, not rules.
    if rest.starts_with('=') || head.contains('=') {
        return None;
    }
    let name = head.trim();
    // `.PHONY`, `.DEFAULT_GOAL` and friends are directives; `%` and `$` mean a
    // pattern or variable rule that cannot be run by name.
    if name.is_empty()
        || name.starts_with('.')
        || name.contains(['%', '$', ' ', '\t', '(', ')'])
    {
        return None;
    }
    Some(name)
}

/* ---------------- just ---------------- */

fn just(dir: &Path) -> Option<TaskGroup> {
    let (file, body) = first_present(dir, &["justfile", "Justfile", ".justfile"])?;
    let mut tasks: Vec<Task> = Vec::new();
    // A `#` line immediately above a recipe is just's documentation comment.
    let mut doc = String::new();
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix('#') {
            doc = rest.trim().to_string();
            continue;
        }
        let Some(name) = just_recipe(line) else {
            if !line.starts_with([' ', '\t']) {
                doc.clear(); // the comment did not belong to a recipe
            }
            continue;
        };
        if !tasks.iter().any(|t| t.name == name) {
            tasks.push(task(name, format!("just {name}"), &doc));
        }
        doc.clear();
    }
    Some(TaskGroup { file: file.into(), runner: "just".into(), tasks })
}

/// The recipe name on a justfile line, or `None`.
fn just_recipe(line: &str) -> Option<&str> {
    if line.starts_with([' ', '\t']) || line.starts_with('@') {
        return None;
    }
    // `name := value` and `alias b := build` are assignments. This has to be
    // tested against the whole line rather than the text before the first
    // colon, because a recipe's parameter default may itself contain `=`
    // (`build target="debug":`) and rejecting on that would drop real recipes.
    if line.contains(":=") {
        return None;
    }
    let (head, _) = line.split_once(':')?;
    // A recipe may declare parameters: `build target="debug":`. The name is the
    // first word; the rest is the signature and is not part of it.
    let name = head.split_whitespace().next()?;
    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return None;
    }
    Some(name)
}

/* ---------------- Task (go-task) ---------------- */

/// Immediate keys of a top-level YAML mapping (`tasks:`, `services:`).
///
/// Not a YAML parser: it reads the two levels of structure these formats fix by
/// convention and ignores everything else. A real parser would be a dependency
/// and a whole class of edge cases in exchange for a list of names. Anything it
/// cannot read plainly contributes nothing, which is the failure mode this
/// module wants (see the module docs).
fn yaml_section_keys(body: &str, section: &str) -> Vec<String> {
    let header = format!("{section}:");
    let mut keys: Vec<String> = Vec::new();
    let mut inside = false;
    let mut indent = None::<usize>;
    for line in body.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        let depth = line.len() - line.trim_start().len();
        if depth == 0 {
            inside = line.trim_end() == header;
            indent = None;
            continue;
        }
        if !inside {
            continue;
        }
        // The first indented line fixes the key depth; anything deeper belongs
        // to an entry's body, not to the list of names.
        let key_depth = *indent.get_or_insert(depth);
        if depth != key_depth {
            continue;
        }
        let Some((name, _)) = line.trim().split_once(':') else { continue };
        let name = name.trim().trim_matches(['"', '\'']);
        // A `-` prefix means a sequence item: this mapping is not the shape
        // being looked for, so its entries are not names.
        if name.is_empty() || name.starts_with('-') || keys.iter().any(|k| k == name) {
            continue;
        }
        keys.push(name.to_string());
    }
    keys
}

fn taskfile(dir: &Path) -> Option<TaskGroup> {
    let (file, body) = first_present(
        dir,
        &["Taskfile.yml", "Taskfile.yaml", "taskfile.yml", "taskfile.yaml"],
    )?;
    let tasks = yaml_section_keys(&body, "tasks")
        .into_iter()
        .map(|name| task(&name, format!("task {name}"), ""))
        .collect();
    Some(TaskGroup { file: file.into(), runner: "task".into(), tasks })
}

/* ---------------- Docker Compose ---------------- */

/// Compose V2 (`docker compose`, the plugin) rather than V1 (`docker-compose`,
/// the standalone Python script), which reached end of life in 2023 and is not
/// installed by current Docker releases. The file name may still be the old
/// `docker-compose.yml`; the command is what changed.
fn compose(dir: &Path) -> Option<TaskGroup> {
    let (file, body) = first_present(
        dir,
        &["compose.yaml", "compose.yml", "docker-compose.yaml", "docker-compose.yml"],
    )?;
    // Whole-stack commands first: they are what a compose file is usually
    // opened for, and they are correct whether or not the services parsed.
    let mut tasks = vec![
        task("up", "docker compose up -d".into(), ""),
        task("down", "docker compose down".into(), ""),
        task("build", "docker compose build".into(), ""),
        task("ps", "docker compose ps".into(), ""),
        task("logs", "docker compose logs -f".into(), ""),
    ];
    // `up -d` per service is the one per-service command worth a button: it is
    // how you bring up a single dependency (a database) without the app. Other
    // per-service verbs are omitted rather than multiplying the list by the
    // number of services.
    for name in yaml_section_keys(&body, "services") {
        tasks.push(task(&name, format!("docker compose up -d {name}"), ""));
    }
    Some(TaskGroup { file: file.into(), runner: "docker compose".into(), tasks })
}

/* ---------------- Dockerfile ---------------- */

/// A Docker image reference this directory name can legally produce, or `None`.
///
/// Docker requires a lowercase path component (`[a-z0-9]+([._-][a-z0-9]+)*`), so
/// `My App` cannot be used as written. Rather than emit a `docker build -t` that
/// fails, the name is folded to lowercase and runs of anything else become a
/// single `-`; if nothing usable survives, the caller drops the tag entirely and
/// builds an untagged image.
fn image_name(dir: &Path) -> Option<String> {
    let raw = dir.file_name()?.to_str()?.to_ascii_lowercase();
    let mut out = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_end_matches('-');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Named build stages: `FROM node:20 AS builder` → `builder`.
///
/// Only the stage *name* is taken, and only from a line that starts with `FROM`
/// — a `# FROM ... AS ...` in a comment is not a stage, and neither is an `AS`
/// inside a `RUN`. Stages exist to be built individually (`--target`), which is
/// exactly the thing that is tedious to type from memory.
fn build_stage(line: &str) -> Option<String> {
    let line = line.trim();
    let rest = line.strip_prefix("FROM ").or_else(|| line.strip_prefix("from "))?;
    let mut parts = rest.split_whitespace();
    let _image = parts.next()?;
    let kw = parts.next()?;
    if !kw.eq_ignore_ascii_case("as") {
        return None;
    }
    let name = parts.next()?;
    // A stage name can be referenced by `--target`, so it must be a plain word.
    // Anything with a `$` is a build-arg expansion whose value is not known
    // here, and a button running `--target $BASE` would simply fail.
    let ok = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    ok.then(|| name.to_ascii_lowercase())
}

fn dockerfile(dir: &Path) -> Option<TaskGroup> {
    let (file, body) = first_present(dir, &["Dockerfile", "dockerfile", "Containerfile"])?;
    let tag = image_name(dir);
    let suffix = tag.as_ref().map(|t| format!(" -t {t}")).unwrap_or_default();
    let mut tasks = vec![task("build", format!("docker build{suffix} ."), "")];
    // A single-stage Dockerfile still parses as one `FROM ... AS x` if it is
    // written that way; the stage list is only interesting when there is more
    // than one, because with one stage `--target` is what `build` already does.
    let stages: Vec<String> = {
        let mut seen: Vec<String> = Vec::new();
        for line in body.lines() {
            if let Some(name) = build_stage(line) {
                if !seen.contains(&name) {
                    seen.push(name);
                }
            }
        }
        seen
    };
    if stages.len() > 1 {
        for stage in stages {
            let suffix = tag.as_ref().map(|t| format!(" -t {t}:{stage}")).unwrap_or_default();
            tasks.push(task(&stage, format!("docker build --target {stage}{suffix} ."), ""));
        }
    }
    if let Some(tag) = tag {
        tasks.push(task("run", format!("docker run --rm -it {tag}"), ""));
    }
    Some(TaskGroup { file: file.into(), runner: "docker".into(), tasks })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, body) in files {
            let path = dir.path().join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        }
        dir
    }

    fn group<'a>(groups: &'a [TaskGroup], file: &str) -> &'a TaskGroup {
        groups.iter().find(|g| g.file == file).unwrap_or_else(|| panic!("no group for {file}"))
    }

    fn commands(g: &TaskGroup) -> Vec<&str> {
        g.tasks.iter().map(|t| t.command.as_str()).collect()
    }

    #[test]
    fn package_scripts_are_listed_alphabetically_and_carry_the_body() {
        // Pinned because the order is a property users see: it must not drift
        // between runs, whichever order it settles on.
        let d = dir_with(&[(
            "package.json",
            r#"{"scripts":{"dev":"vite","build":"vite build","test":"vitest"}}"#,
        )]);
        let g = scan(d.path());
        let g = group(&g, "package.json");
        assert_eq!(g.tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["build", "dev", "test"]);
        // The script body is the subtitle, so a task says what it will run.
        assert_eq!(g.tasks[1].detail, "vite");
    }

    #[test]
    fn the_lockfile_decides_the_package_manager() {
        // Running npm in a pnpm workspace rewrites the lockfile and can resolve
        // a different tree, so this must never fall back to a default when the
        // repository has already said which manager it uses.
        for (lock, want) in [
            ("pnpm-lock.yaml", "pnpm run build"),
            ("yarn.lock", "yarn run build"),
            ("bun.lockb", "bun run build"),
            ("package-lock.json", "npm run build"),
        ] {
            let d = dir_with(&[("package.json", r#"{"scripts":{"build":"x"}}"#), (lock, "")]);
            let g = scan(d.path());
            assert_eq!(commands(group(&g, "package.json")), [want], "{lock}");
        }
        // No lockfile at all: npm is the only safe assumption.
        let d = dir_with(&[("package.json", r#"{"scripts":{"build":"x"}}"#)]);
        assert_eq!(commands(group(&scan(d.path()), "package.json")), ["npm run build"]);
    }

    #[test]
    fn package_manager_field_outranks_the_lockfile() {
        // Corepack's field is a deliberate pin; a stale lockfile is not.
        let d = dir_with(&[
            ("package.json", r#"{"packageManager":"pnpm@9.1.0","scripts":{"build":"x"}}"#),
            ("package-lock.json", ""),
        ]);
        assert_eq!(commands(group(&scan(d.path()), "package.json")), ["pnpm run build"]);
    }

    #[test]
    fn cargo_offers_run_only_when_something_is_runnable() {
        // `cargo run` on a library crate is an error, not a build.
        let lib = dir_with(&[("Cargo.toml", "[package]\nname = \"x\"\n")]);
        let g = scan(lib.path());
        assert!(!commands(group(&g, "Cargo.toml")).contains(&"cargo run"));

        let bin = dir_with(&[("Cargo.toml", "[package]\nname = \"x\"\n"), ("src/main.rs", "")]);
        let g = scan(bin.path());
        assert!(commands(group(&g, "Cargo.toml")).contains(&"cargo run"));
    }

    #[test]
    fn a_virtual_manifest_scopes_every_command_to_the_workspace() {
        // Without --workspace these commands do nothing at a workspace root.
        let d = dir_with(&[("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n")]);
        let g = scan(d.path());
        let g = group(&g, "Cargo.toml");
        assert!(commands(g).contains(&"cargo build --workspace"));
        assert!(commands(g).contains(&"cargo test --workspace"));
        assert!(!commands(g).contains(&"cargo run"));
    }

    #[test]
    fn named_bins_appear_only_when_there_is_a_choice() {
        let one = dir_with(&[("Cargo.toml", "[package]\nname=\"x\"\n[[bin]]\nname=\"only\"\n")]);
        let g = scan(one.path());
        assert!(commands(group(&g, "Cargo.toml")).contains(&"cargo run"));
        assert!(!commands(group(&g, "Cargo.toml")).contains(&"cargo run --bin only"));

        let two = dir_with(&[(
            "Cargo.toml",
            "[package]\nname=\"x\"\n[[bin]]\nname=\"a\"\n[[bin]]\nname=\"b\"\n[[example]]\nname=\"demo\"\n",
        )]);
        let g = scan(two.path());
        let cmds = commands(group(&g, "Cargo.toml"));
        assert!(cmds.contains(&"cargo run --bin a"));
        assert!(cmds.contains(&"cargo run --bin b"));
        assert!(cmds.contains(&"cargo run --example demo"));
    }

    #[test]
    fn make_skips_everything_that_is_not_a_runnable_rule() {
        let d = dir_with(&[(
            "Makefile",
            concat!(
                "CC = gcc\n",
                "FLAGS := -O2\n",
                ".PHONY: build test\n",
                "build: deps ## Compile everything\n",
                "\tgcc -o out\n",
                "test:\n",
                "\t./out --test\n",
                "%.o: %.c\n",
                "$(GEN): src\n",
                "build:\n",
            ),
        )]);
        let g = scan(d.path());
        let g = group(&g, "Makefile");
        assert_eq!(g.tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["build", "test"]);
        assert_eq!(g.tasks[0].detail, "Compile everything");
        assert_eq!(g.tasks[0].command, "make build");
    }

    #[test]
    fn just_reads_the_name_out_of_a_recipe_signature() {
        let d = dir_with(&[(
            "justfile",
            concat!(
                "export RUST_LOG := \"debug\"\n",
                "# Build the thing\n",
                "build target=\"debug\":\n",
                "    cargo build\n",
                "test:\n",
                "    cargo test\n",
            ),
        )]);
        let g = scan(d.path());
        let g = group(&g, "justfile");
        assert_eq!(g.tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["build", "test"]);
        assert_eq!(g.tasks[0].command, "just build");
        assert_eq!(g.tasks[0].detail, "Build the thing");
    }

    #[test]
    fn taskfile_reads_only_the_task_names() {
        // Keys nested inside a task (cmds, desc) must not become tasks.
        let d = dir_with(&[(
            "Taskfile.yml",
            concat!(
                "version: '3'\n",
                "vars:\n",
                "  GREETING: hello\n",
                "tasks:\n",
                "  build:\n",
                "    cmds:\n",
                "      - go build\n",
                "  test:\n",
                "    desc: run tests\n",
                "    cmds:\n",
                "      - go test\n",
            ),
        )]);
        let g = scan(d.path());
        let g = group(&g, "Taskfile.yml");
        assert_eq!(g.tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["build", "test"]);
        assert_eq!(g.tasks[0].command, "task build");
    }

    #[test]
    fn python_runner_follows_the_lockfile_and_tooling() {
        let uv = dir_with(&[
            ("pyproject.toml", "[project]\nname=\"x\"\n[project.scripts]\nserve=\"x:main\"\n"),
            ("uv.lock", ""),
        ]);
        let g = scan(uv.path());
        assert!(commands(group(&g, "pyproject.toml")).contains(&"uv run serve"));

        let poetry = dir_with(&[(
            "pyproject.toml",
            "[tool.poetry]\nname=\"x\"\n[tool.poetry.scripts]\nserve=\"x:main\"\n",
        )]);
        let g = scan(poetry.path());
        assert!(commands(group(&g, "pyproject.toml")).contains(&"poetry run serve"));

        // A pyproject that names no tooling says nothing about how to run it,
        // so guessing a runner here would just produce commands that fail.
        let bare = dir_with(&[("pyproject.toml", "[build-system]\nrequires=[\"setuptools\"]\n")]);
        assert!(scan(bare.path()).iter().all(|g| g.file != "pyproject.toml"));
    }

    #[test]
    fn several_runners_in_one_directory_all_appear() {
        let d = dir_with(&[
            ("package.json", r#"{"scripts":{"dev":"vite"}}"#),
            ("Cargo.toml", "[package]\nname=\"x\"\n"),
            ("Makefile", "all:\n\techo hi\n"),
        ]);
        let g = scan(d.path());
        let files: Vec<&str> = g.iter().map(|g| g.file.as_str()).collect();
        assert_eq!(files, ["package.json", "Cargo.toml", "Makefile"]);
    }

    #[test]
    fn compose_lists_stack_commands_then_one_per_service() {
        let d = dir_with(&[(
            "docker-compose.yml",
            "services:\n  web:\n    build: .\n    ports:\n      - \"8080:80\"\n  db:\n    image: postgres:16\nvolumes:\n  pgdata:\n",
        )]);
        let found = scan(d.path());
        let g = group(&found, "docker-compose.yml");
        assert_eq!(g.runner, "docker compose");
        assert_eq!(
            commands(g),
            [
                "docker compose up -d",
                "docker compose down",
                "docker compose build",
                "docker compose ps",
                "docker compose logs -f",
                "docker compose up -d web",
                "docker compose up -d db",
            ]
        );
        // `volumes:` is a sibling mapping with the same shape; its keys are not
        // services and must not become buttons that fail.
        assert!(!commands(g).iter().any(|c| c.contains("pgdata")));
    }

    #[test]
    fn compose_still_offers_the_stack_commands_when_no_service_parses() {
        // The whole-stack commands do not depend on reading the file, so an
        // unusual (or simply unparsed) layout must not leave an empty card.
        let d = dir_with(&[("compose.yaml", "include:\n  - path: ./other/compose.yaml\n")]);
        let found = scan(d.path());
        let g = group(&found, "compose.yaml");
        assert_eq!(g.tasks.len(), 5);
        assert_eq!(g.tasks[0].command, "docker compose up -d");
    }

    #[test]
    fn dockerfile_tags_the_image_after_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("My Web App");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Dockerfile"), "FROM nginx:alpine\nCOPY . /usr/share/nginx/html\n")
            .unwrap();
        let found = scan(&root);
        let g = group(&found, "Dockerfile");
        assert_eq!(g.runner, "docker");
        // Spaces and capitals are illegal in an image reference, so the name is
        // folded rather than emitted as a command that cannot run.
        assert_eq!(
            commands(g),
            ["docker build -t my-web-app .", "docker run --rm -it my-web-app"]
        );
    }

    #[test]
    fn dockerfile_offers_each_named_stage_only_when_there_are_several() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("api");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("Dockerfile"),
            "# FROM golang:1 AS notastage\n\
             FROM golang:1.22 AS builder\n\
             RUN go build -o /app ./cmd/api\n\
             FROM gcr.io/distroless/static AS runtime\n\
             COPY --from=builder /app /app\n",
        )
        .unwrap();
        let found = scan(&root);
        let g = group(&found, "Dockerfile");
        assert_eq!(
            commands(g),
            [
                "docker build -t api .",
                "docker build --target builder -t api:builder .",
                "docker build --target runtime -t api:runtime .",
                "docker run --rm -it api",
            ]
        );
        // A commented-out FROM is not a stage.
        assert!(!commands(g).iter().any(|c| c.contains("notastage")));
    }

    #[test]
    fn a_build_arg_stage_name_is_not_offered() {
        // `--target $BASE` would expand to nothing in the user's shell and fail;
        // the module's rule is to emit no command rather than a wrong one.
        assert_eq!(build_stage("FROM alpine AS ${BASE}"), None);
        assert_eq!(build_stage("RUN x AS y"), None);
        assert_eq!(build_stage("FROM alpine"), None);
        assert_eq!(build_stage("FROM alpine AS Build"), Some("build".into()));
    }

    #[test]
    fn a_directory_name_with_nothing_usable_builds_an_untagged_image() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("___");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Dockerfile"), "FROM scratch\n").unwrap();
        let found = scan(&root);
        let g = group(&found, "Dockerfile");
        assert_eq!(commands(g), ["docker build ."]);
    }

    #[test]
    fn a_directory_with_nothing_to_run_yields_nothing() {
        let d = dir_with(&[("README.md", "# hi")]);
        assert!(scan(d.path()).is_empty());
        // An empty `scripts` table is not a group with zero tasks; it is no
        // group at all, so the panel shows its empty state rather than a
        // heading with nothing under it.
        let empty = dir_with(&[("package.json", r#"{"scripts":{}}"#)]);
        assert!(scan(empty.path()).is_empty());
    }

    #[test]
    fn malformed_files_are_skipped_rather_than_failing_the_scan() {
        let d = dir_with(&[
            ("package.json", "{ this is not json"),
            ("Cargo.toml", "[package\nname ="),
            ("Makefile", "ok:\n\techo hi\n"),
        ]);
        let g = scan(d.path());
        // The one file that parses still contributes.
        assert_eq!(g.iter().map(|g| g.file.as_str()).collect::<Vec<_>>(), ["Makefile"]);
    }
}
