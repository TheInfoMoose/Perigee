//! Argument parsing for the `perigee` command-line tool.

use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use perigee_host::{HostCommand, HostOptions, parse_host_args};

/// Usage text for `perigee --help`.
pub const USAGE: &str = "\
Usage:
  perigee connect <HOST[:PORT]> [--interface NAME] [--for SECONDS] [--timeout SECONDS]
  perigee host [--bind ADDR] [--interface NAME] [--name NODE]
  perigee --help
  perigee --version

Subcommands:
  connect    Open a client session to <HOST[:PORT]> and print live session status
  host       Run an embedded Perigee host daemon (accepts perigee-host flags)

Connect options:
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

Note: Peer authentication is not implemented yet (scheduled for Phase 4).";

/// Parsed arguments for `perigee connect`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectArgs {
    pub target: String,
    pub interface: Option<String>,
    pub run_for: Option<Duration>,
    pub handshake_timeout: Duration,
}

/// Top-level command parsed from `perigee` CLI arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    Connect(ConnectArgs),
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
        other => bail!("unknown command '{other}'; run perigee --help"),
    }
}

fn parse_connect_args(args: &[String]) -> Result<CliCommand> {
    let mut target: Option<String> = None;
    let mut interface: Option<String> = None;
    let mut run_for: Option<Duration> = None;
    let mut handshake_timeout = perigee_client::DEFAULT_HANDSHAKE_TIMEOUT;

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
    }))
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
}
