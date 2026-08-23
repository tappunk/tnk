pub mod catalog;
pub mod config;
pub mod doctor;
pub mod download;
pub mod engine;
pub mod init;
pub mod lifecycle;
pub mod model;
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
    about = "Local inference sandbox",
    long_about = "Zero-trust sandbox for local inference and AI agent runtimes.",
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
    #[command(about = "inference engine")]
    Engine {
        #[command(subcommand)]
        action: EngineCommands,
    },

    #[command(about = "project sandboxes")]
    Sandbox {
        #[command(subcommand)]
        action: SandboxCommands,
    },

    #[command(about = "start runtime")]
    Run {
        #[arg(long, help = "preset")]
        preset: Option<String>,
        #[arg(long, help = "runtime")]
        runtime: Option<String>,
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

    #[command(about = "download models")]
    Download {
        #[arg(help = "url")]
        url: String,

        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,

        #[arg(short = 'n', long, help = "dry-run")]
        dry_run: bool,

        #[arg(long, help = "revision")]
        revision: Option<String>,

        #[arg(long, default_value_t = 4, help = "workers")]
        workers: usize,

        #[arg(long, help = "force")]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum EngineCommands {
    #[command(about = "start")]
    Start {
        #[arg(long, help = "runtime")]
        runtime: Option<String>,
        #[arg(long, help = "preset")]
        preset: Option<String>,
        #[arg(long, help = "bind host")]
        bind_host: Option<String>,
        #[arg(long, help = "engine server port")]
        engine_server_port: Option<u16>,
        #[arg(long, help = "foreground")]
        foreground: bool,
    },
    #[command(about = "stop")]
    Stop {
        #[arg(long, help = "runtime")]
        runtime: Option<String>,
        #[arg(long, help = "all")]
        all: bool,
    },
    #[command(about = "status")]
    Status {
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    #[command(about = "list presets")]
    Presets {
        #[arg(long, help = "runtime")]
        runtime: Option<String>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
        #[arg(long, help = "strict")]
        strict: bool,
    },
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

async fn boot(preset: Option<String>, runtime: Option<String>) -> Result<(), color_eyre::Report> {
    sandbox::cleanup_untracked_vms(crate::ui::is_verbose()).await?;

    let cfg = config::load().await?;
    let engine_name = engine::resolve_runtime_for_profile(
        runtime,
        cfg.default_engine_runtime.clone(),
        preset.as_deref(),
    )
    .await?;
    let server_port = cfg.server_port.unwrap_or(8080);

    if engine::is_running().await {
        if !crate::ui::is_quiet() {
            eprintln!("engine already running");
        }
    } else {
        if !crate::ui::is_quiet() {
            eprintln!("starting engine...");
        }
        engine::start(&engine_name, preset, server_port, None, false).await?;
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
            engine::print_status().await?;
        }
        Some(Commands::Engine { action }) => match action {
            EngineCommands::Start {
                runtime,
                preset,
                bind_host,
                engine_server_port,
                foreground,
            } => {
                let cfg = config::load().await?;
                let engine_name = engine::resolve_runtime_for_profile(
                    runtime,
                    cfg.default_engine_runtime.clone(),
                    preset.as_deref(),
                )
                .await?;
                let server_port = engine_server_port.unwrap_or(cfg.server_port.unwrap_or(8080));
                engine::start(&engine_name, preset, server_port, bind_host, foreground).await?
            }
            EngineCommands::Status { output } => {
                let cfg = config::load().await?;
                let _ = engine::resolve_runtime_for_profile(
                    None,
                    cfg.default_engine_runtime.clone(),
                    None,
                )
                .await?;
                engine::status(output).await?
            }
            EngineCommands::Stop { runtime, all } => {
                if all {
                    engine::stop_all().await?;
                } else {
                    let cfg = config::load().await?;
                    let engine_name = engine::resolve_runtime_for_profile(
                        runtime,
                        cfg.default_engine_runtime.clone(),
                        None,
                    )
                    .await?;
                    engine::stop(&engine_name).await?;
                }
            }
            EngineCommands::Presets {
                runtime,
                output,
                strict,
            } => {
                let cfg = config::load().await?;
                let engine_name = engine::resolve_runtime_for_profile(
                    runtime,
                    cfg.default_engine_runtime.clone(),
                    None,
                )
                .await?;
                engine::presets_for_runtime(&engine_name, output, strict).await?
            }
        },
        Some(Commands::Sandbox { action }) => match action {
            SandboxCommands::Start {
                profile,
                audit_log,
                shell,
            } => {
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
                    None if all_profiles.iter().any(|name| name == &default_profile) => {
                        default_profile
                    }
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

                let selected_profile = profile_name;
                sandbox::start(selected_profile.clone(), audit_log.clone()).await?;

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
            }
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
            preset,
            runtime,
            dry_run,
        }) => {
            if dry_run {
                crate::ui::log_info("dry run, skipping run actions");
                return Ok(());
            }
            boot(preset, runtime).await?
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
        Some(Commands::Download {
            url,
            output,
            dry_run,
            revision,
            workers,
            force,
        }) => download::run(url, output, dry_run, revision, workers, force).await?,
        Some(Commands::Completion { shell }) => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "tnk", &mut std::io::stdout());
        }
    }

    Ok(())
}
