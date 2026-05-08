use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

use ssh2::{Session, Sftp};

use crate::error::Error;
use crate::model::Host;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const KEEPALIVE_INTERVAL: u32 = 30;

/// Status of an in-progress SFTP connection attempt.
///
/// The connection runs on a background thread and updates a shared
/// `Arc<Mutex<SftpConnectionStatus>>` so the UI can render progress without
/// blocking. `NeedsPassword` pauses the flow until the main loop collects
/// credentials and retries authentication.
#[derive(Debug, Clone)]
pub enum SftpConnectionStatus {
    Connecting,
    NeedsPassword,
    Connected,
    /// Transient state set by the connect thread when it has both the connection
    /// and the initial remote listing ready. The main loop picks up the data via
    /// `App::poll_file_transfers` and moves it into the `FileBrowser`, then
    /// transitions to `Connected`.
    ConnectedWithData {
        home_dir: String,
        entries: Vec<FileEntry>,
    },
    Failed(String),
}

/// A single entry in a directory listing, local or remote.
///
/// This is a UI-oriented struct shared between the local filesystem and SFTP
/// sides of the file browser. Fields are kept simple (no platform-specific
/// metadata) so the same rendering code works for both panes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<u64>,
    pub permissions: Option<u32>,
}

impl FileEntry {
    /// Ordering key: directories sort before files, then alphabetically by name.
    fn sort_key(&self) -> (u8, &str) {
        (if self.is_dir { 0 } else { 1 }, &self.name)
    }
}

/// Sorts entries with directories first, then alphabetically by name.
pub fn sort_entries_by_name(entries: &mut [FileEntry]) {
    entries.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
}

/// An established SFTP connection to a remote host.
///
/// Owns the underlying `ssh2::Session` and `ssh2::Sftp` handle. The session is
/// `Send` but not `Sync`, so callers serialize access through an
/// `Arc<Mutex<SftpConnection>>` when sharing between the UI thread and
/// background transfer threads.
pub struct SftpConnection {
    session: Session,
    sftp: Sftp,
}

impl std::fmt::Debug for SftpConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SftpConnection").finish_non_exhaustive()
    }
}

impl SftpConnection {
    /// Connects to a host and opens the SFTP subsystem.
    ///
    /// The auth cascade tries the host's explicit identity file first, then
    /// the SSH agent, then well-known default key paths. If nothing succeeds,
    /// returns `NeedsPassword` so the UI can prompt for credentials.
    pub fn connect(host: &Host) -> Result<Self, ConnectOutcome> {
        let addr = format!("{}:{}", host.hostname, host.port);
        let tcp = TcpStream::connect_timeout(
            &addr
                .parse()
                .or_else(|_| {
                    use std::net::ToSocketAddrs;
                    addr.to_socket_addrs()
                        .map_err(|e| e.to_string())?
                        .next()
                        .ok_or_else(|| "no addresses found".to_string())
                })
                .map_err(|e| ConnectOutcome::Failed(format!("DNS resolution failed: {e}")))?,
            CONNECT_TIMEOUT,
        )
        .map_err(|e| ConnectOutcome::Failed(format!("TCP connect failed: {e}")))?;

        let mut session = Session::new()
            .map_err(|e| ConnectOutcome::Failed(format!("SSH session init failed: {e}")))?;
        session.set_tcp_stream(tcp);
        session
            .handshake()
            .map_err(|e| ConnectOutcome::Failed(format!("SSH handshake failed: {e}")))?;
        session.set_keepalive(true, KEEPALIVE_INTERVAL);

        if Self::try_auth(&session, host)? {
            let sftp = session.sftp().map_err(|e| {
                ConnectOutcome::Failed(format!("Failed to open SFTP subsystem: {e}"))
            })?;
            return Ok(SftpConnection { session, sftp });
        }

        Err(ConnectOutcome::NeedsPassword)
    }

    /// Attempts non-interactive authentication methods.
    ///
    /// Key files (explicit identity file, then well-known defaults) are tried
    /// first without a passphrase. The SSH agent is last because
    /// `userauth_agent` iterates every loaded key — each rejection counts
    /// against the server's `MaxAuthTries`, which can be exhausted before the
    /// correct key file ever gets a chance.
    fn try_auth(session: &Session, host: &Host) -> Result<bool, ConnectOutcome> {
        // 1. Key files without passphrase
        if Self::try_pubkey_files(session, host, None) {
            return Ok(true);
        }

        // 2. SSH agent
        if session.userauth_agent(&host.user).is_ok() && session.authenticated() {
            return Ok(true);
        }

        Ok(false)
    }

    /// Tries key-file authentication with an optional passphrase.
    ///
    /// Checks the host's explicit identity file first, then well-known default
    /// key paths (~/.ssh/id_ed25519, id_rsa, id_ecdsa). Returns `true` as soon
    /// as one succeeds. Used both for the initial passwordless attempt and for
    /// retrying with a user-supplied passphrase.
    fn try_pubkey_files(session: &Session, host: &Host, passphrase: Option<&str>) -> bool {
        // Explicit identity file from host config — highest priority
        if let Some(ref key_path) = host.identity_file {
            let expanded = shellexpand_tilde(key_path);
            let path = Path::new(&expanded);
            if path.exists()
                && session
                    .userauth_pubkey_file(&host.user, None, path, passphrase)
                    .is_ok()
                && session.authenticated()
            {
                return true;
            }
        }

        // Common default key paths
        if let Some(home) = dirs::home_dir() {
            let default_keys = ["id_ed25519", "id_rsa", "id_ecdsa"];
            for key_name in &default_keys {
                let path = home.join(".ssh").join(key_name);
                if path.exists()
                    && session
                        .userauth_pubkey_file(&host.user, None, &path, passphrase)
                        .is_ok()
                    && session.authenticated()
                {
                    return true;
                }
            }
        }

        false
    }

    /// Retries authentication using a user-supplied credential after `NeedsPassword`.
    ///
    /// The credential is tried as a key-file passphrase first (explicit identity
    /// file, then default key paths). Only if no key succeeds does it fall back
    /// to SSH password authentication. This handles the common case where a
    /// server disables password auth but the user's key is passphrase-protected.
    pub fn connect_with_password(host: &Host, password: &str) -> Result<Self, ConnectOutcome> {
        let addr = format!("{}:{}", host.hostname, host.port);
        let tcp = TcpStream::connect_timeout(
            &addr
                .parse()
                .or_else(|_| {
                    use std::net::ToSocketAddrs;
                    addr.to_socket_addrs()
                        .map_err(|e| e.to_string())?
                        .next()
                        .ok_or_else(|| "no addresses found".to_string())
                })
                .map_err(|e| ConnectOutcome::Failed(format!("DNS resolution failed: {e}")))?,
            CONNECT_TIMEOUT,
        )
        .map_err(|e| ConnectOutcome::Failed(format!("TCP connect failed: {e}")))?;

        let mut session = Session::new()
            .map_err(|e| ConnectOutcome::Failed(format!("SSH session init failed: {e}")))?;
        session.set_tcp_stream(tcp);
        session
            .handshake()
            .map_err(|e| ConnectOutcome::Failed(format!("SSH handshake failed: {e}")))?;
        session.set_keepalive(true, KEEPALIVE_INTERVAL);

        // Try key files with the credential as passphrase first
        if !Self::try_pubkey_files(&session, host, Some(password)) {
            // Fall back to SSH password authentication
            session
                .userauth_password(&host.user, password)
                .map_err(|e| ConnectOutcome::Failed(format!("Authentication failed: {e}")))?;
        }

        if !session.authenticated() {
            return Err(ConnectOutcome::Failed(
                "Authentication failed: invalid credentials".into(),
            ));
        }

        let sftp = session
            .sftp()
            .map_err(|e| ConnectOutcome::Failed(format!("Failed to open SFTP subsystem: {e}")))?;

        Ok(SftpConnection { session, sftp })
    }

    /// Lists directory contents on the remote host.
    pub fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>, Error> {
        let entries = self
            .sftp
            .readdir(Path::new(path))
            .map_err(|e| Error::SftpOperation {
                operation: format!("list_dir '{path}'"),
                source: e,
            })?;

        let mut result: Vec<FileEntry> = entries
            .into_iter()
            .filter_map(|(pathbuf, stat)| {
                let name = pathbuf.file_name()?.to_string_lossy().into_owned();
                Some(FileEntry {
                    name,
                    is_dir: stat.is_dir(),
                    size: stat.size.unwrap_or(0),
                    modified: stat.mtime,
                    permissions: stat.perm,
                })
            })
            .collect();

        sort_entries_by_name(&mut result);
        Ok(result)
    }

    /// Returns metadata for a single remote path.
    pub fn stat(&self, path: &str) -> Result<FileEntry, Error> {
        let stat = self
            .sftp
            .stat(Path::new(path))
            .map_err(|e| Error::SftpOperation {
                operation: format!("stat '{path}'"),
                source: e,
            })?;

        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        Ok(FileEntry {
            name,
            is_dir: stat.is_dir(),
            size: stat.size.unwrap_or(0),
            modified: stat.mtime,
            permissions: stat.perm,
        })
    }

    /// Returns the remote user's home directory path.
    ///
    /// Uses SFTP `realpath(".")` which resolves to the default directory the
    /// server places the session in — typically the user's home.
    pub fn home_dir(&self) -> Result<String, Error> {
        let path = self
            .sftp
            .realpath(Path::new("."))
            .map_err(|e| Error::SftpOperation {
                operation: "home_dir".into(),
                source: e,
            })?;
        Ok(path.to_string_lossy().into_owned())
    }

    /// Returns a reference to the underlying SFTP handle for transfer operations.
    pub fn sftp(&self) -> &Sftp {
        &self.sftp
    }

    /// Returns a reference to the underlying SSH session.
    pub fn session(&self) -> &Session {
        &self.session
    }
}

/// Result of a connection attempt that distinguishes auth-needed from hard failure.
///
/// This is not a general error type — it only covers the connect/auth handshake.
/// Once connected, SFTP operations use `crate::error::Error` instead.
#[derive(Debug)]
pub enum ConnectOutcome {
    NeedsPassword,
    Failed(String),
}

impl std::fmt::Display for ConnectOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectOutcome::NeedsPassword => write!(f, "password authentication required"),
            ConnectOutcome::Failed(msg) => write!(f, "{msg}"),
        }
    }
}

/// Expands a leading `~` to the user's home directory.
fn shellexpand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    } else if path == "~"
        && let Some(home) = dirs::home_dir()
    {
        return home.to_string_lossy().into_owned();
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, is_dir: bool, size: u64) -> FileEntry {
        FileEntry {
            name: name.into(),
            is_dir,
            size,
            modified: None,
            permissions: None,
        }
    }

    #[test]
    fn file_entry_construction() {
        let e = FileEntry {
            name: "test.txt".into(),
            is_dir: false,
            size: 1024,
            modified: Some(1700000000),
            permissions: Some(0o644),
        };
        assert_eq!(e.name, "test.txt");
        assert!(!e.is_dir);
        assert_eq!(e.size, 1024);
        assert_eq!(e.modified, Some(1700000000));
        assert_eq!(e.permissions, Some(0o644));
    }

    #[test]
    fn file_entry_directory() {
        let e = FileEntry {
            name: "src".into(),
            is_dir: true,
            size: 4096,
            modified: None,
            permissions: Some(0o755),
        };
        assert!(e.is_dir);
        assert_eq!(e.permissions, Some(0o755));
    }

    #[test]
    fn sort_entries_dirs_first() {
        let mut entries = vec![
            entry("zebra.txt", false, 100),
            entry("alpha", true, 0),
            entry("beta.txt", false, 200),
            entry("delta", true, 0),
        ];
        sort_entries_by_name(&mut entries);

        assert_eq!(entries[0].name, "alpha");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "delta");
        assert!(entries[1].is_dir);
        assert_eq!(entries[2].name, "beta.txt");
        assert!(!entries[2].is_dir);
        assert_eq!(entries[3].name, "zebra.txt");
        assert!(!entries[3].is_dir);
    }

    #[test]
    fn sort_entries_alphabetical_within_group() {
        let mut entries = vec![
            entry("c_dir", true, 0),
            entry("a_dir", true, 0),
            entry("b_dir", true, 0),
            entry("z.txt", false, 10),
            entry("a.txt", false, 20),
        ];
        sort_entries_by_name(&mut entries);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a_dir", "b_dir", "c_dir", "a.txt", "z.txt"]);
    }

    #[test]
    fn sort_empty_entries() {
        let mut entries: Vec<FileEntry> = vec![];
        sort_entries_by_name(&mut entries);
        assert!(entries.is_empty());
    }

    #[test]
    fn sort_single_entry() {
        let mut entries = vec![entry("only", false, 42)];
        sort_entries_by_name(&mut entries);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "only");
    }

    #[test]
    fn shellexpand_tilde_expands_home() {
        let expanded = shellexpand_tilde("~/foo/bar");
        assert!(!expanded.starts_with('~'));
        assert!(expanded.ends_with("foo/bar"));
    }

    #[test]
    fn shellexpand_tilde_no_tilde() {
        let path = "/absolute/path";
        assert_eq!(shellexpand_tilde(path), path);
    }

    #[test]
    fn shellexpand_tilde_bare() {
        let expanded = shellexpand_tilde("~");
        assert!(!expanded.starts_with('~') || expanded == "~");
    }

    #[test]
    fn connect_outcome_display() {
        let needs_pw = ConnectOutcome::NeedsPassword;
        assert_eq!(needs_pw.to_string(), "password authentication required");

        let failed = ConnectOutcome::Failed("timeout".into());
        assert_eq!(failed.to_string(), "timeout");
    }

    #[test]
    fn file_entry_equality() {
        let a = entry("same.txt", false, 100);
        let b = entry("same.txt", false, 100);
        assert_eq!(a, b);

        let c = entry("different.txt", false, 100);
        assert_ne!(a, c);
    }

    #[test]
    #[ignore]
    fn integration_connect_localhost() {
        let host = Host {
            alias: "localhost".into(),
            hostname: "127.0.0.1".into(),
            user: std::env::var("USER").unwrap_or_else(|_| "root".into()),
            port: 22,
            identity_file: None,
            group: None,
            extra: vec![],
            details: String::new(),
            last_accessed: String::new(),
        };

        match SftpConnection::connect(&host) {
            Ok(conn) => {
                let home = conn.home_dir().expect("should resolve home dir");
                assert!(!home.is_empty());

                let entries = conn.list_dir(&home).expect("should list home dir");
                assert!(!entries.is_empty());
            }
            Err(ConnectOutcome::NeedsPassword) => {
                // Acceptable in CI or environments without agent-forwarded keys
            }
            Err(ConnectOutcome::Failed(msg)) => {
                panic!("connection failed: {msg}");
            }
        }
    }
}
