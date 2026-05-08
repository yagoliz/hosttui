use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use tui_input::Input;

use crate::model::Host;
use crate::sftp::{FileEntry, SftpConnection, SftpConnectionStatus};

/// Which pane of the dual-pane file browser has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileBrowserPane {
    Local,
    Remote,
}

/// Transfer direction between the local and remote panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Upload,
    Download,
}

/// Modal state within the file browser view.
///
/// The file browser is normally in `Normal` mode for navigation. Other modes
/// overlay a confirmation dialog, password prompt, or error message and
/// intercept keyboard input until dismissed.
#[derive(Debug, Default)]
pub enum FileBrowserMode {
    #[default]
    Normal,
    ConfirmTransfer {
        file: String,
        direction: TransferDirection,
    },
    PasswordPrompt(Input),
    TransferError(String),
    Creating(Input),
}

/// Which file attribute to sort directory listings by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortField {
    #[default]
    Name,
    Size,
    Modified,
}

impl SortField {
    /// Advances to the next sort field in the cycle.
    pub fn next(self) -> Self {
        match self {
            SortField::Name => SortField::Size,
            SortField::Size => SortField::Modified,
            SortField::Modified => SortField::Name,
        }
    }

    /// Returns a short label for the status/instructions line.
    pub fn label(self) -> &'static str {
        match self {
            SortField::Name => "name",
            SortField::Size => "size",
            SortField::Modified => "modified",
        }
    }
}

/// Sorts file entries by the given field, always keeping directories first.
///
/// Within the directory and file groups, entries are sorted by the chosen field
/// with `ascending` controlling the direction. Name sort is always
/// case-insensitive.
pub fn sort_entries(entries: &mut [FileEntry], field: SortField, ascending: bool) {
    entries.sort_by(|a, b| {
        let dir_order = a.is_dir.cmp(&b.is_dir).reverse();
        if dir_order != std::cmp::Ordering::Equal {
            return dir_order;
        }

        let ord = match field {
            SortField::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            SortField::Size => a.size.cmp(&b.size),
            SortField::Modified => a.modified.cmp(&b.modified),
        };

        if ascending { ord } else { ord.reverse() }
    });
}

/// Reads the local filesystem and returns a list of `FileEntry` items.
///
/// Errors on individual entries (e.g. broken symlinks) are silently skipped
/// so one bad entry doesn't prevent browsing the rest of the directory.
pub fn list_local_dir(path: &Path) -> std::io::Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path)? {
        let Ok(entry) = entry else { continue };
        let Ok(meta) = entry.metadata() else { continue };

        let name = entry.file_name().to_string_lossy().into_owned();
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());

        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            Some(meta.permissions().mode())
        };
        #[cfg(not(unix))]
        let permissions = None;

        entries.push(FileEntry {
            name,
            is_dir: meta.is_dir(),
            size: meta.len(),
            modified,
            permissions,
        });
    }
    Ok(entries)
}

/// Dual-pane file browser state for a single host connection.
///
/// Each `FileBrowser` instance corresponds to one tab in the tab bar,
/// identified by host alias. It holds the local and remote directory state,
/// the SFTP connection (shared with background transfer threads), and the
/// current interaction mode.
#[derive(Debug)]
pub struct FileBrowser {
    pub alias: String,
    pub host: Host,
    pub focus: FileBrowserPane,

    pub local_path: PathBuf,
    pub local_entries: Vec<FileEntry>,
    pub local_selected: usize,

    pub remote_path: String,
    pub remote_entries: Vec<FileEntry>,
    pub remote_selected: usize,

    pub sftp: Arc<Mutex<Option<SftpConnection>>>,
    pub connection_status: Arc<Mutex<SftpConnectionStatus>>,

    pub error: Option<String>,
    pub sort_by: SortField,
    pub sort_ascending: bool,
    pub show_hidden: bool,
    pub mode: FileBrowserMode,
}

impl FileBrowser {
    /// Creates a new file browser for a host, starting in the user's home directory locally.
    ///
    /// The SFTP connection is initially `None` — callers spawn a background
    /// thread to populate it, updating `connection_status` as the handshake
    /// progresses.
    pub fn new(host: Host) -> Self {
        let local_path = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let local_entries = list_local_dir(&local_path).unwrap_or_default();

        let mut browser = FileBrowser {
            alias: host.alias.clone(),
            host,
            focus: FileBrowserPane::Local,
            local_path,
            local_entries: Vec::new(),
            local_selected: 0,
            remote_path: String::new(),
            remote_entries: Vec::new(),
            remote_selected: 0,
            sftp: Arc::new(Mutex::new(None)),
            connection_status: Arc::new(Mutex::new(SftpConnectionStatus::Connecting)),
            error: None,
            sort_by: SortField::default(),
            sort_ascending: true,
            show_hidden: false,
            mode: FileBrowserMode::Normal,
        };

        browser.local_entries = local_entries;
        browser.apply_filters_and_sort_local();
        browser
    }

    /// Moves the cursor down in the focused pane, wrapping at the end.
    pub fn move_down(&mut self) {
        match self.focus {
            FileBrowserPane::Local => {
                if !self.visible_local_entries().is_empty() {
                    let count = self.visible_local_entries().len();
                    self.local_selected = (self.local_selected + 1) % count;
                }
            }
            FileBrowserPane::Remote => {
                if !self.visible_remote_entries().is_empty() {
                    let count = self.visible_remote_entries().len();
                    self.remote_selected = (self.remote_selected + 1) % count;
                }
            }
        }
    }

    /// Moves the cursor up in the focused pane, wrapping at the start.
    pub fn move_up(&mut self) {
        match self.focus {
            FileBrowserPane::Local => {
                let count = self.visible_local_entries().len();
                if count > 0 {
                    self.local_selected = self.local_selected.checked_sub(1).unwrap_or(count - 1);
                }
            }
            FileBrowserPane::Remote => {
                let count = self.visible_remote_entries().len();
                if count > 0 {
                    self.remote_selected = self.remote_selected.checked_sub(1).unwrap_or(count - 1);
                }
            }
        }
    }

    /// Enters the selected directory in the focused pane.
    ///
    /// Returns `true` if the entry was a directory and navigation succeeded,
    /// `false` if it was a file or if the operation failed.
    pub fn enter_dir(&mut self) -> bool {
        match self.focus {
            FileBrowserPane::Local => self.enter_local_dir(),
            FileBrowserPane::Remote => self.enter_remote_dir(),
        }
    }

    /// Navigates into the selected local directory.
    fn enter_local_dir(&mut self) -> bool {
        let entries = self.visible_local_entries();
        let Some(entry) = entries.get(self.local_selected) else {
            return false;
        };
        if !entry.is_dir {
            return false;
        }

        let new_path = self.local_path.join(&entry.name);
        match list_local_dir(&new_path) {
            Ok(new_entries) => {
                self.local_path = new_path;
                self.local_entries = new_entries;
                self.apply_filters_and_sort_local();
                self.local_selected = 0;
                true
            }
            Err(e) => {
                self.error = Some(format!("Cannot open directory: {e}"));
                false
            }
        }
    }

    /// Navigates into the selected remote directory.
    fn enter_remote_dir(&mut self) -> bool {
        let entries = self.visible_remote_entries();
        let Some(entry) = entries.get(self.remote_selected) else {
            return false;
        };
        if !entry.is_dir {
            return false;
        }

        let new_path = format!("{}/{}", self.remote_path.trim_end_matches('/'), entry.name);

        let result = {
            let sftp_guard = self.sftp.lock().unwrap();
            let Some(ref conn) = *sftp_guard else {
                return false;
            };
            conn.list_dir(&new_path)
        };

        match result {
            Ok(new_entries) => {
                self.remote_path = new_path;
                self.remote_entries = new_entries;
                self.apply_filters_and_sort_remote();
                self.remote_selected = 0;
                true
            }
            Err(e) => {
                self.error = Some(format!("Cannot open directory: {e}"));
                false
            }
        }
    }

    /// Navigates to the parent directory in the focused pane.
    pub fn go_parent(&mut self) {
        match self.focus {
            FileBrowserPane::Local => self.go_parent_local(),
            FileBrowserPane::Remote => self.go_parent_remote(),
        }
    }

    /// Navigates to the parent of the current local directory.
    fn go_parent_local(&mut self) {
        let Some(parent) = self.local_path.parent().map(|p| p.to_path_buf()) else {
            return;
        };
        if parent == self.local_path {
            return;
        }

        match list_local_dir(&parent) {
            Ok(new_entries) => {
                let old_name = self
                    .local_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned());
                self.local_path = parent;
                self.local_entries = new_entries;
                self.apply_filters_and_sort_local();
                self.local_selected = old_name
                    .and_then(|name| {
                        self.visible_local_entries()
                            .iter()
                            .position(|e| e.name == name)
                    })
                    .unwrap_or(0);
            }
            Err(e) => {
                self.error = Some(format!("Cannot open parent: {e}"));
            }
        }
    }

    /// Navigates to the parent of the current remote directory.
    fn go_parent_remote(&mut self) {
        if self.remote_path == "/" || self.remote_path.is_empty() {
            return;
        }

        let parent = match self.remote_path.rfind('/') {
            Some(0) => "/".to_string(),
            Some(pos) => self.remote_path[..pos].to_string(),
            None => return,
        };

        let old_name = self.remote_path.rsplit('/').next().map(String::from);

        let result = {
            let sftp_guard = self.sftp.lock().unwrap();
            let Some(ref conn) = *sftp_guard else {
                return;
            };
            conn.list_dir(&parent)
        };

        match result {
            Ok(new_entries) => {
                self.remote_path = parent;
                self.remote_entries = new_entries;
                self.apply_filters_and_sort_remote();
                self.remote_selected = old_name
                    .and_then(|name| {
                        self.visible_remote_entries()
                            .iter()
                            .position(|e| e.name == name)
                    })
                    .unwrap_or(0);
            }
            Err(e) => {
                self.error = Some(format!("Cannot open parent: {e}"));
            }
        }
    }

    /// Switches keyboard focus between the local and remote pane.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FileBrowserPane::Local => FileBrowserPane::Remote,
            FileBrowserPane::Remote => FileBrowserPane::Local,
        };
    }

    /// Toggles visibility of hidden files (names starting with `.`).
    pub fn toggle_hidden(&mut self) {
        self.show_hidden = !self.show_hidden;
        self.clamp_selections();
    }

    /// Cycles the sort field and re-sorts both panes.
    pub fn cycle_sort(&mut self) {
        self.sort_by = self.sort_by.next();
        self.apply_filters_and_sort_local();
        self.apply_filters_and_sort_remote();
        self.clamp_selections();
    }

    /// Toggles the sort direction and re-sorts both panes.
    pub fn toggle_sort_direction(&mut self) {
        self.sort_ascending = !self.sort_ascending;
        self.apply_filters_and_sort_local();
        self.apply_filters_and_sort_remote();
        self.clamp_selections();
    }

    /// Refreshes the local directory listing from disk.
    pub fn refresh_local(&mut self) {
        if let Ok(entries) = list_local_dir(&self.local_path) {
            self.local_entries = entries;
            self.apply_filters_and_sort_local();
            self.clamp_selections();
        }
    }

    /// Refreshes the remote directory listing via SFTP.
    pub fn refresh_remote(&mut self) {
        let result = {
            let sftp_guard = self.sftp.lock().unwrap();
            let Some(ref conn) = *sftp_guard else {
                return;
            };
            conn.list_dir(&self.remote_path)
        };

        match result {
            Ok(entries) => {
                self.remote_entries = entries;
                self.apply_filters_and_sort_remote();
                self.clamp_selections();
            }
            Err(e) => {
                self.error = Some(format!("Refresh failed: {e}"));
            }
        }
    }

    /// Returns the currently selected file entry in the focused pane, if any.
    pub fn selected_entry(&self) -> Option<&FileEntry> {
        match self.focus {
            FileBrowserPane::Local => self
                .visible_local_entries()
                .get(self.local_selected)
                .copied(),
            FileBrowserPane::Remote => self
                .visible_remote_entries()
                .get(self.remote_selected)
                .copied(),
        }
    }

    /// Returns local entries filtered by hidden-file visibility.
    pub fn visible_local_entries(&self) -> Vec<&FileEntry> {
        self.local_entries
            .iter()
            .filter(|e| self.show_hidden || !e.name.starts_with('.'))
            .collect()
    }

    /// Returns remote entries filtered by hidden-file visibility.
    pub fn visible_remote_entries(&self) -> Vec<&FileEntry> {
        self.remote_entries
            .iter()
            .filter(|e| self.show_hidden || !e.name.starts_with('.'))
            .collect()
    }

    /// Sorts local entries in place by the current sort field and direction.
    fn apply_filters_and_sort_local(&mut self) {
        sort_entries(&mut self.local_entries, self.sort_by, self.sort_ascending);
    }

    /// Sorts remote entries in place by the current sort field and direction.
    fn apply_filters_and_sort_remote(&mut self) {
        sort_entries(&mut self.remote_entries, self.sort_by, self.sort_ascending);
    }

    /// Ensures selection indexes don't point past the visible entry list.
    fn clamp_selections(&mut self) {
        let local_count = self.visible_local_entries().len();
        if self.local_selected >= local_count {
            self.local_selected = local_count.saturating_sub(1);
        }
        let remote_count = self.visible_remote_entries().len();
        if self.remote_selected >= remote_count {
            self.remote_selected = remote_count.saturating_sub(1);
        }
    }
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

    fn entry_with_modified(name: &str, is_dir: bool, size: u64, modified: u64) -> FileEntry {
        FileEntry {
            name: name.into(),
            is_dir,
            size,
            modified: Some(modified),
            permissions: None,
        }
    }

    fn test_host() -> Host {
        Host {
            alias: "test".into(),
            hostname: "127.0.0.1".into(),
            user: "user".into(),
            port: 22,
            identity_file: None,
            group: None,
            extra: vec![],
            details: String::new(),
            last_accessed: String::new(),
        }
    }

    /// Creates a FileBrowser with pre-populated local entries for testing,
    /// bypassing the real filesystem.
    fn browser_with_entries(entries: Vec<FileEntry>) -> FileBrowser {
        let mut browser = FileBrowser {
            alias: "test".into(),
            host: test_host(),
            focus: FileBrowserPane::Local,
            local_path: PathBuf::from("/tmp/test"),
            local_entries: entries,
            local_selected: 0,
            remote_path: String::new(),
            remote_entries: Vec::new(),
            remote_selected: 0,
            sftp: Arc::new(Mutex::new(None)),
            connection_status: Arc::new(Mutex::new(SftpConnectionStatus::Connecting)),
            error: None,
            sort_by: SortField::Name,
            sort_ascending: true,
            show_hidden: false,
            mode: FileBrowserMode::Normal,
        };
        browser.apply_filters_and_sort_local();
        browser
    }

    // --- Navigation tests ---

    #[test]
    fn move_down_wraps() {
        let mut b =
            browser_with_entries(vec![entry("a.txt", false, 10), entry("b.txt", false, 20)]);
        assert_eq!(b.local_selected, 0);

        b.move_down();
        assert_eq!(b.local_selected, 1);

        b.move_down();
        assert_eq!(b.local_selected, 0);
    }

    #[test]
    fn move_up_wraps() {
        let mut b =
            browser_with_entries(vec![entry("a.txt", false, 10), entry("b.txt", false, 20)]);
        assert_eq!(b.local_selected, 0);

        b.move_up();
        assert_eq!(b.local_selected, 1);

        b.move_up();
        assert_eq!(b.local_selected, 0);
    }

    #[test]
    fn move_in_empty_dir() {
        let mut b = browser_with_entries(vec![]);
        b.move_down();
        assert_eq!(b.local_selected, 0);
        b.move_up();
        assert_eq!(b.local_selected, 0);
    }

    #[test]
    fn move_single_entry() {
        let mut b = browser_with_entries(vec![entry("only.txt", false, 1)]);
        b.move_down();
        assert_eq!(b.local_selected, 0);
        b.move_up();
        assert_eq!(b.local_selected, 0);
    }

    #[test]
    fn remote_navigation() {
        let mut b = browser_with_entries(vec![]);
        b.focus = FileBrowserPane::Remote;
        b.remote_entries = vec![
            entry("dir1", true, 0),
            entry("file1.txt", false, 100),
            entry("file2.txt", false, 200),
        ];
        sort_entries(&mut b.remote_entries, b.sort_by, b.sort_ascending);

        b.move_down();
        assert_eq!(b.remote_selected, 1);
        b.move_down();
        assert_eq!(b.remote_selected, 2);
        b.move_down();
        assert_eq!(b.remote_selected, 0);

        b.move_up();
        assert_eq!(b.remote_selected, 2);
    }

    // --- Toggle focus ---

    #[test]
    fn toggle_focus_switches_panes() {
        let mut b = browser_with_entries(vec![]);
        assert_eq!(b.focus, FileBrowserPane::Local);

        b.toggle_focus();
        assert_eq!(b.focus, FileBrowserPane::Remote);

        b.toggle_focus();
        assert_eq!(b.focus, FileBrowserPane::Local);
    }

    // --- Sort tests ---

    #[test]
    fn sort_by_name_ascending() {
        let mut entries = vec![
            entry("cherry", false, 300),
            entry("apple", false, 100),
            entry("zdir", true, 0),
            entry("adir", true, 0),
            entry("banana", false, 200),
        ];
        sort_entries(&mut entries, SortField::Name, true);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["adir", "zdir", "apple", "banana", "cherry"]);
    }

    #[test]
    fn sort_by_name_descending() {
        let mut entries = vec![
            entry("apple", false, 100),
            entry("cherry", false, 300),
            entry("banana", false, 200),
        ];
        sort_entries(&mut entries, SortField::Name, false);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["cherry", "banana", "apple"]);
    }

    #[test]
    fn sort_by_size() {
        let mut entries = vec![
            entry("big", false, 1000),
            entry("small", false, 10),
            entry("medium", false, 500),
            entry("dir", true, 4096),
        ];
        sort_entries(&mut entries, SortField::Size, true);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["dir", "small", "medium", "big"]);
    }

    #[test]
    fn sort_by_modified() {
        let mut entries = vec![
            entry_with_modified("new", false, 10, 3000),
            entry_with_modified("old", false, 10, 1000),
            entry_with_modified("mid", false, 10, 2000),
        ];
        sort_entries(&mut entries, SortField::Modified, true);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["old", "mid", "new"]);
    }

    #[test]
    fn sort_dirs_always_first_regardless_of_field() {
        let mut entries = vec![entry("big_file", false, 99999), entry("small_dir", true, 1)];
        sort_entries(&mut entries, SortField::Size, true);

        assert!(entries[0].is_dir);
        assert_eq!(entries[0].name, "small_dir");
    }

    #[test]
    fn cycle_sort_field() {
        assert_eq!(SortField::Name.next(), SortField::Size);
        assert_eq!(SortField::Size.next(), SortField::Modified);
        assert_eq!(SortField::Modified.next(), SortField::Name);
    }

    #[test]
    fn cycle_sort_on_browser() {
        let mut b =
            browser_with_entries(vec![entry("b.txt", false, 200), entry("a.txt", false, 100)]);
        assert_eq!(b.sort_by, SortField::Name);

        b.cycle_sort();
        assert_eq!(b.sort_by, SortField::Size);

        b.cycle_sort();
        assert_eq!(b.sort_by, SortField::Modified);

        b.cycle_sort();
        assert_eq!(b.sort_by, SortField::Name);
    }

    // --- Hidden file filtering ---

    #[test]
    fn hidden_files_filtered_by_default() {
        let b = browser_with_entries(vec![
            entry(".hidden", false, 10),
            entry("visible", false, 20),
            entry(".secret", true, 0),
            entry("docs", true, 0),
        ]);

        let visible = b.visible_local_entries();
        let names: Vec<&str> = visible.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["docs", "visible"]);
    }

    #[test]
    fn toggle_hidden_shows_all() {
        let mut b = browser_with_entries(vec![
            entry(".hidden", false, 10),
            entry("visible", false, 20),
        ]);

        assert_eq!(b.visible_local_entries().len(), 1);

        b.toggle_hidden();
        assert!(b.show_hidden);
        assert_eq!(b.visible_local_entries().len(), 2);

        b.toggle_hidden();
        assert!(!b.show_hidden);
        assert_eq!(b.visible_local_entries().len(), 1);
    }

    #[test]
    fn toggle_hidden_clamps_selection() {
        let mut b = browser_with_entries(vec![
            entry(".a", false, 10),
            entry(".b", false, 10),
            entry("c", false, 10),
        ]);

        b.show_hidden = true;
        b.local_selected = 2;

        b.toggle_hidden();
        assert_eq!(b.local_selected, 0);
    }

    // --- Empty directory and edge cases ---

    #[test]
    fn empty_directory_selected_entry_is_none() {
        let b = browser_with_entries(vec![]);
        assert!(b.selected_entry().is_none());
    }

    #[test]
    fn selected_entry_returns_correct_entry() {
        let b = browser_with_entries(vec![entry("adir", true, 0), entry("file.txt", false, 100)]);

        let selected = b.selected_entry().unwrap();
        assert_eq!(selected.name, "adir");
        assert!(selected.is_dir);
    }

    #[test]
    fn enter_dir_on_file_returns_false() {
        let mut b = browser_with_entries(vec![entry("file.txt", false, 100)]);
        assert!(!b.enter_dir());
    }

    // --- Local directory listing ---

    #[test]
    fn list_local_dir_reads_temp_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("hello.txt"), "world").unwrap();
        fs::create_dir(dir.path().join("subdir")).unwrap();

        let entries = list_local_dir(dir.path()).unwrap();
        assert_eq!(entries.len(), 2);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"hello.txt"));
        assert!(names.contains(&"subdir"));

        let subdir = entries.iter().find(|e| e.name == "subdir").unwrap();
        assert!(subdir.is_dir);

        let file = entries.iter().find(|e| e.name == "hello.txt").unwrap();
        assert!(!file.is_dir);
        assert_eq!(file.size, 5);
    }

    #[test]
    fn list_local_dir_empty() {
        let dir = tempfile::tempdir().unwrap();
        let entries = list_local_dir(dir.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn list_local_dir_nonexistent() {
        let result = list_local_dir(Path::new("/nonexistent/path/that/doesnt/exist"));
        assert!(result.is_err());
    }

    // --- Parent directory ---

    #[test]
    fn go_parent_at_root_stays() {
        let mut b = browser_with_entries(vec![]);
        b.local_path = PathBuf::from("/");
        b.go_parent();
        assert_eq!(b.local_path, PathBuf::from("/"));
    }

    #[test]
    fn go_parent_local_navigates_up() {
        let dir = tempfile::tempdir().unwrap();
        let child = dir.path().join("child");
        fs::create_dir(&child).unwrap();

        let mut b = browser_with_entries(vec![]);
        b.local_path = child;
        b.go_parent();

        assert_eq!(b.local_path, dir.path());
    }

    #[test]
    fn go_parent_remote_at_root() {
        let mut b = browser_with_entries(vec![]);
        b.focus = FileBrowserPane::Remote;
        b.remote_path = "/".into();
        b.go_parent();
        assert_eq!(b.remote_path, "/");
    }

    #[test]
    fn go_parent_remote_path_calculation() {
        let mut b = browser_with_entries(vec![]);
        b.focus = FileBrowserPane::Remote;
        b.remote_path = "/home/user/docs".into();
        // Can't actually navigate (no SFTP), but we test it doesn't panic
        b.go_parent();
        // Without SFTP connection, it won't change path (early return)
    }

    // --- Sort direction ---

    #[test]
    fn toggle_sort_direction() {
        let mut b =
            browser_with_entries(vec![entry("a.txt", false, 100), entry("b.txt", false, 200)]);
        assert!(b.sort_ascending);

        b.toggle_sort_direction();
        assert!(!b.sort_ascending);

        b.toggle_sort_direction();
        assert!(b.sort_ascending);
    }

    // --- SortField labels ---

    #[test]
    fn sort_field_labels() {
        assert_eq!(SortField::Name.label(), "name");
        assert_eq!(SortField::Size.label(), "size");
        assert_eq!(SortField::Modified.label(), "modified");
    }

    // --- FileBrowserMode default ---

    #[test]
    fn mode_default_is_normal() {
        let mode = FileBrowserMode::default();
        assert!(matches!(mode, FileBrowserMode::Normal));
    }

    // --- Case-insensitive name sort ---

    #[test]
    fn sort_by_name_case_insensitive() {
        let mut entries = vec![
            entry("Banana", false, 10),
            entry("apple", false, 20),
            entry("Cherry", false, 30),
        ];
        sort_entries(&mut entries, SortField::Name, true);

        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["apple", "Banana", "Cherry"]);
    }
}
