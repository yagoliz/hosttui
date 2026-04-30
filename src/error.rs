use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failed to read config from {path}")]
    ReadConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write config to {path}")]
    WriteConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse config from {path}")]
    ParseConfig {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to serialize config")]
    SerializeConfig(#[from] toml::ser::Error),

    #[error("could not determine config directory")]
    NoConfigDir,

    #[error("ssh connection to '{alias}' failed")]
    Ssh {
        alias: String,
        #[source]
        source: std::io::Error,
    },
}
