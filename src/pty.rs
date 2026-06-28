use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::model::Host;
use crate::ssh;

type TerminalParser = vt100::Parser<TerminalCallbacks>;

/// Distinguishes an SSH session from a local-shell session.
///
/// Local shells reuse the same `Session` plumbing as SSH but must not
/// participate in alias-based dedup (their `local-N` label is synthetic, not a
/// user-facing host alias). The kind lets callers filter by purpose instead of
/// guessing from the alias string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Ssh,
    Local,
}

/// Runtime status for an embedded SSH session.
///
/// `Exited` stores a simplified exit code because `portable-pty` abstracts over
/// platforms. A missing code means the child exited but the exact status was not
/// available when polled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Exited(Option<u32>),
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("alias", &self.alias)
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

/// An embedded SSH process connected to a pseudo-terminal.
///
/// `Session` owns the PTY master, the `ssh` child process, a writer for user
/// input, and a background reader thread that feeds output into a `vt100`
/// parser. The parsed screen is cloned for rendering so UI code never reads
/// directly from the PTY.
pub struct Session {
    pub alias: String,
    /// Distinguishes SSH from local-shell sessions for dedup and label allocation.
    pub kind: SessionKind,
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    parser: Arc<Mutex<TerminalParser>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _reader_handle: JoinHandle<()>,
    exited: Arc<AtomicBool>,
    pub unread: Arc<AtomicBool>,
    status: SessionStatus,
}

impl Session {
    /// Spawns `ssh` for a host inside a newly-created PTY of the given size.
    ///
    /// The slave side becomes the controlling terminal for the OpenSSH child.
    /// The master side stays in hosttui: writes send user input to SSH, and a
    /// reader thread parses SSH output into the in-memory terminal screen.
    pub fn spawn(host: &Host, rows: u16, cols: u16) -> io::Result<Self> {
        let args = ssh::ssh_args(host);
        let mut cmd = CommandBuilder::new("ssh");
        for arg in &args {
            cmd.arg(arg);
        }
        Self::spawn_command(cmd, host.alias.clone(), SessionKind::Ssh, rows, cols)
    }

    /// Spawns the user's local shell inside a new PTY.
    ///
    /// Used for "local terminal" tabs that run on the machine hosttui itself
    /// runs on, rather than over SSH. We honor `$SHELL` so the user gets their
    /// configured shell, falling back to `/bin/sh` which is guaranteed present
    /// on POSIX systems. The shell is interactive because it owns a controlling
    /// tty, so no extra flags are needed.
    pub fn spawn_local(alias: String, rows: u16, cols: u16) -> io::Result<Self> {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cmd = CommandBuilder::new(shell);
        Self::spawn_command(cmd, alias, SessionKind::Local, rows, cols)
    }

    /// Shared PTY setup: opens a pty of the given size, spawns `cmd` on the
    /// slave, and starts the background reader thread feeding the vt100 parser.
    ///
    /// Both SSH sessions and local shells differ only in which command they
    /// run, so the terminal plumbing is factored here to avoid duplication.
    fn spawn_command(
        cmd: CommandBuilder,
        alias: String,
        kind: SessionKind,
        rows: u16,
        cols: u16,
    ) -> io::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;

        let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;

        drop(pair.slave);

        let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer = Arc::new(Mutex::new(
            pair.master.take_writer().map_err(io::Error::other)?,
        ));

        let parser = Arc::new(Mutex::new(vt100::Parser::new_with_callbacks(
            rows,
            cols,
            1000,
            TerminalCallbacks::new(Arc::clone(&writer)),
        )));
        let exited = Arc::new(AtomicBool::new(false));
        let unread = Arc::new(AtomicBool::new(false));

        let reader_handle = {
            let parser = Arc::clone(&parser);
            let exited = Arc::clone(&exited);
            let unread = Arc::clone(&unread);
            thread::spawn(move || Self::reader_loop(reader, parser, exited, unread))
        };

        Ok(Session {
            alias,
            kind,
            master: pair.master,
            writer,
            parser,
            child,
            _reader_handle: reader_handle,
            exited,
            unread,
            status: SessionStatus::Running,
        })
    }

    /// Continuously reads PTY output and applies it to the terminal parser.
    ///
    /// This runs on a background thread because terminal output can arrive even
    /// when the main event loop is waiting for keyboard or mouse input. Any read
    /// EOF/error marks the session as exited; the main loop later polls the child
    /// process to update the public status.
    fn reader_loop(
        mut reader: Box<dyn Read + Send>,
        parser: Arc<Mutex<TerminalParser>>,
        exited: Arc<AtomicBool>,
        unread: Arc<AtomicBool>,
    ) {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    exited.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(n) => {
                    parser.lock().unwrap().process(&buf[..n]);
                    unread.store(true, Ordering::SeqCst);
                }
            }
        }
    }

    /// Sends raw bytes to the remote session and exits scrollback mode.
    ///
    /// Typing while scrolled up should behave like most terminal emulators: the
    /// viewport snaps back to live output before input is delivered.
    pub fn write(&mut self, data: &[u8]) {
        self.scroll_reset();
        let _ = self.writer.lock().unwrap().write_all(data);
    }

    /// Pastes text into the remote session, honoring bracketed paste mode.
    ///
    /// Full-screen programs can enable bracketed paste to distinguish pasted
    /// text from typed keystrokes. When the parsed terminal state says that mode
    /// is active, hosttui wraps the pasted bytes in the standard start/end paste
    /// markers before writing them to the PTY.
    pub fn paste(&mut self, text: &str) {
        self.scroll_reset();
        let bracketed_paste = self.parser.lock().unwrap().screen().bracketed_paste();
        let mut writer = self.writer.lock().unwrap();
        if bracketed_paste {
            let _ = writer.write_all(b"\x1b[200~");
        }
        let _ = writer.write_all(text.as_bytes());
        if bracketed_paste {
            let _ = writer.write_all(b"\x1b[201~");
        }
    }

    /// Moves the terminal viewport up into vt100 scrollback history.
    pub fn scroll_up(&self, lines: usize) {
        let mut parser = self.parser.lock().unwrap();
        let current = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(current + lines);
    }

    /// Moves the terminal viewport down toward live output.
    pub fn scroll_down(&self, lines: usize) {
        let mut parser = self.parser.lock().unwrap();
        let current = parser.screen().scrollback();
        parser
            .screen_mut()
            .set_scrollback(current.saturating_sub(lines));
    }

    /// Returns the terminal viewport to live output.
    pub fn scroll_reset(&self) {
        self.parser.lock().unwrap().screen_mut().set_scrollback(0);
    }

    /// Returns how many lines above live output the viewport currently is.
    pub fn scrollback_pos(&self) -> usize {
        self.parser.lock().unwrap().screen().scrollback()
    }

    /// Returns a snapshot of the parsed terminal screen for rendering.
    ///
    /// The clone keeps rendering independent from the reader thread, which may
    /// continue parsing new PTY bytes while ratatui draws the current frame.
    pub fn screen(&self) -> vt100::Screen {
        self.parser.lock().unwrap().screen().clone()
    }

    /// Resizes both the in-memory parser screen and the OS PTY.
    ///
    /// Updating both sides is necessary: the parser needs the new dimensions to
    /// render correctly, while the child process needs a PTY resize so programs
    /// receive SIGWINCH and recalculate their layouts.
    pub fn resize(&self, rows: u16, cols: u16) {
        {
            let mut parser = self.parser.lock().unwrap();
            parser.screen_mut().set_size(rows, cols);
        }
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Polls the child process and records a terminal `SessionStatus`.
    ///
    /// The method is non-blocking and safe to call every event-loop tick. Once a
    /// session is no longer running, the cached status is left unchanged.
    pub fn update_status(&mut self) {
        if !matches!(self.status, SessionStatus::Running) {
            return;
        }
        if self.exited.load(Ordering::SeqCst) {
            self.status = match self.child.try_wait() {
                Ok(Some(es)) => SessionStatus::Exited(if es.success() { Some(0) } else { Some(1) }),
                _ => SessionStatus::Exited(None),
            };
        } else if let Ok(Some(es)) = self.child.try_wait() {
            self.status = SessionStatus::Exited(if es.success() { Some(0) } else { Some(1) });
        }
    }

    /// Returns the last known child-process status.
    pub fn status(&self) -> SessionStatus {
        self.status
    }
}

struct TerminalCallbacks {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
}

impl TerminalCallbacks {
    /// Creates the callback state used by the `vt100` parser.
    ///
    /// `vt100` normally only updates an in-memory screen, but interactive
    /// terminal applications can also send queries that require a response from
    /// the terminal emulator. We keep a shared writer to the PTY master here so
    /// those parser callbacks can send terminal replies back to the SSH process.
    fn new(writer: Arc<Mutex<Box<dyn Write + Send>>>) -> Self {
        Self { writer }
    }

    /// Writes a raw terminal reply back to the child process running in the PTY.
    ///
    /// These replies are escape sequences that a real terminal emulator would
    /// normally generate automatically. Without them, some full-screen SSH apps
    /// wait forever for terminal metadata and never draw their initial UI.
    fn reply(&self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        let _ = writer.write_all(bytes);
        let _ = writer.flush();
    }

    /// Formats and writes a terminal reply whose values depend on screen state.
    ///
    /// Cursor position and terminal-size reports include dynamic row/column
    /// values, so this helper avoids allocating an intermediate `String` while
    /// still sending the completed escape sequence to the PTY.
    fn reply_fmt(&self, args: std::fmt::Arguments<'_>) {
        let mut writer = self.writer.lock().unwrap();
        let _ = writer.write_fmt(args);
        let _ = writer.flush();
    }
}

/// Returns the first numeric parameter from a CSI sequence, defaulting to zero.
///
/// The `vte` parser represents CSI parameters as a list of parameter groups.
/// Most terminal queries handled here are identified by their first numeric
/// value, such as `ESC [ 6 n` for cursor position or `ESC [ 18 t` for size.
fn first_param(params: &[&[u16]]) -> u16 {
    params
        .first()
        .and_then(|param| param.first())
        .copied()
        .unwrap_or(0)
}

impl vt100::Callbacks for TerminalCallbacks {
    /// Handles legacy escape-sequence terminal queries not implemented by `vt100`.
    ///
    /// `ESC Z` asks for primary device attributes. We answer as a VT100-style
    /// terminal with advanced video option (`ESC [?1;2c`), which is a common,
    /// conservative response understood by terminal applications.
    fn unhandled_escape(&mut self, _: &mut vt100::Screen, i1: Option<u8>, i2: Option<u8>, b: u8) {
        if i1.is_none() && i2.is_none() && b == b'Z' {
            self.reply(b"\x1b[?1;2c");
        }
    }

    /// Handles CSI queries that need the terminal emulator to answer back.
    ///
    /// `vt100` consumes many CSI sequences that change the screen, but it does
    /// not automatically reply to status/device/window queries. Remote TUIs can
    /// depend on those replies during startup, so we provide the common answers:
    /// primary/secondary device attributes, terminal status, cursor position,
    /// DEC cursor position, and text-area size.
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        if i2.is_some() {
            return;
        }

        let param = first_param(params);
        match (i1, c, param) {
            (None, 'c', 0) => self.reply(b"\x1b[?1;2c"),
            (Some(b'>'), 'c', _) => self.reply(b"\x1b[>0;136;0c"),
            (None, 'n', 5) => self.reply(b"\x1b[0n"),
            (None, 'n', 6) => {
                let (row, col) = screen.cursor_position();
                self.reply_fmt(format_args!("\x1b[{};{}R", row + 1, col + 1));
            }
            (Some(b'?'), 'n', 6) => {
                let (row, col) = screen.cursor_position();
                self.reply_fmt(format_args!("\x1b[?{};{}R", row + 1, col + 1));
            }
            (None, 't', 18) => {
                let (rows, cols) = screen.size();
                self.reply_fmt(format_args!("\x1b[8;{rows};{cols}t"));
            }
            _ => {}
        }
    }

    /// Handles OSC color queries that expect an answer from the terminal.
    ///
    /// Some TUIs query foreground (`OSC 10`), background (`OSC 11`), or cursor
    /// color (`OSC 12`) before choosing a color palette. We return simple light
    /// foreground/cursor and dark background values so those applications can
    /// proceed instead of waiting for a response that never arrives.
    fn unhandled_osc(&mut self, _: &mut vt100::Screen, params: &[&[u8]]) {
        match params {
            [b"10", b"?"] => self.reply(b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\"),
            [b"11", b"?"] => self.reply(b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
            [b"12", b"?"] => self.reply(b"\x1b]12;rgb:ffff/ffff/ffff\x1b\\"),
            _ => {}
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_local_runs_a_shell() {
        // Skip when the sandbox has no usable shell rather than failing spuriously.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        if !std::path::Path::new(&shell).exists() {
            eprintln!("skipping: no shell at {shell}");
            return;
        }

        let mut session =
            Session::spawn_local("local-1".to_string(), 24, 80).expect("spawn local shell");
        assert_eq!(session.alias, "local-1");
        assert!(matches!(session.status(), SessionStatus::Running));

        // Ask the shell to print a sentinel, then poll the parsed screen for it.
        session.write(b"printf hosttui_ok\n");
        let mut found = false;
        for _ in 0..50 {
            let screen = session.screen();
            if screen.contents().contains("hosttui_ok") {
                found = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(found, "expected sentinel output from local shell");
    }

    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn parser() -> (TerminalParser, Arc<Mutex<Vec<u8>>>) {
        let output = Arc::new(Mutex::new(Vec::new()));
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(Box::new(SharedBuf(Arc::clone(&output)))));
        (
            vt100::Parser::new_with_callbacks(24, 80, 0, TerminalCallbacks::new(writer)),
            output,
        )
    }

    fn output(buffer: &Arc<Mutex<Vec<u8>>>) -> Vec<u8> {
        buffer.lock().unwrap().clone()
    }

    #[test]
    fn replies_to_cursor_position_report() {
        let (mut parser, buffer) = parser();

        parser.process(b"\x1b[10;20H\x1b[6n");

        assert_eq!(output(&buffer), b"\x1b[10;20R".to_vec());
    }

    #[test]
    fn replies_to_device_attributes() {
        let (mut parser, buffer) = parser();

        parser.process(b"\x1b[c\x1b[>c");

        assert_eq!(output(&buffer), b"\x1b[?1;2c\x1b[>0;136;0c".to_vec());
    }

    #[test]
    fn replies_to_terminal_color_queries() {
        let (mut parser, buffer) = parser();

        parser.process(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\");

        assert_eq!(
            output(&buffer),
            b"\x1b]10;rgb:ffff/ffff/ffff\x1b\\\x1b]11;rgb:0000/0000/0000\x1b\\".to_vec()
        );
    }
}
