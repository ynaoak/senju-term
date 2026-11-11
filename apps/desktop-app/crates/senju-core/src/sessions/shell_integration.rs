//! Auto-injects shell-integration hooks into local shell sessions, without
//! ever editing the user's own rc files:
//!
//! - **OSC 133** — the prompt/command/output markers the frontend's
//!   command-block UI reads.
//! - **OSC 7** — the working directory, reported after every command. The
//!   child process `cd`s on its own and the parent is never told, so a feature
//!   that depends on the directory (the task panel reading `package.json` next
//!   to you) needs a source for it.
//! - **The cwd file** — the same directory again, through a channel that does
//!   *not* travel the PTY byte stream. Why it exists: on Windows portable-pty
//!   uses the inbox ConPTY (`CreatePseudoConsole` in kernel32), which parses
//!   the output and silently drops OSC sequences it does not know — OSC 7
//!   never reaches the frontend, so PowerShell showed "working directory
//!   unknown". The ConPTY bundled next to the executable (`src-tauri/conpty/`,
//!   loaded by `conpty_sideload`) now passes OSC 7/133 through verbatim, so
//!   this file is the fallback for when that ConPTY is absent (a build without
//!   it) and for cmd, which has no OSC hook. Every integrated shell therefore also gets `SENJU_CWD_FILE`
//!   (a file inside the session's temp dir) and overwrites it from its prompt
//!   hook; `LocalReader` polls it and forwards changes as a `cwd` event. The
//!   file is additive: OSC 7/133 output is unchanged and still drives command
//!   blocks (and the directory on Unix). Format: line 1 is the path as the
//!   shell sees it, optional line 2 is `$WSL_DISTRO_NAME` (empty or absent
//!   outside WSL) — see [`resolve_reported_cwd`] for how that becomes a path
//!   the Windows side can use (`/c/Users/x` from Git Bash, `\\wsl$\Ubuntu\...`
//!   from WSL).
//!
//! Detected purely by executable basename (lowercased file stem, so
//! `cmd.exe` is `cmd`) — bash/zsh/fish, nushell, PowerShell (`pwsh` /
//! `powershell`), cmd.exe and `wsl` are recognized; every other shell (a
//! custom REPL, …) is left untouched. Each shell gets a different mechanism,
//! chosen to avoid fighting the user's own configuration:
//!
//! - **bash**: `--rcfile <generated>` that sources the real `~/.bashrc`
//!   first, then appends the hooks. Applied when the profile has no args, or
//!   only `-l`/`--login`/`-i` (Git Bash's default profile is `--login -i`).
//!   bash ignores `--rcfile` in login mode, so those flags are dropped from
//!   the launch (`Integration::replace_args`) and the generated rcfile
//!   emulates login startup instead. Any other arg (`-c`, a script, …) means
//!   no integration: we never guess at how to combine `--rcfile` with it.
//! - **zsh**: `ZDOTDIR` pointed at a generated directory whose `.zshenv` /
//!   `.zprofile` / `.zshrc` / `.zlogin` each just source the real file from
//!   the user's actual zdotdir (`$ZDOTDIR` if the environment already set
//!   one, else `$HOME`) — so nothing the user's config relies on is skipped.
//!   Safe regardless of the profile's args (env-var based, not a flag).
//! - **fish**: `-C '<commands>'`, fish's built-in "run this after config
//!   loads" flag — no generated files needed at all.
//! - **nushell**: suffix args `--execute <script>` appending a `pre_prompt`
//!   hook that writes `$env.PWD` to the cwd file. nu already emits OSC 7 and
//!   133 natively, so only the file is added. Skipped when the profile's args
//!   already decide what nu runs.
//! - **PowerShell** (`pwsh` 7 and Windows PowerShell 5.1): suffix args
//!   `-NoExit -EncodedCommand <base64>` that wrap the `prompt` function (and
//!   `PSConsoleHostReadLine` when PSReadLine provides it). `$PROFILE` is never
//!   edited and still loads first. Skipped when the profile's args already
//!   decide what the shell runs (see `build_pwsh`).
//! - **cmd.exe**: nothing to inject — it has no prompt hook. Its directory
//!   is read from the child's PEB instead (`CwdSource::Process`, see
//!   `proc_cwd`). No OSC 133 either, so no command blocks.
//! - **WSL** (`wsl.exe`): the Linux side is the real shell, so the launch is
//!   rewritten to `wsl -e sh -c 'exec sh "$SENJU_WSL_DIR/wsl.sh"'`. The
//!   launcher picks the user's login shell and starts it with generated
//!   bash/zsh/fish hooks (`WSLENV`'s `/p` flag translates the two Windows
//!   paths into Linux ones). Only applied when the profile's args are limited
//!   to distro/user/directory selection.
//!
//! Building an `Integration` is best-effort: any I/O failure (temp dir
//! unwritable, disk full, …) just returns `None`, so a shell always launches
//! even if integration can't be wired up.

use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Where the session's current directory can be read from, besides OSC 7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CwdSource {
    /// A file the shell's prompt hook rewrites (`SENJU_CWD_FILE`).
    File(PathBuf),
    /// Ask the OS for the child process's current directory (cmd.exe).
    Process,
}

/// Extra launch parameters that make a shell emit the OSC markers. The
/// `guard` (when present) owns the generated temp directory and must be kept
/// alive for as long as the spawned shell might still be starting up.
pub struct Integration {
    pub prefix_args: Vec<String>,
    pub suffix_args: Vec<String>,
    /// When set, launch with these instead of the profile's own args (bash:
    /// the profile's `-l`/`--login`/`-i` are dropped, see `build_bash`).
    pub replace_args: Option<Vec<String>>,
    pub env: Vec<(String, String)>,
    pub guard: Option<tempfile::TempDir>,
    pub cwd_source: Option<CwdSource>,
}

impl Integration {
    /// An integration with a fresh temp dir, a cwd file path inside it and the
    /// `SENJU_CWD_FILE` env entry already filled in.
    fn with_cwd_file() -> Option<(Self, PathBuf)> {
        let dir = tempfile::tempdir().ok()?;
        let file = dir.path().join("cwd");
        let integ = Integration {
            prefix_args: vec![],
            suffix_args: vec![],
            replace_args: None,
            env: vec![("SENJU_CWD_FILE".into(), file.to_string_lossy().into_owned())],
            cwd_source: Some(CwdSource::File(file.clone())),
            guard: Some(dir),
        };
        Some((integ, file))
    }

    fn dir(&self) -> &Path {
        self.guard.as_ref().expect("with_cwd_file sets the guard").path()
    }
}

/// Turns what a shell wrote into the cwd file (or what a process query
/// returned) into a path the app can use. `windows` is passed in rather than
/// read from `cfg!` so the branches are testable everywhere; callers pass
/// `cfg!(windows)`. It must be false off Windows: a Linux GUI under WSLg also
/// has `WSL_DISTRO_NAME` set, and its paths are already native.
///
/// - line 2 (distro) set and line 1 absolute POSIX → the `\\wsl$\<distro>`
///   UNC form, which Explorer and Win32 file APIs resolve into the distro;
/// - line 1 like `/c` or `/c/Users/x` (MSYS / Git Bash) → `C:\Users\x`;
/// - anything else unchanged.
pub(super) fn resolve_reported_cwd(raw: &str, windows: bool) -> Option<String> {
    let mut lines = raw.split('\n').map(|l| l.trim_end_matches('\r'));
    let path = lines.next().unwrap_or("");
    if path.is_empty() {
        return None;
    }
    let distro = lines.next().unwrap_or("").trim();
    if windows {
        if !distro.is_empty() && path.starts_with('/') {
            return Some(format!("\\\\wsl$\\{distro}{}", path.replace('/', "\\")));
        }
        let b = path.as_bytes();
        if b.len() >= 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() && (b.len() == 2 || b[2] == b'/') {
            let rest = path.get(3..).unwrap_or("");
            return Some(format!(
                "{}:\\{}",
                (b[1] as char).to_ascii_uppercase(),
                rest.replace('/', "\\")
            ));
        }
    }
    Some(path.to_string())
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
//
// The cwd file is written with plain `printf` and no command substitution: it
// runs at every prompt, and a fork per prompt is felt on Git Bash, where
// process creation is slow. `>|` so a user's `set -o noclobber` cannot make it
// fail; the `2>/dev/null` comes first so a failed open stays silent.
const BASH_SNIPPET: &str = r#"__senju_cwd() { local p=${PWD//%/%25}; printf '\e]7;file://%s%s\a' "${HOSTNAME:-}" "${p// /%20}"; }
__senju_cwdfile() { [ -n "${SENJU_CWD_FILE:-}" ] && printf '%s\n%s\n' "$PWD" "${WSL_DISTRO_NAME:-}" 2>/dev/null >| "$SENJU_CWD_FILE"; return 0; }
__senju_precmd() { __senju_cwd; __senju_cwdfile; printf '\e]133;D;%s\a' "$1"; return "$1"; }
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

const ZSH_SNIPPET: &str = r#"precmd() { print -Pn '\e]133;D;%?\a\e]133;A\a'; local p=${PWD//\%/%25}; print -n "\e]7;file://${HOST}${p// /%20}\a"; [[ -n ${SENJU_CWD_FILE:-} ]] && print -r -- "$PWD"$'\n'"${WSL_DISTRO_NAME:-}" 2>/dev/null >| "$SENJU_CWD_FILE" }
preexec() { print -n '\e]133;C\a' }
PS1="$PS1"$'%{\e]133;B\a%}'
"#;

const FISH_SNIPPET: &str = r#"function __senju_blocks_prompt --on-event fish_prompt
    printf '\e]133;D;%s\a\e]133;A\a' $status
    printf '\e]7;file://%s%s\a' (hostname) (string replace -a ' ' '%20' -- (string replace -a '%' '%25' -- $PWD))
    if set -q SENJU_CWD_FILE
        printf '%s\n%s\n' $PWD "$WSL_DISTRO_NAME" 2>/dev/null >$SENJU_CWD_FILE
    end
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
//
// On Windows the inbox ConPTY drops OSC 7, so the directory is also written
// straight to the file named by `SENJU_CWD_FILE` — the part that actually
// reaches the task pane there. `ProviderPath` is the native path, never a
// provider-qualified `Microsoft.PowerShell.Core\FileSystem::` one.
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
      if ($env:SENJU_CWD_FILE) { try { [System.IO.File]::WriteAllText($env:SENJU_CWD_FILE, $__senjuLoc.ProviderPath) } catch {} }
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
    // Split on both separators by hand: `Path` only knows the host's, and a
    // Windows profile path must be recognized the same way everywhere.
    let base = shell.rsplit(['/', '\\']).next()?;
    let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem);
    if stem.is_empty() {
        return None;
    }
    let name = stem.to_ascii_lowercase();
    match name.as_str() {
        "bash" => build_bash(existing_args),
        "zsh" => build_zsh(),
        "fish" => build_fish(),
        "nu" => build_nu(existing_args),
        "pwsh" | "powershell" => build_pwsh(existing_args),
        "cmd" => build_cmd(),
        "wsl" => build_wsl(existing_args),
        _ => None,
    }
}

/// The rcfile body bash is started with. bash ignores `--rcfile` for login
/// shells, so a profile that asked for `--login` is run non-login and this
/// reproduces what a login shell would have read: `/etc/profile`, then the
/// first of `~/.bash_profile`, `~/.bash_login`, `~/.profile`. (A plain
/// interactive shell reads `~/.bashrc`, which login profiles conventionally
/// source themselves.)
fn bash_rcfile(login: bool) -> String {
    let startup = if login {
        "[ -f /etc/profile ] && . /etc/profile\n\
         if [ -f ~/.bash_profile ]; then . ~/.bash_profile\n\
         elif [ -f ~/.bash_login ]; then . ~/.bash_login\n\
         elif [ -f ~/.profile ]; then . ~/.profile\n\
         fi\n"
    } else {
        "[ -f ~/.bashrc ] && . ~/.bashrc\n"
    };
    format!("# Senju Term shell integration (auto-generated)\n{startup}{BASH_SNIPPET}")
}

fn build_bash(existing_args: &[String]) -> Option<Integration> {
    // `--rcfile` competes with flags like `-c` the user may have chosen for
    // this profile, so only a combination of the login/interactive flags is
    // accepted: Git Bash's default profile is `--login -i`, and macOS users
    // often have `-l`. Anything else → no integration.
    let mut login = false;
    for arg in existing_args {
        match arg.as_str() {
            "-l" | "--login" => login = true,
            "-i" => {}
            _ => return None,
        }
    }
    let (mut integ, _) = Integration::with_cwd_file()?;
    let rcfile = integ.dir().join("bashrc");
    std::fs::write(&rcfile, bash_rcfile(login)).ok()?;
    integ.prefix_args = vec![
        "--rcfile".into(),
        rcfile.to_string_lossy().into_owned(),
        "-i".into(),
    ];
    // The accepted flags are all either replaced by ours (`-i`) or emulated
    // in the rcfile (`-l`), so launching with none of them is equivalent.
    if !existing_args.is_empty() {
        integ.replace_args = Some(vec![]);
    }
    Some(integ)
}

/// Writes the four proxy dotfiles into `dir`. `real_expr` is the shell
/// expression (`$SENJU_ZDOTDIR` or `$HOME`) naming the user's real zdotdir.
fn write_zsh_proxy(dir: &Path, real_expr: &str) -> Option<()> {
    let write = |name: &str, body: String| -> Option<()> {
        std::fs::File::create(dir.join(name))
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
    )
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
    let (mut integ, _) = Integration::with_cwd_file()?;
    write_zsh_proxy(integ.dir(), real_expr)?;
    let zdotdir = integ.dir().to_string_lossy().into_owned();
    integ.env.push(("ZDOTDIR".to_string(), zdotdir));
    if let Some(real) = real {
        integ.env.push(("SENJU_ZDOTDIR".to_string(), real));
    }
    Some(integ)
}

fn build_fish() -> Option<Integration> {
    // fish's -C runs after the user's own config.fish loads — no generated
    // files for the hooks, and it composes fine with whatever other args the
    // profile has. The temp dir exists only to hold the cwd file.
    let (mut integ, _) = Integration::with_cwd_file()?;
    integ.suffix_args = vec!["-C".into(), FISH_SNIPPET.into()];
    Some(integ)
}

// nu already emits OSC 7 and 133 itself, so only the cwd file is added. The
// hook is appended to whatever `pre_prompt` the user's config set (it may be
// unset, `$env.config.hooks.pre_prompt?` yields null then), and `try` keeps a
// failed write from printing an error at every prompt. Valid on nu 0.90+.
const NU_SCRIPT: &str = "$env.config.hooks.pre_prompt = ($env.config.hooks.pre_prompt? | default [] | append {|| try { $env.PWD | save -f $env.SENJU_CWD_FILE } })";

fn build_nu(existing_args: &[String]) -> Option<Integration> {
    // `--execute` runs the script after config/env load and then stays
    // interactive, so it composes with plain switches like `--login` — but a
    // profile that already says what nu runs (`-c`, `--execute`, a script
    // path as a positional) would collide with it.
    for arg in existing_args {
        let flag = arg.split('=').next().unwrap_or(arg);
        if !arg.starts_with('-')
            || matches!(flag, "-c" | "--commands" | "-e" | "--execute")
        {
            return None;
        }
    }
    let (mut integ, _) = Integration::with_cwd_file()?;
    integ.suffix_args = vec!["--execute".into(), NU_SCRIPT.into()];
    Some(integ)
}

fn build_cmd() -> Option<Integration> {
    // cmd.exe has no prompt hook, so nothing can be injected and no OSC 133 is
    // emitted (no command blocks). Its directory is read from the process.
    Some(Integration {
        prefix_args: vec![],
        suffix_args: vec![],
        replace_args: None,
        env: vec![],
        guard: None,
        cwd_source: Some(CwdSource::Process),
    })
}

// The launcher `wsl -e sh -c 'exec sh "$SENJU_WSL_DIR/wsl.sh"'` runs inside
// the distro. It starts the user's login shell (from passwd, as `wsl` itself
// would, falling back to `$SHELL`) with the matching generated hooks. Shell
// state that has to survive `exec` is exported explicitly: `VAR=x exec ...`
// is not portable across sh implementations.
const WSL_LAUNCHER: &str = r#"s=$(getent passwd "$(id -un)" 2>/dev/null | cut -d: -f7)
[ -n "$s" ] && [ -x "$s" ] || s=${SHELL:-/bin/sh}
[ -x "$s" ] || s=/bin/sh
case "${s##*/}" in
  bash) exec "$s" --rcfile "$SENJU_WSL_DIR/bashrc" -i ;;
  zsh) ZDOTDIR="$SENJU_WSL_DIR/zdotdir"; export ZDOTDIR; exec "$s" -l ;;
  fish) exec "$s" -l -C "source \"$SENJU_WSL_DIR/senju.fish\"" ;;
  *) exec "$s" -l ;;
esac
"#;

fn build_wsl(existing_args: &[String]) -> Option<Integration> {
    // Only distro/user/directory selection is understood. Anything else
    // (`-e`, `--exec`, a command, `--system`, …) decides what runs, and the
    // launch is rewritten to run our launcher instead.
    let mut it = existing_args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "~" => {}
            "-d" | "--distribution" | "-u" | "--user" | "--cd" => {
                it.next()?;
            }
            _ => return None,
        }
    }
    let (mut integ, cwd_file) = Integration::with_cwd_file()?;
    let dir = integ.dir().to_path_buf();
    // A Windows-side ZDOTDIR means nothing inside Linux, so the proxy always
    // defers to the Linux `$HOME`.
    let zdotdir = dir.join("zdotdir");
    std::fs::create_dir(&zdotdir).ok()?;
    write_zsh_proxy(&zdotdir, "$HOME")?;
    std::fs::write(dir.join("bashrc"), bash_rcfile(true)).ok()?;
    std::fs::write(dir.join("senju.fish"), FISH_SNIPPET).ok()?;
    std::fs::write(dir.join("wsl.sh"), WSL_LAUNCHER).ok()?;
    // `/p` makes WSL translate these Windows paths into Linux paths.
    let mut wslenv = std::env::var("WSLENV").unwrap_or_default();
    if !wslenv.is_empty() {
        wslenv.push(':');
    }
    wslenv.push_str("SENJU_WSL_DIR/p:SENJU_CWD_FILE/p");
    integ.env = vec![
        ("SENJU_WSL_DIR".into(), dir.to_string_lossy().into_owned()),
        ("SENJU_CWD_FILE".into(), cwd_file.to_string_lossy().into_owned()),
        ("WSLENV".into(), wslenv),
    ];
    integ.suffix_args = vec![
        "-e".into(),
        "sh".into(),
        "-c".into(),
        r#"exec sh "$SENJU_WSL_DIR/wsl.sh""#.into(),
    ];
    Some(integ)
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
    // - it works identically on Windows PowerShell 5.1 and pwsh 7.
    // `-NoExit` keeps the shell interactive after the script has run. The
    // script itself is not written to disk; the temp dir only holds the cwd
    // file (`SENJU_CWD_FILE`) the prompt wrapper writes.
    let utf16le: Vec<u8> = PWSH_SNIPPET.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let (mut integ, _) = Integration::with_cwd_file()?;
    integ.suffix_args =
        vec!["-NoExit".into(), "-EncodedCommand".into(), base64_encode(&utf16le)];
    Some(integ)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrecognized_shell_is_untouched() {
        assert!(build("/bin/dash", &[]).is_none());
        assert!(build("sh", &[]).is_none());
        assert!(build("python3", &[]).is_none());
    }

    #[test]
    fn bash_wraps_only_when_args_are_empty() {
        let integ = build("/bin/bash", &[]).expect("bash should be recognized");
        assert_eq!(integ.prefix_args[0], "--rcfile");
        let rcfile = std::fs::read_to_string(&integ.prefix_args[1]).unwrap();
        assert!(rcfile.contains("PROMPT_COMMAND"));
        assert!(rcfile.contains(". ~/.bashrc"));

        assert!(integ.replace_args.is_none());
        assert!(build("/bin/bash", &["-c".into(), "ls".into()]).is_none());
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
        let dir = env_of(&integ, "ZDOTDIR");
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

        let dir = env_of(&integ, "ZDOTDIR");
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
        assert!(integ.guard.is_some());
        assert_eq!(integ.suffix_args[0], "-C");
        assert!(integ.suffix_args[1].contains("fish_prompt"));
        assert!(integ.suffix_args[1].contains("SENJU_CWD_FILE"));
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
        assert!(integ.guard.is_some(), "the cwd file's temp dir must be kept alive");
        assert!(integ.env.iter().any(|(k, _)| k == "SENJU_CWD_FILE"));
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

    /// The prompt wrapper must also write the cwd file (the part that works on
    /// Windows, where ConPTY drops OSC 7), and the raw string it lives in must
    /// never contain a `#` that could end the Rust raw-string delimiter.
    #[test]
    fn pwsh_snippet_writes_the_cwd_file_and_has_no_hash() {
        assert!(PWSH_SNIPPET.contains(
            "if ($env:SENJU_CWD_FILE) { try { [System.IO.File]::WriteAllText($env:SENJU_CWD_FILE, $__senjuLoc.ProviderPath) } catch {} }"
        ));
        assert!(!PWSH_SNIPPET.contains('#'));
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

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn env_of<'a>(integ: &'a Integration, key: &str) -> &'a str {
        integ.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).unwrap()
    }

    #[test]
    fn every_posix_shell_gets_a_cwd_file_inside_its_guard() {
        for shell in ["bash", "zsh", "fish", "pwsh", "nu"] {
            let integ = build(shell, &[]).unwrap_or_else(|| panic!("{shell}"));
            let file = env_of(&integ, "SENJU_CWD_FILE");
            assert!(Path::new(file).is_absolute(), "{shell}: {file}");
            assert!(
                Path::new(file).starts_with(integ.guard.as_ref().unwrap().path()),
                "{shell}: the file must live in the guarded temp dir"
            );
            assert_eq!(integ.cwd_source, Some(CwdSource::File(PathBuf::from(file))));
        }
    }

    #[test]
    fn resolver_leaves_paths_alone_off_windows() {
        // A Linux GUI under WSLg has WSL_DISTRO_NAME set too: no conversion.
        assert_eq!(
            resolve_reported_cwd("/home/me/proj\nUbuntu\n", false).as_deref(),
            Some("/home/me/proj")
        );
        assert_eq!(resolve_reported_cwd("/c/Users/x\n", false).as_deref(), Some("/c/Users/x"));
    }

    #[test]
    fn resolver_handles_empty_and_line_endings() {
        for raw in ["", "\n", "\r\n", "\nUbuntu\n"] {
            assert_eq!(resolve_reported_cwd(raw, true), None, "{raw:?}");
            assert_eq!(resolve_reported_cwd(raw, false), None, "{raw:?}");
        }
        assert_eq!(resolve_reported_cwd("C:\\Users\\me\r\n", true).as_deref(), Some("C:\\Users\\me"));
        assert_eq!(resolve_reported_cwd("C:\\Users\\me", true).as_deref(), Some("C:\\Users\\me"));
        // Absent or empty distro: line 1 as is.
        assert_eq!(resolve_reported_cwd("/home/me\n\n", true).as_deref(), Some("/home/me"));
    }

    #[test]
    fn resolver_maps_wsl_paths_to_unc() {
        assert_eq!(
            resolve_reported_cwd("/home/me/proj\nUbuntu\n", true).as_deref(),
            Some("\\\\wsl$\\Ubuntu\\home\\me\\proj")
        );
        assert_eq!(
            resolve_reported_cwd("/home/me\r\nUbuntu-22.04\r\n", true).as_deref(),
            Some("\\\\wsl$\\Ubuntu-22.04\\home\\me")
        );
        // A distro with a non-POSIX line 1 (cannot happen in practice) is not rewritten.
        assert_eq!(resolve_reported_cwd("C:\\x\nUbuntu\n", true).as_deref(), Some("C:\\x"));
        // WSL wins over the MSYS form: /c in a distro is a real directory.
        assert_eq!(
            resolve_reported_cwd("/c/x\nUbuntu\n", true).as_deref(),
            Some("\\\\wsl$\\Ubuntu\\c\\x")
        );
    }

    #[test]
    fn resolver_maps_msys_paths_to_drive_letters() {
        for (raw, want) in [
            ("/c", "C:\\"),
            ("/c/", "C:\\"),
            ("/c/Users/x", "C:\\Users\\x"),
            ("/d/a b/c", "D:\\a b\\c"),
            ("/C/Users", "C:\\Users"),
        ] {
            assert_eq!(resolve_reported_cwd(raw, true).as_deref(), Some(want), "{raw}");
        }
        // Not drive forms: untouched.
        for raw in ["/", "/cc", "/cc/x", "/usr/bin", "/1/x"] {
            assert_eq!(resolve_reported_cwd(raw, true).as_deref(), Some(raw), "{raw}");
        }
    }

    #[test]
    fn bash_accepts_login_and_interactive_flags_only() {
        for args in [vec!["-l"], vec!["--login"], vec!["-i"], vec!["--login", "-i"], vec!["-i", "-l"]] {
            let integ = build("bash.exe", &s(&args)).unwrap_or_else(|| panic!("{args:?}"));
            assert_eq!(integ.replace_args, Some(vec![]), "{args:?}");
            assert_eq!(integ.prefix_args[0], "--rcfile");
            assert_eq!(integ.prefix_args[2], "-i");
        }
        for args in [vec!["-c", "ls"], vec!["--norc"], vec!["script.sh"], vec!["-l", "-x"], vec!["--rcfile", "x"]] {
            assert!(build("bash", &s(&args)).is_none(), "{args:?}");
        }
    }

    #[test]
    fn bash_login_rcfile_emulates_login_startup() {
        let login = build("bash", &s(&["--login", "-i"])).unwrap();
        let rc = std::fs::read_to_string(&login.prefix_args[1]).unwrap();
        let etc = rc.find(". /etc/profile").expect("/etc/profile");
        let bp = rc.find(". ~/.bash_profile").expect(".bash_profile");
        let bl = rc.find(". ~/.bash_login").expect(".bash_login");
        let pr = rc.find(". ~/.profile").expect(".profile");
        assert!(etc < bp && bp < bl && bl < pr, "wrong startup order:\n{rc}");
        assert!(rc.contains("elif"), "only the first existing profile may be read");
        assert!(!rc.contains(". ~/.bashrc"), "login startup leaves .bashrc to the profile");
        assert!(rc.contains("__senju_cwdfile"));

        let plain = build("bash", &[]).unwrap();
        let rc = std::fs::read_to_string(&plain.prefix_args[1]).unwrap();
        assert!(rc.contains(". ~/.bashrc") && !rc.contains("/etc/profile"));
    }

    #[test]
    fn nu_arg_rules_and_script() {
        let integ = build("nu.exe", &s(&["--login", "--no-config-file"])).expect("plain switches");
        assert_eq!(integ.suffix_args[0], "--execute");
        let script = &integ.suffix_args[1];
        assert!(script.contains("pre_prompt"));
        assert!(script.contains("save -f $env.SENJU_CWD_FILE"));
        assert!(script.contains("append"));
        assert!(integ.prefix_args.is_empty() && integ.replace_args.is_none());
        assert!(build("nu", &[]).is_some());
        for args in [
            vec!["-c", "ls"],
            vec!["--commands", "ls"],
            vec!["--commands=ls"],
            vec!["-e", "ls"],
            vec!["--execute", "ls"],
            vec!["script.nu"],
            vec!["--login", "script.nu"],
        ] {
            assert!(build("nu", &s(&args)).is_none(), "{args:?}");
        }
    }

    #[test]
    fn cmd_uses_the_process_source_and_changes_nothing() {
        for shell in ["cmd", "cmd.exe", "C:\\Windows\\System32\\CMD.EXE"] {
            let integ = build(shell, &s(&["/k", "echo hi"])).unwrap_or_else(|| panic!("{shell}"));
            assert_eq!(integ.cwd_source, Some(CwdSource::Process));
            assert!(integ.prefix_args.is_empty() && integ.suffix_args.is_empty());
            assert!(integ.env.is_empty() && integ.replace_args.is_none() && integ.guard.is_none());
        }
    }

    #[test]
    fn wsl_arg_rules() {
        for args in [
            vec![],
            vec!["~"],
            vec!["-d", "Ubuntu"],
            vec!["--distribution", "Ubuntu", "-u", "root"],
            vec!["--user", "me", "--cd", "/tmp"],
            vec!["~", "-d", "Debian"],
        ] {
            assert!(build("wsl.exe", &s(&args)).is_some(), "{args:?}");
        }
        for args in [
            vec!["-d"],
            vec!["-e", "ls"],
            vec!["--exec", "ls"],
            vec!["ls"],
            vec!["--system"],
            vec!["-d", "Ubuntu", "htop"],
        ] {
            assert!(build("wsl", &s(&args)).is_none(), "{args:?}");
        }
    }

    #[test]
    fn wsl_generates_launcher_files_env_and_args() {
        // SAFETY: tests that touch WSLENV are the only ones in this module.
        let previous = std::env::var("WSLENV").ok();
        unsafe { std::env::set_var("WSLENV", "FOO/u") };
        let with_existing = build("wsl", &[]).unwrap();
        unsafe { std::env::remove_var("WSLENV") };
        let without = build("wsl", &[]).unwrap();
        if let Some(v) = previous {
            unsafe { std::env::set_var("WSLENV", v) };
        }
        assert_eq!(env_of(&with_existing, "WSLENV"), "FOO/u:SENJU_WSL_DIR/p:SENJU_CWD_FILE/p");
        assert_eq!(env_of(&without, "WSLENV"), "SENJU_WSL_DIR/p:SENJU_CWD_FILE/p");

        let integ = with_existing;
        assert_eq!(
            integ.suffix_args,
            s(&["-e", "sh", "-c", "exec sh \"$SENJU_WSL_DIR/wsl.sh\""])
        );
        let dir = PathBuf::from(env_of(&integ, "SENJU_WSL_DIR"));
        assert_eq!(dir, integ.guard.as_ref().unwrap().path());
        assert_eq!(
            Path::new(env_of(&integ, "SENJU_CWD_FILE")),
            dir.join("cwd"),
        );
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        let launcher = read("wsl.sh");
        for needle in [
            "getent passwd \"$(id -un)\"",
            "--rcfile \"$SENJU_WSL_DIR/bashrc\" -i",
            "ZDOTDIR=\"$SENJU_WSL_DIR/zdotdir\"",
            "exec \"$s\" -l -C",
            "senju.fish",
        ] {
            assert!(launcher.contains(needle), "{needle}\n{launcher}");
        }
        assert!(read("bashrc").contains("/etc/profile") && read("bashrc").contains("__senju_cwdfile"));
        assert!(read("senju.fish").contains("fish_prompt"));
        // Windows-side ZDOTDIR is meaningless in Linux: always $HOME.
        for rc in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            let body = read(&format!("zdotdir/{rc}"));
            assert!(body.contains("$HOME") && !body.contains("SENJU_ZDOTDIR"), "{rc}: {body}");
        }
        for f in ["wsl.sh", "bashrc", "senju.fish", "zdotdir/.zshrc"] {
            assert!(!read(f).contains('\r'), "{f} must use LF endings");
        }
    }

    // -- end to end against real shells ---------------------------------------

    fn which(bin: &str) -> bool {
        std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
    }

    /// Runs `cmd` with `stdin_script` fed on stdin, in an empty HOME so the
    /// developer's own rc files cannot interfere, and returns the cwd file.
    fn run_and_read_cwd_file(
        mut cmd: std::process::Command,
        integ: &Integration,
        home: &Path,
        stdin_script: &str,
    ) -> String {
        use std::process::Stdio;
        // Clear an inherited ZDOTDIR *before* applying the integration env:
        // `env_remove` after `env` would drop the integration's own ZDOTDIR.
        cmd.env_remove("ZDOTDIR").env("HOME", home);
        for (k, v) in &integ.env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin_script.as_bytes()).unwrap();
        child.wait().unwrap();
        std::fs::read_to_string(env_of(integ, "SENJU_CWD_FILE")).unwrap_or_default()
    }

    #[test]
    fn bash_prompt_writes_the_cwd_file() {
        if !which("bash") {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = std::fs::canonicalize(target_dir.path()).unwrap();
        for args in [vec![], vec!["--login", "-i"]] {
            let integ = build("bash", &s(&args)).unwrap();
            let mut cmd = std::process::Command::new("bash");
            cmd.args(&integ.prefix_args);
            cmd.args(integ.replace_args.as_ref().unwrap_or(&s(&args)));
            let got = run_and_read_cwd_file(
                cmd,
                &integ,
                home.path(),
                &format!("cd '{}'\ntrue\nexit\n", target.display()),
            );
            let mut lines = got.lines();
            assert_eq!(lines.next(), Some(target.to_str().unwrap()), "{args:?}: {got:?}");
            assert_eq!(lines.next(), Some(""), "line 2 is the (empty) distro: {got:?}");
        }
    }

    #[test]
    fn bash_login_launch_really_reads_the_login_profile() {
        if !which("bash") {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".bash_profile"), "touch \"$HOME/profile-ran\"\n").unwrap();
        std::fs::write(home.path().join(".profile"), "touch \"$HOME/wrong-profile-ran\"\n").unwrap();
        let integ = build("bash", &s(&["-l"])).unwrap();
        let mut cmd = std::process::Command::new("bash");
        cmd.args(&integ.prefix_args);
        run_and_read_cwd_file(cmd, &integ, home.path(), "exit\n");
        assert!(home.path().join("profile-ran").exists());
        assert!(!home.path().join("wrong-profile-ran").exists(), "only the first existing profile");
    }

    #[test]
    fn zsh_prompt_writes_the_cwd_file() {
        if !which("zsh") {
            eprintln!("skipping: zsh is not installed");
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = std::fs::canonicalize(target_dir.path()).unwrap();
        let integ = build("zsh", &[]).unwrap();
        let mut cmd = std::process::Command::new("zsh");
        cmd.arg("-i");
        // ZDOTDIR comes from the integration env, applied after env_remove.
        let got = run_and_read_cwd_file(
            cmd,
            &integ,
            home.path(),
            &format!("cd '{}'\ntrue\nexit\n", target.display()),
        );
        assert_eq!(got.lines().next(), Some(target.to_str().unwrap()), "{got:?}");
    }

    #[test]
    fn fish_prompt_writes_the_cwd_file() {
        if !which("fish") {
            eprintln!("skipping: fish is not installed");
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = std::fs::canonicalize(target_dir.path()).unwrap();
        let integ = build("fish", &[]).unwrap();
        // fish only fires `fish_prompt` when it really draws a prompt, which
        // it never does with piped stdin; emit the event by hand instead so
        // the test needs no pty. `-C` (the integration) runs before `-c`.
        let mut cmd = std::process::Command::new("fish");
        cmd.args(&integ.suffix_args).args([
            "-c",
            &format!("cd '{}'; emit fish_prompt", target.display()),
        ]);
        cmd.env("XDG_CONFIG_HOME", home.path());
        let got = run_and_read_cwd_file(cmd, &integ, home.path(), "");
        assert_eq!(got.lines().next(), Some(target.to_str().unwrap()), "{got:?}");
    }

    /// The WSL launcher is plain POSIX sh run inside the distro, so on Linux it
    /// can be exercised for real: it must pick the login shell, start it with
    /// our rcfile, and end up writing the cwd file.
    #[test]
    fn wsl_launcher_starts_a_shell_that_writes_the_cwd_file() {
        if !which("sh") || !which("bash") || !which("getent") {
            return;
        }
        let home = tempfile::tempdir().unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = std::fs::canonicalize(target_dir.path()).unwrap();
        let integ = build("wsl", &s(&["-d", "Ubuntu"])).unwrap();
        let launcher = Path::new(env_of(&integ, "SENJU_WSL_DIR")).join("wsl.sh");
        let mut cmd = std::process::Command::new("sh");
        cmd.arg(launcher);
        cmd.env("WSL_DISTRO_NAME", "Ubuntu");
        cmd.env("SHELL", "/bin/bash");
        let got = run_and_read_cwd_file(
            cmd,
            &integ,
            home.path(),
            &format!("cd '{}'\ntrue\nexit\n", target.display()),
        );
        // Whatever the passwd shell is, it is only recorded if it is bash/zsh/fish.
        if !got.is_empty() {
            let mut lines = got.lines();
            assert_eq!(lines.next(), Some(target.to_str().unwrap()), "{got:?}");
            assert_eq!(lines.next(), Some("Ubuntu"), "{got:?}");
        }
    }
}
