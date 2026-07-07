use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

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

pub fn collect_regular_files_recursive(root: &Path) -> Result<Vec<PathBuf>, color_eyre::Report> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];

    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }

    Ok(files)
}

pub async fn compute_specs_revision_hash(
    script_path: &Path,
    lib_dir: &Path,
) -> Result<String, color_eyre::Report> {
    let mut files = vec![script_path.to_path_buf()];
    files.extend(collect_regular_files_recursive(lib_dir)?);
    files.sort();

    let mut shasum_cmd = Command::new("shasum");
    shasum_cmd.args(["-a", "256"]);
    for file in &files {
        shasum_cmd.arg(file);
    }

    let output = shasum_cmd.output().await?;
    if !output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to compute provision hash for script and library"
        ));
    }

    let mut second_pass = Command::new("shasum")
        .args(["-a", "256"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = second_pass.stdin.take() {
        stdin.write_all(&output.stdout).await?;
        stdin.shutdown().await?;
    }
    let second_output = second_pass.wait_with_output().await?;
    if !second_output.status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to finalize provision hash digest"
        ));
    }

    let stdout = String::from_utf8_lossy(&second_output.stdout);
    let hash = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid shasum output"))?;
    Ok(hash.to_string())
}

pub async fn resolve_active_model_and_ctx_impl(
    home: &str,
    port: u16,
    engine_name: &str,
) -> (String, u32) {
    let default_model = crate::config::load()
        .ok()
        .and_then(|cfg| cfg.default_engine_preset.filter(|m| !m.trim().is_empty()))
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| crate::engine::default_model_for_runtime(engine_name).to_string());
    let preset_ctx_hint = crate::model::DEFAULT_CONTEXT_WINDOW;

    let active_model_file = PathBuf::from(home).join(format!(
        ".cache/tnk/{}",
        crate::engine::active_preset_file_for_runtime(engine_name)
    ));
    let active_model_from_file = fs::read_to_string(&active_model_file)
        .await
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let fallback_model = active_model_from_file.unwrap_or_else(|| default_model.clone());

    let parsed_model = match crate::model::poll_loaded_model("127.0.0.1", port, 20, 1.0).await {
        Ok(model) => model,
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

    (active_model, ctx_window)
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
