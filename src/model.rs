use std::collections::HashSet;

use chrono::prelude::*;
use serde::{Deserialize, Serialize};

const DATE_FORMAT: &str = "%FT%H:%M:%S";

/// A single SSH target managed by the application.
///
/// This is the persisted representation used in `hosts.toml`, not just UI
/// state. Fields intentionally map closely to OpenSSH config directives so the
/// same value can be used for the host list, generated SSH config, and direct
/// embedded PTY connections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Host {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    pub group: Option<String>,
    #[serde(default)]
    pub extra: Vec<(String, String)>,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub last_accessed: String,
}

impl Host {
    /// Records the current local time as the last successful access timestamp.
    ///
    /// The timestamp is kept as a formatted string because it is displayed and
    /// persisted directly, and the app does not currently need timezone-aware
    /// comparisons or arithmetic after writing it.
    pub fn update_last_accessed(&mut self) {
        let local = Local::now();
        self.last_accessed = local.format(DATE_FORMAT).to_string();
    }
}

/// A user-defined group used to organize hosts in the browser pane.
///
/// Group membership lives on each `Host` as `host.group`; this struct stores
/// the group names independently so empty groups can exist and be rendered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
}

/// The complete persisted application configuration.
///
/// Hosts and groups are private so all mutation goes through methods that keep
/// cross-references consistent. In particular, deleting or renaming a group
/// must also update affected hosts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    hosts: Vec<Host>,
    groups: Vec<Group>,
}

impl Config {
    /// Creates a configuration from already-validated host and group lists.
    ///
    /// Tests and storage loading use this constructor to preserve the exact
    /// order from fixtures or disk. UI code performs validation before calling
    /// mutation methods, so this function deliberately does not reject duplicate
    /// aliases by itself.
    pub fn new(hosts: Vec<Host>, groups: Vec<Group>) -> Self {
        Config { hosts, groups }
    }

    /// Returns hosts in their persisted order.
    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    /// Returns groups in their persisted order.
    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    /// Finds a host by alias, which is the user-facing unique identifier.
    pub fn find(&self, alias: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.alias == alias)
    }

    /// Returns all hosts assigned to a specific group name.
    ///
    /// The returned references borrow from the config so callers can inspect
    /// host data without cloning. Sorting, if needed, is handled by the caller
    /// because different views may want different ordering.
    pub fn hosts_in_group(&self, group_name: &str) -> Vec<&Host> {
        self.hosts
            .iter()
            .filter(|h| h.group.as_deref() == Some(group_name))
            .collect()
    }

    /// Returns hosts that are not assigned to any group.
    pub fn ungrouped_hosts(&self) -> Vec<&Host> {
        self.hosts.iter().filter(|h| h.group.is_none()).collect()
    }

    /// Checks whether every host alias appears only once.
    ///
    /// Alias uniqueness matters because aliases are used for selection,
    /// generated `Host` entries, and session de-duplication. The method is kept
    /// separate from mutation so loaded configs can be validated in tests or
    /// future startup checks.
    pub fn has_unique_aliases(&self) -> bool {
        let mut seen = HashSet::new();
        self.hosts.iter().all(|h| seen.insert(&h.alias))
    }

    /// Appends a new host to the configuration.
    ///
    /// The caller is responsible for validating alias uniqueness and creating
    /// the referenced group first if the UI should expose it as a real group.
    pub fn add_host(&mut self, host: Host) {
        self.hosts.push(host);
    }

    /// Replaces an existing host identified by its previous alias.
    ///
    /// The lookup alias is separate from `host.alias` so editing a host can
    /// rename the alias without losing track of the original entry.
    pub fn update_host(&mut self, alias: &str, host: Host) {
        if let Some(existing) = self.hosts.iter_mut().find(|h| h.alias == alias) {
            *existing = host;
        }
    }

    /// Removes a host by alias if it exists.
    pub fn remove_host(&mut self, alias: &str) {
        self.hosts.retain(|h| h.alias != alias);
    }

    /// Adds a group name unless it already exists.
    ///
    /// This method is intentionally idempotent because host edits may create
    /// their referenced group automatically, and repeated edits should not
    /// duplicate groups.
    pub fn add_group(&mut self, name: &str) {
        if !self.groups.iter().any(|g| g.name == name) {
            self.groups.push(Group { name: name.into() });
        }
    }

    /// Deletes a group and moves any member hosts back to the ungrouped view.
    ///
    /// Host records are preserved; only the group relationship is cleared. This
    /// keeps deleting a group from being a destructive host deletion operation.
    pub fn remove_group(&mut self, name: &str) {
        self.groups.retain(|g| g.name != name);
        for host in &mut self.hosts {
            if host.group.as_deref() == Some(name) {
                host.group = None;
            }
        }
    }

    /// Finds a group by name.
    pub fn find_group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
    }

    /// Renames a group and rewrites all host memberships that point to it.
    ///
    /// Group names are stored by value on each host, so renaming must touch both
    /// the group list and every host that references the old name.
    pub fn rename_group(&mut self, old_name: &str, new_name: &str) {
        if let Some(group) = self.groups.iter_mut().find(|g| g.name == old_name) {
            group.name = new_name.into();
        }
        for host in &mut self.hosts {
            if host.group.as_deref() == Some(old_name) {
                host.group = Some(new_name.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(alias: &str, group: Option<&str>) -> Host {
        Host {
            alias: alias.into(),
            hostname: "127.0.0.1".into(),
            user: "user".into(),
            port: 22,
            identity_file: Some("~/.ssh/id_rsa".into()),
            group: group.map(Into::into),
            extra: vec![],
            details: "Test host".into(),
            last_accessed: "".into(),
        }
    }

    fn group(name: &str) -> Group {
        Group { name: name.into() }
    }

    fn sample_config() -> Config {
        Config::new(
            vec![
                host("web1", Some("production")),
                host("web2", Some("production")),
                host("staging", Some("staging")),
                host("personal", None),
            ],
            vec![group("production"), group("staging")],
        )
    }

    #[test]
    fn host_fields() {
        let h = host("test", Some("group"));
        assert_eq!(h.alias, "test");
        assert_eq!(h.hostname, "127.0.0.1");
        assert_eq!(h.user, "user");
        assert_eq!(h.port, 22);
        assert_eq!(h.identity_file, Some("~/.ssh/id_rsa".into()));
        assert_eq!(h.group, Some("group".into()));
        assert_eq!(h.details, "Test host");
    }

    #[test]
    fn find_existing_host() {
        let config = sample_config();
        let found = config.find("web1").unwrap();
        assert_eq!(found.alias, "web1");
        assert_eq!(found.group, Some("production".into()));
    }

    #[test]
    fn find_missing_host() {
        let config = sample_config();
        assert!(config.find("nonexistent").is_none());
    }

    #[test]
    fn groups_stored_independently() {
        let config = sample_config();
        let groups = config.groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].name, "production");
        assert_eq!(groups[1].name, "staging");
    }

    #[test]
    fn hosts_in_group() {
        let config = sample_config();
        let prod_hosts = config.hosts_in_group("production");
        let aliases: Vec<&str> = prod_hosts.iter().map(|h| h.alias.as_str()).collect();
        assert_eq!(aliases, vec!["web1", "web2"]);
    }

    #[test]
    fn hosts_in_nonexistent_group() {
        let config = sample_config();
        assert!(config.hosts_in_group("nope").is_empty());
    }

    #[test]
    fn ungrouped_hosts() {
        let config = sample_config();
        let ungrouped = config.ungrouped_hosts();
        assert_eq!(ungrouped.len(), 1);
        assert_eq!(ungrouped[0].alias, "personal");
    }

    #[test]
    fn unique_aliases() {
        let config = sample_config();
        assert!(config.has_unique_aliases());
    }

    #[test]
    fn duplicate_aliases_detected() {
        let config = Config::new(vec![host("dup", None), host("dup", Some("group"))], vec![]);
        assert!(!config.has_unique_aliases());
    }

    #[test]
    fn empty_config() {
        let config = Config::new(vec![], vec![]);
        assert!(config.hosts().is_empty());
        assert!(config.groups().is_empty());
        assert!(config.has_unique_aliases());
        assert!(config.find("anything").is_none());
    }
}
