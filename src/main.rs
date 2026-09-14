pub mod catalog;
pub mod config;
pub mod doctor;
pub mod init;
pub mod lifecycle;
pub mod sandbox;
pub mod shutdown;
pub mod ui;

use clap::{ArgAction, CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use serde::Serialize;

use crate::config::ConfigCommands;

#[derive(Parser)]
#[command(
    name = "tnk",
    version,
    author,
    about = "Per-project sandbox VMs for AI agent runtimes",
    long_about = "Zero-trust per-project sandbox VMs for AI agent runtimes.",
    arg_required_else_help = false,
    propagate_version = true,
    trailing_var_arg = true
)]
struct Cli {
    #[arg(short, long, global = true, help = "quiet")]
    quiet: bool,

    #[arg(short, long, global = true, help = "verbose")]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(ValueEnum, Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Text,
    Json,
    Ndjson,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "project sandboxes")]
    Sandbox {
        #[command(subcommand)]
        action: SandboxCommands,
    },

    #[command(about = "start project sandbox")]
    Run {
        #[arg(long, help = "profile")]
        profile: Option<String>,
        #[arg(long, help = "audit log file")]
        audit_log: Option<String>,
        #[arg(long, alias = "enter", help = "shell")]
        shell: bool,
        #[arg(short = 'n', long, help = "dry-run")]
        dry_run: bool,
    },

    #[command(about = "shutdown")]
    Shutdown {
        #[arg(long, value_name = "SECONDS", help = "timeout")]
        timeout: Option<u64>,
        #[arg(short = 'n', long, help = "dry-run")]
        dry_run: bool,
    },

    #[command(about = "shell completions")]
    Completion {
        #[arg(value_enum, help = "shell")]
        shell: Shell,
    },

    #[command(about = "init")]
    Init {
        #[arg(long, help = "git url")]
        git_url: Option<String>,
        #[arg(long, help = "force")]
        force: bool,
    },

    #[command(about = "config")]
    Config {
        #[command(subcommand)]
        action: ConfigCommands,
    },

    #[command(about = "diagnostics")]
    Doctor,
}

#[derive(Subcommand)]
pub enum SandboxCommands {
    #[command(about = "start")]
    Start {
        #[arg(long, help = "profile")]
        profile: Option<String>,
        #[arg(long, help = "audit log file")]
        audit_log: Option<String>,
        #[arg(long, alias = "enter", help = "shell")]
        shell: bool,
    },
    #[command(about = "shell")]
    Shell {
        #[arg(long, help = "profile")]
        profile: Option<String>,
        #[arg(short, long, help = "command")]
        command: Option<String>,
        #[arg(long, help = "no-tty")]
        no_tty: bool,
        #[arg(short, long, action = ArgAction::Append, help = "env")]
        env: Vec<String>,
        #[arg(long, help = "audit log file")]
        audit_log: Option<String>,
    },
    #[command(about = "stop")]
    Stop {
        #[arg(long, help = "all")]
        all: bool,
        #[arg(long, action = ArgAction::Append, help = "name")]
        name: Vec<String>,
    },
    #[command(about = "delete")]
    Delete {
        #[arg(short, long, help = "yes")]
        yes: bool,
        #[arg(short = 'n', long, help = "dry-run")]
        dry_run: bool,
    },
    #[command(about = "list")]
    Ls {
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
        #[arg(short, long, help = "quiet")]
        quiet: bool,
    },
}

#[tokio::main]
async fn main() {
    color_eyre::install().unwrap();
    if let Err(err) = run().await {
        let code = crate::ui::exit_code_for_error(&err);
        let msg = err.to_string();
        if code == crate::ui::ExitCode::Error {
            eprintln!("{err}");
        } else {
            eprintln!("error: {}", msg.lines().next().unwrap_or(&msg));
        }
        std::process::exit(code.as_i32());
    }
}

async fn start_project_sandbox(
    profile: Option<String>,
    audit_log: Option<String>,
    shell: bool,
) -> Result<(), color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let config_dir = std::path::PathBuf::from(&home).join(".config/tnk");
    let cfg = config::load().await?;
    let default_profile = cfg
        .default_provision_profile
        .unwrap_or_else(|| "pi".to_string());

    let profiles = catalog::list_profiles(&config_dir).await?;
    let all_profiles: Vec<String> = std::iter::once("base".to_string())
        .chain(profiles.iter().map(|p| p.name.clone()))
        .collect();

    let (sandbox_id, _, _) = sandbox::resolve_workspace_context().await?;
    let sandbox_exists = if sandbox_id.is_empty() {
        false
    } else {
        sandbox::sandbox_exists(&sandbox_id).await?
    };

    let profile_name = match profile {
        Some(p) => p,
        None if all_profiles.iter().any(|name| name == &default_profile) => default_profile,
        None => {
            if sandbox_exists {
                "base".to_string()
            } else {
                eprintln!(
                    "warning: default profile '{}' not found, using 'base'",
                    default_profile
                );
                "base".to_string()
            }
        }
    };

    sandbox::start(profile_name, audit_log.clone()).await?;

    if shell {
        use std::io::IsTerminal;
        if !std::io::stdin().is_terminal()
            || !std::io::stdout().is_terminal()
            || !std::io::stderr().is_terminal()
        {
            ui::exit_with(
                ui::ExitCode::Usage,
                "--shell requires an interactive terminal",
            );
        }

        sandbox::shell(None, None, false, Vec::new(), audit_log).await?;
    }

    Ok(())
}

async fn run() -> Result<(), color_eyre::Report> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            let exit_code = crate::ui::clap_exit_code_for_kind(&e.kind());
            if exit_code != 0 {
                eprintln!("error: {}", e);
            } else {
                eprintln!("{}", e);
            }
            std::process::exit(exit_code);
        }
    };

    if cli.quiet {
        crate::ui::set_quiet();
    } else if cli.verbose {
        crate::ui::set_verbose();
    }

    match cli.command {
        None => {
            sandbox::ls(OutputFormat::Text, false).await?;
        }
        Some(Commands::Sandbox { action }) => match action {
            SandboxCommands::Start {
                profile,
                audit_log,
                shell,
            } => start_project_sandbox(profile, audit_log, shell).await?,
            SandboxCommands::Shell {
                profile,
                command,
                no_tty,
                env,
                audit_log,
            } => sandbox::shell(profile, command, no_tty, env, audit_log).await?,
            SandboxCommands::Stop { all, name } => {
                if all && !name.is_empty() {
                    ui::exit_with(ui::ExitCode::Usage, "--all cannot be combined with --name");
                }
                sandbox::stop(name, all).await?
            }
            SandboxCommands::Delete { yes, dry_run } => {
                if dry_run {
                    crate::ui::log_info("dry run, skipping sandbox deletion");
                    return Ok(());
                }
                let (sandbox_id, _, _) = sandbox::resolve_workspace_context().await?;
                if sandbox_id.is_empty() || sandbox_id == "tnk-config" {
                    ui::exit_with(ui::ExitCode::Usage, "must be inside a project directory");
                }
                sandbox::delete_sandbox(&sandbox_id, yes).await?
            }
            SandboxCommands::Ls { output, quiet } => sandbox::ls(output, quiet).await?,
        },
        Some(Commands::Run {
            profile,
            audit_log,
            shell,
            dry_run,
        }) => {
            if dry_run {
                crate::ui::log_info("dry run, skipping sandbox start");
                return Ok(());
            }
            sandbox::cleanup_untracked_vms(crate::ui::is_verbose()).await?;
            start_project_sandbox(profile, audit_log, shell).await?
        }
        Some(Commands::Shutdown { timeout, dry_run }) => {
            shutdown::run(timeout, dry_run).await?;
        }
        Some(Commands::Init { git_url, force }) => {
            match tokio::time::timeout(
                std::time::Duration::from_secs(120),
                tokio::task::spawn_blocking(move || {
                    init::run(init::InitCommands { git_url, force })
                }),
            )
            .await
            {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(err))) => return Err(err),
                Ok(Err(join_err)) => {
                    return Err(color_eyre::eyre::eyre!("init task panicked: {}", join_err));
                }
                Err(_) => {
                    return Err(color_eyre::eyre::eyre!(
                        "init timed out after 120s; check network connectivity"
                    ));
                }
            }
        }
        Some(Commands::Config { action }) => match action {
            ConfigCommands::Init { force } => config::init_config(force)?,
            ConfigCommands::Show => {
                let cfg = config::load().await?;
                cfg.print_resolved();
            }
        },
        Some(Commands::Doctor) => {
            doctor::run().await?;
        }
        Some(Commands::Completion { shell }) => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "tnk", &mut std::io::stdout());
        }
    }

    Ok(())
}
