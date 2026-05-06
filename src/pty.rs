use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::model::Host;
use crate::ssh;

type TerminalParser = vt100::Parser<TerminalCallbacks>;

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

pub struct Session {
    pub alias: String,
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
    pub fn spawn(host: &Host, rows: u16, cols: u16) -> io::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;

        let args = ssh::ssh_args(host);
        let mut cmd = CommandBuilder::new("ssh");
        for arg in &args {
            cmd.arg(arg);
        }

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
            alias: host.alias.clone(),
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

    pub fn write(&mut self, data: &[u8]) {
        self.scroll_reset();
        let _ = self.writer.lock().unwrap().write_all(data);
    }

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

    pub fn scroll_up(&self, lines: usize) {
        let mut parser = self.parser.lock().unwrap();
        let current = parser.screen().scrollback();
        parser.screen_mut().set_scrollback(current + lines);
    }

    pub fn scroll_down(&self, lines: usize) {
        let mut parser = self.parser.lock().unwrap();
        let current = parser.screen().scrollback();
        parser
            .screen_mut()
            .set_scrollback(current.saturating_sub(lines));
    }

    pub fn scroll_reset(&self) {
        self.parser.lock().unwrap().screen_mut().set_scrollback(0);
    }

    pub fn scrollback_pos(&self) -> usize {
        self.parser.lock().unwrap().screen().scrollback()
    }

    pub fn screen(&self) -> vt100::Screen {
        self.parser.lock().unwrap().screen().clone()
    }

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
