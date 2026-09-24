use clap::Subcommand;
use serde::Deserialize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub struct ResolvedConfig {
    pub server_port: u16,
    pub workspace_root: String,
    pub provision_profile: String,
    pub engine_runtime: String,
    pub model: Option<String>,
    pub ctx_window: u32,
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    Init {
        #[arg(long, help = "force")]
        force: bool,
    },
    Show,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct TnkConfig {
    pub server_port: Option<u16>,
    pub workspace_root: Option<String>,
    pub default_provision_profile: Option<String>,
    pub default_engine_runtime: Option<String>,
    pub default_model: Option<String>,
    pub default_context_window: Option<u32>,
}

impl TnkConfig {
    fn resolve(self) -> Result<ResolvedConfig, color_eyre::Report> {
        let server_port = self.server_port.unwrap_or(9931);
        let home = std::env::var("HOME")
            .map_err(|_| color_eyre::eyre::eyre!("could not resolve home directory"))?;
        let workspace_root = match self.workspace_root {
            Some(v) => expand_path(v, &home),
            None => format!("{}/code", home),
        };
        let provision_profile = self
            .default_provision_profile
            .unwrap_or_else(|| "pi".to_string());
        let engine_runtime = self
            .default_engine_runtime
            .unwrap_or_else(|| "llama".to_string());
        let model = self.default_model.clone();
        let ctx_window = self
            .default_context_window
            .unwrap_or(DEFAULT_CONTEXT_WINDOW);
        Ok(ResolvedConfig {
            server_port,
            workspace_root,
            provision_profile,
            engine_runtime,
            model,
            ctx_window,
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
        println!("provision_profile {}", cfg.provision_profile);
        println!("engine_runtime    {}", cfg.engine_runtime);
        println!(
            "model             {}",
            cfg.model.as_deref().unwrap_or("<none>")
        );
        println!("ctx_window        {}", cfg.ctx_window);
    }
}

pub(crate) fn expand_path(path: String, home: &str) -> String {
    if let Some(stripped) = path.strip_prefix("~/") {
        format!("{}/{}", home, stripped)
    } else if let Some(stripped) = path.strip_prefix('~') {
        format!("{}{}", home, stripped)
    } else if let Some(rest) = path.strip_prefix("$HOME/") {
        format!("{}/{}", home, rest)
    } else if path == "$HOME" {
        home.to_string()
    } else if let Some(rest) = path.strip_prefix("${HOME}/") {
        format!("{}/{}", home, rest)
    } else if path == "${HOME}" {
        home.to_string()
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
    if let Ok(v) = std::env::var("TNK_PROVISION_PROFILE") {
        config.default_provision_profile = Some(v);
    }
    if let Ok(v) = std::env::var("TNK_ENGINE_RUNTIME") {
        if crate::sandbox::types::validate_engine_runtime(&v).is_ok() {
            config.default_engine_runtime = Some(v);
        } else {
            crate::ui::log_warn(&format!(
                "invalid TNK_ENGINE_RUNTIME='{}'; ignoring env override",
                v
            ));
        }
    }
    if let Ok(v) = std::env::var("TNK_MODEL") {
        if v.is_empty() {
            crate::ui::log_warn("TNK_MODEL is empty; ignoring env override");
        } else {
            config.default_model = Some(v);
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

pub const DEFAULT_CONTEXT_WINDOW: u32 = 131072;

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

# API port for inference server
server_port = 9931

# Root used for project-to-sandbox mapping (must NOT be your home directory)
workspace_root = "~/code"

# Default sandbox profile
default_provision_profile = "pi"

# Inference runtime key (consumed by provision profiles)
default_engine_runtime = "llama"

# Model name injected into sandboxes as TNK_MODEL_NAME
# default_model = "ai-fast"

# Context window injected into sandboxes as TNK_CTX_WINDOW
# default_context_window = 131072

"##;

    fs::write(&config_path, template)?;
    fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600))?;
    crate::ui::log_info(&format!("created {}", config_path.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{DEFAULT_CONTEXT_WINDOW, ResolvedConfig, TnkConfig, expand_path};

    #[test]
    fn resolve_uses_expected_defaults() {
        let cfg = TnkConfig::default();
        let cfg = ResolvedConfig::resolve(&cfg).expect("resolve defaults");

        assert_eq!(cfg.server_port, 9931);
        assert!(cfg.workspace_root.ends_with("/code"));
        assert_eq!(cfg.provision_profile, "pi");
        assert_eq!(cfg.engine_runtime, "llama");
        assert!(cfg.model.is_none());
        assert_eq!(cfg.ctx_window, DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn resolve_preserves_explicit_values() {
        let cfg = TnkConfig {
            server_port: Some(9001),
            workspace_root: Some("/tmp/ws".to_string()),
            default_provision_profile: Some("base".to_string()),
            default_engine_runtime: Some("llama".to_string()),
            default_model: Some("llama-default".to_string()),
            default_context_window: Some(32768),
        };

        let cfg = ResolvedConfig::resolve(&cfg).expect("resolve explicit values");

        assert_eq!(cfg.server_port, 9001);
        assert_eq!(cfg.workspace_root, "/tmp/ws");
        assert_eq!(cfg.provision_profile, "base");
        assert_eq!(cfg.engine_runtime, "llama");
        assert_eq!(cfg.model.as_deref(), Some("llama-default"));
        assert_eq!(cfg.ctx_window, 32768);
    }

    #[test]
    fn expand_path_replaces_tilde() {
        assert_eq!(
            expand_path("~/code".to_string(), "/home/user"),
            "/home/user/code"
        );
        assert_eq!(
            expand_path("~/models".to_string(), "/home/user"),
            "/home/user/models"
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

    #[test]
    fn expand_path_dollar_home_exact() {
        assert_eq!(expand_path("$HOME".to_string(), "/home/user"), "/home/user");
    }

    #[test]
    fn expand_path_dollar_home_with_slash() {
        assert_eq!(
            expand_path("$HOME/src".to_string(), "/home/user"),
            "/home/user/src"
        );
    }

    #[test]
    fn expand_path_dollar_home_no_separator() {
        assert_eq!(
            expand_path("$HOMEfoo".to_string(), "/home/user"),
            "$HOMEfoo"
        );
    }

    #[test]
    fn expand_path_brace_home_exact() {
        assert_eq!(
            expand_path("${HOME}".to_string(), "/home/user"),
            "/home/user"
        );
    }

    #[test]
    fn expand_path_brace_home_no_separator() {
        assert_eq!(
            expand_path("${HOME}foo".to_string(), "/home/user"),
            "${HOME}foo"
        );
    }
}
