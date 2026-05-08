use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::sftp::SftpConnection;

const CHUNK_SIZE: usize = 32 * 1024;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_progress_initial_state() {
        let progress = TransferProgress {
            file_name: "test.txt".into(),
            label: "Downloading".into(),
            bytes_transferred: 0,
            total_bytes: 1024,
            status: TransferStatus::InProgress,
        };
        assert_eq!(progress.status, TransferStatus::InProgress);
        assert_eq!(progress.bytes_transferred, 0);
        assert_eq!(progress.total_bytes, 1024);
    }

    #[test]
    fn transfer_status_variants() {
        assert_eq!(TransferStatus::InProgress, TransferStatus::InProgress);
        assert_eq!(TransferStatus::Completed, TransferStatus::Completed);
        assert_eq!(TransferStatus::Cancelled, TransferStatus::Cancelled);

        let failed = TransferStatus::Failed("disk full".into());
        assert!(matches!(failed, TransferStatus::Failed(ref msg) if msg == "disk full"));
    }

    #[test]
    fn cancel_flag_cooperative() {
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(!cancel.load(Ordering::Relaxed));
        cancel.store(true, Ordering::Relaxed);
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn transfer_request_upload() {
        let req = TransferRequest::Upload {
            local_path: PathBuf::from("/tmp/file.txt"),
            remote_path: "/home/user/file.txt".into(),
        };
        assert!(matches!(req, TransferRequest::Upload { .. }));
    }

    #[test]
    fn transfer_request_download() {
        let req = TransferRequest::Download {
            remote_path: "/home/user/file.txt".into(),
            local_path: PathBuf::from("/tmp/file.txt"),
        };
        assert!(matches!(req, TransferRequest::Download { .. }));
    }

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
}
