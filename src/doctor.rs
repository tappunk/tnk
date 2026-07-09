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

use crate::sandbox::container_utils::{self, ContainerListItem};
use std::path::PathBuf;
use std::process::Stdio;

use tokio::fs;
use tokio::process::Command;

async fn list_containers() -> Result<Vec<ContainerListItem>, color_eyre::Report> {
    let Some(items) = container_utils::container_list_all().await else {
        return Ok(vec![]);
    };
    Ok(items)
}

async fn list_lima_instances() -> Result<Vec<String>, color_eyre::Report> {
    let output = Command::new("limactl")
        .args(["list", "--format", "{{.Name}}"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await?;
    if !output.status.success() {
        return Ok(vec![]);
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

fn selected_runtime() -> crate::sandbox::Runtime {
    crate::config::load_blocking()
        .ok()
        .and_then(|cfg| crate::sandbox::resolve_runtime(None, cfg.default_sandbox_runtime).ok())
        .unwrap_or_default()
}

fn check_container_cli() -> Result<(), color_eyre::Report> {
    let primary = std::process::Command::new("container")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let ok = matches!(primary, Ok(s) if s.success())
        || matches!(
            std::process::Command::new("container")
                .args(["system", "version"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status(),
            Ok(s) if s.success()
        );

    if ok {
        eprintln!("ok: container CLI available");
        Ok(())
    } else {
        eprintln!("error: container CLI not found or unavailable");
        eprintln!("hint: install Apple container CLI and run 'container system start'");
        Err(color_eyre::eyre::eyre!("container CLI missing"))
    }
}

fn check_default_engine_runtime_binary() -> Result<(), color_eyre::Report> {
    let runtime = crate::config::load_blocking()?
        .default_engine_runtime
        .unwrap_or_else(|| "llama".to_string());

    let spec = crate::engine::runtime_spec(&runtime).ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "unsupported default engine runtime '{}' (supported: {})",
            runtime,
            crate::engine::supported_runtime_names().join(", ")
        )
    })?;

    let status = std::process::Command::new(spec.executable)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => {
            eprintln!("ok: {} available", spec.executable);
            Ok(())
        }
        _ => {
            eprintln!("error: {} not found", spec.executable);
            Err(color_eyre::eyre::eyre!("{} missing", spec.executable))
        }
    }
}

async fn check_native_arm64_buildkit() -> Result<(), color_eyre::Report> {
    let probe_root =
        std::env::temp_dir().join(format!("tnk-doctor-buildkit-{}", std::process::id()));
    let containerfile_path = probe_root.join("Containerfile");
    let probe_tag = "tnk-doctor-buildkit-probe:latest";

    let _ = fs::remove_dir_all(&probe_root).await;
    fs::create_dir_all(&probe_root).await?;
    fs::write(&containerfile_path, "FROM scratch\n").await?;

    let output = Command::new("container")
        .args([
            "build",
            "-q",
            "--platform",
            "linux/arm64",
            "--file",
            containerfile_path.to_str().ok_or_else(|| {
                color_eyre::eyre::eyre!("invalid doctor probe containerfile path")
            })?,
            "--tag",
            probe_tag,
            probe_root
                .to_str()
                .ok_or_else(|| color_eyre::eyre::eyre!("invalid doctor probe build path"))?,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await?;

    let _ = Command::new("container")
        .args(["image", "rm", probe_tag])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    let _ = fs::remove_dir_all(&probe_root).await;

    if output.status.success() {
        eprintln!("ok: container buildkit supports native linux/arm64 builds");
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).to_ascii_lowercase();
    let stdout = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    if stderr.contains("rosetta") || stdout.contains("rosetta") {
        eprintln!("warning: container buildkit reports Rosetta dependency for linux/arm64 builds");
        eprintln!(
            "hint: this is a backend limitation; image build may fail until backend buildkit is configured for pure arm64"
        );
        return Ok(());
    }

    eprintln!(
        "warning: container buildkit linux/arm64 probe failed; golden image builds may be unavailable"
    );
    Ok(())
}

fn check_config() -> Result<(), color_eyre::Report> {
    let cfg = crate::config::load_blocking()?;
    cfg.print_resolved();
    eprintln!("ok: config loaded");
    Ok(())
}

async fn check_engine() -> Result<(), color_eyre::Report> {
    if crate::engine::is_running().await {
        eprintln!("ok: inference engine running");
    } else {
        crate::ui::log_info("inference engine not running");
    }
    Ok(())
}

async fn check_services() -> Result<(), color_eyre::Report> {
    match selected_runtime() {
        crate::sandbox::Runtime::Container => {
            let items = list_containers().await?;
            let mut services_exists = false;
            let mut searxng_exists = false;

            for item in items {
                if let Some(id) = item.id() {
                    if id == "tnk-services" {
                        services_exists = true;
                    }
                    if id == "tnk-searxng" {
                        searxng_exists = true;
                    }
                }
            }

            if services_exists {
                eprintln!("ok: tnk-services container exists");
            } else {
                crate::ui::log_info("tnk-services container not created yet");
            }
            if searxng_exists {
                eprintln!("ok: tnk-searxng container exists");
            } else {
                crate::ui::log_info("tnk-searxng container not created yet");
            }
        }
        crate::sandbox::Runtime::Lima => {
            let items = list_lima_instances().await?;
            if items.iter().any(|id| id == "tnk-services") {
                eprintln!("ok: tnk-services lima instance exists");
            } else {
                crate::ui::log_info("tnk-services lima instance not created yet");
            }
        }
    }

    Ok(())
}

fn check_locks_dir() -> Result<(), color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let lock_dir = PathBuf::from(home).join(".cache/tnk");
    if lock_dir.exists() {
        eprintln!("ok: runtime cache directory exists");
    } else {
        crate::ui::log_info("runtime cache directory not present yet");
    }
    Ok(())
}

async fn check_managed_containers() -> Result<(), color_eyre::Report> {
    match selected_runtime() {
        crate::sandbox::Runtime::Container => {
            let items = list_containers().await?;
            let count = items
                .iter()
                .filter(|item| {
                    item.id()
                        .is_some_and(|id| id.starts_with("tnk-") || id == "tnk-services")
                        && item.label("tnk.managed").is_some_and(|v| v == "true")
                })
                .count();
            crate::ui::log_info(&format!("managed containers detected: {}", count));
        }
        crate::sandbox::Runtime::Lima => {
            let items = list_lima_instances().await?;
            let count = items.iter().filter(|id| id.starts_with("tnk-")).count();
            crate::ui::log_info(&format!("managed lima instances detected: {}", count));
        }
    }
    Ok(())
}

pub async fn run() -> Result<(), color_eyre::Report> {
    eprintln!("tnk doctor");

    let runtime = selected_runtime();
    if matches!(runtime, crate::sandbox::Runtime::Container) {
        check_container_cli()?;
        check_native_arm64_buildkit().await?;
    }
    check_default_engine_runtime_binary()?;
    check_config()?;
    check_engine().await?;
    check_services().await?;
    check_locks_dir()?;
    check_managed_containers().await?;

    eprintln!("ok: diagnostics completed");
    Ok(())
}
