//! Auto-injects OSC 133 shell-integration hooks (the prompt/command/output
//! markers the frontend's command-block UI reads) into local shell sessions,
//! without ever editing the user's own rc files.
//!
//! Detected purely by executable basename — bash/zsh/fish are recognized;
//! every other shell (cmd.exe, PowerShell, a custom REPL, …) is left
//! untouched. Each shell gets a different mechanism, chosen to avoid
//! fighting the user's own configuration:
//!
//! - **bash**: `--rcfile <generated>` that sources the real `~/.bashrc`
//!   first, then appends the OSC hooks. Only applied when the profile has no
//!   other args (so we never guess at how to combine `--rcfile` with a
//!   deliberate `-l`/`-c` the user configured).
//! - **zsh**: `ZDOTDIR` pointed at a generated directory whose `.zshenv` /
//!   `.zprofile` / `.zshrc` / `.zlogin` each just source the real file from
//!   the user's actual zdotdir (`$ZDOTDIR` if the environment already set
//!   one, else `$HOME`) — so nothing the user's config relies on is skipped.
//!   Safe regardless of the profile's args (env-var based, not a flag).
//! - **fish**: `-C '<commands>'`, fish's built-in "run this after config
//!   loads" flag — no generated files needed at all.
//!
//! Building an `Integration` is best-effort: any I/O failure (temp dir
//! unwritable, disk full, …) just returns `None`, so a shell always launches
//! even if integration can't be wired up.

use std::io::Write as _;
use std::path::Path;

/// Extra launch parameters that make a shell emit OSC 133 markers. The
/// `guard` (when present) owns the generated temp directory and must be kept
/// alive for as long as the spawned shell might still be starting up.
pub struct Integration {
    pub prefix_args: Vec<String>,
    pub suffix_args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub guard: Option<tempfile::TempDir>,
}

const BASH_SNIPPET: &str = r#"PS1='\[\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]'
PS0='\[\e]133;C\a\]'
PROMPT_COMMAND='printf "\e]133;D;%s\a" "$?"'"${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
"#;

const ZSH_SNIPPET: &str = r#"precmd() { print -Pn '\e]133;D;%?\a\e]133;A\a' }
preexec() { print -n '\e]133;C\a' }
PS1="$PS1"$'%{\e]133;B\a%}'
"#;

const FISH_SNIPPET: &str = r#"function __senju_blocks_prompt --on-event fish_prompt
    printf '\e]133;D;%s\a\e]133;A\a' $status
end
function __senju_blocks_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
"#;

/// Builds the integration for `shell` (a path or bare executable name), or
/// `None` if it isn't a recognized shell or the files couldn't be written.
pub fn build(shell: &str, existing_args: &[String]) -> Option<Integration> {
    let name = Path::new(shell)
        .file_stem()?
        .to_str()?
        .to_ascii_lowercase();
    match name.as_str() {
        "bash" => build_bash(existing_args),
        "zsh" => build_zsh(),
        "fish" => build_fish(),
        _ => None,
    }
}

fn build_bash(existing_args: &[String]) -> Option<Integration> {
    // `--rcfile` competes with flags like `-l`/`-c` the user may have chosen
    // for this profile — only inject when nothing else was configured.
    if !existing_args.is_empty() {
        return None;
    }
    let dir = tempfile::tempdir().ok()?;
    let rcfile = dir.path().join("bashrc");
    let mut f = std::fs::File::create(&rcfile).ok()?;
    write!(
        f,
        "# Senju Term shell integration (auto-generated)\n[ -f ~/.bashrc ] && . ~/.bashrc\n{BASH_SNIPPET}"
    )
    .ok()?;
    Some(Integration {
        prefix_args: vec![
            "--rcfile".into(),
            rcfile.to_string_lossy().into_owned(),
            "-i".into(),
        ],
        suffix_args: vec![],
        env: vec![],
        guard: Some(dir),
    })
}

fn build_zsh() -> Option<Integration> {
    // Respect an already-customized ZDOTDIR rather than assuming $HOME, so a
    // user whose real dotfiles live elsewhere doesn't have them silently
    // skipped.
    let real = std::env::var("ZDOTDIR").ok().filter(|s| !s.is_empty());
    let real_expr = real.as_deref().unwrap_or("$HOME");
    let dir = tempfile::tempdir().ok()?;
    let write = |name: &str, body: String| -> Option<()> {
        std::fs::File::create(dir.path().join(name))
            .ok()?
            .write_all(body.as_bytes())
            .ok()
    };
    // .zshenv / .zprofile / .zlogin: zsh reads these (when applicable) from
    // whatever ZDOTDIR it resolved at startup — before .zshenv itself can
    // change it — so all four proxy files live in our synthetic dir and each
    // just defers to the real one.
    for rc in ["zshenv", "zprofile", "zlogin"] {
        write(
            &format!(".{rc}"),
            format!("[ -f \"{real_expr}/.{rc}\" ] && source \"{real_expr}/.{rc}\"\n"),
        )?;
    }
    // .zshrc also restores ZDOTDIR to the real directory afterwards, so any
    // plugin that reads $ZDOTDIR at runtime (not just at startup) still sees
    // the user's actual location.
    write(
        ".zshrc",
        format!(
            "[ -f \"{real_expr}/.zshrc\" ] && source \"{real_expr}/.zshrc\"\nZDOTDIR=\"{real_expr}\"\n{ZSH_SNIPPET}"
        ),
    )?;
    Some(Integration {
        prefix_args: vec![],
        suffix_args: vec![],
        env: vec![(
            "ZDOTDIR".into(),
            dir.path().to_string_lossy().into_owned(),
        )],
        guard: Some(dir),
    })
}

fn build_fish() -> Option<Integration> {
    // fish's -C runs after the user's own config.fish loads — no generated
    // files, and it composes fine with whatever other args the profile has.
    Some(Integration {
        prefix_args: vec![],
        suffix_args: vec!["-C".into(), FISH_SNIPPET.into()],
        env: vec![],
        guard: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrecognized_shell_is_untouched() {
        assert!(build("pwsh.exe", &[]).is_none());
        assert!(build("/bin/dash", &[]).is_none());
    }

    #[test]
    fn bash_wraps_only_when_args_are_empty() {
        let integ = build("/bin/bash", &[]).expect("bash should be recognized");
        assert_eq!(integ.prefix_args[0], "--rcfile");
        let rcfile = std::fs::read_to_string(&integ.prefix_args[1]).unwrap();
        assert!(rcfile.contains("PROMPT_COMMAND"));
        assert!(rcfile.contains(". ~/.bashrc"));

        assert!(build("/bin/bash", &["-l".into()]).is_none());
    }

    #[test]
    fn zsh_sets_zdotdir_and_proxies_all_four_files() {
        let integ = build("zsh", &["-l".into()]).expect("zsh should be recognized regardless of args");
        let (key, dir) = &integ.env[0];
        assert_eq!(key, "ZDOTDIR");
        for rc in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            let content = std::fs::read_to_string(std::path::Path::new(dir).join(rc)).unwrap();
            assert!(content.contains("source"), "{rc} should source the real file");
        }
        let zshrc = std::fs::read_to_string(std::path::Path::new(dir).join(".zshrc")).unwrap();
        assert!(zshrc.contains("precmd()"));
    }

    #[test]
    fn fish_uses_init_command_not_files() {
        let integ = build("fish", &["-l".into()]).expect("fish should be recognized");
        assert!(integ.guard.is_none());
        assert_eq!(integ.suffix_args[0], "-C");
        assert!(integ.suffix_args[1].contains("fish_prompt"));
    }
}
