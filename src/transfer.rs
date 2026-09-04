use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::sftp::{FileEntry, SftpConnection};

const CHUNK_SIZE: usize = 32 * 1024;
/// Remote operations needed to walk and download a directory tree.
///
/// Keeping this boundary narrower than `SftpConnection` makes the recursive
/// behavior deterministic to test without changing the production SFTP path.
trait RemoteDirectorySource {
    type File: Read;

    fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>, String>;
    fn open_file(&self, path: &str) -> Result<Self::File, String>;
}

impl RemoteDirectorySource for SftpConnection {
    type File = ssh2::File;

    fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>, String> {
        SftpConnection::list_dir(self, path).map_err(|e| e.to_string())
    }

    fn open_file(&self, path: &str) -> Result<Self::File, String> {
        self.sftp()
            .open(Path::new(path))
            .map_err(|e| format!("open remote '{path}': {e}"))
    }
}

/// What to transfer and in which direction.
#[derive(Debug, Clone)]
pub enum TransferRequest {
    Upload {
        local_path: PathBuf,
        remote_path: String,
    },
    Download {
        remote_path: String,
        local_path: PathBuf,
    },
    UploadDir {
        local_path: PathBuf,
        remote_path: String,
    },
    DownloadDir {
        remote_path: String,
        local_path: PathBuf,
    },
}

/// Terminal state of a transfer. Transitions are one-way out of `InProgress`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferStatus {
    InProgress,
    Completed,
    Failed(String),
    Cancelled,
}

/// Shared snapshot of transfer progress, polled by the main loop each tick.
///
/// The background transfer thread updates `bytes_transferred` after each
/// 32 KB chunk. The UI reads this via `FileBrowser::active_transfer_progress()`
/// to render the progress bar without blocking on the SFTP mutex.
#[derive(Debug, Clone)]
pub struct TransferProgress {
    pub file_name: String,
    pub label: String,
    pub bytes_transferred: u64,
    pub total_bytes: u64,
    pub status: TransferStatus,
}

/// Handle to a running background transfer thread.
///
/// The main loop reads `progress` each tick for the UI, and can set
/// `cancel` to request cooperative cancellation at the next chunk boundary.
pub struct TransferHandle {
    pub progress: Arc<Mutex<TransferProgress>>,
    pub cancel: Arc<AtomicBool>,
    pub join_handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for TransferHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransferHandle").finish_non_exhaustive()
    }
}

/// Spawns a file transfer on a background thread and returns a handle.
///
/// The caller pushes the returned handle into `FileBrowser::transfers`.
/// The main loop polls `handle.progress` each tick and removes the handle
/// once the status leaves `InProgress`.
pub fn spawn_transfer(
    sftp: Arc<Mutex<Option<SftpConnection>>>,
    request: TransferRequest,
) -> TransferHandle {
    let (file_name, label) = match &request {
        TransferRequest::Upload { local_path, .. } => (
            local_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            "Uploading".to_string(),
        ),
        TransferRequest::Download { remote_path, .. } => (
            remote_path
                .rsplit('/')
                .next()
                .unwrap_or(remote_path)
                .to_string(),
            "Downloading".to_string(),
        ),
        TransferRequest::UploadDir { local_path, .. } => (
            local_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            "Uploading folder".to_string(),
        ),
        TransferRequest::DownloadDir { remote_path, .. } => (
            remote_path
                .rsplit('/')
                .next()
                .unwrap_or(remote_path)
                .to_string(),
            "Downloading folder".to_string(),
        ),
    };

    let progress = Arc::new(Mutex::new(TransferProgress {
        file_name,
        label,
        bytes_transferred: 0,
        total_bytes: 0,
        status: TransferStatus::InProgress,
    }));
    let cancel = Arc::new(AtomicBool::new(false));

    let p = Arc::clone(&progress);
    let c = Arc::clone(&cancel);

    let join_handle = std::thread::spawn(move || match request {
        TransferRequest::Download {
            remote_path,
            local_path,
        } => run_download(sftp, remote_path, local_path, p, c),
        TransferRequest::Upload {
            local_path,
            remote_path,
        } => run_upload(sftp, local_path, remote_path, p, c),
        TransferRequest::UploadDir {
            local_path,
            remote_path,
        } => run_upload_dir(sftp, local_path, remote_path, p, c),
        TransferRequest::DownloadDir {
            remote_path,
            local_path,
        } => run_download_dir(sftp, remote_path, local_path, p, c),
    });

    TransferHandle {
        progress,
        cancel,
        join_handle: Some(join_handle),
    }
}

/// Executes a download: remote file → local file.
///
/// The SFTP mutex is held for the entire transfer because `ssh2::File`
/// borrows the `Sftp` handle. A scoped block ensures the mutex drops
/// before setting the final status, so `poll_file_transfers` can acquire
/// the lock for a post-transfer directory refresh.
fn run_download(
    sftp: Arc<Mutex<Option<SftpConnection>>>,
    remote_path: String,
    local_path: PathBuf,
    progress: Arc<Mutex<TransferProgress>>,
    cancel: Arc<AtomicBool>,
) {
    let result: Result<(), String> = (|| {
        let guard = sftp.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| "Not connected".to_string())?;

        let stat = conn
            .sftp()
            .stat(Path::new(&remote_path))
            .map_err(|e| format!("stat failed: {e}"))?;
        let total = stat.size.unwrap_or(0);
        progress.lock().unwrap().total_bytes = total;

        let mut remote_file = conn
            .sftp()
            .open(Path::new(&remote_path))
            .map_err(|e| format!("open remote file failed: {e}"))?;

        let mut local_file =
            std::fs::File::create(&local_path).map_err(|e| format!("create local file: {e}"))?;

        let mut buf = [0u8; CHUNK_SIZE];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("__cancelled__".into());
            }

            let n = remote_file
                .read(&mut buf)
                .map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                break;
            }

            local_file
                .write_all(&buf[..n])
                .map_err(|e| format!("write: {e}"))?;
            progress.lock().unwrap().bytes_transferred += n as u64;
        }

        Ok(())
    })();

    match result {
        Ok(()) => {
            progress.lock().unwrap().status = TransferStatus::Completed;
        }
        Err(msg) if msg == "__cancelled__" => {
            let _ = std::fs::remove_file(&local_path);
            progress.lock().unwrap().status = TransferStatus::Cancelled;
        }
        Err(msg) => {
            let _ = std::fs::remove_file(&local_path);
            progress.lock().unwrap().status = TransferStatus::Failed(msg);
        }
    }
}

/// Executes an upload: local file → remote file.
///
/// Same mutex-holding constraint as `run_download`. On failure, attempts
/// to remove the partial remote file but does not panic if cleanup fails.
fn run_upload(
    sftp: Arc<Mutex<Option<SftpConnection>>>,
    local_path: PathBuf,
    remote_path: String,
    progress: Arc<Mutex<TransferProgress>>,
    cancel: Arc<AtomicBool>,
) {
    let total = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);
    progress.lock().unwrap().total_bytes = total;

    let result: Result<(), String> = (|| {
        let mut local_file =
            std::fs::File::open(&local_path).map_err(|e| format!("open local file: {e}"))?;

        let guard = sftp.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| "Not connected".to_string())?;

        let mut remote_file = conn
            .sftp()
            .create(Path::new(&remote_path))
            .map_err(|e| format!("create remote file: {e}"))?;

        let mut buf = [0u8; CHUNK_SIZE];
        loop {
            if cancel.load(Ordering::Relaxed) {
                drop(remote_file);
                let _ = conn.sftp().unlink(Path::new(&remote_path));
                return Err("__cancelled__".into());
            }

            let n = local_file
                .read(&mut buf)
                .map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                break;
            }

            remote_file
                .write_all(&buf[..n])
                .map_err(|e| format!("write: {e}"))?;
            progress.lock().unwrap().bytes_transferred += n as u64;
        }

        Ok(())
    })();

    match result {
        Ok(()) => {
            progress.lock().unwrap().status = TransferStatus::Completed;
        }
        Err(msg) if msg == "__cancelled__" => {
            progress.lock().unwrap().status = TransferStatus::Cancelled;
        }
        Err(msg) => {
            progress.lock().unwrap().status = TransferStatus::Failed(msg);
        }
    }
}

/// Recursively uploads a local directory to a remote path.
///
/// Walks the local tree depth-first: creates remote directories first, then
/// transfers each file sequentially. `total_bytes` is computed upfront by
/// walking the local tree so the progress bar can show a meaningful percentage.
fn run_upload_dir(
    sftp: Arc<Mutex<Option<SftpConnection>>>,
    local_root: PathBuf,
    remote_root: String,
    progress: Arc<Mutex<TransferProgress>>,
    cancel: Arc<AtomicBool>,
) {
    let total = dir_total_bytes(&local_root);
    progress.lock().unwrap().total_bytes = total;

    let result: Result<(), String> = (|| {
        let guard = sftp.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| "Not connected".to_string())?;

        upload_dir_inner(conn, &local_root, &remote_root, &progress, &cancel)
    })();

    match result {
        Ok(()) => {
            progress.lock().unwrap().status = TransferStatus::Completed;
        }
        Err(msg) if msg == "__cancelled__" => {
            progress.lock().unwrap().status = TransferStatus::Cancelled;
        }
        Err(msg) => {
            progress.lock().unwrap().status = TransferStatus::Failed(msg);
        }
    }
}

/// Inner recursive helper for directory upload (runs while holding the SFTP mutex).
fn upload_dir_inner(
    conn: &SftpConnection,
    local_dir: &Path,
    remote_dir: &str,
    progress: &Arc<Mutex<TransferProgress>>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    if let Err(e) = conn.sftp().mkdir(Path::new(remote_dir), 0o755)
        && conn.sftp().stat(Path::new(remote_dir)).is_err()
    {
        return Err(format!("mkdir '{remote_dir}': {e}"));
    }

    let entries = std::fs::read_dir(local_dir).map_err(|e| format!("read local dir: {e}"))?;

    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err("__cancelled__".into());
        }

        let entry = entry.map_err(|e| format!("read entry: {e}"))?;
        let meta = entry.metadata().map_err(|e| format!("metadata: {e}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let child_remote = format!("{}/{}", remote_dir.trim_end_matches('/'), name);

        if meta.is_dir() {
            upload_dir_inner(conn, &entry.path(), &child_remote, progress, cancel)?;
        } else {
            progress.lock().unwrap().file_name = name;

            let mut local_file =
                std::fs::File::open(entry.path()).map_err(|e| format!("open local: {e}"))?;
            let mut remote_file = conn
                .sftp()
                .create(Path::new(&child_remote))
                .map_err(|e| format!("create remote: {e}"))?;

            let mut buf = [0u8; CHUNK_SIZE];
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("__cancelled__".into());
                }
                let n = local_file
                    .read(&mut buf)
                    .map_err(|e| format!("read: {e}"))?;
                if n == 0 {
                    break;
                }
                remote_file
                    .write_all(&buf[..n])
                    .map_err(|e| format!("write: {e}"))?;
                progress.lock().unwrap().bytes_transferred += n as u64;
            }
        }
    }
    Ok(())
}

/// Recursively downloads a remote directory to a local path.
///
/// Mirrors the remote tree into the local filesystem: creates local
/// directories first, then transfers each file sequentially.
fn run_download_dir(
    sftp: Arc<Mutex<Option<SftpConnection>>>,
    remote_root: String,
    local_root: PathBuf,
    progress: Arc<Mutex<TransferProgress>>,
    cancel: Arc<AtomicBool>,
) {
    let result: Result<(), String> = (|| {
        let guard = sftp.lock().unwrap();
        let conn = guard.as_ref().ok_or_else(|| "Not connected".to_string())?;

        let total = remote_dir_total_bytes(conn, &remote_root)?;
        progress.lock().unwrap().total_bytes = total;

        download_dir_inner(conn, &remote_root, &local_root, &progress, &cancel)
    })();

    match result {
        Ok(()) => {
            progress.lock().unwrap().status = TransferStatus::Completed;
        }
        Err(msg) if msg == "__cancelled__" => {
            progress.lock().unwrap().status = TransferStatus::Cancelled;
        }
        Err(msg) => {
            progress.lock().unwrap().status = TransferStatus::Failed(msg);
        }
    }
}

/// Inner recursive helper for directory download (runs while holding the SFTP mutex).
fn download_dir_inner<R: RemoteDirectorySource>(
    conn: &R,
    remote_dir: &str,
    local_dir: &Path,
    progress: &Arc<Mutex<TransferProgress>>,
    cancel: &Arc<AtomicBool>,
) -> Result<(), String> {
    std::fs::create_dir_all(local_dir).map_err(|e| format!("mkdir local: {e}"))?;

    let entries = conn.list_dir(remote_dir)?;

    for entry in &entries {
        if cancel.load(Ordering::Relaxed) {
            return Err("__cancelled__".into());
        }

        let child_remote = format!("{}/{}", remote_dir.trim_end_matches('/'), entry.name);
        let child_local = local_dir.join(&entry.name);

        if entry.is_dir {
            download_dir_inner(conn, &child_remote, &child_local, progress, cancel)?;
        } else {
            progress.lock().unwrap().file_name = entry.name.clone();

            let mut remote_file = conn.open_file(&child_remote)?;
            let mut local_file =
                std::fs::File::create(&child_local).map_err(|e| format!("create local: {e}"))?;

            let mut buf = [0u8; CHUNK_SIZE];
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err("__cancelled__".into());
                }
                let n = remote_file
                    .read(&mut buf)
                    .map_err(|e| format!("read: {e}"))?;
                if n == 0 {
                    break;
                }
                local_file
                    .write_all(&buf[..n])
                    .map_err(|e| format!("write: {e}"))?;
                progress.lock().unwrap().bytes_transferred += n as u64;
            }
        }
    }
    Ok(())
}

/// Walks a local directory tree and sums all file sizes.
fn dir_total_bytes(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += dir_total_bytes(&entry.path());
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// Walks a remote directory tree via SFTP and sums all file sizes.
fn remote_dir_total_bytes<R: RemoteDirectorySource>(conn: &R, path: &str) -> Result<u64, String> {
    let entries = conn.list_dir(path)?;
    let mut total = 0u64;
    for entry in &entries {
        if entry.is_dir {
            let child = format!("{}/{}", path.trim_end_matches('/'), entry.name);
            total += remote_dir_total_bytes(conn, &child)?;
        } else {
            total += entry.size;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::Cursor;

    #[test]
    fn spawn_transfer_sets_label_upload() {
        let sftp = Arc::new(Mutex::new(None));
        let handle = spawn_transfer(
            sftp,
            TransferRequest::Upload {
                local_path: PathBuf::from("/tmp/readme.md"),
                remote_path: "/home/user/readme.md".into(),
            },
        );
        let progress = handle.progress.lock().unwrap();
        assert_eq!(progress.label, "Uploading");
        assert_eq!(progress.file_name, "readme.md");
        // Transfer will fail (no connection) but we just test initial state
        drop(progress);
        // Wait for thread to finish
        if let Some(jh) = handle.join_handle {
            let _ = jh.join();
        }
    }

    #[test]
    fn spawn_transfer_sets_label_download() {
        let sftp = Arc::new(Mutex::new(None));
        let handle = spawn_transfer(
            sftp,
            TransferRequest::Download {
                remote_path: "/home/user/data.csv".into(),
                local_path: PathBuf::from("/tmp/data.csv"),
            },
        );
        let progress = handle.progress.lock().unwrap();
        assert_eq!(progress.label, "Downloading");
        assert_eq!(progress.file_name, "data.csv");
        drop(progress);
        if let Some(jh) = handle.join_handle {
            let _ = jh.join();
        }
    }

    #[test]
    fn transfer_fails_when_not_connected() {
        let sftp = Arc::new(Mutex::new(None));
        let handle = spawn_transfer(
            sftp,
            TransferRequest::Download {
                remote_path: "/home/user/file.txt".into(),
                local_path: PathBuf::from("/tmp/test_transfer_no_conn.txt"),
            },
        );
        if let Some(jh) = handle.join_handle {
            let _ = jh.join();
        }
        let progress = handle.progress.lock().unwrap();
        assert!(
            matches!(progress.status, TransferStatus::Failed(ref msg) if msg.contains("Not connected"))
        );
    }

    #[test]
    fn dir_total_bytes_sums_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::write(dir.path().join("b.txt"), "world!").unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("c.txt"), "!").unwrap();

        let total = dir_total_bytes(dir.path());
        assert_eq!(total, 5 + 6 + 1);
    }

    #[test]
    fn dir_total_bytes_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dir_total_bytes(dir.path()), 0);
    }

    #[test]
    fn spawn_transfer_sets_label_upload_dir() {
        let sftp = Arc::new(Mutex::new(None));
        let handle = spawn_transfer(
            sftp,
            TransferRequest::UploadDir {
                local_path: PathBuf::from("/tmp/mydir"),
                remote_path: "/home/user/mydir".into(),
            },
        );
        let progress = handle.progress.lock().unwrap();
        assert_eq!(progress.label, "Uploading folder");
        assert_eq!(progress.file_name, "mydir");
        drop(progress);
        if let Some(jh) = handle.join_handle {
            let _ = jh.join();
        }
    }

    #[test]
    fn spawn_transfer_sets_label_download_dir() {
        let sftp = Arc::new(Mutex::new(None));
        let handle = spawn_transfer(
            sftp,
            TransferRequest::DownloadDir {
                remote_path: "/home/user/mydir".into(),
                local_path: PathBuf::from("/tmp/mydir"),
            },
        );
        let progress = handle.progress.lock().unwrap();
        assert_eq!(progress.label, "Downloading folder");
        assert_eq!(progress.file_name, "mydir");
        drop(progress);
        if let Some(jh) = handle.join_handle {
            let _ = jh.join();
        }
    }

    struct FakeRemote {
        directories: HashMap<String, Vec<FileEntry>>,
        files: HashMap<String, Vec<u8>>,
    }

    impl RemoteDirectorySource for FakeRemote {
        type File = Cursor<Vec<u8>>;

        fn list_dir(&self, path: &str) -> Result<Vec<FileEntry>, String> {
            self.directories
                .get(path)
                .cloned()
                .ok_or_else(|| format!("missing remote directory '{path}'"))
        }

        fn open_file(&self, path: &str) -> Result<Self::File, String> {
            self.files
                .get(path)
                .cloned()
                .map(Cursor::new)
                .ok_or_else(|| format!("missing remote file '{path}'"))
        }
    }

    #[test]
    fn downloads_complete_remote_folder_tree() {
        let entry = |name: &str, is_dir: bool, size: u64| FileEntry {
            name: name.into(),
            is_dir,
            is_symlink: false,
            size,
            modified: None,
            permissions: None,
        };
        let remote = FakeRemote {
            directories: HashMap::from([
                (
                    "/srv/project".into(),
                    vec![
                        entry("nested", true, 0),
                        entry("empty", true, 0),
                        entry("root.txt", false, 4),
                    ],
                ),
                (
                    "/srv/project/nested".into(),
                    vec![entry("data.bin", false, 5)],
                ),
                ("/srv/project/empty".into(), Vec::new()),
            ]),
            files: HashMap::from([
                ("/srv/project/root.txt".into(), b"root".to_vec()),
                ("/srv/project/nested/data.bin".into(), b"bytes".to_vec()),
            ]),
        };
        let destination = tempfile::tempdir().unwrap();
        let local_root = destination.path().join("project");
        std::fs::create_dir_all(local_root.join("nested")).unwrap();
        std::fs::write(local_root.join("nested/data.bin"), b"old").unwrap();
        std::fs::write(local_root.join("keep-local.txt"), b"keep").unwrap();

        let total = remote_dir_total_bytes(&remote, "/srv/project").unwrap();
        let progress = Arc::new(Mutex::new(TransferProgress {
            file_name: "project".into(),
            label: "Downloading folder".into(),
            bytes_transferred: 0,
            total_bytes: total,
            status: TransferStatus::InProgress,
        }));
        let cancel = Arc::new(AtomicBool::new(false));

        download_dir_inner(&remote, "/srv/project", &local_root, &progress, &cancel).unwrap();

        assert_eq!(std::fs::read(local_root.join("root.txt")).unwrap(), b"root");
        assert_eq!(
            std::fs::read(local_root.join("nested/data.bin")).unwrap(),
            b"bytes"
        );
        assert!(local_root.join("empty").is_dir());
        assert_eq!(
            std::fs::read(local_root.join("keep-local.txt")).unwrap(),
            b"keep"
        );
        let progress = progress
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(progress.bytes_transferred, 9);
        assert_eq!(progress.total_bytes, 9);
    }
}
