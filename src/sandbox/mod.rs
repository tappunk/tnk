pub mod lima;
pub mod shared;
pub mod types;

use serde::Serialize;
use std::collections::HashMap;

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct SandboxManifest {
    pub resources: Option<ResourceLimits>,
    pub mounts: Option<HashMap<String, String>>,
}

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct ResourceLimits {
    pub cpus: Option<u32>,
    pub memory: Option<String>,
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
    pub workspace_guest_path: String,
}

pub use lima::{LimaBackend, resolve_workspace_context};

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

pub async fn start(profile_name: String) -> Result<(), color_eyre::Report> {
    let cfg = crate::config::load().await?;
    let resolved = crate::config::ResolvedConfig::resolve(&cfg)?;
    let settings = LimaBackend::resolve_settings(&profile_name).await?;

    LimaBackend::start(profile_name, &resolved, &settings).await?;

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
    let resolved = crate::config::ResolvedConfig::resolve(&cfg)?;
    let profile_name = profile.clone().unwrap_or_else(|| "base".to_string());
    let settings = LimaBackend::resolve_settings(&profile_name).await?;

    LimaBackend::shell(
        profile,
        command,
        no_tty,
        explicit_envs,
        audit_log,
        &resolved,
        &settings,
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
