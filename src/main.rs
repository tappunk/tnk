// Copyright 2026 tappunk
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod catalog;
pub mod config;
pub mod doctor;
pub mod engine;
pub mod init;
pub mod lifecycle;
pub mod model;
pub mod sandbox;
pub mod services;
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
    about = "Zero-trust sandbox for local inference and secure AI coding agent runtimes",
    long_about = "Zero-trust sandbox for local inference and secure AI coding agent runtimes.",
    arg_required_else_help = false,
    propagate_version = true,
    trailing_var_arg = true
)]
struct Cli {
    #[arg(
        short,
        long,
        global = true,
        help = "Suppress non-error operational output"
    )]
    quiet: bool,

    #[arg(short, long, global = true, help = "Show detailed operational logs")]
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
    #[command(about = "Manage inference engine")]
    Engine {
        #[command(subcommand)]
        action: EngineCommands,
    },

    #[command(about = "Manage project sandboxes")]
    Sandbox {
        #[command(subcommand)]
        action: SandboxCommands,
    },

    #[command(about = "Manage persistent tnk services runtime")]
    Services {
        #[command(subcommand)]
        action: ServicesCommands,
    },

    #[command(about = "Start inference engine and tnk services runtime")]
    Run {
        #[arg(long, help = "Preset name to use (must match a file in provider.d/)")]
        preset: Option<String>,
        #[arg(long, help = "Inference engine runtime")]
        runtime: Option<String>,
        #[arg(short = 'n', long, help = "Preview actions without side effects")]
        dry_run: bool,
    },

    #[command(about = "Shutdown all managed components")]
    Shutdown {
        #[arg(
            long,
            value_name = "SECONDS",
            help = "Timeout per component in seconds (default: 30)"
        )]
        timeout: Option<u64>,
        #[arg(short = 'n', long, help = "Preview actions without side effects")]
        dry_run: bool,
    },

    #[command(about = "Generate shell completion scripts")]
    Completion {
        #[arg(
            value_enum,
            help = "Target shell environment for completion generation"
        )]
        shell: Shell,
    },

    #[command(about = "Initialize tnk from upstream specs")]
    Init {
        #[arg(long, help = "Custom Git URL for tnk-specs repository source override")]
        git_url: Option<String>,
        #[arg(
            long,
            help = "Force overwrite existing configurations inside ~/.config/tnk/"
        )]
        force: bool,
    },

    #[command(about = "Manage tnk config")]
    Config {
        #[command(subcommand)]
        action: ConfigCommands,
    },

    #[command(about = "Run diagnostics and health checks")]
    Doctor,

    #[command(about = "Manage pre-baked sandbox golden images")]
    Image {
        #[command(subcommand)]
        action: ImageCommands,
    },
}

#[derive(Subcommand)]
pub enum ImageCommands {
    #[command(about = "Build a golden image from a provision profile")]
    Build {
        #[arg(long, help = "Profile to pre-bake into a local image")]
        profile: String,
    },
}

#[derive(Subcommand)]
pub enum ServicesCommands {
    #[command(about = "Start tnk services runtime")]
    Start {
        #[arg(short = 'n', long, help = "Preview actions without side effects")]
        dry_run: bool,
        #[arg(long, help = "Services runtime backend (lima)")]
        runtime: Option<String>,
    },
    #[command(about = "Stop tnk services runtime")]
    Stop {
        #[arg(short = 'n', long, help = "Preview actions without side effects")]
        dry_run: bool,
        #[arg(long, help = "Services runtime backend (lima)")]
        runtime: Option<String>,
    },
    #[command(about = "Show tnk services runtime status")]
    Status {
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
        #[arg(long, help = "Services runtime backend (lima)")]
        runtime: Option<String>,
    },
    #[command(about = "Restart tnk services runtime")]
    Restart {
        #[arg(short = 'n', long, help = "Preview actions without side effects")]
        dry_run: bool,
        #[arg(long, help = "Services runtime backend (lima)")]
        runtime: Option<String>,
    },
    #[command(about = "Delete tnk services runtime")]
    Delete {
        #[arg(short, long, help = "Skip confirmation prompts")]
        yes: bool,
        #[arg(short = 'n', long, help = "Preview actions without side effects")]
        dry_run: bool,
        #[arg(long, help = "Services runtime backend (lima)")]
        runtime: Option<String>,
    },
}

#[derive(Subcommand)]
pub enum EngineCommands {
    #[command(about = "Start inference engine runtime")]
    Start {
        #[arg(long, help = "Inference engine runtime")]
        runtime: Option<String>,
        #[arg(long, help = "Preset name to load (must match a file in provider.d/)")]
        preset: Option<String>,
        #[arg(
            long,
            help = "Bind host for inference server (e.g. 127.0.0.1 or 0.0.0.0)"
        )]
        bind_host: Option<String>,
        #[arg(
            long,
            help = "Port to bind the inference engine server (default from tnk.toml or 8080)"
        )]
        engine_server_port: Option<u16>,
        #[arg(
            long,
            help = "Run in foreground (blocking mode) instead of as a background daemon"
        )]
        foreground: bool,
    },
    #[command(about = "Stop inference engine runtime")]
    Stop {
        #[arg(long, help = "Inference engine runtime")]
        runtime: Option<String>,
        #[arg(long, help = "Stop all running engines")]
        all: bool,
    },
    #[command(about = "Show engine status")]
    Status {
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
    },
    #[command(about = "List configured model profiles")]
    Presets {
        #[arg(long, help = "Inference engine runtime")]
        runtime: Option<String>,
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
        #[arg(long, help = "Only show presets with an explicit runtime field")]
        strict: bool,
    },
}

#[derive(Subcommand)]
pub enum SandboxCommands {
    #[command(about = "Start sandbox for current project")]
    Start {
        #[arg(
            long,
            help = "Profile to apply (run without --profile to list available profiles)!"
        )]
        profile: Option<String>,
        #[arg(long, help = "Write session audit logs to this NDJSON file path")]
        audit_log: Option<String>,
        #[arg(long, help = "Sandbox backend runtime (lima)")]
        runtime: Option<String>,
        #[arg(long, alias = "enter", help = "Attach interactive shell after start")]
        shell: bool,
    },
    #[command(
        about = "Execute an interactive shell or a custom command inside the project sandbox"
    )]
    Shell {
        #[arg(long, help = "Ensure this profile is applied before attaching")]
        profile: Option<String>,
        #[arg(
            short,
            long,
            help = "Execute a non-interactive command instead of opening a login shell"
        )]
        command: Option<String>,
        #[arg(long, help = "Bypass TTY requirements for non-interactive automation")]
        no_tty: bool,
        #[arg(
            short,
            long,
            action = ArgAction::Append,
            help = "Explicit environment additions in KEY=VALUE form"
        )]
        env: Vec<String>,
        #[arg(long, help = "Write session audit logs to this NDJSON file path")]
        audit_log: Option<String>,
        #[arg(long, help = "Sandbox backend runtime (lima)")]
        runtime: Option<String>,
    },
    #[command(about = "Stop active sandbox, selected sandboxes, or all sandboxes")]
    Stop {
        #[arg(long, help = "Stop all managed project sandboxes")]
        all: bool,
        #[arg(
            long,
            action = ArgAction::Append,
            help = "Stop a specific sandbox by name (repeatable)"
        )]
        name: Vec<String>,
        #[arg(long, help = "Sandbox backend runtime (lima)")]
        runtime: Option<String>,
    },
    #[command(about = "Delete active sandbox")]
    Delete {
        #[arg(short, long, help = "Skip confirmation prompt")]
        yes: bool,
        #[arg(short = 'n', long, help = "Preview actions without side effects")]
        dry_run: bool,
        #[arg(long, help = "Sandbox backend runtime (lima)")]
        runtime: Option<String>,
    },
    #[command(about = "List sandbox instances")]
    Ls {
        #[arg(short, long, value_enum, default_value_t = OutputFormat::Text)]
        output: OutputFormat,
        #[arg(short, long, help = "Output only sandbox names (one per line)")]
        quiet: bool,
        #[arg(long, help = "Sandbox backend runtime (lima)")]
        runtime: Option<String>,
    },
}

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;
    run().await
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

    if cfg.services_auto_start.unwrap_or(true) {
        if !crate::ui::is_quiet() {
            eprintln!("starting services...");
        }
        services::start(false, None).await?;
        if !crate::ui::is_quiet() {
            eprintln!("services ready");
        }
    }

    Ok(())
}

async fn resolve_runtime(
    runtime_flag: Option<String>,
    default_engine_runtime: Option<String>,
    preset: Option<&str>,
) -> Result<String, color_eyre::Report> {
    engine::resolve_runtime_for_profile(runtime_flag, default_engine_runtime, preset).await
}

async fn run() -> Result<(), color_eyre::Report> {
    let cli = Cli::parse();

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
                let engine_name = resolve_runtime(
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
                let _ = resolve_runtime(None, cfg.default_engine_runtime.clone(), None).await?;
                engine::status(output).await?
            }
            EngineCommands::Stop { runtime, all } => {
                if all {
                    engine::stop_all().await?;
                } else {
                    let cfg = config::load().await?;
                    let engine_name =
                        resolve_runtime(runtime, cfg.default_engine_runtime.clone(), None).await?;
                    engine::stop(&engine_name).await?;
                }
            }
            EngineCommands::Presets {
                runtime,
                output,
                strict,
            } => {
                let cfg = config::load().await?;
                let engine_name =
                    resolve_runtime(runtime, cfg.default_engine_runtime.clone(), None).await?;
                engine::presets_for_runtime(&engine_name, output, strict).await?
            }
        },
        Some(Commands::Sandbox { action }) => match action {
            SandboxCommands::Start {
                profile,
                audit_log,
                runtime,
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

                let resolved_runtime =
                    sandbox::resolve_runtime(runtime.clone(), cfg.default_sandbox_runtime.clone())?;

                let (container_id, _, _) = sandbox::resolve_workspace_context()?;
                let sandbox_exists = if container_id.is_empty() {
                    false
                } else {
                    sandbox::sandbox_exists_with_runtime(
                        &container_id,
                        Some(resolved_runtime.as_str().to_string()),
                    )
                    .await?
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
                let selected_runtime = runtime.clone();
                sandbox::start(selected_profile.clone(), audit_log.clone(), runtime).await?;

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

                    sandbox::shell(None, None, false, Vec::new(), audit_log, selected_runtime)
                        .await?;
                }
            }
            SandboxCommands::Shell {
                profile,
                command,
                no_tty,
                env,
                audit_log,
                runtime,
            } => sandbox::shell(profile, command, no_tty, env, audit_log, runtime).await?,
            SandboxCommands::Stop { all, name, runtime } => {
                if all && !name.is_empty() {
                    return Err(color_eyre::eyre::eyre!(
                        "--all cannot be combined with --name"
                    ));
                }
                sandbox::stop(name, all, runtime).await?
            }
            SandboxCommands::Delete {
                yes,
                dry_run,
                runtime,
            } => {
                if dry_run {
                    crate::ui::log_info("dry run, skipping sandbox deletion");
                    return Ok(());
                }
                let (container_id, _, _) = sandbox::resolve_workspace_context()?;
                if container_id.is_empty() || container_id == "tnk-config" {
                    ui::exit_with(ui::ExitCode::Usage, "must be inside a project directory");
                }
                sandbox::delete_sandbox(&container_id, yes, runtime).await?
            }
            SandboxCommands::Ls {
                output,
                quiet,
                runtime,
            } => sandbox::ls(output, quiet, runtime).await?,
        },
        Some(Commands::Services { action }) => services::run(action).await?,
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
        Some(Commands::Image { action }) => match action {
            ImageCommands::Build { profile } => sandbox::build_golden_image(profile).await?,
        },
        Some(Commands::Completion { shell }) => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "tnk", &mut std::io::stdout());
        }
    }

    Ok(())
}
