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

use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::Command;

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
    let items = list_lima_instances().await?;
    if items.iter().any(|id| id == "tnk-services") {
        eprintln!("ok: tnk-services lima instance exists");
    } else {
        crate::ui::log_info("tnk-services lima instance not created yet");
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

async fn check_managed_instances() -> Result<(), color_eyre::Report> {
    let items = list_lima_instances().await?;
    let count = items.iter().filter(|id| id.starts_with("tnk-")).count();
    crate::ui::log_info(&format!("managed lima instances detected: {}", count));
    Ok(())
}

pub async fn run() -> Result<(), color_eyre::Report> {
    eprintln!("tnk doctor");

    check_default_engine_runtime_binary()?;
    check_config()?;
    check_engine().await?;
    check_services().await?;
    check_locks_dir()?;
    check_managed_instances().await?;

    eprintln!("ok: diagnostics completed");
    Ok(())
}
