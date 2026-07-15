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

pub mod lima;
pub mod shared;
pub mod types;

use shared::load_profile_manifest;

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct SandboxManifest {
    pub image: Option<String>,
    pub resources: Option<ResourceLimits>,
    pub mounts: Option<HashMap<String, String>>,
    pub security: Option<SecurityCaps>,
}

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct ResourceLimits {
    pub cpus: Option<u32>,
    pub memory: Option<String>,
}

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct SecurityCaps {
    pub network: Option<String>,
    pub workspace_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxEntry {
    pub id: String,
    pub status: String,
    pub mount: String,
}

#[derive(Debug, Clone, Default)]
pub struct ProfileSettings {
    pub cpus: Option<u32>,
    pub memory: Option<String>,
    pub network_none: bool,
    pub workspace_guest_path: String,
    pub image: String,
    pub uses_golden_image: bool,
}

pub use lima::LimaBackend;
pub use lima::resolve_workspace_context;

#[async_trait]
#[allow(clippy::too_many_arguments)]
pub trait SandboxBackend: Sized {
    const BINARY: &'static str;

    async fn resolve_id() -> Result<(String, PathBuf, PathBuf), color_eyre::Report>;

    async fn start(
        profile_name: String,
        audit_log: Option<String>,
        settings: &ProfileSettings,
        runtime_envs: &[(String, String)],
    ) -> Result<(), color_eyre::Report>;

    async fn shell(
        profile: Option<String>,
        command: Option<String>,
        no_tty: bool,
        explicit_envs: Vec<String>,
        audit_log: Option<String>,
        settings: &ProfileSettings,
        runtime_envs: &[(String, String)],
    ) -> Result<(), color_eyre::Report>;

    async fn stop(names: Vec<String>, all: bool) -> Result<(), color_eyre::Report>;

    async fn delete(id: &str, force: bool) -> Result<(), color_eyre::Report>;

    async fn ls() -> Result<Vec<SandboxEntry>, color_eyre::Report>;

    async fn exists(id: &str) -> Result<bool, color_eyre::Report>;

    async fn is_running(id: &str) -> Result<bool, color_eyre::Report>;

    async fn cleanup_untracked(verbose: bool) -> Result<(), color_eyre::Report>;

    async fn provision(
        id: &str,
        profile_name: &str,
        engine_runtime: &str,
        model_name: &str,
        ctx_window: u32,
        mount_point: &Path,
        port: u16,
        settings: &ProfileSettings,
    ) -> Result<(), color_eyre::Report>;

    async fn build_golden_image(profile_name: String) -> Result<(), color_eyre::Report>;

    async fn resolve_gateway(id: &str) -> Result<String, color_eyre::Report>;

    async fn runtime_env(
        id: &str,
        port: u16,
        engine_runtime: &str,
        model_name: &str,
    ) -> Result<Vec<(String, String)>, color_eyre::Report>;

    async fn resolve_active_model_and_ctx(
        port: u16,
        engine_runtime: &str,
    ) -> Result<(String, u32), color_eyre::Report>;
}

pub async fn sandbox_exists(id: &str) -> Result<bool, color_eyre::Report> {
    LimaBackend::exists(id).await
}

pub async fn stop(names: Vec<String>, all: bool) -> Result<(), color_eyre::Report> {
    LimaBackend::stop(names, all).await?;
    Ok(())
}

pub async fn delete_sandbox(id: &str, force: bool) -> Result<(), color_eyre::Report> {
    LimaBackend::delete(id, force).await?;
    Ok(())
}

pub async fn start(
    profile_name: String,
    audit_log: Option<String>,
) -> Result<(), color_eyre::Report> {
    let cfg = crate::config::load().await?;
    let (id, project_root, _workdir) = resolve_workspace_context().await?;

    let settings = resolve_profile_settings(&profile_name, &project_root).await?;
    let home = std::env::var("HOME")?;
    let server_port = cfg.server_port.unwrap_or(8080);
    let engine_name = cfg.default_engine_runtime.as_deref().unwrap_or("llama");
    let (active_model, _ctx_window) =
        crate::sandbox::shared::resolve_active_model_and_ctx_impl(&home, server_port, engine_name)
            .await;
    let runtime_envs =
        LimaBackend::runtime_env(&id, server_port, engine_name, &active_model).await?;

    LimaBackend::start(profile_name, audit_log, &settings, &runtime_envs).await?;

    Ok(())
}

pub async fn shell(
    profile: Option<String>,
    command: Option<String>,
    no_tty: bool,
    explicit_envs: Vec<String>,
    audit_log: Option<String>,
) -> Result<(), color_eyre::Report> {
    let cfg = crate::config::load().await?;
    let (id, project_root, _workdir) = resolve_workspace_context().await?;

    let settings = resolve_profile_settings("base", &project_root).await?;
    let home = std::env::var("HOME")?;
    let server_port = cfg.server_port.unwrap_or(8080);
    let engine_name = cfg.default_engine_runtime.as_deref().unwrap_or("llama");
    let (active_model, _ctx_window) =
        crate::sandbox::shared::resolve_active_model_and_ctx_impl(&home, server_port, engine_name)
            .await;
    let runtime_envs =
        LimaBackend::runtime_env(&id, server_port, engine_name, &active_model).await?;

    LimaBackend::shell(
        profile,
        command,
        no_tty,
        explicit_envs,
        audit_log,
        &settings,
        &runtime_envs,
    )
    .await?;

    Ok(())
}

pub async fn ls(out_fmt: crate::OutputFormat, quiet: bool) -> Result<(), color_eyre::Report> {
    let entries = LimaBackend::ls().await?;

    if entries.is_empty() {
        if out_fmt == crate::OutputFormat::Json {
            println!("[]");
        }
        return Ok(());
    }

    if quiet {
        for entry in &entries {
            println!("{}", entry.id);
        }
        return Ok(());
    }

    if out_fmt == crate::OutputFormat::Json {
        let payload: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| serde_json::json!({"name": e.id, "status": e.status, "mount": e.mount}))
            .collect();
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    if out_fmt == crate::OutputFormat::Ndjson {
        for entry in &entries {
            let payload =
                serde_json::json!({"name": entry.id, "status": entry.status, "mount": entry.mount});
            println!("{}", serde_json::to_string(&payload)?);
        }
        return Ok(());
    }

    for entry in &entries {
        println!("{:<30} {}  mount: {}", entry.id, entry.status, entry.mount);
    }

    Ok(())
}

pub async fn cleanup_untracked_vms(verbose: bool) -> Result<(), color_eyre::Report> {
    LimaBackend::cleanup_untracked(verbose).await
}

async fn resolve_profile_settings(
    profile_name: &str,
    _project_root: &Path,
) -> Result<ProfileSettings, color_eyre::Report> {
    let manifest: Option<SandboxManifest> = load_profile_manifest(profile_name).await?;

    let mut settings = ProfileSettings {
        workspace_guest_path: "/workspace".to_string(),
        ..Default::default()
    };

    if let Some(ref m) = manifest {
        if let Some(ref resources) = m.resources {
            settings.cpus = resources.cpus;
            settings.memory = resources.memory.clone();
        }
        if let Some(ref security) = m.security
            && let Some(ref network) = security.network
        {
            let mode = network.trim();
            if mode.eq_ignore_ascii_case("none") || mode.eq_ignore_ascii_case("restricted") {
                settings.network_none = true;
            }
        }
        if let Some(ref mounts) = m.mounts
            && let Some(guest) = mounts.get("workspace")
            && guest.trim().starts_with('/')
        {
            settings.workspace_guest_path = guest.trim().to_string();
        }
    }

    Ok(settings)
}
