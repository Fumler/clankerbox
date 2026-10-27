mod cli;
mod commands;
mod config;
mod context;
mod policy;
mod sandbox;
mod style;

use std::io;
use std::process::ExitCode;

use anstream::{eprintln, println};
use anyhow::Result;
use clap::CommandFactory;

use crate::cli::{Cli, Command, Invocation};
use crate::style::ERROR;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("cb: {ERROR}error:{ERROR:#} {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let Cli { global, command } = match cli::parse(std::env::args_os().collect())? {
        Invocation::Run(args) => match commands::run::run(&args)? {},
        Invocation::Show { run, json } => {
            commands::show::show(&run, json)?;
            return Ok(ExitCode::SUCCESS);
        }
        Invocation::Subcommand(cli) => cli,
    };
    match command {
        Command::Init { local, force } => commands::init::init(&global, local, force)?,
        Command::Trust => commands::trust::trust(&global)?,
        Command::Untrust => commands::trust::untrust(&global)?,
        Command::Show { .. } => unreachable!("`show` is dispatched by cli::parse"),
        Command::Agents => commands::agents::agents(&global)?,
        Command::Edit { local } => commands::edit::edit(&global, local)?,
        Command::Doctor => return commands::doctor::doctor(&global),
        Command::Schema => {
            let schema = schemars::schema_for!(config::model::ConfigFile);
            println!("{}", serde_json::to_string_pretty(&schema)?);
        }
        Command::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "cb", &mut io::stdout());
        }
    }
    Ok(ExitCode::SUCCESS)
}
