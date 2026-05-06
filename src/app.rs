use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use tui_input::Input;

use std::env;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::model::{Config, Host};
use crate::pty::{Session, SessionStatus};

/// Zero-based coordinate inside a rendered terminal screen.
///
/// These coordinates are relative to the PTY viewport, not the outer ratatui
/// frame. Mouse positions are converted into `ScreenPos` before selection text
/// is read from `vt100::Screen`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenPos {
    pub row: u16,
    pub col: u16,
}

/// A mouse text selection in terminal-screen coordinates.
///
/// `anchor` is where the drag started and `end` is where it most recently ended.
/// They are intentionally not normalized while dragging so the UI can support
/// selecting in either direction.
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub anchor: ScreenPos,
    pub end: ScreenPos,
}

impl Selection {
    /// Returns the selection in top-left to bottom-right order.
    ///
    /// The returned end column is exclusive, matching `vt100::Screen` selection
    /// APIs and the rendering helper. The stored `end` point is inclusive while
    /// dragging, so this method adds one column after normalization.
    pub fn normalized(&self) -> (ScreenPos, ScreenPos) {
        let (start, end) = if self.anchor.row < self.end.row
            || (self.anchor.row == self.end.row && self.anchor.col <= self.end.col)
        {
            (self.anchor, self.end)
        } else {
            (self.end, self.anchor)
        };
        (
            start,
            ScreenPos {
                row: end.row,
                col: end.col + 1,
            },
        )
    }
}

/// Which browser pane currently owns keyboard navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Groups,
    Hosts,
}

/// Top-level view currently displayed by the app.
///
/// The host browser and SSH sessions share one event loop. `Session(usize)` is
/// an index into `App::sessions`, so code that removes sessions must also adjust
/// this view index to avoid pointing past the end of the vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Hosts,
    Session(usize),
}

/// State for the Ctrl+T session-command prefix.
///
/// Ctrl+T is intercepted by hosttui to switch tabs and close sessions. Pressing
/// Ctrl+T twice sends a literal Ctrl+T to the remote session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrefixState {
    #[default]
    Inactive,
    Pending,
}

/// Fields in the add/edit host form.
///
/// The order here matches `FormState::fields`, which lets the form navigate by
/// index while still rendering stable labels for each row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Alias,
    Hostname,
    User,
    Port,
    IdentityFile,
    Group,
    Details,
}

impl Field {
    /// Returns the human-readable label used by the form renderer.
    pub fn label(self) -> &'static str {
        match self {
            Field::Alias => "Alias",
            Field::Hostname => "Hostname",
            Field::User => "User",
            Field::Port => "Port",
            Field::IdentityFile => "Identity File",
            Field::Group => "Group",
            Field::Details => "Details",
        }
    }
}

/// Active input inside the SSH extra-option key/value editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtraField {
    Key,
    Value,
}

/// Nested form for adding or editing one SSH extra option.
///
/// Extra options are stored as key/value pairs and later exported as OpenSSH
/// directives or passed to `ssh -o`. `editing_index` distinguishes a new pair
/// from an edit of an existing pair so duplicate-key validation can ignore the
/// pair currently being edited.
#[derive(Debug, Clone)]
pub struct ExtraEntryForm {
    pub key: Input,
    pub value: Input,
    pub active: ExtraField,
    /// `None` when adding a new pair; `Some(i)` when editing the i-th existing pair.
    pub editing_index: Option<usize>,
}

impl ExtraEntryForm {
    /// Creates an empty key/value editor for a new SSH option.
    fn blank() -> Self {
        ExtraEntryForm {
            key: Input::default(),
            value: Input::default(),
            active: ExtraField::Key,
            editing_index: None,
        }
    }

    /// Creates a key/value editor prefilled from an existing SSH option.
    fn from_pair(index: usize, key: &str, value: &str) -> Self {
        ExtraEntryForm {
            key: Input::new(key.into()),
            value: Input::new(value.into()),
            active: ExtraField::Key,
            editing_index: Some(index),
        }
    }

    /// Returns the currently focused input widget inside the nested editor.
    pub fn active_input(&mut self) -> &mut Input {
        match self.active {
            ExtraField::Key => &mut self.key,
            ExtraField::Value => &mut self.value,
        }
    }

    /// Moves focus between the key and value fields in the nested editor.
    pub fn toggle_field(&mut self) {
        self.active = match self.active {
            ExtraField::Key => ExtraField::Value,
            ExtraField::Value => ExtraField::Key,
        };
    }
}

/// State for the SSH extra-options sub-dialog.
///
/// The dialog can be in list mode (`entry == None`) or editing mode
/// (`entry == Some`). Keeping both states in one struct lets the form retain the
/// selected extra option while temporarily opening the inner entry form.
#[derive(Debug, Clone, Default)]
pub struct ExtrasEditor {
    pub selected: usize,
    pub entry: Option<ExtraEntryForm>,
    pub error: Option<String>,
}

/// Mutable state for the add/edit host form.
///
/// The form stores raw `tui_input::Input` values until submission, when it is
/// validated and converted into a `Host`. `last_accessed` is carried through
/// edits but not shown as an editable field so editing a host does not reset its
/// connection history.
#[derive(Debug, Clone)]
pub struct FormState {
    pub fields: [(Field, Input); 7],
    pub active: usize,
    pub error: Option<String>,
    pub extras: Vec<(String, String)>,
    pub extras_editor: Option<ExtrasEditor>,
    last_accessed: String,
}

impl FormState {
    /// Creates a new blank host form with sensible defaults.
    ///
    /// The default port is `22`, matching OpenSSH. The user field is left blank
    /// here because `to_host` resolves it from the environment only at submit
    /// time, after the user has had a chance to type a value.
    fn blank() -> Self {
        FormState {
            fields: [
                (Field::Alias, Input::default()),
                (Field::Hostname, Input::default()),
                (Field::User, Input::default()),
                (Field::Port, Input::new("22".into())),
                (Field::IdentityFile, Input::default()),
                (Field::Group, Input::default()),
                (Field::Details, Input::default()),
            ],
            active: 0,
            error: None,
            extras: vec![],
            extras_editor: None,
            last_accessed: String::new(),
        }
    }

    /// Creates a new host form prefilled with a selected group.
    ///
    /// This is used when the user is browsing a concrete group and starts adding
    /// a host, reducing repetitive typing without changing validation rules.
    fn with_group(group: &str) -> Self {
        let mut form = Self::blank();
        form.fields[5].1 = Input::new(group.into());
        form
    }

    /// Creates an edit form from an existing host record.
    ///
    /// All persisted fields are copied into editable inputs except
    /// `last_accessed`, which is preserved privately so a save round-trip does
    /// not lose connection-history metadata.
    fn from_host(host: &Host) -> Self {
        FormState {
            fields: [
                (Field::Alias, Input::new(host.alias.clone())),
                (Field::Hostname, Input::new(host.hostname.clone())),
                (Field::User, Input::new(host.user.clone())),
                (Field::Port, Input::new(host.port.to_string())),
                (
                    Field::IdentityFile,
                    Input::new(host.identity_file.clone().unwrap_or_default()),
                ),
                (
                    Field::Group,
                    Input::new(host.group.clone().unwrap_or_default()),
                ),
                (Field::Details, Input::new(host.details.clone())),
            ],
            active: 0,
            error: None,
            extras: host.extra.clone(),
            extras_editor: None,
            last_accessed: host.last_accessed.clone(),
        }
    }

    /// Opens the extra SSH options sub-dialog if it is not already open.
    pub fn open_extras(&mut self) {
        if self.extras_editor.is_none() {
            self.extras_editor = Some(ExtrasEditor::default());
        }
    }

    /// Closes the extra SSH options sub-dialog and returns to the main form.
    pub fn close_extras(&mut self) {
        self.extras_editor = None;
    }

    /// Returns mutable access to the extra-options editor when it is open.
    pub fn extras_editor_mut(&mut self) -> Option<&mut ExtrasEditor> {
        self.extras_editor.as_mut()
    }

    /// Begin adding a new key/value pair within the open extras sub-dialog.
    pub fn extras_begin_add(&mut self) {
        if let Some(ed) = self.extras_editor.as_mut() {
            ed.entry = Some(ExtraEntryForm::blank());
            ed.error = None;
        }
    }

    /// Begin editing the currently-selected pair within the open sub-dialog.
    pub fn extras_begin_edit(&mut self) {
        let Some(ed) = self.extras_editor.as_mut() else {
            return;
        };
        if let Some((k, v)) = self.extras.get(ed.selected) {
            ed.entry = Some(ExtraEntryForm::from_pair(ed.selected, k, v));
            ed.error = None;
        }
    }

    /// Deletes the selected extra SSH option from the form.
    ///
    /// After removal, the selected index is clamped to the final remaining item
    /// so subsequent navigation/rendering never points past the vector.
    pub fn extras_delete_selected(&mut self) {
        let Some(ed) = self.extras_editor.as_mut() else {
            return;
        };
        if ed.selected < self.extras.len() {
            self.extras.remove(ed.selected);
            if ed.selected >= self.extras.len() && !self.extras.is_empty() {
                ed.selected = self.extras.len() - 1;
            }
        }
    }

    /// Moves the selected extra option down, wrapping at the end.
    pub fn extras_move_down(&mut self) {
        if let Some(ed) = self.extras_editor.as_mut()
            && !self.extras.is_empty()
        {
            ed.selected = (ed.selected + 1) % self.extras.len();
        }
    }

    /// Moves the selected extra option up, wrapping at the beginning.
    pub fn extras_move_up(&mut self) {
        if let Some(ed) = self.extras_editor.as_mut()
            && !self.extras.is_empty()
        {
            ed.selected = ed.selected.checked_sub(1).unwrap_or(self.extras.len() - 1);
        }
    }

    /// Commit the open inner entry form. Returns `true` on success; `false` if
    /// validation failed (the editor's `error` is set in that case).
    pub fn extras_commit_entry(&mut self) -> bool {
        let Some(ed) = self.extras_editor.as_mut() else {
            return true;
        };
        let Some(entry) = ed.entry.as_ref() else {
            return true;
        };

        let key = entry.key.value().trim().to_string();
        let value = entry.value.value().trim().to_string();
        let editing_index = entry.editing_index;

        if key.is_empty() {
            ed.error = Some("Key cannot be empty".into());
            return false;
        }

        let duplicate = self
            .extras
            .iter()
            .enumerate()
            .any(|(i, (k, _))| k == &key && Some(i) != editing_index);
        if duplicate {
            ed.error = Some(format!("Key '{key}' already exists"));
            return false;
        }

        match editing_index {
            Some(i) => self.extras[i] = (key, value),
            None => {
                self.extras.push((key, value));
                ed.selected = self.extras.len() - 1;
            }
        }
        ed.entry = None;
        ed.error = None;
        true
    }

    pub fn extras_cancel_entry(&mut self) {
        if let Some(ed) = self.extras_editor.as_mut() {
            ed.entry = None;
            ed.error = None;
        }
    }

    /// Returns the active input widget in the main host form.
    pub fn active_input(&mut self) -> &mut Input {
        &mut self.fields[self.active].1
    }

    /// Advances focus to the next main host-form field, wrapping at the end.
    pub fn next_field(&mut self) {
        self.active = (self.active + 1) % self.fields.len();
    }

    /// Moves focus to the previous main host-form field, wrapping at the start.
    pub fn prev_field(&mut self) {
        self.active = self.active.checked_sub(1).unwrap_or(self.fields.len() - 1);
    }

    /// Reads the current raw string value for a named form field.
    fn value(&self, field: Field) -> &str {
        self.fields
            .iter()
            .find(|(f, _)| *f == field)
            .unwrap()
            .1
            .value()
    }

    /// Validates the form and converts it into a persisted `Host` record.
    ///
    /// Empty alias and hostname values are rejected because they are required by
    /// both hosttui and OpenSSH. An empty user falls back to `$USER`, matching
    /// normal SSH behavior, and an empty identity/group becomes `None` so the
    /// serialized TOML stays semantically clean.
    fn to_host(&self) -> Result<Host, String> {
        let alias = self.value(Field::Alias).trim().to_string();
        if alias.is_empty() {
            return Err("Alias cannot be empty".into());
        }

        let hostname = self.value(Field::Hostname).trim().to_string();
        if hostname.is_empty() {
            return Err("Hostname cannot be empty".into());
        }

        let user = self.value(Field::User).trim().to_string();
        let user = if user.is_empty() {
            env::var("USER").map_err(|e| e.to_string())?
        } else {
            user
        };

        let port: u16 = self
            .value(Field::Port)
            .trim()
            .parse()
            .map_err(|_| "Port must be a number (0-65535)".to_string())?;

        let identity_file = {
            let v = self.value(Field::IdentityFile).trim().to_string();
            if v.is_empty() { None } else { Some(v) }
        };

        let group = {
            let v = self.value(Field::Group).trim().to_string();
            if v.is_empty() { None } else { Some(v) }
        };

        let details = self.value(Field::Details).trim().to_string();

        Ok(Host {
            alias,
            hostname,
            user,
            port,
            identity_file,
            group,
            details,
            extra: self.extras.clone(),
            last_accessed: self.last_accessed.clone(),
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct InputState {
    pub buffer: Input,
    pub error: Option<String>,
}

/// Result state for the asynchronous host reachability check.
///
/// The check runs on a background thread and updates this value through an
/// `Arc<Mutex<_>>` stored in `Mode::TestResult`, allowing the main loop to keep
/// rendering while the TCP connection attempt is in progress.
#[derive(Debug, Clone)]
pub enum TestStatus {
    Testing,
    Done { success: bool, message: String },
}

/// Current modal interaction state.
///
/// Most UI flows are represented here instead of by separate booleans. That
/// keeps rendering and key handling explicit: the app is either in normal host
/// browsing, editing a form, confirming a destructive action, showing a result,
/// or displaying session tab help.
#[derive(Debug, Clone, Default)]
pub enum Mode {
    #[default]
    Normal,
    Adding(FormState),
    Editing {
        original_alias: String,
        form: FormState,
    },
    ConfirmDelete(String),
    AddingGroup(InputState),
    EditingGroup {
        original_name: String,
        input: InputState,
    },
    ConfirmDeleteGroup(String),
    Searching,
    ConnectError {
        alias: String,
        message: String,
    },
    TestResult {
        alias: String,
        status: Arc<Mutex<TestStatus>>,
    },
    TabHelp,
}

/// Entries shown in the group-navigation pane.
///
/// `All` and `Ungrouped` are virtual filters, while `Named` corresponds to a
/// real persisted `Group`. Keeping the virtual entries in the same enum lets the
/// group pane navigate all filters uniformly.
#[derive(Debug, Clone)]
pub enum GroupEntry {
    All,
    Named(String),
    Ungrouped,
}

/// Central application state for rendering and event handling.
///
/// Rendering is intended to be a pure read of this struct; event handlers mutate
/// it and set `dirty` whenever persisted config changes. Embedded SSH sessions
/// live here as runtime-only state and are never serialized.
#[derive(Debug)]
pub struct App {
    pub config: Config,
    pub selected: usize,
    pub mode: Mode,
    pub exit: bool,
    pub dirty: bool,
    pub focus: Pane,
    pub group_selected: usize,
    pub search: Input,
    pub view: View,
    pub sessions: Vec<Session>,
    pub prefix: PrefixState,
    pub selection: Option<Selection>,
    clipboard_notice_until: Option<Instant>,
    group_entries: Vec<GroupEntry>,
    items: Vec<ListItem>,
}

/// Row item displayed in the host list pane.
///
/// Group headers are selectable only visually; navigation skips over them when
/// moving through hosts so actions like connect/edit/delete always target a
/// `Host` item.
#[derive(Debug, Clone)]
pub enum ListItem {
    GroupHeader(String),
    Host(String),
}

impl App {
    /// Creates a new app state from loaded configuration.
    ///
    /// Derived lists are built immediately so the UI can render without doing
    /// additional work, and selection starts on the first host item if one
    /// exists rather than on a group header.
    pub fn new(config: Config) -> Self {
        let group_entries = Self::build_group_entries(&config);
        let items = Self::build_items(&config, &group_entries, 0, "");
        App {
            config,
            selected: Self::first_host_index(&items),
            mode: Mode::Normal,
            exit: false,
            dirty: false,
            focus: Pane::Groups,
            group_selected: 0,
            search: Input::default(),
            view: View::Hosts,
            sessions: Vec::new(),
            prefix: PrefixState::Inactive,
            selection: None,
            clipboard_notice_until: None,
            group_entries,
            items,
        }
    }

    /// Builds the group-pane entries from persisted groups and current hosts.
    ///
    /// Group names are sorted for stable navigation. The virtual `Ungrouped`
    /// entry is only shown when it would contain at least one host.
    fn build_group_entries(config: &Config) -> Vec<GroupEntry> {
        let mut entries = vec![GroupEntry::All];
        let mut group_names: Vec<_> = config.groups().iter().map(|g| g.name.clone()).collect();
        group_names.sort();
        for name in group_names {
            entries.push(GroupEntry::Named(name));
        }
        if !config.ungrouped_hosts().is_empty() {
            entries.push(GroupEntry::Ungrouped);
        }
        entries
    }

    /// Builds the host-list items for the active group filter or search query.
    ///
    /// Search mode ignores the group filter and ranks all hosts globally with
    /// `nucleo-matcher`. Normal mode respects the selected group pane entry and
    /// emits group headers only in the `All` view.
    fn build_items(
        config: &Config,
        group_entries: &[GroupEntry],
        group_selected: usize,
        search: &str,
    ) -> Vec<ListItem> {
        let query = search.trim();
        if !query.is_empty() {
            // Global fuzzy search across all hosts, ranked by score.
            let mut matcher = Matcher::new(MatcherConfig::DEFAULT);
            let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
            let mut hay_buf = Vec::new();
            let mut scored: Vec<(&Host, u32)> = config
                .hosts()
                .iter()
                .filter_map(|host| {
                    let hay = Utf32Str::new(&host.alias, &mut hay_buf);
                    pattern.score(hay, &mut matcher).map(|s| (host, s))
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.alias.cmp(&b.0.alias)));
            return scored
                .into_iter()
                .map(|(h, _)| ListItem::Host(h.alias.clone()))
                .collect();
        }

        let filter = group_entries.get(group_selected);
        let mut items = Vec::new();

        let push_sorted_hosts = |items: &mut Vec<ListItem>, mut hosts: Vec<&Host>| {
            hosts.sort_by(|a, b| a.alias.cmp(&b.alias));
            for host in hosts {
                items.push(ListItem::Host(host.alias.clone()));
            }
        };

        match filter {
            Some(GroupEntry::All) | None => {
                let mut group_names: Vec<_> =
                    config.groups().iter().map(|g| g.name.clone()).collect();
                group_names.sort();
                for name in &group_names {
                    items.push(ListItem::GroupHeader(name.clone()));
                    push_sorted_hosts(&mut items, config.hosts_in_group(name));
                }
                let ungrouped = config.ungrouped_hosts();
                if !ungrouped.is_empty() {
                    items.push(ListItem::GroupHeader("ungrouped".into()));
                    push_sorted_hosts(&mut items, ungrouped);
                }
            }
            Some(GroupEntry::Named(name)) => {
                push_sorted_hosts(&mut items, config.hosts_in_group(name));
            }
            Some(GroupEntry::Ungrouped) => {
                push_sorted_hosts(&mut items, config.ungrouped_hosts());
            }
        }

        items
    }

    /// Returns the index of the first host row, falling back to zero.
    ///
    /// The fallback keeps empty lists and header-only lists safe to render, while
    /// callers still check `selected_host` before performing host actions.
    fn first_host_index(items: &[ListItem]) -> usize {
        items
            .iter()
            .position(|item| matches!(item, ListItem::Host(_)))
            .unwrap_or(0)
    }

    /// Recomputes derived group and host lists after config or search changes.
    ///
    /// This method also repairs selection indexes that may have become invalid
    /// after deletion, renaming, filtering, or group changes.
    pub fn rebuild(&mut self) {
        self.group_entries = Self::build_group_entries(&self.config);
        if self.group_selected >= self.group_entries.len() {
            self.group_selected = 0;
        }
        self.items = Self::build_items(
            &self.config,
            &self.group_entries,
            self.group_selected,
            self.search.value(),
        );
        if self.selected >= self.items.len() {
            self.selected = Self::first_host_index(&self.items);
        }
    }

    /// Returns the current host-list rows for rendering.
    pub fn items(&self) -> &[ListItem] {
        &self.items
    }

    /// Returns the current group-pane entries for rendering.
    pub fn group_entries(&self) -> &[GroupEntry] {
        &self.group_entries
    }

    /// Returns the selected host, if the current row is a host row.
    pub fn selected_host(&self) -> Option<&Host> {
        match self.items.get(self.selected)? {
            ListItem::Host(alias) => self.config.find(alias),
            ListItem::GroupHeader(_) => None,
        }
    }

    /// Toggles keyboard focus between the group pane and host pane.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Groups => Pane::Hosts,
            Pane::Hosts => Pane::Groups,
        };
    }

    /// Moves keyboard focus to the group pane.
    pub fn group_focus(&mut self) {
        self.focus = Pane::Groups;
    }

    /// Moves keyboard focus to the host pane.
    pub fn host_focus(&mut self) {
        self.focus = Pane::Hosts;
    }

    /// Moves selection down in the focused browser pane.
    ///
    /// Host navigation skips group-header rows; group navigation rebuilds the
    /// host list for the newly selected group filter and resets host selection to
    /// the first available host.
    pub fn move_down(&mut self) {
        match self.focus {
            Pane::Hosts => {
                if self.items.is_empty() {
                    return;
                }
                let mut next = self.selected;
                loop {
                    next = (next + 1) % self.items.len();
                    if matches!(self.items[next], ListItem::Host(_)) || next == self.selected {
                        break;
                    }
                }
                self.selected = next;
            }
            Pane::Groups => {
                if !self.group_entries.is_empty() {
                    self.group_selected = (self.group_selected + 1) % self.group_entries.len();
                    self.items = Self::build_items(
                        &self.config,
                        &self.group_entries,
                        self.group_selected,
                        self.search.value(),
                    );
                    self.selected = Self::first_host_index(&self.items);
                }
            }
        }
    }

    /// Moves selection up in the focused browser pane.
    ///
    /// This mirrors `move_down`, including wrapping behavior and skipping
    /// non-host rows in the host pane.
    pub fn move_up(&mut self) {
        match self.focus {
            Pane::Hosts => {
                if self.items.is_empty() {
                    return;
                }
                let mut next = self.selected;
                loop {
                    next = next.checked_sub(1).unwrap_or(self.items.len() - 1);
                    if matches!(self.items[next], ListItem::Host(_)) || next == self.selected {
                        break;
                    }
                }
                self.selected = next;
            }
            Pane::Groups => {
                if !self.group_entries.is_empty() {
                    self.group_selected = self
                        .group_selected
                        .checked_sub(1)
                        .unwrap_or(self.group_entries.len() - 1);
                    self.items = Self::build_items(
                        &self.config,
                        &self.group_entries,
                        self.group_selected,
                        self.search.value(),
                    );
                    self.selected = Self::first_host_index(&self.items);
                }
            }
        }
    }

    /// Starts adding a host, optionally pre-filling the selected group.
    ///
    /// Adding from `All` or `Ungrouped` starts with no group. Adding while a real
    /// group filter is selected pre-populates the group field with that name.
    pub fn start_adding(&mut self) {
        let group = &self.group_entries[self.group_selected];
        match group {
            GroupEntry::All | GroupEntry::Ungrouped => self.mode = Mode::Adding(FormState::blank()),
            GroupEntry::Named(group_name) => {
                self.mode = Mode::Adding(FormState::with_group(group_name))
            }
        }
    }

    /// Starts adding a host by cloning the currently selected host into a form.
    ///
    /// This is a convenience path for creating similar hosts. Validation during
    /// submission still requires the user to change the alias if it would
    /// duplicate an existing host.
    pub fn start_add_from_host(&mut self) {
        if let Some(host) = self.selected_host().cloned() {
            self.mode = Mode::Adding(FormState::from_host(&host))
        }
    }

    /// Starts editing the currently selected host.
    ///
    /// The original alias is stored separately so the edit can rename the alias
    /// while still updating the correct existing config entry on submit.
    pub fn start_editing(&mut self) {
        if let Some(host) = self.selected_host().cloned() {
            self.mode = Mode::Editing {
                original_alias: host.alias.clone(),
                form: FormState::from_host(&host),
            };
        }
    }

    /// Starts a delete confirmation for the focused item.
    ///
    /// Host focus confirms host deletion. Group focus confirms group deletion,
    /// but only for real named groups; virtual entries like `All` and `Ungrouped`
    /// cannot be deleted.
    pub fn start_delete(&mut self) {
        match self.focus {
            Pane::Hosts => {
                if let Some(host) = self.selected_host() {
                    self.mode = Mode::ConfirmDelete(host.alias.clone());
                }
            }
            Pane::Groups => {
                if let Some(GroupEntry::Named(name)) = self.group_entries.get(self.group_selected) {
                    self.mode = Mode::ConfirmDeleteGroup(name.clone());
                }
            }
        }
    }

    /// Starts the modal flow for creating a new group.
    pub fn start_adding_group(&mut self) {
        self.mode = Mode::AddingGroup(InputState::default());
    }

    /// Starts the modal flow for renaming the selected real group.
    pub fn start_editing_group(&mut self) {
        if let Some(GroupEntry::Named(name)) = self.group_entries.get(self.group_selected) {
            self.mode = Mode::EditingGroup {
                original_name: name.clone(),
                input: InputState {
                    buffer: Input::new(name.clone()),
                    error: None,
                },
            };
        }
    }

    /// Applies the currently pending delete confirmation.
    ///
    /// Deleting a group does not delete hosts; `Config::remove_group` moves them
    /// back to ungrouped. Any successful delete marks the config dirty so the
    /// event loop knows to persist it.
    pub fn confirm_delete(&mut self) {
        match &self.mode {
            Mode::ConfirmDelete(alias) => {
                let alias = alias.clone();
                self.config.remove_host(&alias);
                self.rebuild();
                self.dirty = true;
            }
            Mode::ConfirmDeleteGroup(name) => {
                let name = name.clone();
                self.config.remove_group(&name);
                self.rebuild();
                self.dirty = true;
            }
            _ => {}
        }
        self.mode = Mode::Normal;
    }

    /// Ensures a host's referenced group exists in the group list.
    ///
    /// Users can type a new group name directly into the host form. Rather than
    /// forcing a separate group-create step, submission creates the group on
    /// demand so the browser pane can render it afterward.
    fn ensure_host_group_exists(&mut self, host: &Host) {
        if let Some(ref group_name) = host.group {
            self.config.add_group(group_name);
        }
    }

    /// Submits the active add/edit form and mutates config on success.
    ///
    /// Validation errors are written back into the active form state so the UI
    /// can display them without losing the user's input. Successful config
    /// mutations rebuild derived lists, set `dirty`, and return to normal mode.
    pub fn submit_form(&mut self) {
        let mode = self.mode.clone();
        match mode {
            Mode::Adding(mut form) => match form.to_host() {
                Ok(host) => {
                    if self.config.find(&host.alias).is_some() {
                        form.error = Some(format!("Alias '{}' already exists", host.alias));
                        self.mode = Mode::Adding(form);
                        return;
                    }
                    self.ensure_host_group_exists(&host);
                    self.config.add_host(host);
                    self.rebuild();
                    self.dirty = true;
                    self.mode = Mode::Normal;
                }
                Err(e) => {
                    form.error = Some(e);
                    self.mode = Mode::Adding(form);
                }
            },
            Mode::Editing {
                original_alias,
                mut form,
            } => match form.to_host() {
                Ok(host) => {
                    if host.alias != original_alias && self.config.find(&host.alias).is_some() {
                        form.error = Some(format!("Alias '{}' already exists", host.alias));
                        self.mode = Mode::Editing {
                            original_alias,
                            form,
                        };
                        return;
                    }
                    self.ensure_host_group_exists(&host);
                    self.config.update_host(&original_alias, host);
                    self.rebuild();
                    self.dirty = true;
                    self.mode = Mode::Normal;
                }
                Err(e) => {
                    form.error = Some(e);
                    self.mode = Mode::Editing {
                        original_alias,
                        form,
                    };
                }
            },
            Mode::AddingGroup(mut input) => {
                let name = input.buffer.value().trim().to_string();
                if name.is_empty() {
                    input.error = Some("Group name cannot be empty".into());
                    self.mode = Mode::AddingGroup(input);
                    return;
                }
                if self.config.find_group(&name).is_some() {
                    input.error = Some(format!("Group '{name}' already exists"));
                    self.mode = Mode::AddingGroup(input);
                    return;
                }
                self.config.add_group(&name);
                self.rebuild();
                self.dirty = true;
                self.mode = Mode::Normal;
            }
            Mode::EditingGroup {
                original_name,
                mut input,
            } => {
                let new_name = input.buffer.value().trim().to_string();
                if new_name.is_empty() {
                    input.error = Some("Group name cannot be empty".into());
                    self.mode = Mode::EditingGroup {
                        original_name,
                        input,
                    };
                    return;
                }
                if new_name != original_name && self.config.find_group(&new_name).is_some() {
                    input.error = Some(format!("Group '{new_name}' already exists"));
                    self.mode = Mode::EditingGroup {
                        original_name,
                        input,
                    };
                    return;
                }
                self.config.rename_group(&original_name, &new_name);
                self.rebuild();
                self.dirty = true;
                self.mode = Mode::Normal;
            }
            _ => {}
        }
    }

    /// Leaves the current modal state without applying changes.
    pub fn cancel_mode(&mut self) {
        self.mode = Mode::Normal;
    }

    /// Enters search mode and directs navigation focus to the host list.
    pub fn start_search(&mut self) {
        self.focus = Pane::Hosts;
        self.mode = Mode::Searching;
    }

    /// Exits search input mode while keeping the current query applied.
    pub fn commit_search(&mut self) {
        // Exits the input mode but keeps the filter applied.
        self.mode = Mode::Normal;
    }

    /// Clears the search query and rebuilds the unfiltered host list.
    pub fn cancel_search(&mut self) {
        // Drop the filter entirely and return to the normal view.
        self.search.reset();
        self.mode = Mode::Normal;
        self.rebuild();
        self.selected = Self::first_host_index(&self.items);
    }

    /// Rebuilds host-list search results after the query changes.
    pub fn refresh_search(&mut self) {
        self.rebuild();
        self.selected = Self::first_host_index(&self.items);
    }

    /// Returns the active host form when the current mode owns one.
    pub fn form_state_mut(&mut self) -> Option<&mut FormState> {
        match &mut self.mode {
            Mode::Adding(form) | Mode::Editing { form, .. } => Some(form),
            _ => None,
        }
    }

    /// Returns the active group-name input state when the current mode owns one.
    pub fn input_state_mut(&mut self) -> Option<&mut InputState> {
        match &mut self.mode {
            Mode::AddingGroup(input) | Mode::EditingGroup { input, .. } => Some(input),
            _ => None,
        }
    }

    /// Opens an embedded SSH session for the selected host.
    ///
    /// Existing sessions are de-duplicated by alias and switched to instead of
    /// spawning another SSH process. Successful opens update `last_accessed`,
    /// which is persisted through the normal dirty/config save path.
    pub fn open_session(&mut self, rows: u16, cols: u16) {
        let Some(mut host) = self.selected_host().cloned() else {
            return;
        };
        if let Some(idx) = self.find_session_by_alias(&host.alias) {
            self.switch_to_session(idx);
            return;
        }
        match Session::spawn(&host, rows, cols) {
            Ok(session) => {
                host.update_last_accessed();
                let alias = host.alias.clone();
                self.config.update_host(&alias, host);
                self.dirty = true;
                self.sessions.push(session);
                self.switch_to_session(self.sessions.len() - 1);
            }
            Err(e) => {
                self.mode = Mode::ConnectError {
                    alias: host.alias,
                    message: e.to_string(),
                };
            }
        }
    }

    /// Starts a background TCP reachability check for the selected host.
    ///
    /// This tests whether the configured hostname and port accept a TCP
    /// connection; it does not authenticate or run SSH. Results are posted back
    /// through shared `TestStatus` so the main event loop remains responsive.
    pub fn test_host(&mut self) {
        let Some(host) = self.selected_host().cloned() else {
            return;
        };

        let status = Arc::new(Mutex::new(TestStatus::Testing));
        self.mode = Mode::TestResult {
            alias: host.alias.clone(),
            status: Arc::clone(&status),
        };

        let hostname = host.hostname;
        let port = host.port;

        std::thread::spawn(move || {
            let addr_str = format!("{hostname}:{port}");
            let timeout = Duration::from_secs(3);

            let result = match addr_str.to_socket_addrs() {
                Ok(addrs) => {
                    let mut last_err = None;
                    for addr in addrs {
                        match TcpStream::connect_timeout(&addr, timeout) {
                            Ok(_) => {
                                *status.lock().unwrap() = TestStatus::Done {
                                    success: true,
                                    message: format!("Port {port} is open"),
                                };
                                return;
                            }
                            Err(e) => last_err = Some(e),
                        }
                    }
                    last_err
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "No addresses found".into())
                }
                Err(e) => e.to_string(),
            };

            *status.lock().unwrap() = TestStatus::Done {
                success: false,
                message: result,
            };
        });
    }

    /// Switches back to the host browser view.
    pub fn switch_to_hosts(&mut self) {
        self.view = View::Hosts;
    }

    /// Removes sessions whose child process has exited and repairs the active view.
    ///
    /// Session removal shifts indexes, so the active `View::Session` index must
    /// be updated when a session before or at the active tab disappears.
    pub fn close_exited_sessions(&mut self) {
        let mut i = 0;
        while i < self.sessions.len() {
            if matches!(self.sessions[i].status(), SessionStatus::Exited(_)) {
                self.sessions.remove(i);
                match self.view {
                    View::Session(idx) if idx == i => {
                        if self.sessions.is_empty() {
                            self.view = View::Hosts;
                        } else if i >= self.sessions.len() {
                            self.view = View::Session(self.sessions.len() - 1);
                        }
                    }
                    View::Session(idx) if idx > i => {
                        self.view = View::Session(idx - 1);
                    }
                    _ => {}
                }
            } else {
                i += 1;
            }
        }
    }

    /// Closes the active session tab, killing its PTY child through `Session::drop`.
    ///
    /// After removal, focus moves to the next sensible tab: the previous last
    /// session if the closed tab was at the end, or the host browser if no
    /// sessions remain.
    pub fn close_current_session(&mut self) {
        let View::Session(idx) = self.view else {
            return;
        };
        if idx >= self.sessions.len() {
            return;
        }
        self.sessions.remove(idx);
        if self.sessions.is_empty() {
            self.view = View::Hosts;
        } else if idx >= self.sessions.len() {
            self.view = View::Session(self.sessions.len() - 1);
        }
    }

    /// Switches to a session by index and clears its unread marker.
    pub fn switch_to_session(&mut self, idx: usize) {
        if idx < self.sessions.len() {
            self.view = View::Session(idx);
            self.sessions[idx].unread.store(false, Ordering::SeqCst);
        }
    }

    /// Returns mutable access to the active session, if the session view is active.
    pub fn active_session_mut(&mut self) -> Option<&mut Session> {
        match self.view {
            View::Session(idx) => self.sessions.get_mut(idx),
            View::Hosts => None,
        }
    }

    /// Returns true when at least one embedded SSH session is alive in the app.
    pub fn has_active_sessions(&self) -> bool {
        !self.sessions.is_empty()
    }

    /// Moves to the next tab in the host/session tab ring.
    ///
    /// The host browser participates as a tab so repeated next-tab commands cycle
    /// through sessions and then back to hosts.
    pub fn next_tab(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        match self.view {
            View::Hosts => self.switch_to_session(0),
            View::Session(idx) => {
                if idx + 1 < self.sessions.len() {
                    self.switch_to_session(idx + 1);
                } else {
                    self.switch_to_hosts();
                }
            }
        }
    }

    /// Moves to the previous tab in the host/session tab ring.
    pub fn prev_tab(&mut self) {
        if self.sessions.is_empty() {
            return;
        }
        match self.view {
            View::Hosts => self.switch_to_session(self.sessions.len() - 1),
            View::Session(0) => self.switch_to_hosts(),
            View::Session(idx) => self.switch_to_session(idx - 1),
        }
    }

    /// Clears any active terminal text selection.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Shows the transient "copied to clipboard" notice.
    pub fn show_clipboard_notice(&mut self) {
        self.clipboard_notice_until = Some(Instant::now() + Duration::from_secs(2));
    }

    /// Clears transient notices whose deadline has passed.
    pub fn clear_expired_notifications(&mut self) {
        if self
            .clipboard_notice_until
            .is_some_and(|until| Instant::now() >= until)
        {
            self.clipboard_notice_until = None;
        }
    }

    /// Returns whether the clipboard notice should currently be rendered.
    pub fn clipboard_notice_visible(&self) -> bool {
        self.clipboard_notice_until.is_some()
    }

    /// Finds the session tab index for a host alias.
    ///
    /// Aliases are unique in config, so they also serve as a stable session
    /// de-duplication key while the app is running.
    pub fn find_session_by_alias(&self, alias: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.alias == alias)
    }
}
