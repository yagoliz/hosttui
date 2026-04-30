use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};
use tui_input::Input;

use std::sync::atomic::Ordering;

use crate::model::{Config, Host};
use crate::pty::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Groups,
    Hosts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Hosts,
    Session(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrefixState {
    #[default]
    Inactive,
    Pending,
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtraField {
    Key,
    Value,
}

#[derive(Debug, Clone)]
pub struct ExtraEntryForm {
    pub key: Input,
    pub value: Input,
    pub active: ExtraField,
    /// `None` when adding a new pair; `Some(i)` when editing the i-th existing pair.
    pub editing_index: Option<usize>,
}

impl ExtraEntryForm {
    fn blank() -> Self {
        ExtraEntryForm {
            key: Input::default(),
            value: Input::default(),
            active: ExtraField::Key,
            editing_index: None,
        }
    }

    fn from_pair(index: usize, key: &str, value: &str) -> Self {
        ExtraEntryForm {
            key: Input::new(key.into()),
            value: Input::new(value.into()),
            active: ExtraField::Key,
            editing_index: Some(index),
        }
    }

    pub fn active_input(&mut self) -> &mut Input {
        match self.active {
            ExtraField::Key => &mut self.key,
            ExtraField::Value => &mut self.value,
        }
    }

    pub fn toggle_field(&mut self) {
        self.active = match self.active {
            ExtraField::Key => ExtraField::Value,
            ExtraField::Value => ExtraField::Key,
        };
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExtrasEditor {
    pub selected: usize,
    pub entry: Option<ExtraEntryForm>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FormState {
    pub fields: [(Field, Input); 7],
    pub active: usize,
    pub error: Option<String>,
    pub extras: Vec<(String, String)>,
    pub extras_editor: Option<ExtrasEditor>,
}

impl FormState {
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
        }
    }

    fn with_group(group: &str) -> Self {
        let mut form = Self::blank();
        form.fields[5].1 = Input::new(group.into());
        form
    }

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
        }
    }

    pub fn open_extras(&mut self) {
        if self.extras_editor.is_none() {
            self.extras_editor = Some(ExtrasEditor::default());
        }
    }

    pub fn close_extras(&mut self) {
        self.extras_editor = None;
    }

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

    pub fn extras_move_down(&mut self) {
        if let Some(ed) = self.extras_editor.as_mut()
            && !self.extras.is_empty()
        {
            ed.selected = (ed.selected + 1) % self.extras.len();
        }
    }

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

    pub fn active_input(&mut self) -> &mut Input {
        &mut self.fields[self.active].1
    }

    pub fn next_field(&mut self) {
        self.active = (self.active + 1) % self.fields.len();
    }

    pub fn prev_field(&mut self) {
        self.active = self.active.checked_sub(1).unwrap_or(self.fields.len() - 1);
    }

    fn value(&self, field: Field) -> &str {
        self.fields
            .iter()
            .find(|(f, _)| *f == field)
            .unwrap()
            .1
            .value()
    }

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
        if user.is_empty() {
            return Err("User cannot be empty".into());
        }

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
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct InputState {
    pub buffer: Input,
    pub error: Option<String>,
}

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
}

#[derive(Debug, Clone)]
pub enum GroupEntry {
    All,
    Named(String),
    Ungrouped,
}

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
    group_entries: Vec<GroupEntry>,
    items: Vec<ListItem>,
}

#[derive(Debug, Clone)]
pub enum ListItem {
    GroupHeader(String),
    Host(String),
}

impl App {
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
            group_entries,
            items,
        }
    }

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

    fn first_host_index(items: &[ListItem]) -> usize {
        items
            .iter()
            .position(|item| matches!(item, ListItem::Host(_)))
            .unwrap_or(0)
    }

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

    pub fn items(&self) -> &[ListItem] {
        &self.items
    }

    pub fn group_entries(&self) -> &[GroupEntry] {
        &self.group_entries
    }

    pub fn selected_host(&self) -> Option<&Host> {
        match self.items.get(self.selected)? {
            ListItem::Host(alias) => self.config.find(alias),
            ListItem::GroupHeader(_) => None,
        }
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Pane::Groups => Pane::Hosts,
            Pane::Hosts => Pane::Groups,
        };
    }

    pub fn group_focus(&mut self) {
        self.focus = Pane::Groups;
    }

    pub fn host_focus(&mut self) {
        self.focus = Pane::Hosts;
    }

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

    pub fn start_adding(&mut self) {
        let group = &self.group_entries[self.group_selected];
        match group {
            GroupEntry::All | GroupEntry::Ungrouped => self.mode = Mode::Adding(FormState::blank()),
            GroupEntry::Named(group_name) => {
                self.mode = Mode::Adding(FormState::with_group(group_name))
            }
        }
    }

    pub fn start_editing(&mut self) {
        if let Some(host) = self.selected_host().cloned() {
            self.mode = Mode::Editing {
                original_alias: host.alias.clone(),
                form: FormState::from_host(&host),
            };
        }
    }

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

    pub fn start_adding_group(&mut self) {
        self.mode = Mode::AddingGroup(InputState::default());
    }

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

    fn ensure_host_group_exists(&mut self, host: &Host) {
        if let Some(ref group_name) = host.group {
            self.config.add_group(group_name);
        }
    }

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

    pub fn cancel_mode(&mut self) {
        self.mode = Mode::Normal;
    }

    pub fn start_search(&mut self) {
        self.focus = Pane::Hosts;
        self.mode = Mode::Searching;
    }

    pub fn commit_search(&mut self) {
        // Exits the input mode but keeps the filter applied.
        self.mode = Mode::Normal;
    }

    pub fn cancel_search(&mut self) {
        // Drop the filter entirely and return to the normal view.
        self.search.reset();
        self.mode = Mode::Normal;
        self.rebuild();
        self.selected = Self::first_host_index(&self.items);
    }

    pub fn refresh_search(&mut self) {
        self.rebuild();
        self.selected = Self::first_host_index(&self.items);
    }

    pub fn form_state_mut(&mut self) -> Option<&mut FormState> {
        match &mut self.mode {
            Mode::Adding(form) | Mode::Editing { form, .. } => Some(form),
            _ => None,
        }
    }

    pub fn input_state_mut(&mut self) -> Option<&mut InputState> {
        match &mut self.mode {
            Mode::AddingGroup(input) | Mode::EditingGroup { input, .. } => Some(input),
            _ => None,
        }
    }

    pub fn open_session(&mut self, rows: u16, cols: u16) {
        let Some(host) = self.selected_host().cloned() else {
            return;
        };
        if let Some(idx) = self.find_session_by_alias(&host.alias) {
            self.switch_to_session(idx);
            return;
        }
        match Session::spawn(&host, rows, cols) {
            Ok(session) => {
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

    pub fn switch_to_hosts(&mut self) {
        self.view = View::Hosts;
    }

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

    pub fn switch_to_session(&mut self, idx: usize) {
        if idx < self.sessions.len() {
            self.view = View::Session(idx);
            self.sessions[idx].unread.store(false, Ordering::SeqCst);
        }
    }

    pub fn active_session_mut(&mut self) -> Option<&mut Session> {
        match self.view {
            View::Session(idx) => self.sessions.get_mut(idx),
            View::Hosts => None,
        }
    }

    pub fn has_active_sessions(&self) -> bool {
        !self.sessions.is_empty()
    }

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

    pub fn find_session_by_alias(&self, alias: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.alias == alias)
    }
}
