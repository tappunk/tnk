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

use crate::{config, lifecycle, ui};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

use super::types::{
    TerminalStateGuard, resolve_audit_logger, shell_escape, validate_engine_runtime,
    validate_model_name, validate_mount_path, validate_script_name,
};
use super::{ProfileSettings, SandboxBackend, SandboxEntry};

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::fs;

const SAFE_ENV_ALLOWLIST: &[&str] = &["TERM", "COLORTERM", "COLUMNS", "LINES"];

pub async fn run_limactl(
    args: Vec<String>,
    timeout_secs: u64,
    context: &str,
) -> Result<std::process::Output, color_eyre::Report> {
    Ok(tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        Command::new("limactl").args(args).output(),
    )
    .await
    .map_err(|_| color_eyre::eyre::eyre!("{}: timed out after {}s", context, timeout_secs))??)
}

fn escape_yq_value(s: &str) -> String {
    let escaped = s
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    format!("\"{}\"", escaped)
}

fn parse_memory_gib(memory: &str) -> Option<f32> {
    let trimmed = memory.trim();
    if let Some(rest) = trimmed.strip_suffix("GiB") {
        rest.trim().parse::<f32>().ok()
    } else if let Some(rest) = trimmed.strip_suffix("MiB") {
        rest.trim().parse::<f32>().ok().map(|v| v / 1024.0)
    } else {
        trimmed.parse::<f32>().ok()
    }
}

fn build_mount_set_expr(host_path: &str, guest_path: &str, writable: bool) -> String {
    let escaped_host = escape_yq_value(host_path);
    let escaped_guest = escape_yq_value(guest_path);
    format!(
        r#".mounts |= [{{"location": {}, "mountPoint": {}, "writable": {}}}]"#,
        escaped_host, escaped_guest, writable
    )
}

fn build_start_args(id: &str, project_root: &Path, settings: &ProfileSettings) -> Vec<String> {
    let mut args = vec![
        "--tty=false".into(),
        "start".into(),
        "--name".into(),
        id.to_string(),
        "--vm-type=vz".into(),
        "--network=vzNAT".into(),
        "--mount-type=virtiofs".into(),
        "--containerd=system".into(),
    ];

    let cpus = settings.cpus.unwrap_or(4);
    args.extend(["--cpus".into(), cpus.to_string()]);

    if let Some(mem_str) = &settings.memory
        && let Some(mem_gib) = parse_memory_gib(mem_str)
    {
        args.extend(["--memory".into(), format!("{}", mem_gib)]);
    }

    let host_path = project_root.display().to_string();
    args.extend([
        "--set".into(),
        build_mount_set_expr(&host_path, &settings.workspace_guest_path, true),
    ]);

    args.extend(["--set".into(), ".ssh.loadDotSSHPubKeys = false".into()]);

    args.push("template:ubuntu".into());
    args
}

async fn instance_exists(id: &str) -> bool {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Command::new("limactl")
            .args(["list", "--format", "{{.Name}}", id])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok);

    let Some(out) = output else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    String::from_utf8_lossy(&out.stdout).trim() == id
}

async fn instance_is_running(id: &str) -> bool {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Command::new("limactl")
            .args(["list", "--format", "{{.Status}}", id])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok);

    output
        .map(|out| {
            if out.status.success() {
                String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .eq_ignore_ascii_case("running")
            } else {
                false
            }
        })
        .unwrap_or(false)
}

pub struct LimaBackend;

#[async_trait::async_trait]
impl SandboxBackend for LimaBackend {
    const BINARY: &'static str = "limactl";

    async fn resolve_id() -> Result<(String, PathBuf, PathBuf), color_eyre::Report> {
        resolve_workspace_context().await
    }

    async fn start(
        profile_name: String,
        audit_log: Option<String>,
        settings: &ProfileSettings,
        _runtime_envs: &[(String, String)],
    ) -> Result<(), color_eyre::Report> {
        let (id, project_root, _) = Self::resolve_id().await?;
        let _audit = resolve_audit_logger(audit_log, &id).await?;
        let _lock =
            lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20)).await?;

        ui::log_info(&format!("target: {}", id));

        let needs_provision = profile_name != "base";

        if !instance_exists(&id).await {
            ui::log_info("creating lima instance");
            if !ui::is_quiet() {
                eprintln!("creating sandbox {}...", id);
            }

            if ui::is_verbose() {
                let status = tokio::time::timeout(
                    std::time::Duration::from_secs(300),
                    Command::new("limactl")
                        .args(build_start_args(&id, &project_root, settings))
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .status(),
                )
                .await
                .map_err(|_| {
                    color_eyre::eyre::eyre!("timed out creating lima instance '{}' after 300s", id)
                })??;

                if !status.success() {
                    return Err(color_eyre::eyre::eyre!(
                        "failed to create/start lima instance '{}'",
                        id
                    ));
                }
            } else {
                let output = run_limactl(
                    build_start_args(&id, &project_root, settings),
                    300,
                    &format!("create/start {}", id),
                )
                .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(color_eyre::eyre::eyre!(
                        "failed to create/start lima instance '{}': {}",
                        id,
                        stderr.lines().take(8).collect::<Vec<_>>().join("\n")
                    ));
                }
            }

            ui::log_info(&format!("started {}", id));
        } else if !instance_is_running(&id).await {
            if !ui::is_quiet() {
                eprintln!("starting sandbox {}...", id);
            }

            if ui::is_verbose() {
                let status = tokio::time::timeout(
                    std::time::Duration::from_secs(240),
                    Command::new("limactl")
                        .args(["--tty=false", "start", &id])
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .status(),
                )
                .await
                .map_err(|_| {
                    color_eyre::eyre::eyre!("timed out starting lima instance '{}' after 240s", id)
                })??;

                if !status.success() {
                    return Err(color_eyre::eyre::eyre!(
                        "failed to start lima instance '{}'",
                        id
                    ));
                }
            } else {
                let output = run_limactl(
                    vec!["--tty=false".into(), "start".into(), id.clone()],
                    240,
                    &format!("start {}", id),
                )
                .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(color_eyre::eyre::eyre!(
                        "failed to start lima instance '{}': {}",
                        id,
                        stderr.lines().take(8).collect::<Vec<_>>().join("\n")
                    ));
                }
            }

            ui::log_info(&format!("started {}", id));
        }

        if !instance_is_running(&id).await {
            return Err(color_eyre::eyre::eyre!(
                "lima instance '{}' not running after start",
                id
            ));
        }

        let home = std::env::var("HOME")?;

        if needs_provision {
            if !ui::is_quiet() {
                eprintln!("resolving engine model...");
            }
            let cfg = config::load().await?;
            let server_port = cfg.server_port.unwrap_or(8080);
            let engine_name = cfg.default_engine_runtime.as_deref().unwrap_or("llama");
            let (active_model, ctx_window) =
                crate::sandbox::shared::resolve_active_model_and_ctx_impl(
                    &home,
                    server_port,
                    engine_name,
                )
                .await?;

            let cache_dir = PathBuf::from(&home)
                .join(".cache/tnk")
                .join(format!("{}-profiles", id));

            ui::log_info(&format!("applying profile: {}", profile_name));
            if !ui::is_quiet() {
                eprintln!("provisioning profile '{}'...", profile_name);
            }

            run_provision_lima(
                &id,
                &profile_name,
                engine_name,
                &active_model,
                ctx_window,
                &settings.workspace_guest_path,
                server_port,
            )
            .await?;

            if !cache_dir.exists() {
                let Some(cache_parent) = cache_dir.parent() else {
                    return Err(color_eyre::eyre::eyre!("invalid profile cache path"));
                };
                fs::create_dir_all(cache_parent).await?;
                let tmp_path = cache_dir.with_extension("tmp");
                fs::write(&tmp_path, format!("{}\n", profile_name)).await?;
                fs::rename(&tmp_path, &cache_dir).await?;
            } else {
                let mut existing = fs::read_to_string(&cache_dir).await.unwrap_or_default();
                if !existing.lines().any(|l| l.trim() == profile_name) {
                    if !existing.is_empty() && !existing.ends_with('\n') {
                        existing.push('\n');
                    }
                    existing.push_str(&profile_name);
                    existing.push('\n');
                    let tmp_path = cache_dir.with_extension("tmp");
                    fs::write(&tmp_path, existing).await?;
                    fs::rename(&tmp_path, &cache_dir).await?;
                }
            }

            ui::log_info("launching workspace context");
            if !ui::is_quiet() {
                eprintln!("sandbox ready");
            }
        } else {
            let cache_dir = PathBuf::from(&home)
                .join(".cache/tnk")
                .join(format!("{}-profiles", id));
            if !cache_dir.exists() {
                let Some(cache_parent) = cache_dir.parent() else {
                    return Err(color_eyre::eyre::eyre!("invalid profile cache path"));
                };
                fs::create_dir_all(cache_parent).await?;
                let tmp_path = cache_dir.with_extension("tmp");
                fs::write(&tmp_path, "base\n").await?;
                fs::rename(&tmp_path, &cache_dir).await?;
            }
        }

        Ok(())
    }

    async fn shell(
        profile: Option<String>,
        command: Option<String>,
        no_tty: bool,
        explicit_envs: Vec<String>,
        audit_log: Option<String>,
        settings: &ProfileSettings,
        runtime_envs: &[(String, String)],
    ) -> Result<(), color_eyre::Report> {
        let use_tty = std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
            && std::io::stderr().is_terminal();
        let requires_tty = !no_tty;

        if requires_tty && !use_tty {
            return Err(color_eyre::eyre::eyre!(
                "interactive TTY is required; use --no-tty for non-interactive commands"
            ));
        }

        let parsed_envs: Vec<(String, String)> = explicit_envs
            .iter()
            .map(|entry| crate::sandbox::shared::parse_explicit_env(entry))
            .collect::<Result<Vec<_>, _>>()?;

        let (id, project_root, workdir) = Self::resolve_id().await?;
        if id == "tnk-config" {
            return Err(color_eyre::eyre::eyre!(
                "sandbox shell is only available inside a project directory"
            ));
        }

        let _lock =
            lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20)).await?;

        if !instance_exists(&id).await {
            ui::log_info("creating lima instance");
            if ui::is_verbose() {
                let status = tokio::time::timeout(
                    std::time::Duration::from_secs(300),
                    Command::new("limactl")
                        .args(build_start_args(&id, &project_root, settings))
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .status(),
                )
                .await
                .map_err(|_| {
                    color_eyre::eyre::eyre!("timed out creating lima instance '{}' after 300s", id)
                })??;

                if !status.success() {
                    return Err(color_eyre::eyre::eyre!(
                        "failed to create/start lima instance '{}'",
                        id
                    ));
                }
            } else {
                let output = run_limactl(
                    build_start_args(&id, &project_root, settings),
                    300,
                    &format!("create/start {}", id),
                )
                .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(color_eyre::eyre::eyre!(
                        "failed to create/start lima instance '{}': {}",
                        id,
                        stderr.lines().take(8).collect::<Vec<_>>().join("\n")
                    ));
                }
            }
            ui::log_info(&format!("started {}", id));
        } else if !instance_is_running(&id).await {
            if ui::is_verbose() {
                let status = tokio::time::timeout(
                    std::time::Duration::from_secs(240),
                    Command::new("limactl")
                        .args(["--tty=false", "start", &id])
                        .stdout(Stdio::inherit())
                        .stderr(Stdio::inherit())
                        .status(),
                )
                .await
                .map_err(|_| {
                    color_eyre::eyre::eyre!("timed out starting lima instance '{}' after 240s", id)
                })??;

                if !status.success() {
                    return Err(color_eyre::eyre::eyre!(
                        "failed to start lima instance '{}'",
                        id
                    ));
                }
            } else {
                let output = run_limactl(
                    vec!["--tty=false".into(), "start".into(), id.clone()],
                    240,
                    &format!("start {}", id),
                )
                .await?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(color_eyre::eyre::eyre!(
                        "failed to start lima instance '{}': {}",
                        id,
                        stderr.lines().take(8).collect::<Vec<_>>().join("\n")
                    ));
                }
            }
            ui::log_info(&format!("started {}", id));
        }

        if let Some(profile_name) = profile.as_deref()
            && profile_name != "base"
        {
            let home = std::env::var("HOME")?;
            let cfg = config::load().await?;
            let server_port = cfg.server_port.unwrap_or(8080);
            let engine_name = cfg.default_engine_runtime.as_deref().unwrap_or("llama");
            let (active_model, ctx_window) =
                crate::sandbox::shared::resolve_active_model_and_ctx_impl(
                    &home,
                    server_port,
                    engine_name,
                )
                .await?;

            let cache_dir = PathBuf::from(&home)
                .join(".cache/tnk")
                .join(format!("{}-profiles", id));

            ui::log_info(&format!("applying profile: {}", profile_name));

            run_provision_lima(
                &id,
                profile_name,
                engine_name,
                &active_model,
                ctx_window,
                &settings.workspace_guest_path,
                server_port,
            )
            .await?;

            if !cache_dir.exists() {
                let Some(cache_parent) = cache_dir.parent() else {
                    return Err(color_eyre::eyre::eyre!("invalid profile cache path"));
                };
                fs::create_dir_all(cache_parent).await?;
                let tmp_path = cache_dir.with_extension("tmp");
                fs::write(&tmp_path, format!("{}\n", profile_name)).await?;
                fs::rename(&tmp_path, &cache_dir).await?;
            }
        }

        let audit = resolve_audit_logger(audit_log, &id).await?;

        let guest_mount_root = PathBuf::from(&settings.workspace_guest_path);
        let guest_workdir = match workdir.strip_prefix(&project_root) {
            Ok(relative_workdir) => guest_mount_root.join(relative_workdir),
            Err(_) => guest_mount_root,
        };
        let guest_workdir_str = guest_workdir
            .to_str()
            .ok_or_else(|| color_eyre::eyre::eyre!("guest workdir contains invalid UTF-8"))?;

        let mut shell_parts = Vec::new();

        for key in SAFE_ENV_ALLOWLIST {
            if let Ok(value) = std::env::var(key) {
                shell_parts.push(format!("export {}={}", key, shell_escape(&value)));
            }
        }

        for (key, value) in runtime_envs {
            shell_parts.push(format!("export {}={}", key, shell_escape(value)));
        }

        for (key, value) in &parsed_envs {
            shell_parts.push(format!("export {}={}", key, shell_escape(value)));
        }

        match command {
            Some(cmd) => {
                shell_parts.push(format!("exec bash -lc {}", shell_escape(&cmd)));
            }
            None => {
                shell_parts.push("exec bash -l".to_string());
            }
        }

        let script = shell_parts.join("; ");

        if let Some(logger) = &audit {
            logger
                .write_event(serde_json::json!({
                    "event": "session_start",
                    "ts": crate::sandbox::shared::now_unix_seconds(),
                    "container_id": id,
                    "workdir": guest_workdir_str,
                    "tty": use_tty && requires_tty,
                    "requires_tty": requires_tty,
                    "runtime_env": crate::sandbox::shared::runtime_env_summary(runtime_envs),
                }))
                .await?;
            logger.write_event(serde_json::json!({
                "event": "exec_invocation",
                "ts": crate::sandbox::shared::now_unix_seconds(),
                "container_id": id,
                "argv": vec!["limactl".to_string(), "shell".to_string(), id.clone(), "--".to_string(), script.clone()],
                "tty": use_tty && requires_tty,
                "runtime_env": crate::sandbox::shared::runtime_env_summary(runtime_envs),
            })).await?;
        }

        let _terminal_state_guard = (use_tty && requires_tty).then(TerminalStateGuard::capture);

        let mut child_cmd = Command::new("limactl");
        child_cmd
            .args([
                "shell",
                "--workdir",
                guest_workdir_str,
                &id,
                "bash",
                "-lc",
                &script,
            ])
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let mut child = child_cmd.spawn()?;
        let status = child.wait().await?;

        if let Some(logger) = &audit {
            logger
                .write_event(serde_json::json!({
                    "event": "session_exit",
                    "ts": crate::sandbox::shared::now_unix_seconds(),
                    "container_id": id,
                    "exit_code": status.code(),
                }))
                .await?;
        }

        if status.success() {
            return Ok(());
        }

        Err(color_eyre::eyre::eyre!(
            "sandbox shell exited with code {}",
            status.code().unwrap_or(1)
        ))
    }

    async fn stop(names: Vec<String>, all: bool) -> Result<(), color_eyre::Report> {
        if all {
            let instances = discover_managed_lima_instances().await;
            if instances.is_empty() {
                ui::log_info("no managed lima instances found");
                return Ok(());
            }

            let _lock =
                lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20)).await?;

            for id in instances {
                stop_lima_instance_by_id(&id).await?;
            }
            return Ok(());
        }

        if !names.is_empty() {
            let mut unique = names;
            unique.sort();
            unique.dedup();

            let _lock =
                lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20)).await?;

            for id in unique {
                validate_named_lima_sandbox(&id)?;
                if !instance_exists(&id).await {
                    eprintln!("warning: lima instance '{}' does not exist", id);
                    continue;
                }
                stop_lima_instance_by_id(&id).await?;
            }
            return Ok(());
        }

        stop_lima_instance().await
    }

    async fn delete(id: &str, force: bool) -> Result<(), color_eyre::Report> {
        delete_lima_instance(id, force).await
    }

    async fn ls() -> Result<Vec<SandboxEntry>, color_eyre::Report> {
        list_lima_instances().await
    }

    async fn exists(id: &str) -> Result<bool, color_eyre::Report> {
        Ok(instance_exists(id).await)
    }

    async fn is_running(id: &str) -> Result<bool, color_eyre::Report> {
        Ok(instance_is_running(id).await)
    }

    async fn cleanup_untracked(verbose: bool) -> Result<(), color_eyre::Report> {
        let home = std::env::var("HOME")?;
        let cache_dir = PathBuf::from(home).join(".cache/tnk");

        if !cache_dir.exists() {
            if verbose {
                eprintln!(
                    "warning: sandbox cache directory is missing; skipping untracked cleanup"
                );
            }
            return Ok(());
        }

        let output = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            Command::new("limactl")
                .args(["list", "--format", "{{.Name}}"])
                .output(),
        )
        .await
        .ok()
        .and_then(Result::ok);

        let Some(output) = output else {
            if verbose {
                eprintln!("warning: limactl list timed out or failed for cleanup");
            }
            return Ok(());
        };

        if !output.status.success() {
            if verbose {
                eprintln!("warning: failed to list lima instances for cleanup");
            }
            return Ok(());
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines().filter(|l| !l.is_empty()) {
            let id = line.trim();
            if !id.starts_with("tnk-") {
                continue;
            }

            let profile_cache = cache_dir.join(format!("{}-profiles", id));
            if profile_cache.exists() {
                continue;
            }

            let is_running = instance_is_running(id).await;
            if is_running {
                if verbose {
                    eprintln!("info: skipping running untracked lima instance {}", id);
                }
                continue;
            }

            if verbose {
                eprintln!(
                    "warning: detected unlabeled lima instance {} without profile cache; skipping auto-delete for safety",
                    id
                );
            }
        }

        Ok(())
    }

    async fn provision(
        id: &str,
        profile_name: &str,
        engine_runtime: &str,
        model_name: &str,
        ctx_window: u32,
        _mount_point: &Path,
        port: u16,
        settings: &ProfileSettings,
    ) -> Result<(), color_eyre::Report> {
        run_provision_lima(
            id,
            profile_name,
            engine_runtime,
            model_name,
            ctx_window,
            &settings.workspace_guest_path,
            port,
        )
        .await
    }

    async fn resolve_gateway(_id: &str) -> Result<String, color_eyre::Report> {
        Ok("host.lima.internal".to_string())
    }

    async fn runtime_env(
        id: &str,
        port: u16,
        engine_runtime: &str,
        model_name: &str,
    ) -> Result<Vec<(String, String)>, color_eyre::Report> {
        let host_gateway = Self::resolve_gateway(id).await?;
        let inference_url = format!("http://{}:{}/v1", host_gateway, port);
        let mcp_bridge_url = format!("http://{}:18765", host_gateway);
        let searxng_url = format!("http://{}:18766", host_gateway);

        Ok(vec![
            ("TNK_INFERENCE_URL".to_string(), inference_url.clone()),
            ("TNK_OPENAI_URL".to_string(), inference_url),
            ("TNK_MCP_BRIDGE_URL".to_string(), mcp_bridge_url),
            ("TNK_SEARXNG_URL".to_string(), searxng_url),
            ("TNK_MODEL_NAME".to_string(), model_name.to_string()),
            ("TNK_ENGINE_RUNTIME".to_string(), engine_runtime.to_string()),
        ])
    }

    async fn resolve_active_model_and_ctx(
        port: u16,
        engine_runtime: &str,
    ) -> Result<(String, u32), color_eyre::Report> {
        let home = std::env::var("HOME")?;
        crate::sandbox::shared::resolve_active_model_and_ctx_impl(&home, port, engine_runtime).await
    }
}

fn sanitize_project_name(name: &str) -> Option<String> {
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
        .collect();

    if sanitized.is_empty() {
        return None;
    }

    Some(sanitized)
}

fn project_name_suffix(seed: &str) -> String {
    let hash = seed
        .as_bytes()
        .iter()
        .fold(0xcbf29ce484222325_u64, |acc, b| {
            (acc ^ u64::from(*b)).wrapping_mul(0x100000001b3)
        });
    format!("{:08x}", (hash & 0xffff_ffff) as u32)
}

pub async fn resolve_workspace_context() -> Result<(String, PathBuf, PathBuf), color_eyre::Report> {
    let current_dir = std::env::current_dir()?;
    let home = std::env::var("HOME")?;
    let canonical_current_dir = current_dir.canonicalize()?;

    let raw_workspace_root = if let Ok(v) = std::env::var("TNK_WORKSPACE_ROOT") {
        v
    } else if let Ok(cfg) = config::load().await {
        cfg.workspace_root
            .unwrap_or_else(|| format!("{}/src", home))
    } else {
        format!("{}/src", home)
    };

    let workspace_root = raw_workspace_root
        .strip_prefix("~/")
        .map(|p| format!("{}/{}", home, p))
        .unwrap_or(raw_workspace_root);

    let tnk_config_dir = PathBuf::from(&home).join(".config/tnk");
    if tnk_config_dir.exists()
        && let Ok(canonical_tnk_config_dir) = tnk_config_dir.canonicalize()
        && canonical_current_dir.starts_with(&canonical_tnk_config_dir)
    {
        return Ok(("tnk-config".to_string(), tnk_config_dir, current_dir));
    }

    let workspace_path = PathBuf::from(&workspace_root);
    let canonical_workspace_path = match workspace_path.canonicalize() {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("error: workspace root '{}' does not exist", workspace_root);
            eprintln!(
                "info: create it with 'mkdir -p {}' or set TNK_WORKSPACE_ROOT",
                workspace_root
            );
            return Err(color_eyre::eyre::eyre!(
                "workspace root '{}' does not exist",
                workspace_root
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            return Err(color_eyre::eyre::eyre!(
                "permission denied resolving workspace root '{}': {}",
                workspace_root,
                err
            ));
        }
        Err(err) => {
            return Err(color_eyre::eyre::eyre!(
                "failed to canonicalize workspace root '{}': {}",
                workspace_root,
                err
            ));
        }
    };
    let canonical_home = PathBuf::from(&home)
        .canonicalize()
        .map_err(|e| color_eyre::eyre::eyre!("failed to canonicalize home directory: {}", e))?;

    validate_workspace_root(&canonical_workspace_path, &canonical_home)?;

    if canonical_current_dir == canonical_workspace_path {
        return Err(color_eyre::eyre::eyre!(
            "navigate into a project directory first"
        ));
    }

    let relative = canonical_current_dir
        .strip_prefix(&canonical_workspace_path)
        .map_err(|_| {
            color_eyre::eyre::eyre!("current directory is outside the configured workspace root")
        })?;
    let project_component = relative
        .components()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid workspace path"))?
        .as_os_str();
    let project_name = project_component
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid project name"))?;
    let sanitized_project_name = sanitize_project_name(project_name)
        .ok_or_else(|| color_eyre::eyre::eyre!("sanitized project name is empty"))?;
    let project_token = if sanitized_project_name == project_name {
        sanitized_project_name
    } else {
        format!(
            "{}-{}",
            sanitized_project_name,
            project_name_suffix(project_name)
        )
    };

    let project_folder = format!("tnk-{}", project_token);
    let mount_point = canonical_workspace_path.join(project_component);

    Ok((project_folder, mount_point, current_dir))
}

fn validate_workspace_root(workspace: &Path, home: &Path) -> Result<(), color_eyre::Report> {
    if workspace == Path::new("/") {
        return Err(color_eyre::eyre::eyre!("workspace root cannot be '/'"));
    }
    if workspace == Path::new("/Users") {
        return Err(color_eyre::eyre::eyre!("workspace root cannot be '/Users'"));
    }
    if workspace == home {
        return Err(color_eyre::eyre::eyre!(
            "security violation: workspace root cannot be the home directory; use a dedicated subdirectory (for example, ~/code)"
        ));
    }
    if !workspace.starts_with(home) {
        return Err(color_eyre::eyre::eyre!(
            "workspace root must be inside '$HOME'"
        ));
    }
    Ok(())
}

async fn stop_lima_instance() -> Result<(), color_eyre::Report> {
    let _lock = lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20)).await?;
    let (id, _, _) = LimaBackend::resolve_id().await?;

    stop_lima_instance_by_id(&id).await
}

async fn stop_lima_instance_by_id(id: &str) -> Result<(), color_eyre::Report> {
    if !instance_exists(id).await {
        return Ok(());
    }

    if !instance_is_running(id).await {
        ui::log_info(&format!("already stopped {}", id));
        return Ok(());
    }

    let graceful = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        Command::new("limactl").args(["stop", id]).output(),
    )
    .await;

    let graceful_ok = match graceful {
        Ok(Ok(output)) => output.status.success(),
        Ok(Err(_)) | Err(_) => false,
    };

    if graceful_ok || !instance_is_running(id).await {
        ui::log_info(&format!("stopped {}", id));
        return Ok(());
    }

    eprintln!(
        "warning: graceful stop for '{}' did not succeed, retrying",
        id
    );
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    if instance_is_running(id).await {
        let force = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            Command::new("limactl")
                .args(["stop", "--force", id])
                .output(),
        )
        .await;

        match force {
            Ok(Ok(output)) if output.status.success() => {}
            Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
                if instance_is_running(id).await {
                    return Err(color_eyre::eyre::eyre!(
                        "failed to stop lima instance '{}'",
                        id
                    ));
                }
            }
        }
    }

    ui::log_info(&format!("stopped {}", id));
    Ok(())
}

async fn delete_lima_instance(id: &str, force: bool) -> Result<(), color_eyre::Report> {
    let _lock = lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20)).await?;

    if !force && !std::io::stdout().is_terminal() {
        crate::ui::exit_with(
            crate::ui::ExitCode::PermissionDenied,
            "terminal required for deletion, use --yes",
        );
    }

    if !instance_exists(id).await {
        return Ok(());
    }

    if instance_is_running(id).await {
        stop_lima_instance_by_id(id).await?;
    }

    let args = if force {
        vec!["delete".into(), "--force".into(), id.to_string()]
    } else {
        vec!["delete".into(), id.to_string()]
    };

    let output = run_limactl(args, 60, &format!("delete {}", id)).await?;

    if !output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to delete lima instance '{}'",
            id
        ));
    }

    ui::log_info(&format!("deleted {}", id));
    Ok(())
}

fn validate_named_lima_sandbox(id: &str) -> Result<(), color_eyre::Report> {
    if !id.starts_with("tnk-") {
        return Err(color_eyre::eyre::eyre!(
            "invalid lima instance name '{}': must start with 'tnk-'",
            id
        ));
    }
    if id == "tnk-services" || id == "tnk-searxng" {
        return Err(color_eyre::eyre::eyre!(
            "'{}' is a services instance, not a project sandbox",
            id
        ));
    }
    Ok(())
}

async fn discover_managed_lima_instances() -> Vec<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Command::new("limactl")
            .args(["list", "--format", "{{.Name}}"])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok);

    let stdout = match output {
        Some(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => String::new(),
    };

    let mut ids: Vec<String> = stdout
        .lines()
        .filter_map(|line| {
            let id = line.trim().to_string();
            if !id.starts_with("tnk-") {
                return None;
            }
            if id == "tnk-services" || id == "tnk-searxng" {
                return None;
            }
            Some(id)
        })
        .collect();

    ids.sort();
    ids.dedup();
    ids
}

async fn list_lima_instances() -> Result<Vec<SandboxEntry>, color_eyre::Report> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Command::new("limactl")
            .args(["list", "--format", "{{.Name}}\t{{.Status}}"])
            .output(),
    )
    .await
    .map_err(|_| color_eyre::eyre::eyre!("limactl list timed out after 15s"))??;

    if !output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to list lima instances (run 'limactl list' to check lima installation)"
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let entries: Vec<SandboxEntry> = stdout
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 2 {
                return None;
            }
            let id = parts[0].trim().to_string();
            if !id.starts_with("tnk-") {
                return None;
            }
            let status = {
                let raw = parts[1].trim();
                if raw.is_empty() {
                    "unknown".to_string()
                } else {
                    raw.to_string()
                }
            };
            Some(SandboxEntry {
                id,
                status,
                mount: "/workspace".to_string(),
            })
        })
        .collect();

    Ok(entries)
}

async fn wait_for_lima_ready(id: &str, timeout_secs: u64) -> Result<(), color_eyre::Report> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
    let mut attempts = 0u32;
    loop {
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            Command::new("limactl")
                .args(["shell", id, "--", "echo", "ready"])
                .output(),
        )
        .await
        .ok()
        .and_then(Result::ok);

        if output.as_ref().is_some_and(|o| o.status.success()) {
            ui::log_info(&format!("sandbox {} ready after {} attempts", id, attempts));
            return Ok(());
        }

        if std::time::Instant::now() >= deadline {
            return Err(color_eyre::eyre::eyre!(
                "sandbox '{}' did not become ready within {}s",
                id,
                timeout_secs
            ));
        }

        attempts += 1;
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn run_provision_lima(
    id: &str,
    script_name: &str,
    engine_runtime: &str,
    model_name: &str,
    ctx_window: u32,
    mount_path: &str,
    port: u16,
) -> Result<(), color_eyre::Report> {
    validate_script_name(script_name)?;
    validate_engine_runtime(engine_runtime)?;
    validate_model_name(model_name)?;
    validate_mount_path(mount_path)?;

    let home = std::env::var("HOME")?;
    let host_script = PathBuf::from(&home).join(format!(
        ".config/tnk/sandbox.d/provision.d/{}.sh",
        script_name
    ));

    if !host_script.exists() {
        return Err(color_eyre::eyre::eyre!(
            "provision script not found: {:?}",
            host_script
        ));
    }

    let host_lib_dir = host_script
        .parent()
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid provision script path"))?
        .join("lib");
    let has_lib = host_lib_dir.is_dir();
    if !has_lib {
        ui::log_warn(&format!(
            "provision library directory not found at {:?}; the provision script may fail if it \
             sources files from lib/ (run `tnk init --force` to install provision assets)",
            host_lib_dir
        ));
    }

    let guest_provision_dir = format!("/tmp/tnk-provision-{}", script_name);
    let guest_script_path = format!("{}/{}.sh", guest_provision_dir, script_name);

    let host_gateway = LimaBackend::resolve_gateway(id).await?;

    ui::log_info(&format!("provisioning: {}", script_name));

    wait_for_lima_ready(id, 120).await?;

    let mkdir_output = run_limactl(
        vec![
            "shell".into(),
            id.to_string(),
            "--".into(),
            "mkdir".into(),
            "-p".into(),
            guest_provision_dir.clone(),
        ],
        30,
        &format!("prepare provision dir {}", script_name),
    )
    .await?;
    if !mkdir_output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to prepare guest provision directory for '{}'",
            script_name
        ));
    }

    let script_copy_output = run_limactl(
        vec![
            "copy".into(),
            host_script.display().to_string(),
            format!("{}:{}", id, guest_script_path),
        ],
        30,
        &format!("copy script {}", script_name),
    )
    .await?;
    if !script_copy_output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to copy provision script into lima guest for '{}'",
            script_name
        ));
    }

    if has_lib {
        let guest_lib_dir = format!("{}/lib", guest_provision_dir);

        let lib_copy_output = run_limactl(
            vec![
                "copy".into(),
                "--recursive".into(),
                host_lib_dir.display().to_string(),
                format!("{}:{}", id, guest_lib_dir),
            ],
            60,
            &format!("copy lib {}", script_name),
        )
        .await?;
        if !lib_copy_output.status.success() {
            crate::ui::log_warn(
                "limactl copy --recursive failed for provision library, falling back to tar",
            );
            let tar_copy = Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "cd {} && tar cf - . | limactl shell {} tar xf - -C {}",
                    shell_escape(&host_lib_dir.display().to_string()),
                    id,
                    guest_lib_dir
                ))
                .output()
                .await?;
            if !tar_copy.status.success() {
                return Err(color_eyre::eyre::eyre!(
                    "failed to copy provision library into lima guest for '{}' (both copy --recursive and tar fallback failed)",
                    script_name
                ));
            }
        }
    }

    let openai_url = shell_escape(&format!("http://{}:{}/v1", host_gateway, port));
    let mcp_bridge_url = shell_escape(&format!("http://{}:18765", host_gateway));
    let searxng_url = shell_escape(&format!("http://{}:18766", host_gateway));
    let provision_cmd = format!(
        "set -eu -o pipefail\n{}\nbash {}",
        [
            format!("export TNK_OPENAI_URL={}", openai_url),
            format!("export TNK_INFERENCE_URL={}", openai_url),
            format!("export TNK_MCP_BRIDGE_URL={}", mcp_bridge_url),
            format!("export TNK_SEARXNG_URL={}", searxng_url),
            format!("export TNK_MODEL_NAME={}", shell_escape(model_name)),
            format!("export TNK_CTX_WINDOW={}", ctx_window),
            format!("export TNK_WORKSPACE_MOUNT={}", shell_escape(mount_path)),
            format!("export TNK_ENGINE_RUNTIME={}", shell_escape(engine_runtime)),
            format!("export TNK_SPECS_REV={}", shell_escape(&get_specs_rev())),
        ]
        .join("\n"),
        shell_escape(&guest_script_path),
    );

    let mut cmd = Command::new("limactl");
    cmd.args(["shell", id, "--", "bash", "-lc"])
        .arg(provision_cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        color_eyre::eyre::eyre!("provision spawn failed for '{}': {}", script_name, e)
    })?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");

    let stdout_handle = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stdout);
        let mut buf = vec![0u8; 8192];
        let mut actual_stdout = tokio::io::stdout();
        loop {
            match reader.read_buf(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let _ = actual_stdout.write_all(&buf[..n]).await;
                    let _ = actual_stdout.flush().await;
                }
                Err(_) => break,
            }
        }
    });

    let stderr_handle = tokio::spawn(async move {
        let mut reader = tokio::io::BufReader::new(stderr);
        let mut buf = vec![0u8; 8192];
        let mut actual_stderr = tokio::io::stderr();
        loop {
            match reader.read_buf(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let _ = actual_stderr.write_all(&buf[..n]).await;
                    let _ = actual_stderr.flush().await;
                }
                Err(_) => break,
            }
        }
    });

    let provision_status = tokio::time::timeout(
        std::time::Duration::from_secs(1800),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| {
        color_eyre::eyre::eyre!("provision timed out for '{}' after 1800s", script_name)
    })?;

    let _ = stdout_handle.await;
    let _ = stderr_handle.await;

    let output = provision_status.map_err(|e| {
        color_eyre::eyre::eyre!("provision execution failed for '{}': {}", script_name, e)
    })?;

    if !output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "provision failed for '{}'",
            script_name
        ));
    }

    ui::log_info(&format!("provisioned: {}", script_name));
    Ok(())
}

fn get_specs_rev() -> String {
    let Ok(home) = std::env::var("HOME") else {
        return "local".to_string();
    };
    let specs_dir = PathBuf::from(&home).join(".config/tnk");
    if specs_dir.join(".git").exists()
        && let Ok(output) = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(&specs_dir)
            .output()
        && let Ok(rev) = String::from_utf8(output.stdout)
        && let Some(rev) = rev.trim().get(..7)
    {
        return format!("git:{}", rev);
    }
    "local".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_yq_value_quotes_simple_path() {
        assert_eq!(
            escape_yq_value("/Users/user/code/project"),
            "\"/Users/user/code/project\""
        );
    }

    #[test]
    fn escape_yq_value_escapes_special_chars() {
        assert_eq!(escape_yq_value("path/with\"quote"), r#""path/with\"quote""#);
        assert_eq!(escape_yq_value("path/with$var"), r#""path/with\$var""#);
    }

    #[test]
    fn parse_memory_gib_parses_gib() {
        assert_eq!(parse_memory_gib("2GiB"), Some(2.0));
        assert_eq!(parse_memory_gib("4.5GiB"), Some(4.5));
    }

    #[test]
    fn parse_memory_gib_parses_mib() {
        assert_eq!(parse_memory_gib("2048MiB"), Some(2.0));
    }

    #[test]
    fn parse_memory_gib_parses_bare_float() {
        assert_eq!(parse_memory_gib("3"), Some(3.0));
    }

    #[test]
    fn parse_memory_gib_returns_none_for_invalid() {
        assert_eq!(parse_memory_gib("abc"), None);
        assert_eq!(parse_memory_gib("2TB"), None);
    }

    #[test]
    fn build_mount_set_expr_produces_valid_yq() {
        let expr = build_mount_set_expr("/Users/user/code/project", "/workspace", true);
        assert!(expr.starts_with(".mounts |= "));
        assert!(expr.contains("/Users/user/code/project"));
        assert!(expr.contains("/workspace"));
        assert!(expr.contains("writable"));
    }
}
