//! Auto-injects shell-integration hooks into local shell sessions, without
//! ever editing the user's own rc files:
//!
//! - **OSC 133** — the prompt/command/output markers the frontend's
//!   command-block UI reads.
//! - **OSC 7** — the working directory, reported after every command. Nothing
//!   else can tell the app where the shell currently *is*: the child process
//!   `cd`s on its own and the parent is never told, so a feature that depends
//!   on the directory (the task panel reading `package.json` next to you) has
//!   no other source for it.
//!
//! Detected purely by executable basename — bash/zsh/fish and PowerShell
//! (`pwsh` / `powershell`) are recognized; every other shell (cmd.exe, a
//! custom REPL, …) is left untouched. Each shell gets a different mechanism, chosen to avoid
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
//! - **PowerShell** (`pwsh` 7 and Windows PowerShell 5.1): suffix args
//!   `-NoExit -EncodedCommand <base64>` that wrap the `prompt` function (and
//!   `PSConsoleHostReadLine` when PSReadLine provides it). `$PROFILE` is never
//!   edited and still loads first. Skipped when the profile's args already
//!   decide what the shell runs (see `build_pwsh`).
//!
//! Building an `Integration` is best-effort: any I/O failure (temp dir
//! unwritable, disk full, …) just returns `None`, so a shell always launches
//! even if integration can't be wired up.

use std::io::Write as _;
use std::path::Path;

/// Extra launch parameters that make a shell emit the OSC markers. The
/// `guard` (when present) owns the generated temp directory and must be kept
/// alive for as long as the spawned shell might still be starting up.
pub struct Integration {
    pub prefix_args: Vec<String>,
    pub suffix_args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub guard: Option<tempfile::TempDir>,
}

// `PS0` (printed after a command is read, before it runs) is the clean way to
// emit the 133;C "output starts here" marker — but it only exists in bash 4.4+.
// macOS still ships bash 3.2, where PS0 is simply ignored: A/B/D would arrive,
// C never would, and the frontend (which only builds a block once it has seen
// C) would show no command blocks at all. So on older bash, fall back to a
// DEBUG trap — armed once per prompt so it fires on the *first* command of a
// line and not again for each command in a pipeline — and leave any DEBUG trap
// the user already installed (bash-preexec, etc.) alone rather than clobbering
// it.
//
// `__senju_precmd` re-`return`s the status it was handed so a user's own
// PROMPT_COMMAND, appended after ours, still sees the real `$?`.
const BASH_SNIPPET: &str = r#"__senju_cwd() { local p=${PWD//%/%25}; printf '\e]7;file://%s%s\a' "${HOSTNAME:-}" "${p// /%20}"; }
__senju_precmd() { __senju_cwd; printf '\e]133;D;%s\a' "$1"; return "$1"; }
__senju_arm() { __senju_at_prompt=1; }
__senju_preexec() {
  [ -n "$COMP_LINE" ] && return 0
  if [ "$__senju_at_prompt" = 1 ]; then
    __senju_at_prompt=0
    printf '\e]133;C\a'
  fi
  return 0
}
__senju_at_prompt=0
PS1='\[\e]133;A\a\]'"$PS1"'\[\e]133;B\a\]'
if [ "${BASH_VERSINFO[0]}" -gt 4 ] 2>/dev/null || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 4 ]; } 2>/dev/null; then
  PS0='\[\e]133;C\a\]'
elif [ -z "$(trap -p DEBUG)" ]; then
  trap '__senju_preexec' DEBUG
fi
PROMPT_COMMAND='__senju_precmd $?'"${PROMPT_COMMAND:+;$PROMPT_COMMAND}"';__senju_arm'
"#;

const ZSH_SNIPPET: &str = r#"precmd() { print -Pn '\e]133;D;%?\a\e]133;A\a'; local p=${PWD//\%/%25}; print -n "\e]7;file://${HOST}${p// /%20}\a" }
preexec() { print -n '\e]133;C\a' }
PS1="$PS1"$'%{\e]133;B\a%}'
"#;

const FISH_SNIPPET: &str = r#"function __senju_blocks_prompt --on-event fish_prompt
    printf '\e]133;D;%s\a\e]133;A\a' $status
    printf '\e]7;file://%s%s\a' (hostname) (string replace -a ' ' '%20' -- (string replace -a '%' '%25' -- $PWD))
end
function __senju_blocks_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end
"#;

// PowerShell has no hook point like PROMPT_COMMAND, so the one way to run code
// at every prompt is to *be* the `prompt` function: save whatever the user's
// profile (oh-my-posh, starship, posh-git, ...) defined, and replace it with a
// wrapper that emits the markers around a call to the saved one.
//
// Order inside the wrapper matters. `$?` and `$LASTEXITCODE` describe the
// command that just ran and are clobbered by the very next statement, so they
// are captured first. `$LASTEXITCODE` is then written back before returning,
// otherwise merely drawing the prompt would reset it and a user's
// `$LASTEXITCODE` check on the next line would see our value, not theirs.
//
// Must stay valid on Windows PowerShell 5.1, which is what ships with Windows:
// no `` `e `` / `` `a `` escapes (7+ only) so ESC and BEL are built with
// [char]; no `??`, no ternary. No `#` anywhere either, so the raw string
// delimiter can never be closed early.
//
// The 133;C marker needs the moment *after* Enter, which `prompt` cannot see.
// PSReadLine exposes that as `PSConsoleHostReadLine` (the host calls it to
// read a line), so it is wrapped only if it exists; without PSReadLine you get
// A/B/D and the frontend simply builds no command blocks, while OSC 7 (the
// part the task pane needs) still works.
const PWSH_SNIPPET: &str = r#"if (-not $global:__senjuInstalled) {
  $global:__senjuInstalled = $true
  $global:__senjuOrigPrompt = $function:prompt
  function global:prompt {
    $__senjuOk = $?
    $__senjuLast = $global:LASTEXITCODE
    $__senjuEsc = [string][char]27
    $__senjuBel = [string][char]7
    $__senjuCode = 0
    if (-not $__senjuOk) {
      if (($null -ne $__senjuLast) -and ($__senjuLast -ne 0)) { $__senjuCode = $__senjuLast } else { $__senjuCode = 1 }
    }
    $__senjuOut = $__senjuEsc + ']133;D;' + $__senjuCode + $__senjuBel
    $__senjuLoc = $executionContext.SessionState.Path.CurrentLocation
    if ($__senjuLoc.Provider.Name -eq 'FileSystem') {
      $__senjuPath = $__senjuLoc.ProviderPath.Replace('%', '%25').Replace('\', '/').Replace(' ', '%20')
      if (-not $__senjuPath.StartsWith('/')) { $__senjuPath = '/' + $__senjuPath }
      $__senjuOut += $__senjuEsc + ']7;file://' + [System.Net.Dns]::GetHostName() + $__senjuPath + $__senjuBel
    }
    $__senjuOut += $__senjuEsc + ']133;A' + $__senjuBel
    [Console]::Write($__senjuOut)
    $global:LASTEXITCODE = $__senjuLast
    $__senjuText = & $global:__senjuOrigPrompt
    $global:LASTEXITCODE = $__senjuLast
    return ([string]$__senjuText + $__senjuEsc + ']133;B' + $__senjuBel)
  }
  if (Test-Path function:PSConsoleHostReadLine) {
    $global:__senjuOrigReadLine = $function:PSConsoleHostReadLine
    function global:PSConsoleHostReadLine {
      $__senjuLine = & $global:__senjuOrigReadLine
      [Console]::Write(([string][char]27) + ']133;C' + ([string][char]7))
      $__senjuLine
    }
  }
}
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
        "pwsh" | "powershell" => build_pwsh(existing_args),
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
    //
    // The path is handed to the generated proxy files through an environment
    // variable rather than being interpolated into their text: splicing a
    // filesystem path straight into a shell script is a code-injection shape
    // (a path containing `"`, `$(…)` or a backtick would execute), and it also
    // simply breaks for ordinary paths with quotes in them. `$SENJU_ZDOTDIR` /
    // `$HOME` are expanded by zsh itself, inside double quotes, so any path
    // works and none of it is ever parsed as code.
    let real = std::env::var("ZDOTDIR").ok().filter(|s| !s.is_empty());
    let real_expr = if real.is_some() { "$SENJU_ZDOTDIR" } else { "$HOME" };
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
    let mut env = vec![("ZDOTDIR".to_string(), dir.path().to_string_lossy().into_owned())];
    if let Some(real) = real {
        env.push(("SENJU_ZDOTDIR".to_string(), real));
    }
    Some(Integration {
        prefix_args: vec![],
        suffix_args: vec![],
        env,
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

/// Standard-alphabet base64 with `=` padding. `-EncodedCommand` wants exactly
/// that, and a dozen lines here beat a crate dependency for one call site.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

fn build_pwsh(existing_args: &[String]) -> Option<Integration> {
    // Only inject when the profile's args don't already decide what the shell
    // runs. A command/file/encoded-command argument, `-NoExit` (the user has
    // chosen how the session behaves) or any positional argument (an implicit
    // `-File`/`-Command`) would collide with ours: PowerShell takes only one
    // command source, so we would either replace theirs or be ignored. The
    // rule is deliberately crude — anything not starting with `-` is treated
    // as positional, which also skips `-ExecutionPolicy Bypass` (its value has
    // no dash); losing integration there is cheaper than guessing wrong. Plain
    // switches such as `-NoLogo` / `-NoProfile` are harmless and still inject.
    const DECIDES: [&str; 9] =
        ["-command", "-c", "-file", "-f", "-encodedcommand", "-e", "-ec", "-enc", "-noexit"];
    for arg in existing_args {
        let lower = arg.to_ascii_lowercase();
        if !arg.starts_with('-') || DECIDES.contains(&lower.as_str()) {
            return None;
        }
    }
    // Why `-NoExit -EncodedCommand`, and not the alternatives:
    // - unlike `-NoProfile`, the user's profile still loads first, so the
    //   prompt we wrap is their prompt (oh-my-posh, starship, ...);
    // - `-EncodedCommand` takes base64 of UTF-16LE, so the script contains no
    //   quote characters for Windows command-line handling to mangle
    //   (powershell.exe strips `"` when it re-joins its arguments);
    // - it is not subject to the execution policy the way `-File` on a
    //   generated .ps1 would be (a Restricted policy refuses such a file);
    // - it works identically on Windows PowerShell 5.1 and pwsh 7;
    // - nothing is written to disk, so there is no temp dir and no guard.
    // `-NoExit` keeps the shell interactive after the script has run.
    let utf16le: Vec<u8> = PWSH_SNIPPET.encode_utf16().flat_map(u16::to_le_bytes).collect();
    Some(Integration {
        prefix_args: vec![],
        suffix_args: vec!["-NoExit".into(), "-EncodedCommand".into(), base64_encode(&utf16le)],
        env: vec![],
        guard: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrecognized_shell_is_untouched() {
        assert!(build("cmd.exe", &[]).is_none());
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

    /// The 133;C ("output starts here") marker must be emitted on bash 3.2 —
    /// what macOS ships — where `PS0` does not exist. Without the DEBUG-trap
    /// fallback the frontend never sees a C and builds no command blocks at
    /// all, so the whole feature silently does nothing on macOS bash.
    #[test]
    fn bash_emits_output_marker_without_ps0_on_old_bash() {
        let integ = build("/bin/bash", &[]).expect("bash should be recognized");
        let rcfile = std::fs::read_to_string(&integ.prefix_args[1]).unwrap();
        assert!(rcfile.contains("PS0="), "modern bash should still use PS0");
        assert!(
            rcfile.contains("trap '__senju_preexec' DEBUG"),
            "old bash needs a DEBUG-trap fallback for the 133;C marker"
        );
        // The fallback must not stomp on a DEBUG trap the user already set
        // (bash-preexec and friends install one).
        assert!(
            rcfile.contains("trap -p DEBUG"),
            "an existing DEBUG trap must be left alone"
        );
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

    /// The real zdotdir must reach the generated proxy files as an environment
    /// variable, never spliced into their text — a path is data, and pasting it
    /// into a shell script would both execute `$(…)`/backticks it contains and
    /// break on an ordinary path with a quote in it.
    #[test]
    fn zsh_passes_the_real_zdotdir_by_env_not_by_interpolation() {
        // SAFETY: single-threaded test process; the var is restored below.
        let previous = std::env::var("ZDOTDIR").ok();
        let hostile = "/tmp/\"$(touch /tmp/senju-pwned)`id`";
        unsafe { std::env::set_var("ZDOTDIR", hostile) };
        let integ = build("zsh", &[]).expect("zsh should be recognized");
        match previous {
            Some(v) => unsafe { std::env::set_var("ZDOTDIR", v) },
            None => unsafe { std::env::remove_var("ZDOTDIR") },
        }

        let dir = &integ.env[0].1;
        for rc in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            let body = std::fs::read_to_string(std::path::Path::new(dir).join(rc)).unwrap();
            assert!(
                !body.contains("touch") && !body.contains("`id`"),
                "{rc} must not contain the raw ZDOTDIR value: {body}"
            );
            assert!(body.contains("$SENJU_ZDOTDIR"), "{rc} should defer to the env var");
        }
        assert!(
            integ.env.iter().any(|(k, v)| k == "SENJU_ZDOTDIR" && v == hostile),
            "the real path is passed through the environment verbatim"
        );
    }

    #[test]
    fn fish_uses_init_command_not_files() {
        let integ = build("fish", &["-l".into()]).expect("fish should be recognized");
        assert!(integ.guard.is_none());
        assert_eq!(integ.suffix_args[0], "-C");
        assert!(integ.suffix_args[1].contains("fish_prompt"));
    }

    /// Test-only inverse of `base64_encode`, so the injected script can be
    /// read back out of the launch args.
    fn base64_decode(s: &str) -> Vec<u8> {
        let val = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("bad base64 byte {c}"),
        };
        let mut out = Vec::new();
        for chunk in s.as_bytes().chunks(4) {
            let pad = chunk.iter().filter(|&&c| c == b'=').count();
            let mut n = 0u32;
            for &c in chunk {
                n = (n << 6) | if c == b'=' { 0 } else { u32::from(val(c)) };
            }
            let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
            out.extend_from_slice(&bytes[..3 - pad]);
        }
        out
    }

    fn decoded_pwsh_script(integ: &Integration) -> String {
        let bytes = base64_decode(&integ.suffix_args[2]);
        let units: Vec<u16> =
            bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16(&units).unwrap()
    }

    #[test]
    fn base64_matches_known_vectors() {
        // RFC 4648 section 10.
        for (plain, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_encode(plain.as_bytes()), enc);
            assert_eq!(base64_decode(enc), plain.as_bytes());
        }
        // Bytes outside ASCII exercise the high alphabet entries (`+`, `/`).
        assert_eq!(base64_encode(&[0xfb, 0xff, 0xbf]), "+/+/");
    }

    #[test]
    fn pwsh_and_powershell_are_recognized() {
        for shell in ["pwsh", "pwsh.exe", "/usr/bin/pwsh", "PowerShell.exe", "powershell"] {
            assert!(build(shell, &[]).is_some(), "{shell} should be recognized");
        }
    }

    #[test]
    fn pwsh_uses_noexit_encodedcommand_suffix_args() {
        let integ = build("pwsh", &[]).unwrap();
        assert!(integ.prefix_args.is_empty());
        assert!(integ.env.is_empty());
        assert!(integ.guard.is_none());
        assert_eq!(integ.suffix_args[0], "-NoExit");
        assert_eq!(integ.suffix_args[1], "-EncodedCommand");
        assert_eq!(integ.suffix_args.len(), 3);

        let script = decoded_pwsh_script(&integ);
        assert_eq!(script, PWSH_SNIPPET);
        assert!(script.contains("]7;file://"));
        assert!(script.contains("]133;D;"));
        assert!(script.contains("PSConsoleHostReadLine"));
        assert!(script.contains("__senjuInstalled"));
        // 5.1 compatibility: no 7+-only escapes or operators.
        assert!(!script.contains("`e") && !script.contains("`a") && !script.contains("??"));
    }

    #[test]
    fn pwsh_is_skipped_when_args_decide_what_runs() {
        for args in [
            vec!["-Command", "ls"],
            vec!["-c", "ls"],
            vec!["-File", "x.ps1"],
            vec!["-f", "x.ps1"],
            vec!["-EncodedCommand", "AAAA"],
            vec!["-ENC", "AAAA"],
            vec!["-e", "AAAA"],
            vec!["-ec", "AAAA"],
            vec!["-NoExit"],
            vec!["script.ps1"],
            vec!["-ExecutionPolicy", "Bypass"],
        ] {
            let args: Vec<String> = args.into_iter().map(String::from).collect();
            assert!(build("pwsh", &args).is_none(), "{args:?} should skip injection");
        }
    }

    #[test]
    fn pwsh_is_injected_alongside_plain_switches() {
        let args = vec!["-NoLogo".to_string(), "-NoProfile".to_string()];
        assert!(build("powershell.exe", &args).is_some());
    }
}
