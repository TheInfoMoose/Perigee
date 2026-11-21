//! Argument parsing for the `perigee` command-line tool.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use perigee_host::{HostCommand, HostOptions, parse_host_args};
use perigee_net::{
    ClientAuth, IDENTITY_FILE_NAME, KNOWN_HOSTS_FILE_NAME, NODE_KEY_LEN, NodeIdentity, TrustedKeys,
    default_config_dir, node_key_from_hex, node_key_to_hex,
};

/// Usage text for `perigee --help`.
pub const USAGE: &str = "\
Usage:
  perigee connect <HOST[:PORT]> [--host-key KEY] [--config-dir DIR] [--interface NAME]
                  [--for SECONDS] [--timeout SECONDS]
  perigee identity [--config-dir DIR]
  perigee trust add <HOST[:PORT]> <KEY> [--config-dir DIR]
  perigee trust list [--config-dir DIR]
  perigee trust remove <HOST[:PORT]> [--config-dir DIR]
  perigee host [--bind ADDR] [--interface NAME] [--name NODE] [--config-dir DIR]
  perigee host identity | host trust add <KEY> <LABEL> | host trust list | host trust remove <KEY>
  perigee --help
  perigee --version

Subcommands:
  connect    Open a client session to <HOST[:PORT]> and print live session status
  identity   Print this machine's node key (created on first use)
  trust      Pin a host's node key for a <HOST[:PORT]> target, list or remove pins
  host       Run an embedded Perigee host daemon (accepts perigee-host flags)

Connect options:
  --host-key <KEY>       Host node key to pin (default: the pin saved with perigee trust add)
  --config-dir <DIR>     Directory holding identity.key and known_hosts
                         (default: PERIGEE_CONFIG_DIR, else ~/.config/perigee)
  --interface <NAME>     Bind outbound socket to network/VPN interface or IP NAME
  --for <SECONDS>        Disconnect cleanly after whole SECONDS have elapsed
  --timeout <SECONDS>    Handshake timeout in whole SECONDS (default: 5)

Host options:
  --bind <ADDR>          UDP socket bind address (default: 127.0.0.1:4433)
  --interface <NAME>     Bind exclusively to network/VPN interface NAME (fails closed)
  --name <NODE>          Host node identifier (default: perigee-host-node)

General options:
  -h, --help             Print this help text and exit
  -V, --version          Print version information and exit

Trust is mutual and set up once per pair of machines:
  on the host:    perigee-host identity            -> give KEY to the client
  on the client:  perigee trust add <HOST[:PORT]> <KEY>
  on the client:  perigee identity                 -> give KEY to the host
  on the host:    perigee-host trust add <KEY> <LABEL>";

/// Parsed arguments for `perigee connect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectArgs {
    pub target: String,
    pub interface: Option<String>,
    pub run_for: Option<Duration>,
    pub handshake_timeout: Duration,
    /// Host node key from `--host-key`; `None` uses the `known_hosts` pin for `target`.
    pub host_key: Option<[u8; NODE_KEY_LEN]>,
    pub config_dir: Option<PathBuf>,
}

/// A `perigee` client key-management subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientAdmin {
    /// Print this machine's node key.
    Identity { config_dir: Option<PathBuf> },
    /// Pin `key` as the host key for connect target `target`.
    TrustAdd {
        config_dir: Option<PathBuf>,
        target: String,
        key: [u8; NODE_KEY_LEN],
    },
    /// List pinned host keys.
    TrustList { config_dir: Option<PathBuf> },
    /// Remove the pin for connect target `target`.
    TrustRemove {
        config_dir: Option<PathBuf>,
        target: String,
    },
}

/// Top-level command parsed from `perigee` CLI arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    Connect(ConnectArgs),
    Admin(ClientAdmin),
    Host(HostCommand),
    Help,
    Version,
}

/// Parses `perigee` command-line arguments (`args` excludes the program name).
pub fn parse_cli_args(args: &[String], host_base: HostOptions) -> Result<CliCommand> {
    let Some(first) = args.first().map(String::as_str) else {
        return Ok(CliCommand::Help);
    };

    match first {
        "-h" | "--help" => Ok(CliCommand::Help),
        "-V" | "--version" => Ok(CliCommand::Version),
        "host" => {
            let cmd = parse_host_args(&args[1..], host_base)?;
            Ok(CliCommand::Host(cmd))
        }
        "connect" => parse_connect_args(&args[1..]),
        "identity" | "trust" => parse_client_admin(args).map(CliCommand::Admin),
        other => bail!("unknown command '{other}'; run perigee --help"),
    }
}

/// Resolves the `perigee` command line. Only `perigee host` (a run) reads the host
/// environment variables (through `get`); help, version and `connect` never do.
pub fn resolve_cli_command(
    args: &[String],
    get: impl Fn(&str) -> Option<String>,
) -> Result<CliCommand> {
    match parse_cli_args(args, HostOptions::default())? {
        CliCommand::Host(HostCommand::Run(_)) => {
            parse_cli_args(args, HostOptions::from_env_with(get)?)
        }
        other => Ok(other),
    }
}

fn parse_connect_args(args: &[String]) -> Result<CliCommand> {
    let mut target: Option<String> = None;
    let mut interface: Option<String> = None;
    let mut run_for: Option<Duration> = None;
    let mut handshake_timeout = perigee_client::DEFAULT_HANDSHAKE_TIMEOUT;
    let mut host_key: Option<[u8; NODE_KEY_LEN]> = None;
    let mut config_dir: Option<PathBuf> = None;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => return Ok(CliCommand::Help),
            "-V" | "--version" => return Ok(CliCommand::Version),
            "--interface" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--interface needs a value"))?;
                if val.is_empty() {
                    bail!("--interface needs a value");
                }
                interface = Some(val.clone());
                i += 2;
            }
            "--for" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--for needs a value in whole seconds"))?;
                let secs: u64 = val.parse().with_context(|| {
                    format!("invalid --for value '{val}': expected whole seconds")
                })?;
                run_for = Some(Duration::from_secs(secs));
                i += 2;
            }
            "--timeout" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--timeout needs a value in whole seconds"))?;
                let secs: u64 = val.parse().with_context(|| {
                    format!("invalid --timeout value '{val}': expected whole seconds")
                })?;
                handshake_timeout = Duration::from_secs(secs);
                i += 2;
            }
            "--host-key" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--host-key needs a value"))?;
                host_key = Some(node_key_from_hex(val).context("invalid --host-key")?);
                i += 2;
            }
            "--config-dir" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--config-dir needs a value"))?;
                if val.is_empty() {
                    bail!("--config-dir needs a value");
                }
                config_dir = Some(PathBuf::from(val));
                i += 2;
            }
            arg if arg.starts_with('-') => {
                bail!("unknown connect option '{arg}'; run perigee --help");
            }
            positional => {
                if target.is_some() {
                    bail!("unexpected extra argument '{positional}'; connect takes one target");
                }
                if positional.trim().is_empty() {
                    bail!("connect needs a target, e.g. perigee connect 192.0.2.10:4433");
                }
                target = Some(positional.to_string());
                i += 1;
            }
        }
    }

    let Some(target) = target else {
        bail!("connect needs a target, e.g. perigee connect 192.0.2.10:4433");
    };

    Ok(CliCommand::Connect(ConnectArgs {
        target,
        interface,
        run_for,
        handshake_timeout,
        host_key,
        config_dir,
    }))
}

fn parse_client_admin(args: &[String]) -> Result<ClientAdmin> {
    let mut config_dir = None;
    let mut positional: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--config-dir" => {
                let val = args
                    .get(i + 1)
                    .ok_or_else(|| anyhow!("--config-dir needs a value"))?;
                if val.is_empty() {
                    bail!("--config-dir needs a value");
                }
                config_dir = Some(PathBuf::from(val));
                i += 2;
            }
            arg if arg.starts_with('-') => bail!("unknown argument '{arg}'; run perigee --help"),
            arg => {
                positional.push(arg);
                i += 1;
            }
        }
    }
    match positional.as_slice() {
        ["identity"] => Ok(ClientAdmin::Identity { config_dir }),
        ["trust", "list"] => Ok(ClientAdmin::TrustList { config_dir }),
        ["trust", "add", target, key] => Ok(ClientAdmin::TrustAdd {
            config_dir,
            target: (*target).to_string(),
            key: node_key_from_hex(key)?,
        }),
        ["trust", "remove", target] => Ok(ClientAdmin::TrustRemove {
            config_dir,
            target: (*target).to_string(),
        }),
        _ => bail!(
            "usage: perigee identity | trust add <HOST[:PORT]> <KEY> | trust list | trust remove <HOST[:PORT]>"
        ),
    }
}

/// The client config directory: `config_dir`, else [`default_config_dir`].
pub fn client_config_dir(config_dir: &Option<PathBuf>) -> Result<PathBuf> {
    config_dir
        .clone()
        .or_else(default_config_dir)
        .ok_or_else(|| {
            anyhow!("no Perigee config directory: set PERIGEE_CONFIG_DIR or pass --config-dir")
        })
}

/// Builds the mutual-TLS credentials for `perigee connect`: this machine's node identity, and
/// the host key from `--host-key` or from the `known_hosts` pin saved for `args.target`.
pub fn client_auth_for(args: &ConnectArgs) -> Result<ClientAuth> {
    perigee_client::load_client_auth(
        &client_config_dir(&args.config_dir)?,
        &args.target,
        args.host_key,
    )
}

/// Runs a client key-management subcommand and returns the text to print.
pub fn run_client_admin(admin: &ClientAdmin) -> Result<String> {
    match admin {
        ClientAdmin::Identity { config_dir } => {
            let dir = client_config_dir(config_dir)?;
            Ok(NodeIdentity::load_or_create(&dir.join(IDENTITY_FILE_NAME))?.public_key_hex())
        }
        ClientAdmin::TrustAdd {
            config_dir,
            target,
            key,
        } => {
            let pins =
                TrustedKeys::load(client_config_dir(config_dir)?.join(KNOWN_HOSTS_FILE_NAME))?;
            let target = perigee_client::canonical_target(target)?;
            pins.pin(*key, &target)?;
            Ok(format!("pinned {} for {target}", node_key_to_hex(key)))
        }
        ClientAdmin::TrustList { config_dir } => {
            let pins =
                TrustedKeys::load(client_config_dir(config_dir)?.join(KNOWN_HOSTS_FILE_NAME))?;
            let entries = pins.entries();
            if entries.is_empty() {
                return Ok("(no pinned hosts)".to_string());
            }
            Ok(entries
                .iter()
                .map(|entry| format!("{} {}", node_key_to_hex(&entry.key), entry.label))
                .collect::<Vec<_>>()
                .join("\n"))
        }
        ClientAdmin::TrustRemove { config_dir, target } => {
            let pins =
                TrustedKeys::load(client_config_dir(config_dir)?.join(KNOWN_HOSTS_FILE_NAME))?;
            let target = perigee_client::canonical_target(target)?;
            if !pins.remove_label(&target)? {
                bail!("no pinned host key for '{target}'");
            }
            Ok(format!("removed pin for {target}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_string()).collect()
    }

    #[test]
    fn test_parse_connect() {
        let base = HostOptions::default();

        let cmd = parse_cli_args(&s(&["connect", "192.0.2.10:4433"]), base.clone()).unwrap();
        assert_eq!(
            cmd,
            CliCommand::Connect(ConnectArgs {
                target: "192.0.2.10:4433".to_string(),
                interface: None,
                run_for: None,
                handshake_timeout: perigee_client::DEFAULT_HANDSHAKE_TIMEOUT,
                host_key: None,
                config_dir: None,
            })
        );

        let cmd_flags = parse_cli_args(
            &s(&[
                "connect",
                "192.0.2.10:4433",
                "--interface",
                "wg0",
                "--for",
                "3",
                "--timeout",
                "2",
            ]),
            base,
        )
        .unwrap();
        assert_eq!(
            cmd_flags,
            CliCommand::Connect(ConnectArgs {
                target: "192.0.2.10:4433".to_string(),
                interface: Some("wg0".to_string()),
                run_for: Some(Duration::from_secs(3)),
                handshake_timeout: Duration::from_secs(2),
                host_key: None,
                config_dir: None,
            })
        );
    }

    #[test]
    fn test_parse_connect_errors() {
        let base = HostOptions::default();

        let missing_err = parse_cli_args(&s(&["connect"]), base.clone()).unwrap_err();
        assert!(
            missing_err
                .to_string()
                .contains("connect needs a target, e.g. perigee connect 192.0.2.10:4433"),
            "unexpected missing target error: {missing_err}"
        );

        let for_err =
            parse_cli_args(&s(&["connect", "192.0.2.10:4433", "--for", "abc"]), base).unwrap_err();
        let for_text = format!("{for_err:#}");
        assert!(
            for_text.contains("--for"),
            "expected error naming --for, got: {for_text}"
        );
    }

    #[test]
    fn test_parse_host_passthrough() {
        let base = HostOptions::default();
        let cmd = parse_cli_args(&s(&["host", "--bind", "127.0.0.1:0"]), base.clone()).unwrap();
        assert_eq!(
            cmd,
            CliCommand::Host(HostCommand::Run(HostOptions {
                bind_addr: "127.0.0.1:0".parse().unwrap(),
                ..base
            }))
        );
    }

    #[test]
    fn test_parse_help_version_unknown() {
        let base = HostOptions::default();

        assert_eq!(parse_cli_args(&[], base.clone()).unwrap(), CliCommand::Help);
        assert_eq!(
            parse_cli_args(&s(&["-h"]), base.clone()).unwrap(),
            CliCommand::Help
        );
        assert_eq!(
            parse_cli_args(&s(&["--help"]), base.clone()).unwrap(),
            CliCommand::Help
        );
        assert_eq!(
            parse_cli_args(&s(&["-V"]), base.clone()).unwrap(),
            CliCommand::Version
        );
        assert_eq!(
            parse_cli_args(&s(&["--version"]), base.clone()).unwrap(),
            CliCommand::Version
        );

        let unknown_err = parse_cli_args(&s(&["frobnicate"]), base).unwrap_err();
        assert!(
            unknown_err
                .to_string()
                .contains("unknown command 'frobnicate'; run perigee --help"),
            "unexpected unknown command error: {unknown_err}"
        );
    }

    #[test]
    fn test_resolve_cli_command_ignores_env_outside_host() {
        let bad_env = |k: &str| (k == "PERIGEE_BIND_ADDR").then(|| "not-an-addr".to_string());
        assert_eq!(
            resolve_cli_command(&s(&["--help"]), bad_env).unwrap(),
            CliCommand::Help
        );
        assert_eq!(
            resolve_cli_command(&s(&["host", "--help"]), bad_env).unwrap(),
            CliCommand::Host(HostCommand::Help)
        );
        assert!(matches!(
            resolve_cli_command(&s(&["connect", "192.0.2.10:4433"]), bad_env).unwrap(),
            CliCommand::Connect(_)
        ));
        let err = resolve_cli_command(&s(&["host"]), bad_env).unwrap_err();
        assert!(format!("{err:#}").contains("PERIGEE_BIND_ADDR"));
    }

    #[test]
    fn test_parse_connect_host_key_and_config_dir() {
        let key_hex = "ab".repeat(NODE_KEY_LEN);
        let cmd = parse_cli_args(
            &s(&[
                "connect",
                "192.0.2.10:4433",
                "--host-key",
                &key_hex,
                "--config-dir",
                "/tmp/perigee-test",
            ]),
            HostOptions::default(),
        )
        .unwrap();
        let CliCommand::Connect(args) = cmd else {
            panic!("expected Connect, got {cmd:?}");
        };
        assert_eq!(args.host_key, Some([0xab; NODE_KEY_LEN]));
        assert_eq!(args.config_dir, Some(PathBuf::from("/tmp/perigee-test")));
        assert!(
            parse_cli_args(
                &s(&["connect", "192.0.2.10:4433", "--host-key", "nothex"]),
                HostOptions::default()
            )
            .is_err()
        );
    }

    #[test]
    fn test_parse_client_admin() {
        let base = HostOptions::default;
        let key_hex = "ab".repeat(NODE_KEY_LEN);
        assert_eq!(
            parse_cli_args(&s(&["identity"]), base()).unwrap(),
            CliCommand::Admin(ClientAdmin::Identity { config_dir: None })
        );
        assert_eq!(
            parse_cli_args(
                &s(&[
                    "trust",
                    "add",
                    "192.0.2.10:4433",
                    &key_hex,
                    "--config-dir",
                    "/tmp/perigee-test"
                ]),
                base()
            )
            .unwrap(),
            CliCommand::Admin(ClientAdmin::TrustAdd {
                config_dir: Some(PathBuf::from("/tmp/perigee-test")),
                target: "192.0.2.10:4433".to_string(),
                key: [0xab; NODE_KEY_LEN],
            })
        );
        assert_eq!(
            parse_cli_args(&s(&["trust", "list"]), base()).unwrap(),
            CliCommand::Admin(ClientAdmin::TrustList { config_dir: None })
        );
        assert_eq!(
            parse_cli_args(&s(&["trust", "remove", "192.0.2.10:4433"]), base()).unwrap(),
            CliCommand::Admin(ClientAdmin::TrustRemove {
                config_dir: None,
                target: "192.0.2.10:4433".to_string(),
            })
        );
        assert!(
            parse_cli_args(&s(&["trust", "add", "192.0.2.10:4433", "nothex"]), base()).is_err()
        );
    }

    #[test]
    fn test_client_auth_uses_known_hosts_pin() {
        let dir = std::env::temp_dir().join(format!("perigee-cli-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut args = ConnectArgs {
            target: "192.0.2.10:4433".to_string(),
            interface: None,
            run_for: None,
            handshake_timeout: perigee_client::DEFAULT_HANDSHAKE_TIMEOUT,
            host_key: None,
            config_dir: Some(dir.clone()),
        };
        let err = client_auth_for(&args).unwrap_err();
        assert!(
            format!("{err:#}").contains("perigee trust add"),
            "got: {err:#}"
        );

        let pinned = [0x66; NODE_KEY_LEN];
        run_client_admin(&ClientAdmin::TrustAdd {
            config_dir: Some(dir.clone()),
            target: "192.0.2.10:4433".to_string(),
            key: pinned,
        })
        .unwrap();
        let auth = client_auth_for(&args).unwrap();
        assert_eq!(auth.host_key, pinned);
        let printed = run_client_admin(&ClientAdmin::Identity {
            config_dir: Some(dir.clone()),
        })
        .unwrap();
        assert_eq!(auth.identity.public_key_hex(), printed);

        args.host_key = Some([0x77; NODE_KEY_LEN]);
        assert_eq!(
            client_auth_for(&args).unwrap().host_key,
            [0x77; NODE_KEY_LEN]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_run_client_admin_round_trip() {
        let dir = std::env::temp_dir().join(format!("perigee-cli-admin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config_dir = Some(dir.clone());
        let key = [0x88; NODE_KEY_LEN];
        run_client_admin(&ClientAdmin::TrustAdd {
            config_dir: config_dir.clone(),
            target: "192.0.2.10:4433".to_string(),
            key,
        })
        .unwrap();
        assert_eq!(
            run_client_admin(&ClientAdmin::TrustList {
                config_dir: config_dir.clone()
            })
            .unwrap(),
            format!("{} 192.0.2.10:4433", node_key_to_hex(&key))
        );
        run_client_admin(&ClientAdmin::TrustRemove {
            config_dir: config_dir.clone(),
            target: "192.0.2.10:4433".to_string(),
        })
        .unwrap();
        assert!(
            run_client_admin(&ClientAdmin::TrustRemove {
                config_dir: config_dir.clone(),
                target: "192.0.2.10:4433".to_string(),
            })
            .is_err()
        );
        assert_eq!(
            run_client_admin(&ClientAdmin::TrustList { config_dir }).unwrap(),
            "(no pinned hosts)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_trust_add_canonicalizes_target() {
        let dir = std::env::temp_dir().join(format!("perigee-cli-canon-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config_dir = Some(dir.clone());
        let key = [0x99; NODE_KEY_LEN];
        assert_eq!(
            run_client_admin(&ClientAdmin::TrustAdd {
                config_dir: config_dir.clone(),
                target: "Host.Example.com".to_string(),
                key,
            })
            .unwrap(),
            format!("pinned {} for host.example.com:4433", node_key_to_hex(&key))
        );
        run_client_admin(&ClientAdmin::TrustAdd {
            config_dir: config_dir.clone(),
            target: "192.0.2.10".to_string(),
            key,
        })
        .unwrap();
        let args = ConnectArgs {
            target: "host.example.com".to_string(),
            interface: None,
            run_for: None,
            handshake_timeout: perigee_client::DEFAULT_HANDSHAKE_TIMEOUT,
            host_key: None,
            config_dir: config_dir.clone(),
        };
        assert_eq!(client_auth_for(&args).unwrap().host_key, key);
        run_client_admin(&ClientAdmin::TrustRemove {
            config_dir: config_dir.clone(),
            target: "192.0.2.10:4433".to_string(),
        })
        .unwrap();
        assert_eq!(
            run_client_admin(&ClientAdmin::TrustList { config_dir }).unwrap(),
            format!("{} host.example.com:4433", node_key_to_hex(&key))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
