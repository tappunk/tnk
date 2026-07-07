// Copyright 2026 tappunk
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "IS BASIS",
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

pub mod container;
pub mod lima;
pub mod shared;

use async_trait::async_trait;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::fs;

use crate::config;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Runtime {
    Container,
    #[default]
    Lima,
}

impl Runtime {
    pub fn as_str(&self) -> &'static str {
        match self {
            Runtime::Container => "container",
            Runtime::Lima => "lima",
        }
    }

    pub fn try_from_str(s: &str) -> Option<Self> {
        match s {
            "container" => Some(Runtime::Container),
            "lima" => Some(Runtime::Lima),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SandboxEntry {
    pub id: String,
    pub status: String,
    pub mount: String,
}

pub fn resolve_runtime(
    runtime_flag: Option<String>,
    default_sandbox_runtime: Option<String>,
) -> Result<Runtime, color_eyre::Report> {
    if let Some(flag) = runtime_flag {
        return Runtime::try_from_str(&flag)
            .ok_or_else(|| color_eyre::eyre::eyre!("unsupported sandbox runtime: {}", flag));
    }
    Ok(default_sandbox_runtime
        .as_deref()
        .and_then(Runtime::try_from_str)
        .unwrap_or_default())
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

pub trait BackendRuntimeContract {
    fn host_gateway_url(port: u16) -> String;
    fn inference_url(host: &str, port: u16) -> String;
    fn mcp_bridge_url(host: &str) -> String;
    fn searxng_url(host: &str) -> String;
}

impl BackendRuntimeContract for Runtime {
    fn host_gateway_url(port: u16) -> String {
        format!("http://127.0.0.1:{}", port)
    }

    fn inference_url(host: &str, port: u16) -> String {
        format!("http://{}:{}/v1", host, port)
    }

    fn mcp_bridge_url(host: &str) -> String {
        format!("http://{}:18765", host)
    }

    fn searxng_url(host: &str) -> String {
        format!("http://{}:18766", host)
    }
}

pub use container::ContainerBackend;
pub use lima::LimaBackend;

pub use container::build_golden_image_impl as build_golden_image;
pub use container::{cleanup_untracked_vms, resolve_workspace_context, sandbox_exists};

pub async fn sandbox_exists_with_runtime(
    id: &str,
    runtime_flag: Option<String>,
) -> Result<bool, color_eyre::Report> {
    let cfg = config::load()?;
    let runtime = resolve_runtime(runtime_flag, cfg.default_sandbox_runtime.clone())?;

    match runtime {
        Runtime::Container => ContainerBackend::exists(id).await,
        Runtime::Lima => LimaBackend::exists(id).await,
    }
}

pub async fn stop(
    names: Vec<String>,
    all: bool,
    runtime_flag: Option<String>,
) -> Result<(), color_eyre::Report> {
    let cfg = config::load()?;
    let runtime = resolve_runtime(runtime_flag, cfg.default_sandbox_runtime.clone())?;

    match runtime {
        Runtime::Container => ContainerBackend::stop(names, all).await?,
        Runtime::Lima => LimaBackend::stop(names, all).await?,
    }
    Ok(())
}

pub async fn delete_sandbox(
    id: &str,
    force: bool,
    runtime_flag: Option<String>,
) -> Result<(), color_eyre::Report> {
    let cfg = config::load()?;
    let runtime = resolve_runtime(runtime_flag, cfg.default_sandbox_runtime.clone())?;

    match runtime {
        Runtime::Container => ContainerBackend::delete(id, force).await?,
        Runtime::Lima => LimaBackend::delete(id, force).await?,
    }
    Ok(())
}

pub async fn start(
    profile_name: String,
    audit_log: Option<String>,
    runtime_flag: Option<String>,
) -> Result<(), color_eyre::Report> {
    let cfg = config::load()?;
    let runtime = resolve_runtime(runtime_flag, cfg.default_sandbox_runtime.clone())?;
    let (id, project_root, _workdir) = resolve_workspace_context()?;

    let settings = resolve_profile_settings(&profile_name, &project_root).await?;
    let home = std::env::var("HOME")?;
    let server_port = cfg.server_port.unwrap_or(8080);
    let engine_name = cfg.default_engine_runtime.as_deref().unwrap_or("llama");
    let (active_model, _ctx_window) =
        crate::sandbox::shared::resolve_active_model_and_ctx_impl(&home, server_port, engine_name)
            .await;
    let runtime_envs = match runtime {
        Runtime::Container => {
            ContainerBackend::runtime_env(&id, server_port, engine_name, &active_model).await?
        }
        Runtime::Lima => {
            LimaBackend::runtime_env(&id, server_port, engine_name, &active_model).await?
        }
    };

    match runtime {
        Runtime::Container => {
            ContainerBackend::start(profile_name, audit_log, &settings, &runtime_envs).await?
        }
        Runtime::Lima => {
            LimaBackend::start(profile_name, audit_log, &settings, &runtime_envs).await?
        }
    }

    Ok(())
}

pub async fn shell(
    profile: Option<String>,
    command: Option<String>,
    no_tty: bool,
    explicit_envs: Vec<String>,
    audit_log: Option<String>,
    runtime_flag: Option<String>,
) -> Result<(), color_eyre::Report> {
    let cfg = config::load()?;
    let runtime = resolve_runtime(runtime_flag, cfg.default_sandbox_runtime.clone())?;
    let (id, project_root, _workdir) = resolve_workspace_context()?;

    let settings = resolve_profile_settings("base", &project_root).await?;
    let home = std::env::var("HOME")?;
    let server_port = cfg.server_port.unwrap_or(8080);
    let engine_name = cfg.default_engine_runtime.as_deref().unwrap_or("llama");
    let (active_model, _ctx_window) =
        crate::sandbox::shared::resolve_active_model_and_ctx_impl(&home, server_port, engine_name)
            .await;
    let runtime_envs = match runtime {
        Runtime::Container => {
            ContainerBackend::runtime_env(&id, server_port, engine_name, &active_model).await?
        }
        Runtime::Lima => {
            LimaBackend::runtime_env(&id, server_port, engine_name, &active_model).await?
        }
    };

    match runtime {
        Runtime::Container => {
            ContainerBackend::shell(
                profile,
                command,
                no_tty,
                explicit_envs,
                audit_log,
                &settings,
                &runtime_envs,
            )
            .await?
        }
        Runtime::Lima => {
            LimaBackend::shell(
                profile,
                command,
                no_tty,
                explicit_envs,
                audit_log,
                &settings,
                &runtime_envs,
            )
            .await?
        }
    }

    Ok(())
}

pub async fn ls(
    out_fmt: crate::OutputFormat,
    quiet: bool,
    runtime_flag: Option<String>,
) -> Result<(), color_eyre::Report> {
    let cfg = config::load()?;
    let runtime = resolve_runtime(runtime_flag, cfg.default_sandbox_runtime.clone())?;

    let entries = match runtime {
        Runtime::Container => ContainerBackend::ls().await?,
        Runtime::Lima => LimaBackend::ls().await?,
    };

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

async fn resolve_profile_settings(
    profile_name: &str,
    _project_root: &Path,
) -> Result<ProfileSettings, color_eyre::Report> {
    let manifest: Option<SandboxManifest> = load_profile_manifest(profile_name)
        .await
        .unwrap_or_default();

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

async fn load_profile_manifest(
    profile_name: &str,
) -> Result<Option<crate::sandbox::SandboxManifest>, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let config_dir = PathBuf::from(&home).join(".config/tnk");
    let manifest_path = crate::catalog::resolve_manifest(&config_dir, profile_name);
    if !manifest_path.is_file() {
        return Ok(None);
    }

    let content = fs::read_to_string(&manifest_path).await?;
    let manifest: crate::sandbox::SandboxManifest = serde_yaml::from_str(&content)?;
    Ok(Some(manifest))
}
