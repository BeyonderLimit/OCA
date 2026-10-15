//! OCA — Offline Creative Assistant: CLI entry point.
//!
//! Thin dispatch only (AGENTS rule 8): parse args → load config → init logging
//! → run the requested command. Business logic lands in `oca-core` in Phase 1.

mod cli;
mod commands;
mod config;
mod doctor;
mod logging;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};

use cli::{Cli, Command};

fn main() -> ExitCode {
    match run() {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::from(1)
        }
    }
}

fn run() -> anyhow::Result<u8> {
    let cli = Cli::parse();

    let cwd = std::env::current_dir().ok();
    let project_dir = config::detect_project_dir(cwd.as_deref());
    let home = config::home_dir();

    let cfg = config::Config::load(
        home.as_deref().map(config::global_config_path).as_deref(),
        project_dir
            .as_deref()
            .map(config::project_config_path)
            .as_deref(),
    )?;

    let logging = logging::init(
        cli.verbose,
        cli.log_content,
        &cfg,
        project_dir.as_deref(),
        home.as_deref(),
    );
    tracing::debug!(project_dir = ?project_dir, "resolved context");

    match cli.command {
        Some(Command::Doctor) => Ok(doctor::run(
            &cfg,
            &logging,
            project_dir.as_deref(),
            home.as_deref(),
        )),
        Some(cmd) => commands::dispatch(cmd, home.as_deref(), &cfg),
        None => {
            Cli::command().print_help()?;
            Ok(0)
        }
    }
}
