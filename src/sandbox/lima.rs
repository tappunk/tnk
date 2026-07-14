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

use super::types::{
    TerminalStateGuard, resolve_audit_logger, shell_escape, validate_engine_runtime,
    validate_script_name,
};
use super::{ProfileSettings, SandboxBackend, SandboxEntry};

use std::fmt::Write;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::fs;
use tokio::process::Command;

const SAFE_ENV_ALLOWLIST: &[&str] = &["TERM", "COLORTERM", "COLUMNS", "LINES"];

#[derive(Debug, Clone)]
struct LimaProfileSettings {
    cpus: u32,
    memory: String,
    disk_gib: u32,
    workspace_guest_path: String,
}

fn lima_dir() -> Result<PathBuf, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home).join(".lima"))
}

fn lima_instance_dir(name: &str) -> Result<PathBuf, color_eyre::Report> {
    Ok(lima_dir()?.join(name))
}

fn yaml_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn generate_lima_yaml(host_mount_path: &Path, settings: &LimaProfileSettings) -> String {
    let mut yaml = String::new();
    let _ = writeln!(yaml, "# tnk-managed instance");
    let _ = writeln!(yaml, "vmType: vz");
    let _ = writeln!(yaml, "arch: aarch64");
    let _ = writeln!(yaml, "cpus: {}", settings.cpus);
    let _ = writeln!(yaml, "memory: {}", settings.memory);
    let _ = writeln!(yaml, "disk: {}GiB", settings.disk_gib);
    let _ = writeln!(yaml, "images:");
    let _ = writeln!(
        yaml,
        "  - location: https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-arm64.img"
    );
    let _ = writeln!(yaml, "    arch: aarch64");

    let _ = writeln!(yaml, "mounts:");
    let _ = writeln!(
        yaml,
        "  - location: {}",
        yaml_quote(&host_mount_path.display().to_string())
    );
    let _ = writeln!(
        yaml,
        "    mountPoint: {}",
        yaml_quote(&settings.workspace_guest_path)
    );
    let _ = writeln!(yaml, "    writable: true");

    let _ = writeln!(yaml, "provision:");
    let _ = writeln!(yaml, "  - mode: system");
    let _ = writeln!(yaml, "    script: |");
    let _ = writeln!(yaml, "      #!/bin/bash");
    let _ = writeln!(yaml, "      set -eux -o pipefail");
    let _ = writeln!(yaml, "      export DEBIAN_FRONTEND=noninteractive");
    let _ = writeln!(yaml, "      apt-get update -qq");
    let _ = writeln!(
        yaml,
        "      apt-get install -y -qq bash curl ca-certificates sudo git rsync"
    );
    let _ = writeln!(yaml, "      if ! id -u tnk >/dev/null 2>&1; then");
    let _ = writeln!(yaml, "        useradd -m -s /bin/bash tnk");
    let _ = writeln!(yaml, "      fi");
    let _ = writeln!(yaml, "      usermod -aG sudo tnk");
    let _ = writeln!(yaml, "      install -d -m 755 /etc/sudoers.d");
    let _ = writeln!(
        yaml,
        "      printf 'tnk ALL=(ALL) NOPASSWD:ALL\\n' >/etc/sudoers.d/tnk"
    );
    let _ = writeln!(yaml, "      chmod 0440 /etc/sudoers.d/tnk");
    let _ = writeln!(yaml, "      mkdir -p /var/lib/tnk");
    let _ = writeln!(yaml, "      touch /var/lib/tnk/lima-baseline-v2");

    let _ = writeln!(yaml, "ssh:");
    let _ = writeln!(yaml, "  loadDotSSHPubKeys: false");
    yaml
}

fn instance_yaml_path(name: &str) -> Result<PathBuf, color_eyre::Report> {
    Ok(lima_instance_dir(name)?.join("lima.yaml"))
}

fn instance_dir(name: &str) -> Result<PathBuf, color_eyre::Report> {
    lima_instance_dir(name)
}

fn generated_template_path(name: &str) -> Result<PathBuf, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home)
        .join(".cache/tnk/lima")
        .join(format!("{}.yaml", name)))
}

async fn instance_exists(name: &str) -> Result<bool, color_eyre::Report> {
    Ok(instance_yaml_path(name)?.exists())
}

async fn instance_is_running(name: &str) -> Result<bool, color_eyre::Report> {
    let output = Command::new("limactl")
        .args(["list", "--format", "{{.Status}}", name])
        .output()
        .await?;

    if !output.status.success() {
        return Ok(false);
    }

    let status = String::from_utf8_lossy(&output.stdout);
    Ok(status.trim().eq_ignore_ascii_case("running"))
}

async fn stale_hostagent_pids(id: &str) -> Result<Vec<u32>, color_eyre::Report> {
    let pattern = format!("limactl hostagent .* {}$", id);
    let output = Command::new("pgrep")
        .args(["-f", &pattern])
        .output()
        .await?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            pids.push(pid);
        }
    }
    Ok(pids)
}

async fn cleanup_stale_hostagents(id: &str) -> Result<(), color_eyre::Report> {
    if instance_is_running(id).await? {
        return Ok(());
    }

    let pids = stale_hostagent_pids(id).await?;
    if pids.is_empty() {
        return Ok(());
    }

    for pid in pids {
        let _ = Command::new("kill")
            .args(["-9", &pid.to_string()])
            .output()
            .await;
    }

    Ok(())
}

async fn create_and_start_instance(
    id: &str,
    host_mount_path: &Path,
    settings: &LimaProfileSettings,
) -> Result<(), color_eyre::Report> {
    cleanup_stale_hostagents(id).await?;
    let template = generate_lima_yaml(host_mount_path, settings);
    let template_path = generated_template_path(id)?;
    if let Some(parent) = template_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(&template_path, template).await?;

    if ui::is_verbose() {
        let mut cmd = Command::new("limactl");
        cmd.args(["--tty=false", "start", "--name", id])
            .arg(&template_path)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        match tokio::time::timeout(std::time::Duration::from_secs(300), cmd.status()).await {
            Ok(Ok(status)) => {
                if !status.success() {
                    return Err(color_eyre::eyre::eyre!(
                        "failed to create/start lima instance '{}'",
                        id
                    ));
                }
            }
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => {
                if !instance_is_running(id).await? {
                    return Err(color_eyre::eyre::eyre!(
                        "timed out creating/starting lima instance '{}'",
                        id
                    ));
                }
            }
        }
    } else {
        let mut cmd = Command::new("limactl");
        cmd.args(["--tty=false", "start", "--name", id])
            .arg(&template_path);

        match tokio::time::timeout(std::time::Duration::from_secs(300), cmd.output()).await {
            Ok(Ok(out)) => {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(color_eyre::eyre::eyre!(
                        "failed to create/start lima instance '{}': {}",
                        id,
                        stderr.lines().take(8).collect::<Vec<_>>().join("\n")
                    ));
                }
            }
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => {
                if !instance_is_running(id).await? {
                    return Err(color_eyre::eyre::eyre!(
                        "timed out creating/starting lima instance '{}'",
                        id
                    ));
                }
            }
        }
    }

    Ok(())
}

async fn start_existing_instance(id: &str) -> Result<(), color_eyre::Report> {
    cleanup_stale_hostagents(id).await?;

    if ui::is_verbose() {
        let mut cmd = Command::new("limactl");
        cmd.args(["--tty=false", "start", id])
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        match tokio::time::timeout(std::time::Duration::from_secs(240), cmd.status()).await {
            Ok(Ok(status)) => {
                if !status.success() {
                    return Err(color_eyre::eyre::eyre!(
                        "failed to start lima instance '{}'",
                        id
                    ));
                }
            }
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => {
                if !instance_is_running(id).await? {
                    return Err(color_eyre::eyre::eyre!(
                        "timed out starting lima instance '{}'",
                        id
                    ));
                }
            }
        }
    } else {
        let mut cmd = Command::new("limactl");
        cmd.args(["--tty=false", "start", id]);

        match tokio::time::timeout(std::time::Duration::from_secs(240), cmd.output()).await {
            Ok(Ok(out)) => {
                if !out.status.success() {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    return Err(color_eyre::eyre::eyre!(
                        "failed to start lima instance '{}': {}",
                        id,
                        stderr.lines().take(8).collect::<Vec<_>>().join("\n")
                    ));
                }
            }
            Ok(Err(err)) => return Err(err.into()),
            Err(_) => {
                if !instance_is_running(id).await? {
                    return Err(color_eyre::eyre::eyre!(
                        "timed out starting lima instance '{}'",
                        id
                    ));
                }
            }
        }
    }

    Ok(())
}

pub struct LimaBackend;

#[async_trait::async_trait]
impl SandboxBackend for LimaBackend {
    const BINARY: &'static str = "limactl";

    async fn resolve_id() -> Result<(String, PathBuf, PathBuf), color_eyre::Report> {
        resolve_workspace_context()
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

        let lima_settings = lima_settings_from_profile_settings(settings)?;
        let needs_provision = profile_name != "base";

        if !instance_exists(&id).await? {
            ui::log_info("creating lima instance");
            if !ui::is_quiet() {
                eprintln!("creating sandbox {}...", id);
            }
            create_and_start_instance(&id, &project_root, &lima_settings).await?;
            ui::log_info(&format!("started {}", id));
        } else if !instance_is_running(&id).await? {
            if !ui::is_quiet() {
                eprintln!("starting sandbox {}...", id);
            }
            start_existing_instance(&id).await?;
            ui::log_info(&format!("started {}", id));
        }

        let started_at = Instant::now();
        while started_at.elapsed().as_secs() < 120 {
            if instance_is_running(&id).await? {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
        if !instance_is_running(&id).await? {
            return Err(color_eyre::eyre::eyre!(
                "timed out waiting for lima instance '{}' to become running",
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
                .await;

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
                &lima_settings.workspace_guest_path,
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

        let default_settings = lima_settings_from_profile_settings(settings)?;

        if !instance_exists(&id).await? {
            ui::log_info("creating lima instance");
            create_and_start_instance(&id, &project_root, &default_settings).await?;
            ui::log_info(&format!("started {}", id));
        } else if !instance_is_running(&id).await? {
            start_existing_instance(&id).await?;
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
                .await;

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

        shell_parts.push(format!("cd {} || exit 1", shell_escape(guest_workdir_str)));

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

        let script = shell_parts.join(" && ");

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

        return Err(color_eyre::eyre::eyre!(
            "sandbox shell exited with code {}",
            status.code().unwrap_or(1)
        ));
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
                if !instance_exists(&id).await? {
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
        Ok(instance_exists(id).await?)
    }

    async fn is_running(id: &str) -> Result<bool, color_eyre::Report> {
        Ok(instance_is_running(id).await?)
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

        let output = Command::new("limactl")
            .args(["list", "--format", "{{.Name}}"])
            .output()
            .await?;

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

            let is_running = instance_is_running(id).await?;
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

    async fn build_golden_image(profile_name: String) -> Result<(), color_eyre::Report> {
        let _ = profile_name;
        Ok(())
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
        Ok(
            crate::sandbox::shared::resolve_active_model_and_ctx_impl(&home, port, engine_runtime)
                .await,
        )
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

fn lima_settings_from_profile_settings(
    settings: &ProfileSettings,
) -> Result<LimaProfileSettings, color_eyre::Report> {
    if settings.network_none {
        return Err(color_eyre::eyre::eyre!(
            "lima backend does not support network=none or network=restricted"
        ));
    }

    Ok(LimaProfileSettings {
        cpus: settings.cpus.unwrap_or(4),
        memory: settings
            .memory
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("4GiB")
            .to_string(),
        disk_gib: 50,
        workspace_guest_path: settings.workspace_guest_path.clone(),
    })
}

async fn stop_lima_instance() -> Result<(), color_eyre::Report> {
    let _lock = lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20)).await?;
    let (id, _, _) = LimaBackend::resolve_id().await?;

    stop_lima_instance_by_id(&id).await
}

async fn stop_lima_instance_by_id(id: &str) -> Result<(), color_eyre::Report> {
    if !instance_exists(id).await? {
        return Ok(());
    }

    if !instance_is_running(id).await? {
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

    if graceful_ok || !instance_is_running(id).await? {
        ui::log_info(&format!("stopped {}", id));
        return Ok(());
    }

    eprintln!(
        "warning: graceful stop for '{}' did not succeed, retrying",
        id
    );
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    if instance_is_running(id).await? {
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
                if instance_is_running(id).await? {
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
        return Err(color_eyre::eyre::eyre!(
            "terminal required for deletion, use --yes"
        ));
    }

    if !instance_exists(id).await? {
        return Ok(());
    }

    if instance_is_running(id).await? {
        stop_lima_instance_by_id(id).await?;
    }

    let mut args = vec!["delete"];
    if force {
        args.push("--force");
    }
    args.push(id);

    let output = Command::new("limactl").args(&args).output().await?;

    if !output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to delete lima instance '{}'",
            id
        ));
    }

    let yaml_path = instance_yaml_path(id)?;
    let _ = fs::remove_file(&yaml_path).await;

    let dir = instance_dir(id)?;
    let _ = fs::remove_dir_all(&dir).await;

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
    let output = Command::new("limactl")
        .args(["list", "--format", "{{.Name}}"])
        .output()
        .await;

    let stdout = match output {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
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
    let output = Command::new("limactl")
        .args(["list", "--format", "{{.Name}}\t{{.Status}}"])
        .output()
        .await?;

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

    let specs_rev = crate::sandbox::shared::compute_specs_revision_hash(
        &host_script,
        has_lib.then_some(&host_lib_dir),
    )
    .await?;

    let guest_provision_dir = format!("/tmp/tnk-provision-{}", script_name);
    let guest_script_path = format!("{}/{}.sh", guest_provision_dir, script_name);

    let host_gateway = LimaBackend::resolve_gateway(id).await?;

    ui::log_info(&format!("provisioning: {}", script_name));

    let prepare_output = Command::new("limactl")
        .args(["shell", id, "--", "mkdir", "-p", &guest_provision_dir])
        .output()
        .await?;
    if !prepare_output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to prepare guest provision directory for '{}'",
            script_name
        ));
    }

    let script_copy_output = Command::new("limactl")
        .args(["copy"])
        .arg(&host_script)
        .arg(format!("{}:{}", id, guest_script_path))
        .output()
        .await?;
    if !script_copy_output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to copy provision script into lima guest for '{}'",
            script_name
        ));
    }

    if has_lib {
        let guest_lib_dir = format!("{}/lib", guest_provision_dir);
        let guest_target_lib_dir = format!("{}:{}", id, guest_lib_dir);
        let mkdir_output = Command::new("limactl")
            .args(["shell", id, "--", "mkdir", "-p", &guest_lib_dir])
            .output()
            .await?;
        if !mkdir_output.status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to prepare guest provision directory for '{}'",
                script_name
            ));
        }

        let lib_copy_output = Command::new("limactl")
            .args(["copy", "--recursive"])
            .arg(&host_lib_dir)
            .arg(&guest_target_lib_dir)
            .output()
            .await?;
        if !lib_copy_output.status.success() {
            crate::ui::log_warn(
                "limactl copy --recursive failed for provision library, falling back to tar",
            );
            let tar_copy = Command::new("bash")
                .arg("-c")
                .arg(format!(
                    "cd {} && tar cf - . | limactl shell {} tar xf - -C {}",
                    host_lib_dir.display(),
                    id,
                    guest_target_lib_dir
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
    let host_gateway_escaped = shell_escape(&host_gateway);
    let provision_cmd = format!(
        r#"set -eu -o pipefail
export TNK_OPENAI_URL={}
export TNK_INFERENCE_URL={}
export TNK_MCP_BRIDGE_URL={}
export TNK_SEARXNG_URL={}
export TNK_MODEL_NAME={}
export TNK_CTX_WINDOW={}
export TNK_WORKSPACE_MOUNT={}
export TNK_SPECS_REV={}
export TNK_CONTAINER_HOST_GATEWAY={}
export TNK_ENGINE_RUNTIME={}
bash {}"#,
        openai_url,
        openai_url,
        mcp_bridge_url,
        searxng_url,
        shell_escape(model_name),
        ctx_window,
        shell_escape(mount_path),
        shell_escape(&specs_rev),
        host_gateway_escaped,
        shell_escape(engine_runtime),
        shell_escape(&guest_script_path),
    );

    let provision_status = tokio::time::timeout(
        std::time::Duration::from_secs(1800),
        Command::new("limactl")
            .args(["shell", id, "--", "bash", "-lc"])
            .arg(provision_cmd)
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status(),
    )
    .await
    .map_err(|_| {
        color_eyre::eyre::eyre!("provision timed out for '{}' after 1800s", script_name)
    })??;

    if !provision_status.success() {
        return Err(color_eyre::eyre::eyre!(
            "provision failed for '{}'",
            script_name
        ));
    }

    ui::log_info(&format!("provisioned: {}", script_name));
    Ok(())
}
