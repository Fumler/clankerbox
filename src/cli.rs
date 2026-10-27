//! Running an agent uses a hand-rolled split rather than clap: unknown flags must pass straight
//! through to the sandbox backend, which clap can't express. Subcommands are parsed with clap.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Parser, Subcommand, ValueHint};
use clap_complete::Shell;

/// Project-aware sandbox wrapper for AI coding agents.
///
/// Without a subcommand, runs the configured agent inside the sandbox:
///
///   cb [OPTIONS | SANDBOX_ARGS]... [-- AGENT_ARGS...]
///
/// OPTIONS (-A, -p, --nono-profile, -C, --no-project, --unsandboxed) are recognised anywhere
/// before a lone `--`; every other argument there is passed to the sandbox backend (e.g.
/// `--allow ./dir` for nono). Everything after `--` is passed to the agent.
#[derive(Debug, Parser)]
#[command(name = "cb", version, verbatim_doc_comment)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalOpts,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Args)]
pub struct GlobalOpts {
    /// Agent to use instead of the configured default.
    #[arg(short = 'A', long, global = true, value_name = "NAME")]
    pub agent: Option<String>,
    /// Apply a profile from the config (repeatable).
    #[arg(short = 'p', long = "profile", global = true, value_name = "NAME")]
    pub profiles: Vec<String>,
    /// nono profile (name or path) to use instead of the configured one.
    #[arg(long, global = true, value_name = "NAME")]
    pub nono_profile: Option<String>,
    /// Run as if started in DIR.
    #[arg(short = 'C', global = true, value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub dir: Option<PathBuf>,
    /// Ignore project config files.
    #[arg(long, global = true)]
    pub no_project: bool,
    /// Run the agent without any sandbox. For debugging.
    #[arg(long, global = true)]
    pub unsandboxed: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create a project config file in the current directory.
    Init {
        /// Create a personal `.clankerbox.local.toml` and exclude it from git.
        #[arg(long)]
        local: bool,
        /// Overwrite an existing file and skip confirmation prompts.
        #[arg(long, short)]
        force: bool,
    },
    /// Trust the project config files that apply here.
    Trust,
    /// Revoke trust for the project config files that apply here.
    Untrust,
    /// Show resolved configuration and the command that would run.
    Show {
        /// Output JSON.
        #[arg(long)]
        json: bool,
        /// Arguments as for a normal run: [SANDBOX_ARGS...] [-- AGENT_ARGS...].
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<OsString>,
    },
    /// List available agents.
    Agents,
    /// Edit the nearest project config file, then trust it.
    Edit {
        /// Edit `.clankerbox.local.toml` instead.
        #[arg(long)]
        local: bool,
    },
    /// Check that the sandbox, agents and config files are usable.
    Doctor,
    /// Print the JSON Schema for config files.
    Schema,
    /// Print shell completions.
    Completions { shell: Shell },
}

pub const SUBCOMMANDS: &[&str] = &[
    "init",
    "trust",
    "untrust",
    "show",
    "agents",
    "edit",
    "doctor",
    "schema",
    "completions",
    "help",
];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RunArgs {
    pub global: GlobalOpts,
    pub sandbox: Vec<OsString>,
    pub agent_args: Vec<OsString>,
}

pub enum Invocation {
    Run(RunArgs),
    /// `show` takes run arguments, including `--`, which clap would swallow.
    Show {
        run: RunArgs,
        json: bool,
    },
    Subcommand(Cli),
}

/// Parse full argv (including the program name).
pub fn parse(argv: Vec<OsString>) -> Result<Invocation> {
    let mut run = split_run_args(argv.iter().skip(1).cloned())?;
    let first = run.sandbox.first().and_then(|a| a.to_str());
    let wants_help = run.sandbox.iter().any(|a| a == "-h" || a == "--help");
    if first == Some("show") && !wants_help {
        run.sandbox.remove(0);
        let before = run.sandbox.len();
        run.sandbox.retain(|a| a != "--json");
        let json = run.sandbox.len() != before;
        return Ok(Invocation::Show { run, json });
    }
    let wants_clap = first.is_some_and(|a| {
        SUBCOMMANDS.contains(&a) || ["-h", "--help", "-V", "--version"].contains(&a)
    });
    if wants_clap {
        return Ok(Invocation::Subcommand(Cli::parse_from(argv)));
    }
    if let Some(word) = first.filter(|a| !a.starts_with('-')) {
        bail!(
            "unknown subcommand `{word}` (subcommands: {}; sandbox arguments start with `-`)",
            SUBCOMMANDS.join(", ")
        );
    }
    Ok(Invocation::Run(run))
}

pub fn split_run_args(args: impl IntoIterator<Item = OsString>) -> Result<RunArgs> {
    let mut run = RunArgs::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            run.agent_args.extend(args);
            break;
        }
        let text = arg.to_string_lossy();
        let (flag, inline) = match text.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(OsString::from(value))),
            _ => match text.get(..2) {
                Some(short @ ("-A" | "-C" | "-p")) if text.len() > 2 => {
                    (short, Some(OsString::from(&text[2..])))
                }
                _ => (&*text, None),
            },
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| args.next())
                .with_context(|| format!("`{flag}` requires a value"))
        };
        let utf8 = |v: OsString| {
            v.into_string()
                .map_err(|_| anyhow!("`{flag}` value is not UTF-8"))
        };
        let g = &mut run.global;
        match flag {
            "-A" | "--agent" => g.agent = Some(utf8(value()?)?),
            "-p" | "--profile" => g.profiles.push(utf8(value()?)?),
            "--nono-profile" => g.nono_profile = Some(utf8(value()?)?),
            "-C" => g.dir = Some(value()?.into()),
            "--no-project" => g.no_project = true,
            "--unsandboxed" => g.unsandboxed = true,
            _ => run.sandbox.push(arg),
        }
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(args: &[&str]) -> RunArgs {
        split_run_args(args.iter().map(OsString::from)).unwrap()
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn empty() {
        assert_eq!(split(&[]), RunArgs::default());
    }

    #[test]
    fn own_options_anywhere_before_separator() {
        let run = split(&[
            "--read",
            "x",
            "-A",
            "pi",
            "-p",
            "personal",
            "--no-project",
            "--allow",
            "./d",
            "--profile=two",
            "--nono-profile=mine",
            "--",
            "--model",
            "--",
            "-p",
        ]);
        let g = &run.global;
        assert_eq!(g.agent.as_deref(), Some("pi"));
        assert_eq!(g.profiles, ["personal", "two"]);
        assert_eq!(g.nono_profile.as_deref(), Some("mine"));
        assert!(g.no_project);
        assert_eq!(run.sandbox, os(&["--read", "x", "--allow", "./d"]));
        assert_eq!(run.agent_args, os(&["--model", "--", "-p"]));
    }

    #[test]
    fn inline_values() {
        let run = split(&["--agent=codex", "-C/tmp", "-pwork", "--", "-x"]);
        assert_eq!(run.global.agent.as_deref(), Some("codex"));
        assert_eq!(run.global.dir, Some("/tmp".into()));
        assert_eq!(run.global.profiles, ["work"]);
        assert_eq!(run.sandbox, [] as [std::ffi::OsString; 0]);
        assert_eq!(run.agent_args, os(&["-x"]));
    }

    #[test]
    fn missing_value_is_an_error() {
        assert!(split_run_args(os(&["-A"])).is_err());
        assert!(split_run_args(os(&["--read", "x", "-p"])).is_err());
    }

    #[test]
    fn bare_word_must_be_subcommand() {
        let err = parse(os(&["cb", "clude"])).err().unwrap();
        assert!(err.to_string().contains("unknown subcommand"));
        for argv in [
            &["cb", "agents"][..],
            &["cb", "-C", "x", "agents"],
            &["cb", "-p", "w", "show"],
        ] {
            assert!(
                matches!(
                    parse(os(argv)).unwrap(),
                    Invocation::Subcommand(_) | Invocation::Show { .. }
                ),
                "{argv:?}"
            );
        }
        for argv in [
            &["cb", "--", "agents"][..],
            &["cb", "--read", "x"],
            &["cb", "-p", "w"],
        ] {
            assert!(
                matches!(parse(os(argv)).unwrap(), Invocation::Run(_)),
                "{argv:?}"
            );
        }
    }

    #[test]
    fn show_keeps_agent_separator() {
        let Invocation::Show { run, json } = parse(os(&[
            "cb", "-p", "w", "show", "--json", "--read", "x", "--", "--model", "m",
        ]))
        .unwrap() else {
            panic!("expected show");
        };
        assert!(json);
        assert_eq!(run.global.profiles, ["w"]);
        assert_eq!(run.sandbox, os(&["--read", "x"]));
        assert_eq!(run.agent_args, os(&["--model", "m"]));
    }

    #[test]
    fn clap_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
