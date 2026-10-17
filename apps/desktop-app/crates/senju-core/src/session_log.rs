//! Session transcripts on disk.
//!
//! A [`SessionLogger`] appends everything a session prints to a file under a
//! dated directory, the way a serial-console program keeps a log: evidence of
//! what was run and what came back, kept per session so a day's work on one
//! host is one file. [`LoggingSink`] is the [`EventSink`] decorator that feeds
//! it — the GUI layer wraps its own sink in one and never has to remember to
//! log anywhere else.
//!
//! Logging is opt-in per session (`start`), so a sink can carry many sessions
//! of which only some are recorded. Nothing is buffered in userland: each
//! chunk is written straight to the OS, so a crash loses at most what the OS
//! had not flushed, not a buffer's worth.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::sessions::EventSink;

/// How a log is written.
#[derive(Debug, Clone, PartialEq)]
pub struct LogConfig {
    /// Directory the dated subfolders go under.
    pub dir: PathBuf,
    /// Strip escape sequences and normalize line endings, so the file reads
    /// as text. `false` keeps the raw stream for replaying in a terminal.
    pub plain: bool,
}

struct LogFile {
    file: File,
    path: PathBuf,
    plain: bool,
    strip: AnsiStripper,
}

/// Keeps the open log files, keyed by session id.
pub struct SessionLogger {
    config: Mutex<LogConfig>,
    files: Mutex<HashMap<String, LogFile>>,
}

impl SessionLogger {
    pub fn new(config: LogConfig) -> Self {
        Self {
            config: Mutex::new(config),
            files: Mutex::new(HashMap::new()),
        }
    }

    /// Replaces the configuration for logs started from now on. Logs already
    /// open keep writing where they were opened — moving a half-written file
    /// under the user would be worse than finishing it in place.
    pub fn configure(&self, config: LogConfig) {
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;
    }

    pub fn config(&self) -> LogConfig {
        self.config.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Opens a log for `id`, named after `label` (a host or profile name,
    /// sanitized) and the local time: `<dir>/<YYYY-MM-DD>/<label>_<HHMMSS>.log`,
    /// with a numeric suffix if that name is taken. Returns the path.
    pub fn start(&self, id: &str, label: &str) -> std::io::Result<PathBuf> {
        let config = self.config();
        let now = chrono::Local::now();
        let day_dir = config.dir.join(now.format("%Y-%m-%d").to_string());
        fs::create_dir_all(&day_dir)?;
        let stem = format!("{}_{}", sanitize_label(label), now.format("%H%M%S"));
        let (path, mut file) = create_unique(&day_dir, &stem)?;
        // A header so a file found later says what it is without its
        // directory. Written in both modes: it is text either way.
        writeln!(
            file,
            "# Senju Term session log: {label} — {}",
            now.format("%Y-%m-%d %H:%M:%S %Z")
        )?;
        self.files.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id.to_string(),
            LogFile {
                file,
                path: path.clone(),
                plain: config.plain,
                strip: AnsiStripper::default(),
            },
        );
        Ok(path)
    }

    /// Closes the log for `id`, if one is open. Idempotent.
    pub fn stop(&self, id: &str) {
        if let Some(mut lf) = self.files.lock().unwrap_or_else(|e| e.into_inner()).remove(id) {
            let _ = lf.file.flush();
        }
    }

    /// Appends session output. A no-op for sessions without an open log.
    /// Write failures are swallowed: the session is the thing that matters,
    /// and a full disk should not take the terminal down with it.
    pub fn data(&self, id: &str, bytes: &[u8]) {
        let mut files = self.files.lock().unwrap_or_else(|e| e.into_inner());
        let Some(lf) = files.get_mut(id) else { return };
        if lf.plain {
            let text = lf.strip.feed(bytes);
            let _ = lf.file.write_all(&text);
        } else {
            let _ = lf.file.write_all(bytes);
        }
    }

    /// Path of the open log for `id`, if any.
    pub fn path_for(&self, id: &str) -> Option<PathBuf> {
        self.files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|lf| lf.path.clone())
    }
}

/// Creates `<dir>/<stem>.log`, or `<stem>_2.log`, `_3`… if taken. Two
/// sessions to one host in the same second is unusual but not impossible
/// (connect-all does exactly that), and silently appending to the other
/// session's file would interleave two transcripts.
fn create_unique(dir: &Path, stem: &str) -> std::io::Result<(PathBuf, File)> {
    for n in 1..1000u32 {
        let name = if n == 1 { format!("{stem}.log") } else { format!("{stem}_{n}.log") };
        let path = dir.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((path, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("could not find a free log file name"))
}

/// A label that is safe as a file name on every platform: anything outside
/// letters, digits, `-`, `_` and `.` becomes `_`, runs collapse, and the result
/// is capped so a long host name cannot push the path over OS limits.
fn sanitize_label(label: &str) -> String {
    let mut out = String::new();
    let mut last_us = false;
    for c in label.chars() {
        let keep = c.is_alphanumeric() || matches!(c, '-' | '_' | '.');
        if keep {
            out.push(c);
            last_us = false;
        } else if !last_us {
            out.push('_');
            last_us = true;
        }
    }
    let trimmed = out.trim_matches(|c| c == '_' || c == '.');
    let capped: String = trimmed.chars().take(40).collect();
    if capped.is_empty() { "session".into() } else { capped }
}

/// Wraps an [`EventSink`], copying every session's output into the logger
/// before passing it on, and closing the log when the session exits.
pub struct LoggingSink<S: EventSink> {
    inner: Arc<S>,
    logger: Arc<SessionLogger>,
}

impl<S: EventSink> LoggingSink<S> {
    pub fn new(inner: Arc<S>, logger: Arc<SessionLogger>) -> Self {
        Self { inner, logger }
    }
}

impl<S: EventSink> EventSink for LoggingSink<S> {
    fn data(&self, id: &str, data: &[u8]) {
        self.logger.data(id, data);
        self.inner.data(id, data);
    }

    fn exit(&self, id: &str, code: i32) {
        self.logger.stop(id);
        self.inner.exit(id, code);
    }
}

/// Removes terminal control sequences from a byte stream, keeping only what
/// a person would read: printable text, newlines and tabs. Stateful, because
/// a sequence can be split across two reads from the PTY.
///
/// Handled: CSI (`ESC [ … final`), OSC (`ESC ] … BEL` / `ESC ] … ESC \`),
/// two-byte designations (`ESC ( B` etc.), other single-byte `ESC x`, and the
/// C0 controls. `\r\n` and bare `\r` both become one `\n`: a progress bar that
/// redraws its line with `\r` is unreadable in a text file either way, and
/// one line per redraw is the lesser evil.
#[derive(Default)]
pub struct AnsiStripper {
    state: StripState,
    /// The last emitted byte was a `\r` (written as `\n`): the `\n` that
    /// usually follows is the same line ending, not a second one. Only
    /// *emitted* bytes clear it, so `\r ESC[K \n` still collapses.
    after_cr: bool,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum StripState {
    #[default]
    Text,
    Esc,
    Csi,
    Osc,
    /// Saw ESC inside an OSC: the next byte should be `\` (ST).
    OscEsc,
    /// `ESC (`, `ESC )` …: one more byte names the charset, then done.
    Designate,
}

impl AnsiStripper {
    /// One byte in the plain-text state.
    fn text_byte(&mut self, b: u8, out: &mut Vec<u8>) {
        match b {
            0x1b => self.state = StripState::Esc,
            b'\r' => {
                out.push(b'\n');
                self.after_cr = true;
            }
            b'\n' => {
                if !self.after_cr {
                    out.push(b'\n');
                }
                self.after_cr = false;
            }
            b'\t' => {
                out.push(b);
                self.after_cr = false;
            }
            0x00..=0x1f | 0x7f => {}
            _ => {
                out.push(b);
                self.after_cr = false;
            }
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(bytes.len());
        for &b in bytes {
            match self.state {
                StripState::Text => self.text_byte(b, &mut out),
                StripState::Esc => {
                    self.state = match b {
                        b'[' => StripState::Csi,
                        b']' => StripState::Osc,
                        b'(' | b')' | b'*' | b'+' | b'#' | b'%' => StripState::Designate,
                        _ => StripState::Text,
                    }
                }
                StripState::Csi => {
                    // Parameter and intermediate bytes continue; a final byte
                    // (0x40–0x7e) ends it. Anything else is a malformed
                    // sequence: abandon it and treat this byte as text, so
                    // neither it nor what follows is swallowed — dropping it
                    // would also split a multi-byte UTF-8 character.
                    if (0x40..=0x7e).contains(&b) {
                        self.state = StripState::Text;
                    } else if !(0x20..=0x3f).contains(&b) {
                        self.state = StripState::Text;
                        self.text_byte(b, &mut out);
                    }
                }
                StripState::Osc => match b {
                    0x07 => self.state = StripState::Text,
                    0x1b => self.state = StripState::OscEsc,
                    _ => {}
                },
                StripState::OscEsc => {
                    self.state = if b == b'\\' { StripState::Text } else { StripState::Osc };
                }
                StripState::Designate => self.state = StripState::Text,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn strip(input: &[u8]) -> String {
        String::from_utf8(AnsiStripper::default().feed(input)).unwrap()
    }

    #[test]
    fn strips_colors_and_cursor_movement() {
        assert_eq!(strip(b"\x1b[1;32mok\x1b[0m done\x1b[2K\x1b[1A"), "ok done");
    }

    #[test]
    fn strips_osc_with_either_terminator() {
        assert_eq!(strip(b"\x1b]0;title\x07text"), "text");
        assert_eq!(strip(b"\x1b]7;file://h/path\x1b\\text"), "text");
    }

    #[test]
    fn strips_charset_designation_and_bare_escapes() {
        assert_eq!(strip(b"\x1b(Babc\x1b=\x1b>def\x1b7\x1b8"), "abcdef");
    }

    #[test]
    fn line_endings_become_single_newlines() {
        assert_eq!(strip(b"a\r\nb\rc\nd"), "a\nb\nc\nd");
        // CR, erase-to-end-of-line, LF: the common prompt redraw.
        assert_eq!(strip(b"a\r\x1b[K\nb"), "a\nb");
    }

    #[test]
    fn drops_other_c0_but_keeps_tabs_and_utf8() {
        assert_eq!(strip("x\x07\x08\ty 日本語\x7f".as_bytes()), "x\ty 日本語");
    }

    /// The PTY hands over whatever it has; a sequence can end up split across
    /// two reads. The stripper has to carry its state over, or the tail of a
    /// color code lands in the log as text.
    #[test]
    fn sequences_split_across_feeds() {
        let mut st = AnsiStripper::default();
        let mut out = st.feed(b"a\x1b[1;");
        out.extend(st.feed(b"32mb\x1b]0;ti"));
        out.extend(st.feed(b"tle\x07c\r"));
        out.extend(st.feed(b"\nd"));
        assert_eq!(String::from_utf8(out).unwrap(), "abc\nd");
    }

    /// A malformed CSI must not eat the byte that broke it: that byte may be
    /// the first of a multi-byte character.
    #[test]
    fn malformed_csi_does_not_swallow_text() {
        assert_eq!(strip(b"\x1b[12\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip(b"\x1b[\xc3\xa9x"), "\u{e9}x");
    }

    #[test]
    fn labels_become_safe_file_names() {
        assert_eq!(sanitize_label("prod web-01 (eu)"), "prod_web-01_eu");
        assert_eq!(sanitize_label("deploy@host:22/x"), "deploy_host_22_x");
        assert_eq!(sanitize_label("日本語ホスト"), "日本語ホスト");
        assert_eq!(sanitize_label("///"), "session");
        assert_eq!(sanitize_label(&"x".repeat(100)).chars().count(), 40);
    }

    struct Recorder(StdMutex<Vec<(String, Vec<u8>)>>, StdMutex<Vec<(String, i32)>>);
    impl EventSink for Recorder {
        fn data(&self, id: &str, data: &[u8]) {
            self.0.lock().unwrap().push((id.into(), data.to_vec()));
        }
        fn exit(&self, id: &str, code: i32) {
            self.1.lock().unwrap().push((id.into(), code));
        }
    }

    fn logger(dir: &Path, plain: bool) -> Arc<SessionLogger> {
        Arc::new(SessionLogger::new(LogConfig { dir: dir.to_path_buf(), plain }))
    }

    #[test]
    fn logs_only_started_sessions_and_closes_on_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let inner = Arc::new(Recorder(StdMutex::new(vec![]), StdMutex::new(vec![])));
        let lg = logger(tmp.path(), true);
        let sink = LoggingSink::new(inner.clone(), lg.clone());

        sink.data("quiet", b"not logged\n");
        let path = lg.start("loud", "web-01").unwrap();
        assert!(path.starts_with(tmp.path()));
        assert!(path.to_string_lossy().contains("web-01_"));
        sink.data("loud", b"\x1b[32m$ ls\x1b[0m\r\nfile\r\n");
        assert_eq!(lg.path_for("loud").as_deref(), Some(path.as_path()));
        sink.exit("loud", 0);
        assert!(lg.path_for("loud").is_none(), "closed on exit");
        sink.data("loud", b"after exit\n");

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Senju Term session log: web-01"), "{text}");
        assert!(text.ends_with("$ ls\nfile\n"), "{text:?}");
        assert!(!text.contains("after exit") && !text.contains("not logged"));
        // The inner sink still saw everything, logged or not.
        assert_eq!(inner.0.lock().unwrap().len(), 3);
        assert_eq!(inner.1.lock().unwrap().as_slice(), &[("loud".to_string(), 0)]);
        // Only one dated directory with one file in it.
        let days: Vec<_> = fs::read_dir(tmp.path()).unwrap().collect();
        assert_eq!(days.len(), 1);
    }

    #[test]
    fn raw_mode_keeps_bytes_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let lg = logger(tmp.path(), false);
        let path = lg.start("s", "h").unwrap();
        lg.data("s", b"\x1b[31mred\x1b[0m\r\n");
        lg.stop("s");
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.ends_with(b"\x1b[31mred\x1b[0m\r\n"));
    }

    /// Two sessions to one host in the same second must not share a file.
    #[test]
    fn same_label_same_second_gets_distinct_files() {
        let tmp = tempfile::tempdir().unwrap();
        let lg = logger(tmp.path(), true);
        let a = lg.start("a", "db").unwrap();
        let b = lg.start("b", "db").unwrap();
        assert_ne!(a, b);
        lg.data("a", b"A\n");
        lg.data("b", b"B\n");
        lg.stop("a");
        lg.stop("b");
        assert!(fs::read_to_string(&a).unwrap().ends_with("A\n"));
        assert!(fs::read_to_string(&b).unwrap().ends_with("B\n"));
    }

    /// Reconfiguring changes where *new* logs go; an open log stays put.
    #[test]
    fn configure_applies_to_new_logs_only() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        let lg = logger(tmp1.path(), true);
        let first = lg.start("one", "h").unwrap();
        lg.configure(LogConfig { dir: tmp2.path().to_path_buf(), plain: false });
        let second = lg.start("two", "h").unwrap();
        assert!(first.starts_with(tmp1.path()));
        assert!(second.starts_with(tmp2.path()));
        lg.data("one", b"x\r\n");
        lg.stop("one");
        assert!(fs::read_to_string(&first).unwrap().ends_with("x\n"), "first stays plain");
    }

    #[test]
    fn unwritable_dir_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let file_not_dir = tmp.path().join("f");
        fs::write(&file_not_dir, b"").unwrap();
        let lg = logger(&file_not_dir, true);
        assert!(lg.start("s", "h").is_err());
        lg.data("s", b"ignored");
        lg.stop("s");
    }
}
