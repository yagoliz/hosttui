use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::process::Command;
use std::time::Duration;

use crossterm::{cursor, execute, terminal};

use crate::error::Error;
use crate::model::Host;

/// TCP-level reachability check run before handing the terminal to ssh,
/// so an unreachable host can be cancelled from the TUI instead of blocking
/// on ssh's full ConnectTimeout with no way to recover.
pub fn probe(hostname: &str, port: u16, timeout: Duration) -> io::Result<()> {
    let addr = format!("{hostname}:{port}")
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no address for host"))?;
    TcpStream::connect_timeout(&addr, timeout).map(|_| ())
}

struct SuspendGuard;

impl SuspendGuard {
    fn new() -> io::Result<Self> {
        terminal::disable_raw_mode()?;
        execute!(io::stdout(), terminal::LeaveAlternateScreen, cursor::Show)?;
        Ok(SuspendGuard)
    }
}

impl Drop for SuspendGuard {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide
        );
        let _ = terminal::enable_raw_mode();
    }
}

pub fn connect(host: &Host) -> Result<(), Error> {
    let _guard = SuspendGuard::new().map_err(|e| Error::Ssh {
        alias: host.alias.clone(),
        source: e,
    })?;

    let mut cmd = Command::new("ssh");

    if let Some(ref identity) = host.identity_file {
        cmd.args(["-i", identity]);
    }

    if host.port != 22 {
        cmd.args(["-p", &host.port.to_string()]);
    }

    // Defaults — bounded so a silently-dropped connection eventually surfaces
    // instead of hanging forever. Extras below may override any of these.
    cmd.args(["-o", "ConnectTimeout=5"]);
    cmd.args(["-o", "ServerAliveInterval=10"]);
    cmd.args(["-o", "ServerAliveCountMax=3"]);

    for (key, val) in &host.extra {
        cmd.args(["-o", &format!("{key}={val}")]);
    }

    cmd.arg(format!("{}@{}", host.user, host.hostname));

    cmd.status().map_err(|e| Error::Ssh {
        alias: host.alias.clone(),
        source: e,
    })?;

    Ok(())
}
