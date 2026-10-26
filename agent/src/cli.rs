//! The agent's command line. Hand-parsed: four subcommands don't need a
//! parser library, and clap would cost the size budget (design §16) more
//! than this file does.

use std::path::PathBuf;

pub const USAGE: &str = "\
usage: vercelify-agent [--config FILE] [COMMAND]

commands:
  serve                       run the HTTP API (default)
  token [--write [--force]]   generate a random token; --write stores it in the token file
  pair [options]              print a pairing code for the cockpit
      --host HOST               host name or IP the Mac should use (instead of the mesh)
      --short                   also create a one-time short code (printed instead)
      --with-code               with --short: print the full code on a second line too
      --ttl SECONDS             lifetime of the short code
      --name NAME               name of the device token minted for it
      --tailscale               pair over Tailscale (a server not migrated yet)
  tokens [list | revoke ID]   device tokens
  promote [--force]           on the standby: take over as the main server (see: vercelify promote)
  migrate [STEP]              from Coolify to the platform engine (see: vercelify migrate)
      plan [--json]             what would happen (default)
      apply [--force]           take over Coolify's apps and data, stop Coolify
      check [--wait S] [--json] after the restart: every app answers?
      rollback                  back to Coolify
      finish [--purge]          remove Coolify
      status [--json]

options:
  -c, --config FILE   config file (default: $VERCELIFY_CONFIG or /etc/vercelify/agent.toml)
  -V, --version       print the version
  -h, --help          print this help";

#[derive(Debug, Clone, PartialEq)]
pub struct Cli {
    pub config: PathBuf,
    pub cmd: Cmd,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    Serve,
    Token {
        write: bool,
        force: bool,
    },
    Pair {
        host: Option<String>,
        short: bool,
        with_code: bool,
        ttl: u64,
        name: Option<String>,
        tailscale: bool,
    },
    TokensList,
    TokensRevoke {
        id: String,
    },
    Promote {
        force: bool,
    },
    Migrate(crate::migrate::Step),
    Version,
    Help,
}

/// `--name value` or `--name=value`.
struct Args {
    items: std::collections::VecDeque<String>,
}

impl Args {
    fn next(&mut self) -> Option<String> {
        self.items.pop_front()
    }

    /// The value of the option `flag` (just taken from the list).
    fn value(&mut self, flag: &str, inline: Option<String>) -> Result<String, String> {
        inline
            .or_else(|| self.items.pop_front())
            .filter(|v| !v.is_empty())
            .ok_or_else(|| format!("{flag} needs a value"))
    }
}

fn split(arg: &str) -> (String, Option<String>) {
    match arg.split_once('=') {
        Some((k, v)) if arg.starts_with("--") => (k.to_string(), Some(v.to_string())),
        _ => (arg.to_string(), None),
    }
}

fn number<T: std::str::FromStr>(flag: &str, v: &str) -> Result<T, String> {
    v.parse().map_err(|_| format!("{flag}: `{v}` is not a number"))
}

/// Parse the arguments after the program name. `env_config` is
/// `$VERCELIFY_CONFIG`.
pub fn parse(args: impl IntoIterator<Item = String>, env_config: Option<String>) -> Result<Cli, String> {
    let mut a = Args {
        items: args.into_iter().collect(),
    };
    let mut config = PathBuf::from(env_config.unwrap_or_else(|| "/etc/vercelify/agent.toml".into()));
    let mut cmd: Option<String> = None;
    let mut rest: Vec<String> = vec![];
    // Global options may come before or after the command.
    while let Some(arg) = a.next() {
        let (k, inline) = split(&arg);
        match k.as_str() {
            "-c" | "--config" => config = PathBuf::from(a.value("--config", inline)?),
            "-V" | "--version" => {
                return Ok(Cli {
                    config,
                    cmd: Cmd::Version,
                })
            }
            "-h" | "--help" => return Ok(Cli { config, cmd: Cmd::Help }),
            _ if cmd.is_none() && !arg.starts_with('-') => cmd = Some(arg),
            _ => rest.push(arg),
        }
    }
    let mut a = Args { items: rest.into() };
    let unknown = |arg: &str| format!("unexpected `{arg}`\n\n{USAGE}");
    let cmd = match cmd.as_deref().unwrap_or("serve") {
        "serve" => match a.next() {
            None => Cmd::Serve,
            Some(x) => return Err(unknown(&x)),
        },
        "token" => {
            let (mut write, mut force) = (false, false);
            while let Some(arg) = a.next() {
                match arg.as_str() {
                    "--write" => write = true,
                    "--force" => force = true,
                    _ => return Err(unknown(&arg)),
                }
            }
            Cmd::Token { write, force }
        }
        "pair" => {
            let mut host = None;
            let (mut short, mut with_code, mut tailscale) = (false, false, false);
            let mut ttl = vercelify_core::pairing::SHORT_CODE_TTL_SECS;
            let mut name = None;
            while let Some(arg) = a.next() {
                let (k, inline) = split(&arg);
                match k.as_str() {
                    "--host" => host = Some(a.value(&k, inline)?),
                    "--ttl" => ttl = number(&k, &a.value(&k, inline)?)?,
                    "--name" => name = Some(a.value(&k, inline)?),
                    "--short" => short = true,
                    "--with-code" => with_code = true,
                    "--tailscale" => tailscale = true,
                    _ => return Err(unknown(&arg)),
                }
            }
            if with_code && !short {
                return Err("--with-code needs --short".into());
            }
            Cmd::Pair {
                host,
                short,
                with_code,
                ttl,
                name,
                tailscale,
            }
        }
        "promote" => match a.next().as_deref() {
            None => Cmd::Promote { force: false },
            Some("--force") if a.next().is_none() => Cmd::Promote { force: true },
            Some(x) => return Err(unknown(x)),
        },
        "migrate" => {
            use crate::migrate::Step;
            let step = a.next().unwrap_or_else(|| "plan".into());
            let (mut json, mut force, mut purge, mut wait) = (false, false, false, 300);
            while let Some(arg) = a.next() {
                let (k, inline) = split(&arg);
                match (k.as_str(), step.as_str()) {
                    ("--json", "plan" | "check" | "status") => json = true,
                    ("--force", "apply") => force = true,
                    ("--purge", "finish") => purge = true,
                    ("--wait", "check") => wait = number(&k, &a.value(&k, inline)?)?,
                    _ => return Err(unknown(&arg)),
                }
            }
            Cmd::Migrate(match step.as_str() {
                "plan" => Step::Plan { json },
                "apply" => Step::Apply { force },
                "check" => Step::Check { wait_secs: wait, json },
                "rollback" => Step::Rollback,
                "finish" => Step::Finish { purge },
                "status" => Step::Status { json },
                other => return Err(unknown(other)),
            })
        }
        "tokens" => match (a.next().as_deref(), a.next()) {
            (None | Some("list"), None) => Cmd::TokensList,
            (Some("revoke"), Some(id)) if a.next().is_none() => Cmd::TokensRevoke { id },
            (Some("revoke"), None) => return Err("usage: vercelify-agent tokens revoke ID".into()),
            (Some(x), _) => return Err(unknown(x)),
            (None, Some(x)) => return Err(unknown(&x)),
        },
        other => return Err(format!("unknown command `{other}`\n\n{USAGE}")),
    };
    Ok(Cli { config, cmd })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Cli, String> {
        parse(args.iter().map(|s| s.to_string()), None)
    }

    #[test]
    fn commands_and_options() {
        assert_eq!(p(&[]).unwrap().cmd, Cmd::Serve);
        assert_eq!(p(&[]).unwrap().config, PathBuf::from("/etc/vercelify/agent.toml"));
        let c = parse(["serve".to_string()], Some("/tmp/a.toml".into())).unwrap();
        assert_eq!(c.config, PathBuf::from("/tmp/a.toml"));
        // As the host scripts call it.
        let c = p(&[
            "--config",
            "/etc/x.toml",
            "pair",
            "--short",
            "--with-code",
            "--ttl",
            "3600",
        ])
        .unwrap();
        assert_eq!(c.config, PathBuf::from("/etc/x.toml"));
        match c.cmd {
            Cmd::Pair {
                short, with_code, ttl, ..
            } => assert!(short && with_code && ttl == 3600),
            other => panic!("{other:?}"),
        }
        match p(&["pair", "--host=10.77.0.1", "--name", "Mac", "--tailscale"])
            .unwrap()
            .cmd
        {
            Cmd::Pair {
                host, name, tailscale, ..
            } => {
                assert_eq!(host.as_deref(), Some("10.77.0.1"));
                assert_eq!(name.as_deref(), Some("Mac"));
                assert!(tailscale);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            p(&["token", "--write", "--force"]).unwrap().cmd,
            Cmd::Token {
                write: true,
                force: true
            }
        );
        assert_eq!(p(&["tokens"]).unwrap().cmd, Cmd::TokensList);
        assert_eq!(
            p(&["-c", "/x", "tokens", "revoke", "tok_1"]).unwrap().cmd,
            Cmd::TokensRevoke { id: "tok_1".into() }
        );
        assert_eq!(p(&["promote", "--force"]).unwrap().cmd, Cmd::Promote { force: true });
        use crate::migrate::Step;
        assert_eq!(p(&["migrate"]).unwrap().cmd, Cmd::Migrate(Step::Plan { json: false }));
        assert_eq!(
            p(&["migrate", "check", "--wait=60", "--json"]).unwrap().cmd,
            Cmd::Migrate(Step::Check {
                wait_secs: 60,
                json: true
            })
        );
        assert_eq!(
            p(&["migrate", "finish", "--purge"]).unwrap().cmd,
            Cmd::Migrate(Step::Finish { purge: true })
        );
        assert!(p(&["migrate", "apply", "--purge"]).is_err());
        assert!(p(&["migrate", "sideways"]).is_err());
        assert_eq!(p(&["--version"]).unwrap().cmd, Cmd::Version);
        assert_eq!(p(&["pair", "-h"]).unwrap().cmd, Cmd::Help);
    }

    #[test]
    fn mistakes_are_explained() {
        assert!(p(&["pair", "--with-code"]).unwrap_err().contains("needs --short"));
        assert!(p(&["pair", "--ttl", "soon"]).unwrap_err().contains("not a number"));
        assert!(p(&["pair", "--host"]).unwrap_err().contains("needs a value"));
        assert!(p(&["bogus"]).unwrap_err().contains("unknown command"));
        assert!(p(&["token", "--nope"]).unwrap_err().contains("unexpected `--nope`"));
        assert!(p(&["tokens", "revoke"]).is_err());
        assert!(p(&["--config"]).is_err());
    }
}
