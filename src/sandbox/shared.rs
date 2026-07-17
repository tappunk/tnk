use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::fs;

pub const BASELINE_PROVISION_SCRIPT: &str = "\
#!/usr/bin/env bash
set -eu
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq bash ca-certificates curl nodejs npm sudo
if ! id -u tnk >/dev/null 2>&1; then
  useradd -m -s /bin/bash tnk
fi
usermod -aG sudo tnk
install -d -m 755 /etc/sudoers.d
printf 'tnk ALL=(ALL) NOPASSWD:ALL\\n' >/etc/sudoers.d/tnk
chmod 0440 /etc/sudoers.d/tnk
";

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
            "TNK_INFERENCE_URL"
                | "TNK_MCP_BRIDGE_URL"
                | "TNK_SEARXNG_URL"
                | "TNK_MODEL_NAME"
                | "TNK_ENGINE_RUNTIME"
        ) {
            map.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
    }
    serde_json::Value::Object(map)
}

pub async fn collect_regular_files_recursive(
    root: &Path,
) -> Result<Vec<PathBuf>, color_eyre::Report> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];

    while let Some(dir) = dirs.pop() {
        let mut entries = fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let metadata = fs::metadata(&path).await?;
            if metadata.is_dir() {
                dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }

    Ok(files)
}

pub async fn compute_specs_revision_hash(
    script_path: &Path,
    lib_dir: Option<&Path>,
) -> Result<String, color_eyre::Report> {
    use sha2::{Digest, Sha256};

    let mut files = vec![script_path.to_path_buf()];
    if let Some(dir) = lib_dir {
        files.extend(collect_regular_files_recursive(dir).await?);
    }
    files.sort();

    let mut first_pass_output = String::new();
    for file in &files {
        let content = fs::read(file).await?;
        let mut hasher = Sha256::new();
        hasher.update(&content);
        let digest = hasher.finalize();
        first_pass_output.push_str(&format!("{:x}  {}\n", digest, file.display()));
    }

    let mut final_hasher = Sha256::new();
    final_hasher.update(first_pass_output.as_bytes());
    let final_digest = final_hasher.finalize();
    Ok(format!("{:x}", final_digest))
}

pub async fn resolve_active_model_and_ctx_impl(
    home: &str,
    port: u16,
    engine_name: &str,
) -> Result<(String, u32), color_eyre::Report> {
    let default_model = crate::config::load()
        .await
        .ok()
        .and_then(|cfg| cfg.default_engine_preset.filter(|m| !m.trim().is_empty()))
        .or_else(|| crate::engine::default_model_for_runtime(engine_name).map(String::from))
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "engine runtime '{}' has no default model configured; \
                 set default_engine_preset in tnk.toml",
                engine_name
            )
        })?;
    let preset_ctx_hint = crate::model::DEFAULT_CONTEXT_WINDOW;

    let active_preset_file = crate::engine::active_preset_file_for_runtime(engine_name)
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "engine runtime '{}' has no preset file mapping",
                engine_name
            )
        })?;
    let active_model_file =
        PathBuf::from(home).join(format!(".cache/tnk/{}", active_preset_file));
    let active_model_from_file = fs::read_to_string(&active_model_file)
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let fallback_model = active_model_from_file.unwrap_or_else(|| default_model.clone());

    let parsed_model = match crate::model::poll_loaded_model("127.0.0.1", port, 5, 1.0).await {
        Ok(Some(model)) => model,
        Ok(None) => fallback_model.clone(),
        Err(err) => {
            eprintln!(
                "warning: failed to poll loaded model from inference server, using fallback: {}",
                err
            );
            fallback_model.clone()
        }
    };
    let sanitized_model = if parsed_model.contains('/') || parsed_model.contains('\\') {
        std::path::Path::new(&parsed_model)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&parsed_model)
            .to_string()
    } else {
        parsed_model.clone()
    };
    let active_model = if sanitized_model.trim().is_empty() {
        fallback_model
    } else {
        sanitized_model
    };
    let model_ctx_window = crate::model::get_ctx_window("127.0.0.1", port)
        .await
        .unwrap_or(preset_ctx_hint);
    let ctx_window = std::cmp::max(model_ctx_window, preset_ctx_hint);

    Ok((active_model, ctx_window))
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
