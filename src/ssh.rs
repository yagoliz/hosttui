use crate::model::Host;

/// Converts a `Host` record into command-line arguments for `ssh`.
///
/// The binary uses the system OpenSSH client inside an embedded PTY instead of
/// implementing SSH itself. Options are emitted before the final destination
/// argument, matching the order expected by `ssh`. Advanced per-host settings
/// from `Host::extra` are passed as `-o key=value` so they work without needing
/// to be present in the generated config fragment first.
pub fn ssh_args(host: &Host) -> Vec<String> {
    let mut args = Vec::new();

    if let Some(ref identity) = host.identity_file {
        args.extend(["-i".into(), identity.clone()]);
    }

    if host.port != 22 {
        args.extend(["-p".into(), host.port.to_string()]);
    }

    args.extend(["-o".into(), "ConnectTimeout=5".into()]);
    args.extend(["-o".into(), "ServerAliveInterval=10".into()]);
    args.extend(["-o".into(), "ServerAliveCountMax=3".into()]);

    for (key, val) in &host.extra {
        args.extend(["-o".into(), format!("{key}={val}")]);
    }

    args.push(format!("{}@{}", host.user, host.hostname));
    args
}
