use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Host {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    pub group: Option<String>,
    pub extra: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    hosts: Vec<Host>,
    groups: Vec<Group>,
}

impl Config {
    pub fn new(hosts: Vec<Host>, groups: Vec<Group>) -> Self {
        Config { hosts, groups }
    }

    pub fn hosts(&self) -> &[Host] {
        &self.hosts
    }

    pub fn groups(&self) -> &[Group] {
        &self.groups
    }

    pub fn find(&self, alias: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.alias == alias)
    }

    pub fn hosts_in_group(&self, group_name: &str) -> Vec<&Host> {
        self.hosts
            .iter()
            .filter(|h| h.group.as_deref() == Some(group_name))
            .collect()
    }

    pub fn ungrouped_hosts(&self) -> Vec<&Host> {
        self.hosts.iter().filter(|h| h.group.is_none()).collect()
    }

    pub fn has_unique_aliases(&self) -> bool {
        let mut seen = HashSet::new();
        self.hosts.iter().all(|h| seen.insert(&h.alias))
    }

    pub fn add_host(&mut self, host: Host) {
        self.hosts.push(host);
    }

    pub fn update_host(&mut self, alias: &str, host: Host) {
        if let Some(existing) = self.hosts.iter_mut().find(|h| h.alias == alias) {
            *existing = host;
        }
    }

    pub fn remove_host(&mut self, alias: &str) {
        self.hosts.retain(|h| h.alias != alias);
    }

    pub fn add_group(&mut self, name: &str) {
        if !self.groups.iter().any(|g| g.name == name) {
            self.groups.push(Group { name: name.into() });
        }
    }

    pub fn remove_group(&mut self, name: &str) {
        self.groups.retain(|g| g.name != name);
        for host in &mut self.hosts {
            if host.group.as_deref() == Some(name) {
                host.group = None;
            }
        }
    }

    pub fn find_group(&self, name: &str) -> Option<&Group> {
        self.groups.iter().find(|g| g.name == name)
    }

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
        let config = Config::new(
            vec![host("dup", None), host("dup", Some("group"))],
            vec![],
        );
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
