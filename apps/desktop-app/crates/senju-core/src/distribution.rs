//! Where this copy of the app came from, and therefore **who is allowed to
//! update it**.
//!
//! The same binary ships through several channels — a downloaded installer, the
//! Microsoft Store, a system package manager — and each one has a different
//! owner for updates. Getting this wrong is not cosmetic:
//!
//! - A Microsoft Store build that self-updates violates Store policy, and its
//!   install directory (`…\WindowsApps\`) is read-only anyway, so the attempt
//!   fails after the user has already agreed to it.
//! - A `.deb`/`.rpm`/Homebrew install replaced in place would desynchronise the
//!   package database from what is actually on disk.
//! - An NSIS install updated with an MSI (or the reverse) leaves the machine
//!   with two registered copies of the app, because the two installers keep
//!   separate uninstall entries.
//!
//! This module answers one question — **may the in-app updater run?** The
//! separate question of *which artifact* a permitted update downloads is
//! already solved by `tauri-plugin-updater`: it looks up `{os}-{arch}-{installer}`
//! in the release manifest before falling back to `{os}-{arch}`, using an
//! installer marker the bundler stamps into each installer's copy of the binary.
//! Keeping per-format releases working is therefore a *release-manifest* job
//! (see `scripts/split-update-manifest.mjs`), not a client-side one.
//!
//! Detection is split into a pure [`detect`] over an explicit [`DetectInput`]
//! (unit tested below) and a thin [`current`] that gathers the inputs from the
//! process. Nothing here touches the network.

use std::path::Path;

/// The installer a direct download came from. Each format gets its own update
/// manifest so an update never crosses formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallerFormat {
    /// Windows `*-setup.exe` (NSIS).
    Nsis,
    /// Windows `*.msi` (WiX).
    Msi,
    /// macOS `.app` bundle, distributed in a `.dmg`.
    MacApp,
    /// Linux `.AppImage` (the only Linux format the updater can replace).
    AppImage,
}

impl InstallerFormat {
    /// Short stable id — used in the update manifest name and reported to the UI.
    pub fn id(self) -> &'static str {
        match self {
            InstallerFormat::Nsis => "nsis",
            InstallerFormat::Msi => "msi",
            InstallerFormat::MacApp => "app",
            InstallerFormat::AppImage => "appimage",
        }
    }
}

/// Who owns updates for this copy of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DistChannel {
    /// Installed from an installer downloaded from GitHub Releases. The in-app
    /// updater applies signed updates for this exact installer format.
    Direct(InstallerFormat),
    /// Installed from the Microsoft Store (the process runs with MSIX package
    /// identity). The Store applies updates; the in-app updater must stay off.
    MsStore,
    /// Installed from the Mac App Store (a `_MASReceipt` sits in the bundle).
    MacAppStore,
    /// Installed by a system package manager — apt/dnf/Homebrew. That manager
    /// applies updates.
    SystemPackage,
    /// Running out of a build directory (`cargo tauri dev`, `cargo run`). There
    /// is nothing to update.
    Development,
}

impl DistChannel {
    /// Short stable id, safe to show in diagnostics and to branch on in the UI.
    pub fn id(self) -> &'static str {
        match self {
            DistChannel::Direct(f) => match f {
                InstallerFormat::Nsis => "direct-nsis",
                InstallerFormat::Msi => "direct-msi",
                InstallerFormat::MacApp => "direct-app",
                InstallerFormat::AppImage => "direct-appimage",
            },
            DistChannel::MsStore => "msstore",
            DistChannel::MacAppStore => "macappstore",
            DistChannel::SystemPackage => "system-package",
            DistChannel::Development => "development",
        }
    }

    /// Whether the **in-app** updater should run. False means some other
    /// mechanism (a store, a package manager) owns updates and the app must not
    /// touch its own install — see the module docs for why each case matters.
    pub fn in_app_updates(self) -> bool {
        matches!(self, DistChannel::Direct(_))
    }

    /// The installer this copy came from, when it is known.
    pub fn installer_format(self) -> Option<InstallerFormat> {
        match self {
            DistChannel::Direct(f) => Some(f),
            _ => None,
        }
    }

    /// The release-manifest key the updater will look for — `{os}-{arch}` with
    /// the installer appended, mirroring `tauri-plugin-updater`'s own lookup.
    /// Used for diagnostics and for the release script's cross-check; the
    /// plugin still does the real resolution from its own bundler marker.
    pub fn updater_platform_key(self, os_arch: &str) -> Option<String> {
        self.installer_format()
            .map(|f| format!("{os_arch}-{}", f.id()))
    }

    /// Where the user should go to update instead, when updates are external.
    /// Store URIs use their OS scheme so the click lands in the right app.
    pub fn external_update_target(self) -> Option<&'static str> {
        match self {
            // Opens the Store's "Downloads and updates" pane directly.
            DistChannel::MsStore => Some("ms-windows-store://downloadsandupdates"),
            DistChannel::MacAppStore => Some("macappstore://showUpdatesPage"),
            // No single URL fits apt/dnf/brew — the UI explains instead.
            DistChannel::SystemPackage | DistChannel::Development => None,
            DistChannel::Direct(_) => None,
        }
    }
}

/// Everything [`detect`] needs, so the rules can be tested without a filesystem.
#[derive(Debug, Clone)]
pub struct DetectInput<'a> {
    /// `std::env::consts::OS` — `"windows"`, `"macos"`, `"linux"`, …
    pub os: &'a str,
    /// Path of the running executable.
    pub exe_path: &'a Path,
    /// Explicit channel id from the `SENJU_DIST_CHANNEL` environment variable —
    /// a support/diagnostics override. Wins over every heuristic below.
    pub forced: Option<&'a str>,
    /// The installer marker the Tauri bundler stamped into this binary —
    /// `"nsis"`, `"msi"`, `"app"`, `"appimage"`, `"deb"`, `"rpm"`. Authoritative
    /// when present (the bundler writes it per installer, so two installers
    /// built from one compile still disagree correctly); `None` for an
    /// unbundled binary.
    pub bundle: Option<&'a str>,
    /// `APPIMAGE` is set in the environment (the AppImage runtime sets it).
    pub appimage_env: bool,
    /// An `uninstall.exe` sits next to the executable — the NSIS installer
    /// writes one there, the MSI installer does not. Only consulted when
    /// `bundle` is absent.
    pub sibling_uninstaller: bool,
    /// `Contents/_MASReceipt/receipt` exists in the enclosing `.app` bundle.
    pub mas_receipt: bool,
}

/// Parses a forced channel id (the inverse of [`DistChannel::id`]).
fn parse_forced(id: &str) -> Option<DistChannel> {
    match id.trim().to_ascii_lowercase().as_str() {
        "direct-nsis" | "nsis" => Some(DistChannel::Direct(InstallerFormat::Nsis)),
        "direct-msi" | "msi" => Some(DistChannel::Direct(InstallerFormat::Msi)),
        "direct-app" | "app" | "dmg" => Some(DistChannel::Direct(InstallerFormat::MacApp)),
        "direct-appimage" | "appimage" => Some(DistChannel::Direct(InstallerFormat::AppImage)),
        "msstore" | "microsoft-store" => Some(DistChannel::MsStore),
        "macappstore" | "mac-app-store" => Some(DistChannel::MacAppStore),
        "system-package" | "deb" | "rpm" | "homebrew" => Some(DistChannel::SystemPackage),
        "development" | "dev" => Some(DistChannel::Development),
        _ => None,
    }
}

/// Splits a path into directory/file names on **both** separators.
///
/// Deliberately not `Path::components()`: that honours only the *host's*
/// separator, so a Windows path examined anywhere else (these rules are decided
/// by `os`, not by the machine running them — which is exactly what the tests
/// exercise) would collapse into a single component and every check below would
/// silently return false.
fn segments(exe: &Path) -> impl Iterator<Item = &str> {
    exe.to_str()
        .unwrap_or_default()
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
}

/// True when the executable sits inside a Cargo build directory — i.e. this is
/// a `cargo run` / `cargo tauri dev` binary, not an installed one.
fn looks_like_build_dir(exe: &Path) -> bool {
    let mut it = segments(exe).peekable();
    while let Some(seg) = it.next() {
        if seg == "target" && matches!(it.peek(), Some(&"debug") | Some(&"release")) {
            return true;
        }
    }
    false
}

/// True when the path lies under a directory named `dir` (case-insensitively —
/// Windows paths reach us with whatever casing the OS chose).
fn has_dir(exe: &Path, dir: &str) -> bool {
    segments(exe).any(|s| s.eq_ignore_ascii_case(dir))
}

/// Decides the channel from the inputs. Pure — see [`current`] for the wiring.
///
/// Order matters: an explicit channel id beats everything (it is how a Store
/// build labels itself), a build directory beats the install heuristics, and a
/// store install beats the bundler's installer marker — an MSIX wraps the very
/// same NSIS-or-MSI-stamped binary, so the marker alone would call a Store copy
/// a self-updating download.
pub fn detect(input: &DetectInput) -> DistChannel {
    if let Some(forced) = input.forced.and_then(parse_forced) {
        return forced;
    }
    if looks_like_build_dir(input.exe_path) {
        return DistChannel::Development;
    }
    // MSIX packages are always deployed under `…\WindowsApps\`, and nothing
    // else installs there — a reliable stand-in for the package-identity API
    // without pulling in a Windows-only crate.
    if input.os == "windows" && has_dir(input.exe_path, "WindowsApps") {
        return DistChannel::MsStore;
    }
    if input.os == "macos" {
        if input.mas_receipt {
            return DistChannel::MacAppStore;
        }
        // Homebrew's own prefix — `brew upgrade` owns this copy.
        if has_dir(input.exe_path, "Cellar") || has_dir(input.exe_path, "Caskroom") {
            return DistChannel::SystemPackage;
        }
    }
    // The bundler's marker is the precise answer where it exists.
    match input.bundle.map(|b| b.trim().to_ascii_lowercase()).as_deref() {
        Some("nsis") => return DistChannel::Direct(InstallerFormat::Nsis),
        Some("msi") => return DistChannel::Direct(InstallerFormat::Msi),
        Some("app" | "dmg") => return DistChannel::Direct(InstallerFormat::MacApp),
        Some("appimage") => return DistChannel::Direct(InstallerFormat::AppImage),
        // Owned by apt/dnf: replacing the files in place would leave the
        // package database describing something that is no longer on disk.
        Some("deb" | "rpm") => return DistChannel::SystemPackage,
        _ => {}
    }
    // No marker (unbundled binary, or a bundle type we do not ship): fall back
    // to the install location.
    match input.os {
        "windows" => {
            if input.sibling_uninstaller {
                DistChannel::Direct(InstallerFormat::Nsis)
            } else {
                DistChannel::Direct(InstallerFormat::Msi)
            }
        }
        "macos" => DistChannel::Direct(InstallerFormat::MacApp),
        _ => {
            if input.appimage_env {
                DistChannel::Direct(InstallerFormat::AppImage)
            } else {
                DistChannel::SystemPackage
            }
        }
    }
}

/// Detects the channel for the running process.
///
/// `bundle` is the Tauri bundler's installer marker — the GUI crate passes
/// `tauri::utils::platform::bundle_type()`; this crate stays GUI-independent
/// and takes it as an argument.
pub fn current(bundle: Option<&str>) -> DistChannel {
    let exe = std::env::current_exe().unwrap_or_default();
    let sibling_uninstaller = exe
        .parent()
        .map(|d| d.join("uninstall.exe").exists())
        .unwrap_or(false);
    // …/Senju Term.app/Contents/MacOS/senju-term → …/Contents/_MASReceipt/receipt
    let mas_receipt = exe
        .parent()
        .and_then(|macos| macos.parent())
        .map(|contents| contents.join("_MASReceipt").join("receipt").exists())
        .unwrap_or(false);

    // Escape hatch for support, and for exercising the managed-channel paths on
    // a machine that is not actually a Store install. Deliberately read at run
    // time only: an `option_env!` would be baked in at compile time without
    // Cargo tracking the variable, so a rebuild could silently keep a stale
    // channel.
    let runtime_forced = std::env::var("SENJU_DIST_CHANNEL")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let forced = runtime_forced.as_deref();

    detect(&DetectInput {
        os: std::env::consts::OS,
        exe_path: &exe,
        forced,
        bundle,
        appimage_env: std::env::var_os("APPIMAGE").is_some(),
        sibling_uninstaller,
        mas_receipt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn input<'a>(os: &'a str, exe: &'a Path) -> DetectInput<'a> {
        DetectInput {
            os,
            exe_path: exe,
            forced: None,
            bundle: None,
            appimage_env: false,
            sibling_uninstaller: false,
            mas_receipt: false,
        }
    }

    #[test]
    fn windows_splits_store_nsis_and_msi() {
        let store = PathBuf::from(r"C:\Program Files\WindowsApps\Senju.Term_0.1.0_x64\senju-term.exe");
        assert_eq!(detect(&input("windows", &store)), DistChannel::MsStore);

        let installed = PathBuf::from(r"C:\Program Files\Senju Term\senju-term.exe");
        let mut nsis = input("windows", &installed);
        nsis.sibling_uninstaller = true;
        assert_eq!(detect(&nsis), DistChannel::Direct(InstallerFormat::Nsis));

        // Same path, no uninstall.exe → the MSI put it there.
        assert_eq!(
            detect(&input("windows", &installed)),
            DistChannel::Direct(InstallerFormat::Msi)
        );
    }

    #[test]
    fn unix_channels_follow_the_install_location() {
        let app = PathBuf::from("/Applications/Senju Term.app/Contents/MacOS/senju-term");
        assert_eq!(
            detect(&input("macos", &app)),
            DistChannel::Direct(InstallerFormat::MacApp)
        );

        let mut mas = input("macos", &app);
        mas.mas_receipt = true;
        assert_eq!(detect(&mas), DistChannel::MacAppStore);

        let cask =
            PathBuf::from("/opt/homebrew/Caskroom/senju-term/0.1.0/Senju Term.app/Contents/MacOS/senju-term");
        assert_eq!(detect(&input("macos", &cask)), DistChannel::SystemPackage);

        let deb = PathBuf::from("/usr/bin/senju-term");
        assert_eq!(detect(&input("linux", &deb)), DistChannel::SystemPackage);

        let mut appimage = input("linux", &deb);
        appimage.appimage_env = true;
        assert_eq!(
            detect(&appimage),
            DistChannel::Direct(InstallerFormat::AppImage)
        );
    }

    #[test]
    fn build_dirs_and_forced_ids_win_over_heuristics() {
        let dev = PathBuf::from("/home/u/senju-term/apps/desktop-app/target/release/senju-term");
        assert_eq!(detect(&input("linux", &dev)), DistChannel::Development);

        // Backslash paths must split the same way — otherwise a Windows dev
        // build would be reported as an MSI install and offered updates.
        let win_dev = PathBuf::from(r"C:\src\senju-term\apps\desktop-app\target\debug\senju-term.exe");
        assert_eq!(detect(&input("windows", &win_dev)), DistChannel::Development);

        // A forced id beats even the build-directory check, so the packaging
        // job can label a binary it builds straight out of `target/`.
        let mut forced = input("linux", &dev);
        forced.forced = Some("msstore");
        assert_eq!(detect(&forced), DistChannel::MsStore);

        let mut junk = input("linux", &dev);
        junk.forced = Some("not-a-channel");
        assert_eq!(detect(&junk), DistChannel::Development);
    }

    #[test]
    fn bundler_marker_decides_the_format_but_not_store_installs() {
        let installed = PathBuf::from(r"C:\Program Files\Senju Term\senju-term.exe");
        let mut msi = input("windows", &installed);
        msi.bundle = Some("msi");
        // No uninstall.exe would have said MSI anyway; assert the marker is
        // what answered by flipping the heuristic against it.
        msi.sibling_uninstaller = true;
        assert_eq!(detect(&msi), DistChannel::Direct(InstallerFormat::Msi));

        // deb/rpm belong to the package manager even though they are "direct"
        // downloads — the updater cannot replace them coherently.
        let deb_path = PathBuf::from("/usr/bin/senju-term");
        let mut deb = input("linux", &deb_path);
        deb.bundle = Some("deb");
        assert_eq!(detect(&deb), DistChannel::SystemPackage);

        // An MSIX wraps a binary the bundler stamped as nsis/msi, so the Store
        // location has to win — otherwise a Store copy would self-update.
        let store =
            PathBuf::from(r"C:\Program Files\WindowsApps\Senju.Term_0.1.0_x64\senju-term.exe");
        let mut store_input = input("windows", &store);
        store_input.bundle = Some("nsis");
        assert_eq!(detect(&store_input), DistChannel::MsStore);
    }

    #[test]
    fn update_policy_matches_the_channel() {
        let nsis = DistChannel::Direct(InstallerFormat::Nsis);
        assert!(nsis.in_app_updates());
        assert_eq!(nsis.external_update_target(), None);
        assert_eq!(
            nsis.updater_platform_key("windows-x86_64").as_deref(),
            Some("windows-x86_64-nsis")
        );

        // Formats never cross: an MSI install resolves its own manifest key.
        assert_eq!(
            DistChannel::Direct(InstallerFormat::Msi)
                .updater_platform_key("windows-x86_64")
                .as_deref(),
            Some("windows-x86_64-msi")
        );

        for external in [
            DistChannel::MsStore,
            DistChannel::MacAppStore,
            DistChannel::SystemPackage,
            DistChannel::Development,
        ] {
            assert!(!external.in_app_updates(), "{} must not self-update", external.id());
            assert!(external.installer_format().is_none());
        }
        assert_eq!(
            DistChannel::MsStore.external_update_target(),
            Some("ms-windows-store://downloadsandupdates")
        );
    }
}
