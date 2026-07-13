// Copyright 2026 tappunk
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use crate::{config, lifecycle, ui};

use super::container_utils;
use super::shared::load_profile_manifest;
use super::types::{
    AuditLogger, TerminalStateGuard, resolve_audit_logger, validate_engine_runtime,
    validate_env_value, validate_model_name,
};
use super::{ProfileSettings, SandboxBackend, SandboxEntry, types};

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};

const SAFE_ENV_ALLOWLIST: &[&str] = &["TERM", "COLORTERM", "COLUMNS", "LINES"];
const NATIVE_PLATFORM: &str = "linux/arm64";

fn quiet_cmd(cmd: &str) -> Command {
    let mut cmd = Command::new(cmd);
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());
    cmd
}

#[derive(Debug, Clone)]
struct MountSpec {
    host: String,
    guest: String,
    read_only: bool,
}

#[derive(Debug, Clone)]
struct ContainerProfileSettings {
    image: String,
    workspace_guest_path: String,
    mounts: Vec<MountSpec>,
    network_none: bool,
    cpus: Option<u32>,
    memory: Option<String>,
    uses_golden_image: bool,
}

#[derive(Clone, Copy)]
struct TerminalDimensions {
    rows: u16,
    cols: u16,
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

async fn container_profile_settings(
    profile_name: &str,
    project_root: &Path,
) -> Result<ContainerProfileSettings, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let mount_src = project_root
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("workspace path contains invalid UTF-8"))?
        .to_string();

    let mut settings = ContainerProfileSettings {
        image: "debian:13-slim".to_string(),
        workspace_guest_path: "/workspace".to_string(),
        mounts: Vec::new(),
        network_none: false,
        cpus: None,
        memory: None,
        uses_golden_image: false,
    };

    if let Some(manifest) = load_profile_manifest(profile_name).await? {
        if let Some(image) = manifest.image
            && !image.trim().is_empty()
        {
            settings.image = image.trim().to_string();
        }

        if let Some(resources) = manifest.resources {
            settings.cpus = resources.cpus;
            settings.memory = resources.memory;
        }

        if let Some(security) = manifest.security {
            if let Some(network) = security.network {
                let mode = network.trim().to_ascii_lowercase();
                if mode == "none" || mode == "restricted" {
                    settings.network_none = true;
                }
            }

            if let Some(workspace_mode) = security.workspace_mode {
                let mode = workspace_mode.trim().to_ascii_lowercase();
                if mode == "overlay" {
                    eprintln!(
                        "warning: workspace_mode=overlay requested; overlay semantics are not implemented yet, using direct mount"
                    );
                }
            }
        }

        if let Some(mounts) = manifest.mounts {
            for (host_key, guest_value) in mounts {
                if host_key == "workspace" {
                    if guest_value.trim().starts_with('/') {
                        settings.workspace_guest_path = guest_value.trim().to_string();
                    }
                    continue;
                }

                let host = expand_home_path(host_key.trim(), &home);
                if host.is_empty() {
                    continue;
                }

                let (guest, read_only) = parse_guest_mount_target(&guest_value)?;
                settings.mounts.push(MountSpec {
                    host,
                    guest,
                    read_only,
                });
            }
        }
    }

    settings.mounts.insert(
        0,
        MountSpec {
            host: mount_src,
            guest: settings.workspace_guest_path.clone(),
            read_only: false,
        },
    );

    let golden_tag = golden_image_tag(profile_name);
    if image_exists(&golden_tag).await {
        settings.image = golden_tag;
        settings.uses_golden_image = true;
    }

    Ok(settings)
}

fn golden_image_tag(profile_name: &str) -> String {
    format!("tnk-profile-{}:latest", profile_name)
}

async fn image_exists(image: &str) -> bool {
    quiet_cmd("container")
        .args(["image", "inspect", image])
        .status()
        .await
        .is_ok_and(|s| s.success())
}

fn expand_home_path(input: &str, home: &str) -> String {
    input
        .strip_prefix("~/")
        .map(|tail| format!("{}/{}", home, tail))
        .unwrap_or_else(|| input.to_string())
}

fn parse_guest_mount_target(value: &str) -> Result<(String, bool), color_eyre::Report> {
    let raw = value.trim();
    if raw.is_empty() {
        return Err(color_eyre::eyre::eyre!("invalid guest mount target: empty"));
    }

    let mut read_only = false;
    let mut guest = raw;
    if let Some(stripped) = raw.strip_suffix(":ro") {
        read_only = true;
        guest = stripped;
    }

    if !guest.starts_with('/') {
        return Err(color_eyre::eyre::eyre!(
            "invalid guest mount target '{}' (must be absolute)",
            value
        ));
    }

    Ok((guest.to_string(), read_only))
}

pub struct ContainerBackend;

#[async_trait::async_trait]
impl SandboxBackend for ContainerBackend {
    const BINARY: &'static str = "container";

    async fn resolve_id() -> Result<(String, PathBuf, PathBuf), color_eyre::Report> {
        tokio::task::spawn_blocking(resolve_workspace_context).await?
    }

    async fn start(
        profile_name: String,
        audit_log: Option<String>,
        _settings: &ProfileSettings,
        runtime_envs: &[(String, String)],
    ) -> Result<(), color_eyre::Report> {
        let (id, project_root, workdir) = Self::resolve_id().await?;
        let audit = resolve_audit_logger(audit_log, &id).await?;

        let settings = container_profile_settings(&profile_name, &project_root).await?;
        let needs_profile_provision = profile_name != "base" && !settings.uses_golden_image;
        let deferred_network_isolation = settings.network_none && needs_profile_provision;

        let exists = container_exists(&id).await;
        if !exists {
            let args = create_args_for_settings(&id, &settings);
            let status = quiet_cmd("container").args(&args).status().await?;
            if !status.success() {
                return Err(color_eyre::eyre::eyre!(
                    "failed to create container '{}' (run 'container system start' if the service is not running)",
                    id
                ));
            }
        }

        if !container_is_running(&id).await {
            let status = quiet_cmd("container").args(["start", &id]).status().await?;
            if !status.success() {
                return Err(color_eyre::eyre::eyre!(
                    "failed to start container '{}'",
                    id
                ));
            }
        }

        ensure_container_runtime_baseline(&id).await?;

        let guest_workdir = match workdir.strip_prefix(&project_root) {
            Ok(relative_workdir) => {
                PathBuf::from(&settings.workspace_guest_path).join(relative_workdir)
            }
            Err(_) => PathBuf::from(&settings.workspace_guest_path),
        };
        let guest_workdir_str = guest_workdir
            .to_str()
            .ok_or_else(|| color_eyre::eyre::eyre!("guest workdir contains invalid UTF-8"))?;

        let home = std::env::var("HOME")?;
        let cfg = config::load().await?;
        let server_port = cfg.server_port.unwrap_or(8080);
        let engine_name = cfg.default_engine_runtime.as_deref().unwrap_or("llama");
        let (active_model, ctx_window) =
            resolve_active_model_and_ctx(&home, server_port, engine_name).await;

        if profile_name != "base" {
            let cache_dir = PathBuf::from(&home)
                .join(".cache/tnk")
                .join(format!("{}-profiles", id));

            ui::log_info(&format!("applying profile: {}", profile_name));

            let provision_result = if needs_profile_provision {
                run_provision_container(
                    &id,
                    &profile_name,
                    engine_name,
                    &active_model,
                    ctx_window,
                    Path::new(&settings.workspace_guest_path),
                    server_port,
                )
                .await
            } else {
                eprintln!(
                    "info: using pre-baked image {} for profile {}",
                    settings.image, profile_name
                );
                Ok(())
            };

            if deferred_network_isolation {
                ui::log_info("sealing sandbox boundary -> cutting off network access");
                update_container_network_mode(&id, "none").await?;
            }

            provision_result?;

            let existing_profiles = fs::read_to_string(&cache_dir).await.unwrap_or_else(|err| {
                crate::ui::log_warn(&format!("failed to read profile cache: {err}"));
                String::new()
            });
            if !existing_profiles.lines().any(|l| l.trim() == profile_name) {
                let mut existing = existing_profiles;
                if !existing.is_empty() && !existing.ends_with('\n') {
                    existing.push('\n');
                }
                existing.push_str(&profile_name);
                existing.push('\n');
                let Some(cache_parent) = cache_dir.parent() else {
                    return Err(color_eyre::eyre::eyre!("invalid profile cache path"));
                };
                fs::create_dir_all(cache_parent).await?;
                let tmp_path = cache_dir.with_extension("tmp");
                fs::write(&tmp_path, existing).await?;
                fs::rename(&tmp_path, &cache_dir).await?;
            }

            mark_container_profile(&id, &profile_name).await;

            ui::log_info("launching workspace context");
            let target_args = match profile_name.as_str() {
                "opencode" => vec![
                    "bash",
                    "-lc",
                    "export PATH=\"$HOME/.opencode/bin:$HOME/.local/bin:$PATH\"; exec opencode",
                ],
                _ => vec!["sh"],
            };

            let requires_tty = profile_name == "opencode";
            run_container_session(
                &id,
                guest_workdir_str,
                runtime_envs,
                &target_args,
                requires_tty,
                audit.as_ref(),
            )
            .await?;

            return Ok(());
        }

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

        mark_container_profile(&id, "base").await;

        ui::log_info("container ready, launching shell");
        run_container_session(
            &id,
            guest_workdir_str,
            runtime_envs,
            &["sh"],
            false,
            audit.as_ref(),
        )
        .await
        .map_err(|_| color_eyre::eyre::eyre!("shell session exited with error"))?;

        Ok(())
    }

    async fn shell(
        profile: Option<String>,
        command: Option<String>,
        no_tty: bool,
        explicit_envs: Vec<String>,
        audit_log: Option<String>,
        _settings: &ProfileSettings,
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

        let (id, project_root, workdir) =
            tokio::task::spawn_blocking(resolve_workspace_context).await??;
        if id == "tnk-config" {
            return Err(color_eyre::eyre::eyre!(
                "sandbox shell is only available inside a project directory"
            ));
        }

        let _lock =
            lifecycle::acquire("container-lifecycle", std::time::Duration::from_secs(20)).await?;

        let settings = ensure_container_infrastructure(&id, &project_root).await?;
        let home = std::env::var("HOME")?;

        let cfg = config::load().await?;
        let server_port = cfg.server_port.unwrap_or(8080);
        let engine_name = cfg.default_engine_runtime.as_deref().unwrap_or("llama");
        let (active_model, ctx_window) =
            resolve_active_model_and_ctx(&home, server_port, engine_name).await;

        {
            if let Some(profile_name) = profile.as_deref() {
                if profile_name != "base" {
                    let profile_settings =
                        container_profile_settings(profile_name, &project_root).await?;
                    let deferred_network_isolation = profile_settings.network_none;

                    let cache_dir = PathBuf::from(&home)
                        .join(".cache/tnk")
                        .join(format!("{}-profiles", id));

                    ui::log_info(&format!("applying profile: {}", profile_name));

                    let provision_result = run_provision_container(
                        &id,
                        profile_name,
                        engine_name,
                        &active_model,
                        ctx_window,
                        Path::new(&settings.workspace_guest_path),
                        server_port,
                    )
                    .await;

                    if deferred_network_isolation {
                        ui::log_info("sealing sandbox boundary -> cutting off network access");
                        update_container_network_mode(&id, "none").await?;
                    }

                    provision_result?;

                    let existing_profiles =
                        fs::read_to_string(&cache_dir).await.unwrap_or_else(|err| {
                            crate::ui::log_warn(&format!("failed to read profile cache: {err}"));
                            String::new()
                        });
                    if !existing_profiles.lines().any(|l| l.trim() == profile_name) {
                        let mut existing = existing_profiles;
                        if !existing.is_empty() && !existing.ends_with('\n') {
                            existing.push('\n');
                        }
                        existing.push_str(profile_name);
                        existing.push('\n');
                        let Some(cache_parent) = cache_dir.parent() else {
                            return Err(color_eyre::eyre::eyre!("invalid profile cache path"));
                        };
                        fs::create_dir_all(cache_parent).await?;
                        let tmp_path = cache_dir.with_extension("tmp");
                        fs::write(&tmp_path, existing).await?;
                        fs::rename(&tmp_path, &cache_dir).await?;
                    }
                }

                mark_container_profile(&id, profile_name).await;
            } else {
                mark_container_profile(&id, "base").await;
            }
        }

        let audit = resolve_audit_logger(audit_log, &id).await?;

        let guest_workdir = match workdir.strip_prefix(&project_root) {
            Ok(relative_workdir) => {
                PathBuf::from(&settings.workspace_guest_path).join(relative_workdir)
            }
            Err(_) => PathBuf::from(&settings.workspace_guest_path),
        };
        let guest_workdir_str = guest_workdir
            .to_str()
            .ok_or_else(|| color_eyre::eyre::eyre!("guest workdir contains invalid UTF-8"))?;

        let mut args: Vec<String> = vec!["exec".to_string()];
        if use_tty && requires_tty {
            args.push("--interactive".to_string());
            args.push("--tty".to_string());
        }
        args.push("--workdir".to_string());
        args.push(guest_workdir_str.to_string());
        args.push("--user".to_string());
        args.push("tnk".to_string());

        for key in SAFE_ENV_ALLOWLIST {
            if let Ok(value) = std::env::var(key) {
                args.push("--env".to_string());
                args.push(format!("{}={}", key, value));
            }
        }

        for (key, value) in runtime_envs {
            args.push("--env".to_string());
            args.push(format!("{}={}", key, value));
        }

        for (key, value) in &parsed_envs {
            args.push("--env".to_string());
            args.push(format!("{}={}", key, value));
        }

        args.push(id.clone());

        match command {
            Some(cmd) => {
                args.push("bash".to_string());
                args.push("-lc".to_string());
                args.push(cmd);
            }
            None => {
                args.push("bash".to_string());
                args.push("-l".to_string());
            }
        }

        let _terminal_state_guard = (use_tty && requires_tty).then(TerminalStateGuard::capture);
        if let Some(logger) = &audit {
            logger.write_event(serde_json::json!({
                "event": "session_start",
                "ts": crate::sandbox::shared::now_unix_seconds(),
                "container_id": id,
                "workdir": guest_workdir_str,
                "tty": use_tty && requires_tty,
                "requires_tty": requires_tty,
                "runtime_env": crate::sandbox::shared::runtime_env_summary(runtime_envs),
            }))?;
            logger.write_event(serde_json::json!({
                "event": "exec_invocation",
                "ts": crate::sandbox::shared::now_unix_seconds(),
                "container_id": id,
                "argv": args,
                "tty": use_tty && requires_tty,
                "runtime_env": crate::sandbox::shared::runtime_env_summary(runtime_envs),
            }))?;
        }
        let mut child_cmd = Command::new("container");
        child_cmd.args(&args);
        if use_tty && requires_tty {
            child_cmd
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        } else {
            child_cmd
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
        }

        let mut child = child_cmd.spawn()?;

        let resize_task = if use_tty && requires_tty {
            let resize_id = id.clone();

            if let Some(initial_dims) = get_current_tty_dimensions() {
                resize_container_pty(&resize_id, initial_dims).await;
            }

            Some(tokio::spawn(async move {
                let Ok(mut sigwinch) = signal(SignalKind::from_raw(libc::SIGWINCH)) else {
                    return;
                };

                while sigwinch.recv().await.is_some() {
                    if let Some(dims) = get_current_tty_dimensions() {
                        resize_container_pty(&resize_id, dims).await;
                    }
                }
            }))
        } else {
            None
        };

        let status = if use_tty && requires_tty {
            child.wait().await?
        } else {
            let output = child.wait_with_output().await?;
            if !output.stdout.is_empty() {
                let mut stdout = tokio::io::stdout();
                stdout.write_all(&output.stdout).await?;
                stdout.flush().await?;
            }
            if !output.stderr.is_empty() {
                let mut stderr = tokio::io::stderr();
                stderr.write_all(&output.stderr).await?;
                stderr.flush().await?;
            }
            output.status
        };

        if let Some(logger) = &audit {
            logger.write_event(serde_json::json!({
                "event": "session_exit",
                "ts": crate::sandbox::shared::now_unix_seconds(),
                "container_id": id,
                "exit_code": status.code(),
            }))?;
        }

        if let Some(task) = resize_task {
            task.abort();
        }

        if status.success() {
            return Ok(());
        }

        return Err(color_eyre::eyre::eyre!(
            "sandbox shell exited with code {}",
            status.code().unwrap_or(1)
        ));
    }

    async fn stop(names: Vec<String>, all: bool) -> Result<(), color_eyre::Report> {
        if all {
            let sandboxes = discover_managed_project_sandboxes().await;
            if sandboxes.is_empty() {
                ui::log_info("no managed sandbox containers found");
                return Ok(());
            }

            for id in sandboxes {
                if !container_is_running(&id).await {
                    ui::log_info(&format!("already stopped {}", id));
                    continue;
                }

                let status = quiet_cmd("container").args(["stop", &id]).status().await?;
                if !status.success() {
                    return Err(color_eyre::eyre::eyre!("failed to stop container '{}'", id));
                }
                ui::log_info(&format!("stopped {}", id));
            }
            return Ok(());
        }

        if !names.is_empty() {
            let mut unique = names;
            unique.sort();
            unique.dedup();

            for id in unique {
                validate_named_sandbox(&id)?;
                if !container_exists(&id).await {
                    eprintln!("warning: sandbox '{}' does not exist", id);
                    continue;
                }
                if !container_is_running(&id).await {
                    ui::log_info(&format!("already stopped {}", id));
                    continue;
                }

                let status = quiet_cmd("container").args(["stop", &id]).status().await?;
                if !status.success() {
                    return Err(color_eyre::eyre::eyre!("failed to stop container '{}'", id));
                }
                ui::log_info(&format!("stopped {}", id));
            }
            return Ok(());
        }

        stop_container().await
    }

    async fn delete(id: &str, force: bool) -> Result<(), color_eyre::Report> {
        delete_container(id, force).await
    }

    async fn ls() -> Result<Vec<SandboxEntry>, color_eyre::Report> {
        list_containers().await
    }

    async fn exists(id: &str) -> Result<bool, color_eyre::Report> {
        Ok(container_exists(id).await)
    }

    async fn is_running(id: &str) -> Result<bool, color_eyre::Report> {
        Ok(container_is_running(id).await)
    }

    async fn cleanup_untracked(verbose: bool) -> Result<(), color_eyre::Report> {
        let _lock =
            lifecycle::acquire("container-lifecycle", std::time::Duration::from_secs(20)).await?;
        let home = std::env::var("HOME")?;
        let cache_dir = PathBuf::from(home).join(".cache/tnk");

        if !cache_dir.exists() {
            if verbose {
                eprintln!(
                    "warning: sandbox cache directory is missing; skipping untracked cleanup to avoid accidental deletion"
                );
            }
            return Ok(());
        }

        let Some(items) = container_utils::container_list_all().await else {
            if verbose {
                eprintln!("warning: failed to list containers for cleanup");
            }
            return Ok(());
        };

        for item in items {
            let Some(container_id) = item.id().map(|s| s.to_owned()) else {
                continue;
            };
            if !container_id.starts_with("tnk-")
                || container_id == "tnk-services"
                || container_id == "tnk-searxng"
            {
                continue;
            }

            let is_managed_project = item.label("tnk.managed").is_some_and(|v| v == "true")
                && item.label("tnk.owner").is_some_and(|v| v == "project");
            if !is_managed_project {
                if verbose {
                    eprintln!(
                        "info: skipping unlabeled sandbox container {}",
                        container_id
                    );
                }
                continue;
            }

            if item.has_profile_label() {
                continue;
            }

            let profile_cache = cache_dir.join(format!("{}-profiles", container_id));
            if profile_cache.exists() {
                continue;
            }

            let running = item
                .status_state()
                .map(|s| s.eq_ignore_ascii_case("running"))
                .unwrap_or(false);
            if running {
                if verbose {
                    eprintln!(
                        "info: skipping running untracked sandbox container {}",
                        container_id
                    );
                }
                continue;
            }

            if verbose {
                eprintln!(
                    "warning: detected unlabeled sandbox container {} without profile cache; skipping auto-delete for safety",
                    container_id
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
        mount_point: &Path,
        port: u16,
        _settings: &ProfileSettings,
    ) -> Result<(), color_eyre::Report> {
        run_provision_container(
            id,
            profile_name,
            engine_runtime,
            model_name,
            ctx_window,
            mount_point,
            port,
        )
        .await
    }

    async fn build_golden_image(profile_name: String) -> Result<(), color_eyre::Report> {
        build_golden_image_impl(profile_name).await
    }

    async fn resolve_gateway(id: &str) -> Result<String, color_eyre::Report> {
        resolve_container_host_gateway(id).await
    }

    async fn runtime_env(
        id: &str,
        port: u16,
        engine_runtime: &str,
        model_name: &str,
    ) -> Result<Vec<(String, String)>, color_eyre::Report> {
        runtime_env_contract(id, port, engine_runtime, model_name).await
    }

    async fn resolve_active_model_and_ctx(
        port: u16,
        engine_runtime: &str,
    ) -> Result<(String, u32), color_eyre::Report> {
        let home = std::env::var("HOME")?;
        Ok(
            crate::sandbox::shared::resolve_active_model_and_ctx_impl(&home, port, engine_runtime)
                .await,
        )
    }
}

pub fn resolve_workspace_context() -> Result<(String, PathBuf, PathBuf), color_eyre::Report> {
    let current_dir = std::env::current_dir()?;
    let home = std::env::var("HOME")?;
    let canonical_current_dir = current_dir.canonicalize()?;

    let raw_workspace_root = if let Ok(v) = std::env::var("TNK_WORKSPACE_ROOT") {
        v
    } else if let Ok(cfg) = config::load_blocking() {
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
            "security violation: workspace root cannot be the home directory; use a dedicated subdirectory (for example, ~/src)"
        ));
    }
    if !workspace.starts_with(home) {
        return Err(color_eyre::eyre::eyre!(
            "workspace root must be inside '$HOME'"
        ));
    }
    Ok(())
}

async fn discover_managed_project_sandboxes() -> Vec<String> {
    let Some(items) = container_utils::container_list_all().await else {
        return Vec::new();
    };

    let mut ids: Vec<String> = items
        .iter()
        .filter_map(|item| {
            let id = item.id()?.to_owned();
            if !id.starts_with("tnk-") || id == "tnk-services" || id == "tnk-searxng" {
                return None;
            }

            let managed = item.label("tnk.managed").is_some_and(|v| v == "true");
            let owner_project = item.label("tnk.owner").is_some_and(|v| v == "project");

            if managed && owner_project {
                Some(id)
            } else {
                None
            }
        })
        .collect();

    ids.sort();
    ids.dedup();
    ids
}

fn validate_named_sandbox(id: &str) -> Result<(), color_eyre::Report> {
    if !id.starts_with("tnk-") {
        return Err(color_eyre::eyre::eyre!(
            "invalid sandbox name '{}': must start with 'tnk-'",
            id
        ));
    }
    if id == "tnk-services" || id == "tnk-searxng" {
        return Err(color_eyre::eyre::eyre!(
            "'{}' is a services container, not a project sandbox",
            id
        ));
    }
    Ok(())
}

fn create_args_for_settings(id: &str, settings: &ContainerProfileSettings) -> Vec<String> {
    let mut args = vec![
        "create".to_string(),
        "--name".to_string(),
        id.to_string(),
        "--platform".to_string(),
        NATIVE_PLATFORM.to_string(),
        "--detach".to_string(),
        "--label".to_string(),
        "tnk.managed=true".to_string(),
        "--label".to_string(),
        "tnk.owner=project".to_string(),
        "--label".to_string(),
        "tnk.profile.base=true".to_string(),
    ];

    if settings.network_none {
        args.push("--network".to_string());
        args.push("none".to_string());
    }

    if let Some(cpus) = settings.cpus {
        args.push(format!("--cpus={}", cpus));
    }

    if let Some(memory) = &settings.memory
        && !memory.trim().is_empty()
    {
        args.push(format!("--memory={}", memory.trim()));
    }

    for mount in &settings.mounts {
        args.push("--volume".to_string());
        if mount.read_only {
            args.push(format!("{}:{}:ro", mount.host, mount.guest));
        } else {
            args.push(format!("{}:{}", mount.host, mount.guest));
        }
    }

    args.push("--workdir".to_string());
    args.push(settings.workspace_guest_path.clone());
    args.push(settings.image.clone());
    args.push("sh".to_string());
    args.push("-lc".to_string());
    args.push("while true; do sleep 3600; done".to_string());
    args
}

async fn mark_container_profile(id: &str, profile_name: &str) {
    let label = format!("tnk.profile.{}=true", profile_name);
    let result = Command::new("container")
        .args(["update", id, "--label", &label])
        .output()
        .await;

    match result {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("Plugin 'container-update' not found") {
                return;
            }
            eprintln!(
                "warning: failed to persist profile label '{}' for container '{}'",
                profile_name, id
            );
        }
        Err(_) => {
            eprintln!(
                "warning: failed to persist profile label '{}' for container '{}'",
                profile_name, id
            );
        }
    }
}

async fn container_exists(id: &str) -> bool {
    let Some(items) = container_utils::container_list_all().await else {
        return false;
    };

    items.iter().any(|item| item.id().is_some_and(|i| i == id))
}

async fn container_is_running(id: &str) -> bool {
    let Some(items) = container_utils::container_list_all().await else {
        return false;
    };

    items.iter().any(|item| {
        item.id().is_some_and(|i| i == id)
            && item
                .status_state()
                .map(|s| s.eq_ignore_ascii_case("running"))
                .unwrap_or(false)
    })
}

async fn run_container_session(
    id: &str,
    guest_workdir: &str,
    injected_envs: &[(String, String)],
    target_args: &[&str],
    requires_tty: bool,
    audit: Option<&AuditLogger>,
) -> Result<(), color_eyre::Report> {
    let use_tty = std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal();

    if requires_tty && !use_tty {
        return Err(color_eyre::eyre::eyre!(
            "interactive TTY is required for this profile; run from a local terminal"
        ));
    }

    let started_at = Instant::now();
    if let Some(logger) = audit {
        logger.write_event(serde_json::json!({
            "event": "session_start",
            "ts": crate::sandbox::shared::now_unix_seconds(),
            "container_id": id,
            "workdir": guest_workdir,
            "tty": use_tty,
            "requires_tty": requires_tty,
            "runtime_env": crate::sandbox::shared::runtime_env_summary(injected_envs),
            "target_args": target_args,
        }))?;
    }

    async fn run_once(
        id: &str,
        guest_workdir: &str,
        injected_envs: &[(String, String)],
        target_args: &[&str],
        tty: bool,
        audit: Option<&AuditLogger>,
    ) -> Result<std::process::ExitStatus, color_eyre::Report> {
        let _terminal_state_guard = tty.then(TerminalStateGuard::capture);

        let mut args: Vec<String> = vec!["exec".to_string()];
        if tty {
            args.push("--interactive".to_string());
            args.push("--tty".to_string());
        }
        args.push("--workdir".to_string());
        args.push(guest_workdir.to_string());
        args.push("--user".to_string());
        args.push("tnk".to_string());
        for (key, value) in injected_envs {
            args.push("--env".to_string());
            args.push(format!("{}={}", key, value));
        }
        args.push(id.to_string());
        args.extend(target_args.iter().map(|s| s.to_string()));

        if let Some(logger) = audit {
            logger.write_event(serde_json::json!({
                "event": "exec_invocation",
                "ts": crate::sandbox::shared::now_unix_seconds(),
                "container_id": id,
                "argv": args,
                "tty": tty,
            }))?;
        }

        let status = Command::new("container")
            .args(&args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;
        Ok(status)
    }

    let status = match run_once(
        id,
        guest_workdir,
        injected_envs,
        target_args,
        use_tty,
        audit,
    )
    .await
    {
        Ok(status) => status,
        Err(err) => {
            if use_tty && !requires_tty {
                eprintln!("warning: interactive TTY launch failed; retrying without TTY");
                run_once(id, guest_workdir, injected_envs, target_args, false, audit).await?
            } else {
                return Err(err);
            }
        }
    };

    if let Some(logger) = audit {
        logger.write_event(serde_json::json!({
            "event": "session_exit",
            "ts": crate::sandbox::shared::now_unix_seconds(),
            "container_id": id,
            "exit_code": status.code(),
            "duration_ms": started_at.elapsed().as_millis(),
        }))?;
    }

    if status.success() {
        return Ok(());
    }

    Err(color_eyre::eyre::eyre!(
        "application session exited with error"
    ))
}

fn host_uid_gid() -> (u32, u32) {
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    (uid, gid)
}

fn get_current_tty_dimensions() -> Option<TerminalDimensions> {
    let mut ws = unsafe { std::mem::zeroed::<libc::winsize>() };
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if !ok || ws.ws_row == 0 || ws.ws_col == 0 {
        return None;
    }

    Some(TerminalDimensions {
        rows: ws.ws_row,
        cols: ws.ws_col,
    })
}

async fn resize_container_pty(id: &str, dims: TerminalDimensions) {
    let _ = quiet_cmd("container")
        .args(["resize", id, &dims.rows.to_string(), &dims.cols.to_string()])
        .status()
        .await;
}

async fn sync_container_user_ids(id: &str) {
    let (uid, gid) = host_uid_gid();
    let uid_str = uid.to_string();
    let gid_str = gid.to_string();
    let script = "UID_TARGET=\"$1\"; GID_TARGET=\"$2\"; \
if id -u tnk >/dev/null 2>&1; then \
CURRENT_UID=\"$(id -u tnk)\"; CURRENT_GID=\"$(id -g tnk)\"; \
if [ \"$CURRENT_GID\" != \"$GID_TARGET\" ]; then groupmod -o -g \"$GID_TARGET\" tnk >/dev/null 2>&1 || true; fi; \
if [ \"$CURRENT_UID\" != \"$UID_TARGET\" ]; then usermod -o -u \"$UID_TARGET\" -g \"$GID_TARGET\" tnk >/dev/null 2>&1 || true; fi; \
chown -h tnk:tnk /home/tnk >/dev/null 2>&1 || true; \
fi";

    let status = quiet_cmd("container")
        .args(["exec", id, "sh", "-lc", script, "--"])
        .arg(uid_str)
        .arg(gid_str)
        .status()
        .await;

    if let Ok(exit) = status
        && !exit.success()
    {
        eprintln!(
            "warning: failed to synchronize sandbox uid/gid mapping for '{}'",
            id
        );
    }
}

async fn ensure_container_infrastructure(
    id: &str,
    project_root: &Path,
) -> Result<ContainerProfileSettings, color_eyre::Report> {
    let settings = container_profile_settings("base", project_root).await?;

    if !container_exists(id).await {
        let args = create_args_for_settings(id, &settings);
        let status = quiet_cmd("container").args(&args).status().await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to create container '{}' (run 'container system start' if the service is not running)",
                id
            ));
        }
    }

    if !container_is_running(id).await {
        let status = quiet_cmd("container").args(["start", id]).status().await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to start container '{}'",
                id
            ));
        }
    }

    ensure_container_runtime_baseline(id).await?;
    sync_container_user_ids(id).await;
    Ok(settings)
}

async fn update_container_network_mode(id: &str, mode: &str) -> Result<(), color_eyre::Report> {
    let support_check = Command::new("container")
        .args(["help", "update"])
        .output()
        .await?;

    if !support_check.status.success() {
        let stderr = String::from_utf8_lossy(&support_check.stderr).to_ascii_lowercase();
        if stderr.contains("unknown command")
            || stderr.contains("not found")
            || stderr.contains("plugin")
        {
            eprintln!(
                "warning: container CLI does not support 'update'; continuing without deferred network isolation for '{}' (mode '{}')",
                id, mode
            );
            return Ok(());
        }

        return Err(color_eyre::eyre::eyre!(
            "failed to verify container 'update' command support for '{}'",
            id
        ));
    }

    let output = Command::new("container")
        .args(["update", id, "--network", mode])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
        if stderr.contains("unknown command")
            || stderr.contains("not found")
            || stderr.contains("plugin")
        {
            eprintln!(
                "warning: container update plugin unavailable; continuing without deferred network isolation for '{}' (mode '{}')",
                id, mode
            );
            return Ok(());
        }

        return Err(color_eyre::eyre::eyre!(
            "failed to set network mode '{}' for container '{}'",
            mode,
            id
        ));
    }

    Ok(())
}

async fn discover_gateway_from_container(id: &str) -> Option<String> {
    let output = Command::new("container")
        .args([
            "exec",
            id,
            "sh",
            "-lc",
            "ip route show default 2>/dev/null || route -n get default 2>/dev/null || true",
        ])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    container_utils::parse_gateway_from_route_output(&stdout)
}

async fn resolve_container_host_gateway(id: &str) -> Result<String, color_eyre::Report> {
    let cfg = config::load().await?;
    let host = if let Some(configured) = cfg.container_host_gateway {
        configured.trim().to_string()
    } else if let Ok(env_host) = std::env::var("TNK_CONTAINER_HOST_GATEWAY") {
        env_host.trim().to_string()
    } else if let Some(discovered) = container_utils::discover_container_gateway().await {
        discovered
    } else {
        discover_gateway_from_container(id)
            .await
            .ok_or_else(|| {
                color_eyre::eyre::eyre!(
                    "could not determine container host gateway; set TNK_CONTAINER_HOST_GATEWAY or container_host_gateway in config"
                )
            })?
    };

    if host.is_empty() {
        return Err(color_eyre::eyre::eyre!(
            "container host gateway resolved to an empty value"
        ));
    }

    Ok(host)
}

async fn backend_openai_url(id: &str, port: u16) -> Result<String, color_eyre::Report> {
    let host = resolve_container_host_gateway(id).await?;
    Ok(format!("http://{}:{}/v1", host, port))
}

async fn runtime_env_contract(
    id: &str,
    port: u16,
    engine_runtime: &str,
    model_name: &str,
) -> Result<Vec<(String, String)>, color_eyre::Report> {
    let host_gateway = resolve_container_host_gateway(id).await?;
    let inference_url = backend_openai_url(id, port).await?;
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

async fn resolve_active_model_and_ctx(home: &str, port: u16, engine_name: &str) -> (String, u32) {
    crate::sandbox::shared::resolve_active_model_and_ctx_impl(home, port, engine_name).await
}

async fn run_provision_container(
    id: &str,
    script_name: &str,
    engine_runtime: &str,
    model_name: &str,
    ctx_window: u32,
    mount_point: &Path,
    port: u16,
) -> Result<(), color_eyre::Report> {
    types::validate_script_name(script_name)?;
    validate_engine_runtime(engine_runtime)?;

    let home = std::env::var("HOME")?;
    let host_script = PathBuf::from(&home).join(format!(
        ".config/tnk/sandbox.d/container/provision.d/{}.sh",
        script_name
    ));

    if !host_script.exists() {
        return Err(color_eyre::eyre::eyre!(
            "provision script not found: {:?}",
            host_script
        ));
    }

    let mount_str = mount_point
        .to_str()
        .ok_or_else(|| color_eyre::eyre::eyre!("workspace mount path contains invalid UTF-8"))?;

    types::validate_model_name(model_name)?;

    ui::log_info(&format!("provisioning: {}", script_name));
    let runtime_envs = runtime_env_contract(id, port, engine_runtime, model_name).await?;
    let mut env_map = std::collections::HashMap::new();
    for (k, v) in &runtime_envs {
        env_map.insert(k.as_str(), v.as_str());
    }

    let openai_url = env_map
        .get("TNK_OPENAI_URL")
        .copied()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing TNK_OPENAI_URL env value"))?;
    let host_gateway = resolve_container_host_gateway(id).await?;
    let searxng_url = env_map
        .get("TNK_SEARXNG_URL")
        .copied()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing TNK_SEARXNG_URL env value"))?;
    let inference_url = env_map
        .get("TNK_INFERENCE_URL")
        .copied()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing TNK_INFERENCE_URL env value"))?;
    let mcp_bridge_url = env_map
        .get("TNK_MCP_BRIDGE_URL")
        .copied()
        .ok_or_else(|| color_eyre::eyre::eyre!("missing TNK_MCP_BRIDGE_URL env value"))?;

    validate_env_value(openai_url, "TNK_OPENAI_URL")?;
    validate_env_value(inference_url, "TNK_INFERENCE_URL")?;
    validate_env_value(mcp_bridge_url, "TNK_MCP_BRIDGE_URL")?;
    validate_env_value(&host_gateway, "TNK_CONTAINER_HOST_GATEWAY")?;
    validate_env_value(searxng_url, "TNK_SEARXNG_URL")?;
    validate_env_value(model_name, "TNK_MODEL_NAME")?;
    validate_model_name(model_name)?;
    validate_env_value(mount_str, "TNK_WORKSPACE_MOUNT")?;
    validate_env_value(engine_runtime, "TNK_ENGINE_RUNTIME")?;

    let host_lib_dir = host_script
        .parent()
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid provision script path"))?
        .join("lib");
    let has_lib = host_lib_dir.is_dir();
    if !has_lib {
        crate::ui::log_warn(&format!(
            "provision library directory not found at {:?}; the provision script may fail if it \
             sources files from lib/ (run `tnk init --force` to install provision assets)",
            host_lib_dir
        ));
    }

    let specs_rev = crate::sandbox::shared::compute_specs_revision_hash(
        &host_script,
        has_lib.then_some(&host_lib_dir),
    )
    .await?;
    let guest_provision_dir = format!("/tmp/tnk-provision-{}", script_name);
    let guest_script_path = format!("{}/{}.sh", guest_provision_dir, script_name);

    let mkdir_status = if has_lib {
        let guest_lib_dir = format!("{}/lib", guest_provision_dir);
        Command::new("container")
            .args(["exec", id, "mkdir", "-p", &guest_lib_dir])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?
    } else {
        std::process::Command::new("true")
            .status()
            .unwrap_or_else(|e| {
                eprintln!("warning: failed to create provision dir: {}", e);
                std::process::ExitStatus::default()
            })
    };
    if !mkdir_status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to prepare guest provision directory"
        ));
    }

    let copy_script_status = Command::new("container")
        .args([
            "copy",
            host_script.to_str().ok_or_else(|| {
                color_eyre::eyre::eyre!("provision script path contains invalid UTF-8")
            })?,
            &format!("{}:{}", id, guest_script_path),
        ])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;
    if !copy_script_status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to copy provision script into container"
        ));
    }

    if has_lib {
        let guest_lib_dir = format!("{}/lib", guest_provision_dir);
        let copy_lib_status = Command::new("container")
            .args([
                "copy",
                host_lib_dir.to_str().ok_or_else(|| {
                    color_eyre::eyre::eyre!("provision lib path contains invalid UTF-8")
                })?,
                &format!("{}:{}", id, guest_lib_dir),
            ])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;
        if !copy_lib_status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to copy provision library into container"
            ));
        }
    }

    let mut child = Command::new("container")
        .args(["exec", "--workdir", "/tmp"])
        .arg("--user")
        .arg("tnk")
        .arg("--env")
        .arg(format!("TNK_OPENAI_URL={}", openai_url))
        .arg("--env")
        .arg(format!("TNK_INFERENCE_URL={}", inference_url))
        .arg("--env")
        .arg(format!("TNK_MCP_BRIDGE_URL={}", mcp_bridge_url))
        .arg("--env")
        .arg(format!("TNK_MODEL_NAME={}", model_name))
        .arg("--env")
        .arg(format!("TNK_CTX_WINDOW={}", ctx_window))
        .arg("--env")
        .arg(format!("TNK_WORKSPACE_MOUNT={}", mount_str))
        .arg("--env")
        .arg(format!("TNK_SPECS_REV={}", specs_rev))
        .arg("--env")
        .arg(format!("TNK_CONTAINER_HOST_GATEWAY={}", host_gateway))
        .arg("--env")
        .arg(format!("TNK_SEARXNG_URL={}", searxng_url))
        .arg("--env")
        .arg(format!("TNK_ENGINE_RUNTIME={}", engine_runtime))
        .arg(id)
        .arg("bash")
        .arg(&guest_script_path)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()?;

    let status =
        match tokio::time::timeout(std::time::Duration::from_secs(1800), child.wait()).await {
            Ok(wait_result) => wait_result?,
            Err(_) => {
                let _ = child.kill().await;
                return Err(color_eyre::eyre::eyre!(
                    "provision timed out for '{}' after 1800s",
                    script_name
                ));
            }
        };
    if !status.success() {
        return Err(color_eyre::eyre::eyre!("provision failed: {}", script_name));
    }

    ui::log_info(&format!("provisioned: {}", script_name));
    Ok(())
}

async fn ensure_container_runtime_baseline(id: &str) -> Result<(), color_eyre::Report> {
    let marker = "/var/lib/tnk/container-baseline-v2";
    let has_marker = Command::new("container")
        .args(["exec", id, "sh", "-lc", &format!("test -f {}", marker)])
        .status()
        .await?;
    if has_marker.success() {
        return Ok(());
    }

    let install_status = Command::new("container")
        .args([
            "exec", id,
            "sh", "-lc",
            "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends bash curl ca-certificates sudo git nodejs npm && if ! id -u tnk >/dev/null 2>&1; then useradd -m -s /bin/bash tnk; fi && usermod -aG sudo tnk && install -d -m 755 /etc/sudoers.d && printf 'tnk ALL=(ALL) NOPASSWD:ALL\\n' >/etc/sudoers.d/tnk && chmod 0440 /etc/sudoers.d/tnk",
        ])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await?;
    if !install_status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to install container baseline dependencies"
        ));
    }

    let marker_status = Command::new("container")
        .args([
            "exec",
            id,
            "sh",
            "-lc",
            "mkdir -p /var/lib/tnk && touch /var/lib/tnk/container-baseline-v2",
        ])
        .status()
        .await?;
    if !marker_status.success() {
        eprintln!("warning: failed to persist container baseline marker");
    }

    Ok(())
}

async fn stop_container() -> Result<(), color_eyre::Report> {
    let _lock =
        lifecycle::acquire("container-lifecycle", std::time::Duration::from_secs(20)).await?;
    let (id, _, _) = resolve_workspace_context()?;
    if !container_exists(&id).await {
        return Ok(());
    }

    if !container_is_running(&id).await {
        ui::log_info(&format!("already stopped {}", id));
        return Ok(());
    }

    let status = quiet_cmd("container").args(["stop", &id]).status().await?;
    if !status.success() {
        return Err(color_eyre::eyre::eyre!("failed to stop container '{}'", id));
    }

    ui::log_info(&format!("stopped {}", id));
    Ok(())
}

async fn delete_container(id: &str, force: bool) -> Result<(), color_eyre::Report> {
    let _lock =
        lifecycle::acquire("container-lifecycle", std::time::Duration::from_secs(20)).await?;
    delete_container_impl(id, force).await
}

async fn delete_container_impl(id: &str, force: bool) -> Result<(), color_eyre::Report> {
    if !force && !std::io::stdout().is_terminal() {
        return Err(color_eyre::eyre::eyre!(
            "terminal required for deletion, use --yes"
        ));
    }

    if !container_exists(id).await {
        return Ok(());
    }

    if container_is_running(id).await && !force {
        let stop_status = quiet_cmd("container").args(["stop", id]).status().await?;
        if !stop_status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to stop container '{}' before deletion",
                id
            ));
        }
    }

    let mut args: Vec<&str> = vec!["delete"];
    if force {
        args.push("--force");
    }
    args.push(id);

    let status = quiet_cmd("container").args(args).status().await?;
    if !status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to delete container '{}'",
            id
        ));
    }

    ui::log_info(&format!("deleted {}", id));
    Ok(())
}

async fn list_containers() -> Result<Vec<SandboxEntry>, color_eyre::Report> {
    let Some(items) = container_utils::container_list_all().await else {
        return Err(color_eyre::eyre::eyre!(
            "failed to list containers (run 'container system start' if the service is not running)"
        ));
    };

    let entries: Vec<SandboxEntry> = items
        .iter()
        .filter_map(|item| {
            let id = item.id()?;
            if !id.starts_with("tnk-") || id == "tnk-services" || id == "tnk-searxng" {
                return None;
            }
            let status = item.status_state().unwrap_or("unknown").to_string();
            Some(SandboxEntry {
                id: id.to_owned(),
                status,
                mount: "/workspace".to_string(),
            })
        })
        .collect();

    Ok(entries)
}

pub async fn build_golden_image_impl(profile_name: String) -> Result<(), color_eyre::Report> {
    if profile_name.trim().is_empty() {
        return Err(color_eyre::eyre::eyre!("profile cannot be empty"));
    }

    let _lock =
        lifecycle::acquire("container-lifecycle", std::time::Duration::from_secs(20)).await?;

    let sanitized = sanitize_project_name(&profile_name)
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid profile name"))?;
    let builder_id = format!("tnk-builder-{}", sanitized);
    let image_tag = golden_image_tag(&profile_name);

    let cfg = config::load().await?;
    let server_port = cfg.server_port.unwrap_or(8080);
    let engine_name = cfg.default_engine_runtime.as_deref().unwrap_or("llama");
    let model_name = cfg.default_engine_preset.unwrap_or_else(|| {
        crate::engine::default_model_for_runtime(engine_name)
            .unwrap_or("llama")
            .to_string()
    });
    let ctx_window = 131072_u32;

    let mut settings = container_profile_settings(&profile_name, Path::new("/tmp")).await?;
    settings.mounts.clear();
    settings.workspace_guest_path = "/workspace".to_string();
    settings.network_none = false;
    settings.uses_golden_image = false;

    let create_args = create_args_for_settings(&builder_id, &settings);

    if container_exists(&builder_id).await {
        let _ = quiet_cmd("container")
            .args(["delete", "--force", &builder_id])
            .status()
            .await;
    }

    let temp_dir = std::env::temp_dir().join(format!("tnk-image-build-{}", sanitized));
    if temp_dir.exists() {
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
    std::fs::create_dir_all(&temp_dir)?;

    let result: Result<(), color_eyre::Report> = async {
        let status = quiet_cmd("container")
            .args(&create_args)
            .status()
            .await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!("failed to create builder container '{}'", builder_id));
        }

        let status = quiet_cmd("container")
            .args(["start", &builder_id])
            .status()
            .await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!("failed to start builder container '{}'", builder_id));
        }

        ensure_container_runtime_baseline(&builder_id).await?;
        sync_container_user_ids(&builder_id).await;

        run_provision_container(
            &builder_id,
            &profile_name,
            engine_name,
            &model_name,
            ctx_window,
            Path::new("/workspace"),
            server_port,
        )
        .await?;

        let status = quiet_cmd("container")
            .args(["stop", &builder_id])
            .status()
            .await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!("failed to stop builder container before export"));
        }

        let rootfs_tar = temp_dir.join("rootfs.tar");
        let status = Command::new("container")
            .args([
                "export",
                "--output",
                rootfs_tar.to_str().ok_or_else(|| {
                    color_eyre::eyre::eyre!("invalid temporary export path")
                })?,
                &builder_id,
            ])
            .status()
            .await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!("failed to export builder container filesystem"));
        }

        let containerfile = temp_dir.join("Containerfile");
        std::fs::write(&containerfile, "FROM scratch\nADD rootfs.tar /\n")?;

        let output = Command::new("container")
            .args([
                "build",
                "--platform",
                NATIVE_PLATFORM,
                "--file",
                containerfile
                    .to_str()
                    .ok_or_else(|| color_eyre::eyre::eyre!("invalid Containerfile path"))?,
                "--tag",
                &image_tag,
                temp_dir
                    .to_str()
                    .ok_or_else(|| color_eyre::eyre::eyre!("invalid build context path"))?,
            ])
            .output()
            .await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
            let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
            if stderr.contains("rosetta") || stdout.contains("rosetta") {
                return Err(color_eyre::eyre::eyre!(
                    "golden image build failed because the container backend buildkit requires Rosetta on this host, even with '{}'. This is a backend limitation; use prebuilt arm64 images or install Rosetta for buildkit",
                    NATIVE_PLATFORM
                ));
            }
            return Err(color_eyre::eyre::eyre!("failed to build golden image '{}'", image_tag));
        }

        ui::log_info(&format!("built golden image {}", image_tag));
        Ok(())
    }
    .await;

    if container_exists(&builder_id).await {
        let _ = quiet_cmd("container")
            .args(["delete", "--force", &builder_id])
            .status()
            .await;
    }
    let _ = std::fs::remove_dir_all(&temp_dir);

    result
}

pub async fn sandbox_exists(id: &str) -> bool {
    container_exists(id).await
}

pub async fn sandbox_is_running(id: &str) -> bool {
    container_is_running(id).await
}

pub async fn cleanup_untracked_vms(verbose: bool) -> Result<(), color_eyre::Report> {
    ContainerBackend::cleanup_untracked(verbose).await
}

pub async fn delete_sandbox(id: &str, force: bool) -> Result<(), color_eyre::Report> {
    ContainerBackend::delete(id, force).await
}

pub async fn run_provision_script(
    id: &str,
    script_name: &str,
    engine_runtime: &str,
    model_name: &str,
    ctx_window: u32,
    mount_point: &Path,
    port: u16,
) -> Result<(), color_eyre::Report> {
    ContainerBackend::provision(
        id,
        script_name,
        engine_runtime,
        model_name,
        ctx_window,
        mount_point,
        port,
        &ProfileSettings::default(),
    )
    .await
}
