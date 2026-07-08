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

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

type ResolvedConfig = (
    u16,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

#[derive(Subcommand)]
pub enum ConfigCommands {
    Init {
        #[arg(long, help = "Force overwrite existing tnk.toml")]
        force: bool,
    },
    Show,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct TnkConfig {
    pub server_port: Option<u16>,
    pub workspace_root: Option<String>,
    pub model_dir: Option<String>,
    pub default_provision_profile: Option<String>,
    pub default_engine_runtime: Option<String>,
    pub default_engine_preset: Option<String>,
    pub default_engine_bind_host: Option<String>,
    pub default_sandbox_runtime: Option<String>,
    pub container_host_gateway: Option<String>,
    pub services_auto_start: Option<bool>,
}

impl TnkConfig {
    fn resolve(self) -> Result<ResolvedConfig, color_eyre::Report> {
        let server_port = self.server_port.unwrap_or(8080);
        let home = dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| color_eyre::eyre::eyre!("could not resolve home directory"))?;
        let workspace_root = match self.workspace_root {
            Some(v) => v,
            None => format!("{}/src", home),
        };
        let model_dir = match self.model_dir {
            Some(v) => v,
            None => format!("{}/opt/models", home),
        };
        let provision_profile = self
            .default_provision_profile
            .unwrap_or_else(|| "pi".to_string());
        let engine_runtime = self.default_engine_runtime.clone();
        let engine_preset = self.default_engine_preset.clone();
        let engine_bind_host = self.default_engine_bind_host.clone();
        let sandbox_runtime = self.default_sandbox_runtime.clone();
        let container_host_gateway = self.container_host_gateway.clone();
        let services_auto_start = self.services_auto_start.unwrap_or(true);
        Ok((
            server_port,
            workspace_root,
            model_dir,
            provision_profile,
            engine_runtime,
            engine_preset,
            engine_bind_host,
            sandbox_runtime,
            container_host_gateway,
            services_auto_start,
        ))
    }

    pub fn print_resolved(&self) {
        let (
            server_port,
            workspace_root,
            model_dir,
            provision_profile,
            engine_runtime,
            engine_preset,
            engine_bind_host,
            sandbox_runtime,
            container_host_gateway,
            services_auto_start,
        ) = match self.clone().resolve() {
            Ok(v) => v,
            Err(err) => {
                eprintln!("error: {}", err);
                return;
            }
        };
        println!("server_port        {}", server_port);
        println!("workspace_root    {}", workspace_root);
        println!("model_dir         {}", model_dir);
        println!("provision_profile {}", provision_profile);
        println!(
            "engine_runtime    {}",
            engine_runtime.as_deref().unwrap_or("llama")
        );
        println!(
            "engine_preset     {}",
            engine_preset.as_deref().unwrap_or("<none>")
        );
        println!(
            "engine_bind_host  {}",
            engine_bind_host.as_deref().unwrap_or("127.0.0.1")
        );
        println!(
            "sandbox_runtime   {}",
            sandbox_runtime.as_deref().unwrap_or("lima")
        );
        println!(
            "container_gateway   {}",
            container_host_gateway.unwrap_or_else(|| "<auto>".to_string())
        );
        println!("services_auto_start  {}", services_auto_start);
    }
}

pub fn load() -> Result<TnkConfig, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let config_path = PathBuf::from(&home).join(".config/tnk/tnk.toml");

    let mut config = if config_path.exists() {
        let content = fs::read_to_string(&config_path)?;
        toml::from_str(&content)?
    } else {
        TnkConfig::default()
    };

    if let Ok(v) = std::env::var("TNK_SERVER_PORT") {
        config.server_port = v.parse().ok();
    }
    if let Ok(v) = std::env::var("TNK_WORKSPACE_ROOT") {
        config.workspace_root = Some(v);
    }
    if let Ok(v) = std::env::var("TNK_MODEL_DIR") {
        config.model_dir = Some(v);
    }
    if let Ok(v) = std::env::var("TNK_PROVISION_PROFILE") {
        config.default_provision_profile = Some(v);
    }
    if let Ok(v) = std::env::var("TNK_ENGINE_RUNTIME") {
        config.default_engine_runtime = Some(v);
    }
    if let Ok(v) = std::env::var("TNK_ENGINE_PRESET") {
        config.default_engine_preset = Some(v);
    }
    if let Ok(v) = std::env::var("TNK_ENGINE_BIND_HOST") {
        config.default_engine_bind_host = Some(v);
    }
    if let Ok(v) = std::env::var("TNK_SANDBOX_RUNTIME") {
        config.default_sandbox_runtime = Some(v);
    }
    if let Ok(v) = std::env::var("TNK_CONTAINER_HOST_GATEWAY") {
        config.container_host_gateway = Some(v);
    }
    if let Ok(v) = std::env::var("TNK_SERVICES_AUTO_START") {
        config.services_auto_start = Some(matches!(v.as_str(), "true" | "1"));
    }

    Ok(config)
}

pub fn init_config(force: bool) -> Result<(), color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let config_dir = PathBuf::from(&home).join(".config/tnk");
    let config_path = config_dir.join("tnk.toml");

    if config_path.exists() && !force {
        return Ok(());
    }

    fs::create_dir_all(&config_dir)?;
    fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o700))?;

    let template = r##"# tnk configuration

# API port for local inference server
server_port = 8080

# Root used for project-to-sandbox mapping (must NOT be your home directory)
workspace_root = "~/src"

# Base directory for local model files
model_dir = "~/opt/models"

# Default sandbox profile
default_provision_profile = "pi"

# Inference runtime: "llama"
default_engine_runtime = "llama"

# Auto-start tnk services when running `tnk run`
services_auto_start = true

# Sandbox backend: lima
default_sandbox_runtime = "lima"

# Bind host for inference server (127.0.0.1 for host-only, 0.0.0.0 for sandbox access)
default_engine_bind_host = "127.0.0.1"

# Preset to load when --preset is omitted from engine start.
# Must match the filename stem of a file in ~/.config/tnk/provider.d/
# Example: "llama-default" loads ~/.config/tnk/provider.d/llama-default.ini
# default_engine_preset = "llama-default"

"##;

    fs::write(&config_path, template)?;
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;
    crate::ui::log_info(&format!("created {}", config_path.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::TnkConfig;

    #[test]
    fn resolve_uses_expected_defaults() {
        let cfg = TnkConfig::default();
        let (
            port,
            workspace_root,
            model_dir,
            profile,
            runtime,
            preset,
            bind_host,
            sandbox_runtime,
            gateway,
            services_auto_start,
        ) = cfg.resolve().expect("resolve defaults");

        assert_eq!(port, 8080);
        assert!(workspace_root.ends_with("/src"));
        assert!(model_dir.ends_with("/opt/models"));
        assert_eq!(profile, "pi");
        assert!(runtime.is_none());
        assert!(preset.is_none());
        assert!(bind_host.is_none());
        assert!(sandbox_runtime.is_none());
        assert!(gateway.is_none());
        assert!(services_auto_start);
    }

    #[test]
    fn resolve_preserves_explicit_values() {
        let cfg = TnkConfig {
            server_port: Some(9001),
            workspace_root: Some("/tmp/ws".to_string()),
            model_dir: Some("/tmp/models".to_string()),
            default_provision_profile: Some("base".to_string()),
            default_engine_runtime: Some("llama".to_string()),
            default_engine_preset: Some("llama-default".to_string()),
            default_engine_bind_host: Some("127.0.0.1".to_string()),
            default_sandbox_runtime: Some("container".to_string()),
            container_host_gateway: Some("10.0.0.1".to_string()),
            services_auto_start: Some(false),
        };

        let (
            port,
            workspace_root,
            model_dir,
            profile,
            runtime,
            preset,
            bind_host,
            sandbox_runtime,
            gateway,
            services_auto_start,
        ) = cfg.resolve().expect("resolve explicit values");

        assert_eq!(port, 9001);
        assert_eq!(workspace_root, "/tmp/ws");
        assert_eq!(model_dir, "/tmp/models");
        assert_eq!(profile, "base");
        assert_eq!(runtime.as_deref(), Some("llama"));
        assert_eq!(preset.as_deref(), Some("llama-default"));
        assert_eq!(bind_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(sandbox_runtime.as_deref(), Some("container"));
        assert_eq!(gateway.as_deref(), Some("10.0.0.1"));
        assert!(!services_auto_start);
    }
}
