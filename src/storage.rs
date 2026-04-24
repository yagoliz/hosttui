use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::model::Config;

pub fn config_path() -> Result<PathBuf, Error> {
    let dir = dirs::config_dir().ok_or(Error::NoConfigDir)?;
    Ok(dir.join("hosttui").join("hosts.toml"))
}

pub fn load(path: &Path) -> Result<Config, Error> {
    if !path.exists() {
        return Ok(Config::new(vec![], vec![]));
    }

    let contents = fs::read_to_string(path).map_err(|e| Error::ReadConfig {
        path: path.to_path_buf(),
        source: e,
    })?;

    toml::from_str(&contents).map_err(|e| Error::ParseConfig {
        path: path.to_path_buf(),
        source: e,
    })
}

pub fn save(path: &Path, config: &Config) -> Result<(), Error> {
    let contents = toml::to_string_pretty(config)?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| Error::WriteConfig {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let tmp = path.with_extension("toml.tmp");

    fs::write(&tmp, &contents).map_err(|e| Error::WriteConfig {
        path: tmp.clone(),
        source: e,
    })?;

    fs::rename(&tmp, path).map_err(|e| Error::WriteConfig {
        path: path.to_path_buf(),
        source: e,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Group, Host};
    use tempfile::TempDir;

    fn host(alias: &str, group: Option<&str>) -> Host {
        Host {
            alias: alias.into(),
            hostname: "10.0.0.1".into(),
            user: "admin".into(),
            port: 22,
            identity_file: None,
            group: group.map(Into::into),
            extra: vec![],
        }
    }

    #[test]
    fn round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hosts.toml");

        let config = Config::new(
            vec![host("web", Some("prod")), host("local", None)],
            vec![Group {
                name: "prod".into(),
            }],
        );

        save(&path, &config).unwrap();
        let loaded = load(&path).unwrap();

        assert_eq!(loaded.hosts().len(), 2);
        assert_eq!(loaded.groups().len(), 1);
        assert_eq!(loaded.find("web").unwrap().hostname, "10.0.0.1");
        assert_eq!(loaded.find("local").unwrap().group, None);
    }

    #[test]
    fn load_missing_file_returns_empty_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does_not_exist.toml");

        let config = load(&path).unwrap();
        assert!(config.hosts().is_empty());
        assert!(config.groups().is_empty());
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nested").join("dir").join("hosts.toml");

        let config = Config::new(vec![], vec![]);
        save(&path, &config).unwrap();

        assert!(path.exists());
    }

    #[test]
    fn add_host_then_round_trip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("hosts.toml");

        let mut config = Config::new(
            vec![host("web", Some("prod"))],
            vec![Group {
                name: "prod".into(),
            }],
        );
        save(&path, &config).unwrap();

        config.add_host(host("new", None));
        save(&path, &config).unwrap();

        let reloaded = load(&path).unwrap();
        assert_eq!(reloaded.hosts().len(), 2);
        assert!(reloaded.find("new").is_some());
    }

    #[test]
    fn load_invalid_toml() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.toml");
        fs::write(&path, "not valid { toml }}}").unwrap();

        let err = load(&path).unwrap_err();
        assert!(matches!(err, Error::ParseConfig { .. }));
    }
}
