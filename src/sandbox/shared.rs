use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::fs;

pub fn parse_explicit_env(input: &str) -> Result<(String, String), color_eyre::Report> {
    let Some((key, value)) = input.split_once('=') else {
        return Err(color_eyre::eyre::eyre!(
            "invalid --env value '{}': expected KEY=VALUE",
            input
        ));
    };

    if key.is_empty() {
        return Err(color_eyre::eyre::eyre!(
            "invalid --env value '{}': key cannot be empty",
            input
        ));
    }

    if key
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '_'))
    {
        return Err(color_eyre::eyre::eyre!(
            "invalid --env key '{}': use [A-Za-z0-9_] only",
            key
        ));
    }

    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return Err(color_eyre::eyre::eyre!(
            "invalid --env value for '{}': contains control characters",
            key
        ));
    }

    const MAX_ENV_VALUE_LEN: usize = 4096;
    if value.len() > MAX_ENV_VALUE_LEN {
        return Err(color_eyre::eyre::eyre!(
            "invalid --env value for '{}': exceeds {} bytes (got {})",
            key,
            MAX_ENV_VALUE_LEN,
            value.len()
        ));
    }

    Ok((key.to_string(), value.to_string()))
}

pub fn now_unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn runtime_env_summary(envs: &[(String, String)]) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in envs {
        if matches!(
            k.as_str(),
            "TNK_INFERENCE_URL" | "TNK_MODEL_NAME" | "TNK_ENGINE_RUNTIME"
        ) {
            map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
    }
    serde_json::Value::Object(map)
}

pub const DEFAULT_CONTEXT_WINDOW: u32 = 131072;

pub async fn resolve_active_model_and_ctx_impl(
    engine_name: &str,
) -> Result<(String, u32), color_eyre::Report> {
    let cfg = crate::config::load().await?;
    let model = cfg
        .default_model
        .filter(|m| !m.trim().is_empty())
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "engine runtime '{}' has no model configured; set default_model in tnk.toml",
                engine_name
            )
        })?;
    let model = model.trim().to_string();
    crate::sandbox::types::validate_model_name(&model)?;

    Ok((model, DEFAULT_CONTEXT_WINDOW))
}

pub async fn load_profile_manifest(
    profile_name: &str,
) -> Result<Option<crate::sandbox::SandboxManifest>, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let config_dir = PathBuf::from(&home).join(".config/tnk");
    let manifest_path = crate::catalog::resolve_manifest(&config_dir, profile_name);
    let Some(manifest_path) = manifest_path else {
        return Ok(None);
    };

    let content = fs::read_to_string(&manifest_path).await?;
    let manifest: Option<crate::sandbox::SandboxManifest> =
        match serde_yaml::from_str::<crate::sandbox::SandboxManifest>(&content) {
            Ok(m) => Ok::<_, color_eyre::Report>(Some(m)),
            Err(e) => {
                crate::ui::log_warn(&format!(
                    "failed to parse manifest {}: {e}",
                    manifest_path.display()
                ));
                Ok(None)
            }
        }?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::parse_explicit_env;

    #[test]
    fn parses_valid_env_pair() {
        let parsed = parse_explicit_env("FOO=bar").expect("valid env");
        assert_eq!(parsed.0, "FOO");
        assert_eq!(parsed.1, "bar");
    }

    #[test]
    fn rejects_invalid_env_key() {
        assert!(parse_explicit_env("BAD-KEY=1").is_err());
    }
}
