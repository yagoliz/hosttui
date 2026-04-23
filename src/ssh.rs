use std::io;
use std::process::Command;

use crossterm::{cursor, execute, terminal};

use crate::error::Error;
use crate::model::Host;

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
