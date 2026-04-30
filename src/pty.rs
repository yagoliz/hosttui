use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::model::Host;
use crate::ssh;

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
    writer: Box<dyn Write + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
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

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(io::Error::other)?;

        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(io::Error::other)?;
        let writer = pair
            .master
            .take_writer()
            .map_err(io::Error::other)?;

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 0)));
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
        parser: Arc<Mutex<vt100::Parser>>,
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
        let _ = self.writer.write_all(data);
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
                Ok(Some(es)) => {
                    SessionStatus::Exited(if es.success() { Some(0) } else { Some(1) })
                }
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

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}
