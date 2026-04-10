// Host struct will hold each hostname with their alias
pub struct Host {
    pub alias: String,
    pub hostname: String,
    pub user: String,
    pub port: u16,
    pub identity_file: String,
    pub group: Option<String>,
    pub extra: Vec<(String, String)>,
}

// Group struct allows us to divide hosts by different groups
pub struct Group {
    pub name: String,
    pub hosts: Vec<String>,
}

// Config struct
pub struct Config {
    hosts: Vec<Host>,
}

impl Config {
    pub fn new(hosts: Vec<Host>) -> Self {
        Config { hosts }
    }

    pub fn find(&self, alias: &str) -> Option<&Host> {
        for host in &self.hosts {
            if host.alias == alias {
                return Some(host);
            }
        }

        return None;
    }
}

// Tests
#[cfg(test)]
mod tests {
    // Note this useful idiom: importing names from outer (for mod tests) scope.
    use super::*;

    fn create_single_host() -> Host {
        let alias = String::from("test");
        let hostname = String::from("127.0.0.1");
        let user = String::from("user");
        let port = 22;
        let identity_file = String::from("~/.ssh/id_rsa");
        let group = Some(String::from("group"));
        let extra = vec![
            (String::from("opt1"), String::from("val1")),
            (String::from("opt2"), String::from("val2")),
        ];

        Host {
            alias,
            hostname,
            user,
            port,
            identity_file,
            group,
            extra,
        }
    }

    #[test]
    fn host_create() {
        let host = create_single_host();

        assert_eq!(host.alias, "test");
        assert_eq!(host.user, "user");
        assert_eq!(host.hostname, "127.0.0.1");
        assert_eq!(host.port, 22);
        assert_eq!(host.identity_file, "~/.ssh/id_rsa");
        assert_eq!(host.group, Some("group".into()));
    }
}
