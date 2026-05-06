use std::path::PathBuf;

/// Error type used by library modules.
///
/// The binary converts these errors through `anyhow`, but library code keeps a
/// structured enum so callers can inspect failure categories in tests and future
/// UI flows. Path-carrying variants include the path that failed rather than
/// relying only on the lower-level IO error message.
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

    #[error("PTY error for '{alias}'")]
    Pty {
        alias: String,
        #[source]
        source: std::io::Error,
    },
}
