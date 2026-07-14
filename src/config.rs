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

pub struct ResolvedConfig {
    pub server_port: u16,
    pub workspace_root: String,
    pub model_dir: String,
    pub provision_profile: String,
    pub engine_runtime: Option<String>,
    pub engine_preset: Option<String>,
    pub engine_bind_host: Option<String>,
    pub services_auto_start: bool,
}

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
    pub services_auto_start: Option<bool>,
}

impl TnkConfig {
    fn resolve(self) -> Result<ResolvedConfig, color_eyre::Report> {
        let server_port = self.server_port.unwrap_or(8080);
        let home = dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .ok_or_else(|| color_eyre::eyre::eyre!("could not resolve home directory"))?;
        let workspace_root = match self.workspace_root {
            Some(v) => expand_path(v, &home),
            None => format!("{}/src", home),
        };
        let model_dir = match self.model_dir {
            Some(v) => expand_path(v, &home),
            None => format!("{}/opt/models", home),
        };
        let provision_profile = self
            .default_provision_profile
            .unwrap_or_else(|| "pi".to_string());
        let engine_runtime = self.default_engine_runtime.clone();
        let engine_preset = self.default_engine_preset.clone();
        let engine_bind_host = self.default_engine_bind_host.clone();
        let services_auto_start = self.services_auto_start.unwrap_or(true);
        Ok(ResolvedConfig {
            server_port,
            workspace_root,
            model_dir,
            provision_profile,
            engine_runtime,
            engine_preset,
            engine_bind_host,
            services_auto_start,
        })
    }

    pub fn print_resolved(&self) {
        let cfg = match self.clone().resolve() {
            Ok(v) => v,
            Err(err) => {
                eprintln!("error: {}", err);
                return;
            }
        };
        println!("server_port       {}", cfg.server_port);
        println!("workspace_root    {}", cfg.workspace_root);
        println!("model_dir         {}", cfg.model_dir);
        println!("provision_profile {}", cfg.provision_profile);
        println!(
            "engine_runtime    {}",
            cfg.engine_runtime.as_deref().unwrap_or("llama")
        );
        println!(
            "engine_preset     {}",
            cfg.engine_preset.as_deref().unwrap_or("<none>")
        );
        println!(
            "engine_bind_host  {}",
            cfg.engine_bind_host.as_deref().unwrap_or("127.0.0.1")
        );
        println!("services_auto_start  {}", cfg.services_auto_start);
    }
}

fn expand_path(path: String, home: &str) -> String {
    if let Some(stripped) = path.strip_prefix("~/") {
        format!("{}/{}", home, stripped)
    } else if let Some(stripped) = path.strip_prefix('~') {
        format!("{}{}", home, stripped)
    } else if let Some(rest) = path.strip_prefix("$HOME") {
        format!("{}{}", home, rest)
    } else if let Some(rest) = path.strip_prefix("${HOME}") {
        if rest.starts_with('/') {
            format!("{}{}", home, rest)
        } else {
            path
        }
    } else {
        path
    }
}

impl ResolvedConfig {
    pub fn resolve(cfg: &TnkConfig) -> Result<Self, color_eyre::Report> {
        cfg.clone().resolve()
    }
}

fn apply_env_overrides(config: &mut TnkConfig) {
    if let Ok(v) = std::env::var("TNK_SERVER_PORT") {
        match v.parse() {
            Ok(port) => config.server_port = Some(port),
            Err(_) => {
                crate::ui::log_warn(&format!(
                    "invalid TNK_SERVER_PORT='{}'; ignoring env override",
                    v
                ));
            }
        }
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
    if let Ok(v) = std::env::var("TNK_SERVICES_AUTO_START") {
        match v.as_str() {
            "true" | "1" => config.services_auto_start = Some(true),
            "false" | "0" => config.services_auto_start = Some(false),
            _ => {
                crate::ui::log_warn(&format!(
                    "invalid TNK_SERVICES_AUTO_START='{}'; ignoring env override",
                    v
                ));
            }
        }
    }
}

pub async fn load() -> Result<TnkConfig, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let config_path = PathBuf::from(&home).join(".config/tnk/tnk.toml");

    let mut config = if config_path.exists() {
        let content = tokio::fs::read_to_string(&config_path).await?;
        toml::from_str(&content)?
    } else {
        crate::ui::log_info("using default settings (run `tnk init` to configure)");
        TnkConfig::default()
    };

    apply_env_overrides(&mut config);
    Ok(config)
}

pub fn load_blocking() -> Result<TnkConfig, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let config_path = PathBuf::from(&home).join(".config/tnk/tnk.toml");

    let mut config = if config_path.exists() {
        let content = fs::read_to_string(&config_path)?;
        toml::from_str(&content)?
    } else {
        crate::ui::log_info("using default settings (run `tnk init` to configure)");
        TnkConfig::default()
    };

    apply_env_overrides(&mut config);
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

    let template = format!(
        r##"# tnk configuration

# API port for local inference server
server_port = 8080

# Root used for project-to-sandbox mapping (must NOT be your home directory)
workspace_root = "{home}/src"

# Base directory for local model files
model_dir = "{home}/opt/models"

# Default sandbox profile
default_provision_profile = "pi"

# Inference runtime: "llama"
default_engine_runtime = "llama"

# Auto-start tnk services when running `tnk run`
services_auto_start = true

# Bind host for inference server (127.0.0.1 for host-only, 0.0.0.0 for sandbox access)
default_engine_bind_host = "127.0.0.1"

# Preset to load when --preset is omitted from engine start.
# Must match the filename stem of a file in ~/.config/tnk/provider.d/
# Example: "llama-default" loads ~/.config/tnk/provider.d/llama-default.ini
# default_engine_preset = "llama-default"

"##
    );

    fs::write(&config_path, template)?;
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;
    crate::ui::log_info(&format!("created {}", config_path.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ResolvedConfig, TnkConfig, expand_path};

    #[test]
    fn resolve_uses_expected_defaults() {
        let cfg = TnkConfig::default();
        let cfg = ResolvedConfig::resolve(&cfg).expect("resolve defaults");

        assert_eq!(cfg.server_port, 8080);
        assert!(cfg.workspace_root.ends_with("/src"));
        assert!(cfg.model_dir.ends_with("/opt/models"));
        assert_eq!(cfg.provision_profile, "pi");
        assert!(cfg.engine_runtime.is_none());
        assert!(cfg.engine_preset.is_none());
        assert!(cfg.engine_bind_host.is_none());
        assert!(cfg.services_auto_start);
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
            services_auto_start: Some(false),
        };

        let cfg = ResolvedConfig::resolve(&cfg).expect("resolve explicit values");

        assert_eq!(cfg.server_port, 9001);
        assert_eq!(cfg.workspace_root, "/tmp/ws");
        assert_eq!(cfg.model_dir, "/tmp/models");
        assert_eq!(cfg.provision_profile, "base");
        assert_eq!(cfg.engine_runtime.as_deref(), Some("llama"));
        assert_eq!(cfg.engine_preset.as_deref(), Some("llama-default"));
        assert_eq!(cfg.engine_bind_host.as_deref(), Some("127.0.0.1"));
        assert!(!cfg.services_auto_start);
    }

    #[test]
    fn expand_path_replaces_tilde() {
        assert_eq!(
            expand_path("~/src".to_string(), "/home/user"),
            "/home/user/src"
        );
        assert_eq!(
            expand_path("~/opt/models".to_string(), "/home/user"),
            "/home/user/opt/models"
        );
    }

    #[test]
    fn expand_path_preserves_absolute() {
        assert_eq!(expand_path("/tmp/ws".to_string(), "/home/user"), "/tmp/ws");
    }

    #[test]
    fn expand_path_preserves_relative() {
        assert_eq!(expand_path("./src".to_string(), "/home/user"), "./src");
    }
}
