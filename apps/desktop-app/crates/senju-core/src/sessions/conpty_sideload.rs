//! Pins the bundled side-by-side ConPTY (`conpty.dll` + `OpenConsole.exe`,
//! shipped next to the executable — see `src-tauri/conpty/`) into the process
//! before portable-pty looks for it.
//!
//! Why: portable-pty tries `LoadLibrary("conpty.dll")` by bare name and falls
//! back to the inbox ConPTY in kernel32, which drops the OSC 7 / OSC 133
//! sequences the shell integration relies on. A bare-name load depends on the
//! process's DLL search settings, which a dependency can narrow with
//! `SetDefaultDllDirectories` and thereby exclude the application directory.
//! Loading the DLL here by absolute path makes the later bare-name
//! `LoadLibrary` bind to the module that is already loaded, whatever the search
//! settings are. The module is intentionally leaked: it must stay loaded for
//! the life of the process.
//!
//! It is loaded only when `OpenConsole.exe` sits beside it. `conpty.dll`
//! launches the console host from its own directory and silently falls back to
//! the system conhost when it is missing, which would be a mismatched pairing;
//! without both files we leave portable-pty to its own fallback.
//!
//! On every other platform this is a no-op.

use std::path::Path;
use std::sync::Once;

const CONPTY_DLL: &str = "conpty.dll";
const OPEN_CONSOLE_EXE: &str = "OpenConsole.exe";

/// Loads the bundled ConPTY once per process (Windows only; no-op elsewhere).
pub(crate) fn ensure_loaded() {
    static LOAD: Once = Once::new();
    LOAD.call_once(|| {
        #[cfg(windows)]
        {
            if let Some(dir) = std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf))
            {
                if let Some(dll) = sideload_candidate(&dir) {
                    load_leaking(&dll);
                }
            }
        }
    });
}

/// The decision: the DLL to load from `dir`, or `None` unless both the DLL and
/// `OpenConsole.exe` exist there. Pure so it is testable on any platform.
#[cfg_attr(not(windows), allow(dead_code))]
fn sideload_candidate(dir: &Path) -> Option<std::path::PathBuf> {
    let dll = dir.join(CONPTY_DLL);
    (dll.is_file() && dir.join(OPEN_CONSOLE_EXE).is_file()).then_some(dll)
}

#[cfg(windows)]
fn load_leaking(dll: &Path) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

    let wide: Vec<u16> = dll.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the call; the
    // reserved file-handle argument must be null. The returned module handle is
    // deliberately never freed. Failure (null) just leaves portable-pty on its
    // own lookup/fallback, so the result is ignored.
    let _ = unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn loads_only_when_both_files_exist() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(sideload_candidate(dir.path()), None);

        touch(dir.path(), CONPTY_DLL);
        assert_eq!(sideload_candidate(dir.path()), None, "dll without OpenConsole.exe");

        touch(dir.path(), OPEN_CONSOLE_EXE);
        assert_eq!(sideload_candidate(dir.path()), Some(dir.path().join(CONPTY_DLL)));
    }

    #[test]
    fn openconsole_alone_is_not_enough() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), OPEN_CONSOLE_EXE);
        assert_eq!(sideload_candidate(dir.path()), None);
    }

    #[test]
    fn directories_do_not_count_as_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(CONPTY_DLL)).unwrap();
        fs::create_dir(dir.path().join(OPEN_CONSOLE_EXE)).unwrap();
        assert_eq!(sideload_candidate(dir.path()), None);
    }

    #[test]
    fn ensure_loaded_is_idempotent() {
        ensure_loaded();
        ensure_loaded();
    }
}
