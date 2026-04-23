use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config as MatcherConfig, Matcher, Utf32Str};

use crate::model::{Config, Host};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Groups,
    Hosts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Alias,
    Hostname,
    User,
    Port,
    IdentityFile,
    Group,
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
        }
    }
}

#[derive(Debug, Clone)]
pub struct FormState {
    pub fields: [(Field, String); 6],
    pub active: usize,
    pub error: Option<String>,
}

impl FormState {
    fn blank() -> Self {
        FormState {
            fields: [
                (Field::Alias, String::new()),
                (Field::Hostname, String::new()),
                (Field::User, String::new()),
                (Field::Port, "22".into()),
                (Field::IdentityFile, String::new()),
                (Field::Group, String::new()),
            ],
            active: 0,
            error: None,
        }
    }

    fn from_host(host: &Host) -> Self {
        FormState {
            fields: [
                (Field::Alias, host.alias.clone()),
                (Field::Hostname, host.hostname.clone()),
                (Field::User, host.user.clone()),
                (Field::Port, host.port.to_string()),
                (
                    Field::IdentityFile,
                    host.identity_file.clone().unwrap_or_default(),
                ),
                (Field::Group, host.group.clone().unwrap_or_default()),
            ],
            active: 0,
            error: None,
        }
    }

    pub fn active_buffer(&mut self) -> &mut String {
        &mut self.fields[self.active].1
    }

    pub fn next_field(&mut self) {
        self.active = (self.active + 1) % self.fields.len();
    }

    pub fn prev_field(&mut self) {
        self.active = self.active.checked_sub(1).unwrap_or(self.fields.len() - 1);
    }

    fn value(&self, field: Field) -> &str {
        &self.fields.iter().find(|(f, _)| *f == field).unwrap().1
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

        Ok(Host {
            alias,
            hostname,
            user,
            port,
            identity_file,
            group,
            extra: vec![],
        })
    }
}

#[derive(Debug, Clone)]
pub struct InputState {
    pub buffer: String,
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
    pub search: String,
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
            focus: Pane::Hosts,
            group_selected: 0,
            search: String::new(),
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

        match filter {
            Some(GroupEntry::All) | None => {
                for group in config.groups() {
                    items.push(ListItem::GroupHeader(group.name.clone()));
                    for host in config.hosts_in_group(&group.name) {
                        items.push(ListItem::Host(host.alias.clone()));
                    }
                }
                let ungrouped = config.ungrouped_hosts();
                if !ungrouped.is_empty() {
                    items.push(ListItem::GroupHeader("ungrouped".into()));
                    for host in ungrouped {
                        items.push(ListItem::Host(host.alias.clone()));
                    }
                }
            }
            Some(GroupEntry::Named(name)) => {
                for host in config.hosts_in_group(name) {
                    items.push(ListItem::Host(host.alias.clone()));
                }
            }
            Some(GroupEntry::Ungrouped) => {
                for host in config.ungrouped_hosts() {
                    items.push(ListItem::Host(host.alias.clone()));
                }
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
            &self.search,
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
                        &self.search,
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
                        &self.search,
                    );
                    self.selected = Self::first_host_index(&self.items);
                }
            }
        }
    }

    pub fn start_adding(&mut self) {
        self.mode = Mode::Adding(FormState::blank());
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
        self.mode = Mode::AddingGroup(InputState {
            buffer: String::new(),
            error: None,
        });
    }

    pub fn start_editing_group(&mut self) {
        if let Some(GroupEntry::Named(name)) = self.group_entries.get(self.group_selected) {
            self.mode = Mode::EditingGroup {
                original_name: name.clone(),
                input: InputState {
                    buffer: name.clone(),
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
                let name = input.buffer.trim().to_string();
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
                let new_name = input.buffer.trim().to_string();
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
        self.search.clear();
        self.mode = Mode::Normal;
        self.rebuild();
        self.selected = Self::first_host_index(&self.items);
    }

    pub fn push_search_char(&mut self, c: char) {
        self.search.push(c);
        self.rebuild();
        self.selected = Self::first_host_index(&self.items);
    }

    pub fn pop_search_char(&mut self) {
        self.search.pop();
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
}
