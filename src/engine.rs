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

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::{collections::HashMap, fs as stdfs};

use tokio::fs;
use tokio::process::Command as AsyncCommand;
use tokio::signal::unix::{SignalKind, signal};

use crate::config;
use crate::model;

use shell_words;

#[derive(Clone, Copy)]
pub struct EngineRuntimeSpec {
    pub name: &'static str,
    pub executable: &'static str,
    pub pid_file_name: &'static str,
    pub active_preset_file: &'static str,
    pub log_stdout: &'static str,
    pub log_stderr: &'static str,
    pub default_model_id: &'static str,
    pub default_bind_host: &'static str,
}

#[derive(Debug, Clone)]
struct PresetSpec {
    name: String,
    runtime: Option<String>,
    model: String,
    extra: Vec<String>,
}

const MLXCEL_SPEC: EngineRuntimeSpec = EngineRuntimeSpec {
    name: "mlxcel",
    executable: "mlxcel-server",
    pid_file_name: "mlxcel-server.pid",
    active_preset_file: "active-preset-name-mlxcel",
    log_stdout: "mlxcel-server.log",
    log_stderr: "mlxcel-server-err.log",
    default_model_id: "mlx-community/Qwen3.6-35B-A3B-4bit",
    default_bind_host: "0.0.0.0",
};

const LLAMA_SPEC: EngineRuntimeSpec = EngineRuntimeSpec {
    name: "llama",
    executable: "llama-server",
    pid_file_name: "llama-server.pid",
    active_preset_file: "active-preset-name-llama",
    log_stdout: "llama-server.log",
    log_stderr: "llama-server-err.log",
    default_model_id: "unsloth/Qwen3.6-35B-A3B-GGUF/Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf",
    default_bind_host: "0.0.0.0",
};

const VLLM_MLX_SPEC: EngineRuntimeSpec = EngineRuntimeSpec {
    name: "vllm-mlx",
    executable: "vllm-mlx",
    pid_file_name: "vllm-mlx.pid",
    active_preset_file: "active-preset-name-vllm-mlx",
    log_stdout: "vllm-mlx.log",
    log_stderr: "vllm-mlx-err.log",
    default_model_id: "mlx-community/Qwen3.6-35B-A3B-4bit",
    default_bind_host: "0.0.0.0",
};

const SUPPORTED_RUNTIMES: [EngineRuntimeSpec; 3] = [LLAMA_SPEC, MLXCEL_SPEC, VLLM_MLX_SPEC];

pub fn runtime_spec(runtime: &str) -> Option<EngineRuntimeSpec> {
    match runtime {
        "llama" => Some(LLAMA_SPEC),
        "mlxcel" => Some(MLXCEL_SPEC),
        "vllm-mlx" => Some(VLLM_MLX_SPEC),
        _ => None,
    }
}

pub fn supports_runtime(runtime: &str) -> bool {
    runtime_spec(runtime).is_some()
}

pub fn supported_runtime_names() -> &'static [&'static str] {
    &["llama", "mlxcel", "vllm-mlx"]
}

pub fn resolve_runtime_for_profile(
    runtime_flag: Option<String>,
    configured_runtime: Option<String>,
    profile: Option<&str>,
) -> Result<String, color_eyre::Report> {
    if let Some(runtime) = runtime_flag {
        if !supports_runtime(&runtime) {
            return Err(color_eyre::eyre::eyre!(
                "unsupported engine runtime '{}' (supported: {})",
                runtime,
                supported_runtime_names().join(", ")
            ));
        }
        return Ok(runtime);
    }

    if let Some(profile_name) = profile
        && let Ok(Some(preset)) = resolve_preset(profile_name)
        && let Some(runtime) = preset.runtime
    {
        if supports_runtime(&runtime) {
            return Ok(runtime);
        }

        return Err(color_eyre::eyre::eyre!(
            "preset '{}' declares unsupported runtime '{}' (supported: {})",
            profile_name,
            runtime,
            supported_runtime_names().join(", ")
        ));
    }

    let runtime = configured_runtime.unwrap_or_else(|| "llama".to_string());
    if !supports_runtime(&runtime) {
        return Err(color_eyre::eyre::eyre!(
            "unsupported engine runtime '{}' (supported: {})",
            runtime,
            supported_runtime_names().join(", ")
        ));
    }

    Ok(runtime)
}

pub fn active_preset_file_for_runtime(runtime: &str) -> &'static str {
    runtime_spec(runtime)
        .unwrap_or(LLAMA_SPEC)
        .active_preset_file
}

pub fn default_model_for_runtime(runtime: &str) -> &'static str {
    runtime_spec(runtime).unwrap_or(LLAMA_SPEC).default_model_id
}

fn resolve_bind_host(
    spec: EngineRuntimeSpec,
    bind_host: Option<String>,
) -> Result<String, color_eyre::Report> {
    if let Some(host) = bind_host
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        return Ok(host);
    }

    if let Ok(cfg) = config::load()
        && let Some(host) = cfg
            .default_engine_bind_host
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    {
        return Ok(host);
    }

    Ok(spec.default_bind_host.to_string())
}

pub async fn verify_health(port: u16) -> bool {
    model::verify_health("127.0.0.1", port).await
}

fn parse_ini_file(path: &std::path::Path) -> Result<HashMap<String, String>, color_eyre::Report> {
    let mut map = HashMap::new();
    let content = stdfs::read_to_string(path)?;
    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with(';')
            || (line.starts_with('[') && line.ends_with(']'))
        {
            continue;
        }

        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let value = v.trim().trim_matches('"').trim_matches('\'').to_string();
        if !key.is_empty() && !value.is_empty() {
            map.insert(key, value);
        }
    }
    Ok(map)
}

fn preset_from_kv(name: &str, kv: &HashMap<String, String>) -> Option<PresetSpec> {
    let model = kv
        .get("model")
        .or_else(|| kv.get("model_id"))
        .or_else(|| kv.get("profile"))
        .cloned()?;
    let runtime = kv
        .get("runtime")
        .or_else(|| kv.get("engine_runtime"))
        .cloned();
    let extra = kv
        .get("extra")
        .map(|s| shell_words::split(s).unwrap_or_default())
        .unwrap_or_default();

    Some(PresetSpec {
        name: name.to_string(),
        runtime,
        model,
        extra,
    })
}

fn discover_presets() -> Result<Vec<PresetSpec>, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let root = PathBuf::from(home).join(".config/tnk/provider.d");
    if !root.is_dir() {
        return Ok(Vec::new());
    }

    let mut presets = Vec::new();
    let mut stack = vec![root];

    while let Some(dir) = stack.pop() {
        for entry in stdfs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
                continue;
            }

            if path.extension().and_then(|s| s.to_str()) != Some("ini") {
                continue;
            }

            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                continue;
            }

            let kv = parse_ini_file(&path)?;
            if let Some(preset) = preset_from_kv(&name, &kv) {
                presets.push(preset);
            }
        }
    }

    presets.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(presets)
}

fn resolve_preset(profile: &str) -> Result<Option<PresetSpec>, color_eyre::Report> {
    let trimmed = profile.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    let presets = discover_presets()?;
    Ok(presets.into_iter().find(|p| p.name == trimmed))
}

fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn pid_file_path(spec: EngineRuntimeSpec) -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join(format!(".cache/tnk/{}", spec.pid_file_name)))
}

fn kill_runtime_target(pid: u32, sig: i32) {
    let pgid = unsafe { libc::getpgid(pid as i32) };
    if pgid > 0 {
        unsafe {
            libc::kill(-pgid, sig);
        }
    } else {
        unsafe {
            libc::kill(pid as i32, sig);
        }
    }
}

fn last_errno() -> i32 {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        // SAFETY: libc exposes thread-local errno pointer for current thread.
        unsafe { *libc::__error() }
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        // SAFETY: libc exposes thread-local errno pointer for current thread.
        unsafe { *libc::__errno_location() }
    }
}

fn matches_runtime_process(spec: EngineRuntimeSpec, comm: &str, args: &str) -> bool {
    let executable = spec.executable;

    let comm_basename = comm.rsplit('/').next().unwrap_or(comm);
    if comm_basename == executable {
        return true;
    }

    let argv0 = args.split_whitespace().next().unwrap_or("");
    let argv0_basename = argv0.rsplit('/').next().unwrap_or(argv0);
    argv0_basename == executable
}

async fn is_runtime_pid(spec: EngineRuntimeSpec, pid: u32) -> bool {
    if !is_process_alive(pid) {
        return false;
    }

    let output = AsyncCommand::new("ps")
        .args(["-p", &pid.to_string(), "-o", "comm=", "-o", "args="])
        .output()
        .await;

    if let Ok(out) = output {
        let ps_output = String::from_utf8_lossy(&out.stdout);
        for line in ps_output.lines() {
            let trimmed = line.trim();
            let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
            if parts.len() < 2 {
                continue;
            }
            let comm = parts[0].trim();
            let args = parts[1];
            if matches_runtime_process(spec, comm, args) {
                return true;
            }
        }
    }

    false
}

async fn list_runtime_pids(spec: EngineRuntimeSpec) -> Vec<u32> {
    let own_pid = std::process::id();
    let output = AsyncCommand::new("ps")
        .args(["-axo", "pid=,comm=,args="])
        .output()
        .await;
    let mut pids = Vec::new();

    if let Ok(out) = output {
        let stdout = String::from_utf8_lossy(&out.stdout);
        for line in stdout.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let mut parts = trimmed.split_whitespace();
            let pid_str = match parts.next() {
                Some(v) => v,
                None => continue,
            };
            let comm = match parts.next() {
                Some(v) => v,
                None => continue,
            };
            let args = parts.collect::<Vec<_>>().join(" ");

            if let Ok(pid) = pid_str.parse::<u32>()
                && pid != own_pid
                && matches_runtime_process(spec, comm, &args)
            {
                pids.push(pid);
            }
        }
    }

    pids.sort_unstable();
    pids.dedup();
    pids
}

fn extract_model_from_args(args: &str) -> Option<String> {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    for (idx, token) in tokens.iter().enumerate() {
        if *token == "--model"
            && let Some(value) = tokens.get(idx + 1)
        {
            let model = value.trim_matches('"').trim_matches('\'').trim();
            if !model.is_empty() {
                return Some(model.to_string());
            }
        }

        if let Some(value) = token.strip_prefix("--model=") {
            let model = value.trim_matches('"').trim_matches('\'').trim();
            if !model.is_empty() {
                return Some(model.to_string());
            }
        }
    }

    for (idx, token) in tokens.iter().enumerate() {
        if *token == "serve"
            && let Some(value) = tokens.get(idx + 1)
        {
            let model = value.trim_matches('"').trim_matches('\'').trim();
            if !model.is_empty() && !model.starts_with('-') {
                return Some(model.to_string());
            }
        }
    }

    None
}

async fn detect_running_model_for_runtime(spec: EngineRuntimeSpec) -> Option<String> {
    let mut pids = list_runtime_pids(spec).await;
    pids.sort_by_key(|pid| std::cmp::Reverse(*pid));

    for pid in pids {
        let output = AsyncCommand::new("ps")
            .args(["-p", &pid.to_string(), "-o", "args="])
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            continue;
        }

        let args = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if args.is_empty() {
            continue;
        }

        if let Some(model) = extract_model_from_args(&args) {
            return Some(model);
        }
    }

    None
}

fn selected_sandbox_runtime() -> crate::sandbox::Runtime {
    config::load()
        .ok()
        .and_then(|cfg| crate::sandbox::resolve_runtime(None, cfg.default_sandbox_runtime).ok())
        .unwrap_or_default()
}

async fn list_container_sandboxes() -> Vec<(String, String)> {
    #[derive(serde::Deserialize)]
    struct ContainerConfiguration {
        #[serde(default)]
        id: Option<String>,
        #[serde(default, alias = "ID", alias = "Id")]
        id_alias: Option<String>,
    }

    #[derive(serde::Deserialize)]
    struct ContainerItem {
        #[serde(default)]
        id: Option<String>,
        #[serde(default, alias = "ID", alias = "Id")]
        id_alias: Option<String>,
        #[serde(default)]
        status: Option<serde_json::Value>,
        #[serde(default, alias = "Status")]
        status_alias: Option<serde_json::Value>,
        #[serde(default)]
        state: Option<String>,
        #[serde(default, alias = "State")]
        state_alias: Option<String>,
        #[serde(default)]
        configuration: Option<ContainerConfiguration>,
        #[serde(default, alias = "Configuration", alias = "config", alias = "Config")]
        configuration_alias: Option<ContainerConfiguration>,
    }

    let output = AsyncCommand::new("container")
        .args(["list", "--all", "--format", "json"])
        .output()
        .await;
    let Ok(out) = output else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }

    let Ok(items) = serde_json::from_slice::<Vec<ContainerItem>>(&out.stdout) else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    for item in items {
        let id = item
            .id
            .as_deref()
            .or(item.id_alias.as_deref())
            .or_else(|| {
                item.configuration
                    .as_ref()
                    .or(item.configuration_alias.as_ref())
                    .and_then(|c| c.id.as_deref().or(c.id_alias.as_deref()))
            })
            .unwrap_or_default()
            .to_string();
        if !id.starts_with("tnk-") || id == "tnk-services" || id == "tnk-searxng" {
            continue;
        }

        let status = item
            .status
            .as_ref()
            .or(item.status_alias.as_ref())
            .and_then(|v| {
                if let Some(s) = v.as_str() {
                    return Some(s);
                }
                v.get("state")
                    .or_else(|| v.get("State"))
                    .and_then(|s| s.as_str())
            })
            .or(item.state.as_deref())
            .or(item.state_alias.as_deref())
            .unwrap_or("unknown")
            .to_string();

        let token = id.strip_prefix("tnk-").unwrap_or(&id).to_string();
        rows.push((token, status));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

async fn list_lima_sandboxes() -> Vec<(String, String)> {
    let output = AsyncCommand::new("limactl")
        .args(["list", "--format", "{{.Name}}\t{{.Status}}"])
        .output()
        .await;
    let Ok(out) = output else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }

    let mut rows: Vec<(String, String)> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let id = parts.next()?.trim();
            let status = parts.next().unwrap_or("unknown").trim().to_lowercase();
            if !id.starts_with("tnk-")
                || id == "tnk-services"
                || id == "tnk-searxng"
                || id == "tnk-config"
            {
                return None;
            }
            Some((id.strip_prefix("tnk-").unwrap_or(id).to_string(), status))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

fn expand_model_path(model: &str) -> String {
    if model.starts_with('/') {
        return model.to_string();
    }

    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return model.to_string(),
    };

    if let Some(rest) = model.strip_prefix("~/") {
        return format!("{}/{}", home, rest);
    }

    let model_dir = config::load()
        .ok()
        .and_then(|cfg| cfg.model_dir)
        .map(|d| {
            if let Some(rest) = d.strip_prefix("~/") {
                format!("{}/{}", home, rest)
            } else {
                d
            }
        })
        .unwrap_or_else(|| format!("{}/opt/models", home));

    format!("{}/{}", model_dir, model)
}

fn resolve_model_id_for_runtime(
    spec: EngineRuntimeSpec,
    preset: Option<String>,
) -> (String, Vec<String>) {
    let trimmed = preset
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());

    if let Some(ref name) = trimmed
        && let Ok(Some(p)) = resolve_preset(name)
    {
        return (expand_model_path(&p.model), p.extra);
    }

    if let Ok(cfg) = config::load()
        && let Some(name) = cfg
            .default_engine_preset
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
        && let Ok(Some(p)) = resolve_preset(&name)
    {
        return (expand_model_path(&p.model), p.extra);
    }

    let model = trimmed.unwrap_or_else(|| spec.default_model_id.to_string());
    (expand_model_path(&model), Vec::new())
}

fn runtime_args(
    spec: EngineRuntimeSpec,
    model_id: &str,
    host: &str,
    port: u16,
    extra: &[String],
) -> Vec<String> {
    let mut args = match spec.name {
        "vllm-mlx" => vec![
            "serve".to_string(),
            model_id.to_string(),
            "--host".to_string(),
            host.to_string(),
            "--port".to_string(),
            port.to_string(),
        ],
        _ => vec![
            "--model".to_string(),
            model_id.to_string(),
            "--host".to_string(),
            host.to_string(),
            "--port".to_string(),
            port.to_string(),
        ],
    };
    args.extend_from_slice(extra);
    args
}

fn build_command(
    spec: EngineRuntimeSpec,
    model_id: &str,
    host: &str,
    port: u16,
    extra: &[String],
) -> std::process::Command {
    let mut cmd = std::process::Command::new(spec.executable);
    cmd.args(runtime_args(spec, model_id, host, port, extra));
    cmd
}

pub async fn running_runtime() -> Option<EngineRuntimeSpec> {
    for spec in SUPPORTED_RUNTIMES {
        if is_running_for_runtime(spec).await {
            return Some(spec);
        }
    }
    None
}

pub async fn is_running() -> bool {
    for spec in SUPPORTED_RUNTIMES {
        if is_running_for_runtime(spec).await {
            return true;
        }
    }

    false
}

async fn is_running_for_runtime(spec: EngineRuntimeSpec) -> bool {
    let pid_file = match pid_file_path(spec) {
        Some(path) => path,
        None => return !list_runtime_pids(spec).await.is_empty(),
    };

    if pid_file.exists() {
        let pid_bytes = match fs::read_to_string(&pid_file).await {
            Ok(b) => b,
            Err(_) => return !list_runtime_pids(spec).await.is_empty(),
        };

        let pid = match pid_bytes.trim().parse::<u32>() {
            Ok(p) => p,
            Err(_) => {
                fs::remove_file(&pid_file).await.ok();
                return !list_runtime_pids(spec).await.is_empty();
            }
        };

        if is_runtime_pid(spec, pid).await {
            return true;
        }

        fs::remove_file(&pid_file).await.ok();
    }

    !list_runtime_pids(spec).await.is_empty()
}

pub async fn start(
    runtime: &str,
    preset: Option<String>,
    port: u16,
    bind_host: Option<String>,
    foreground: bool,
) -> Result<(), color_eyre::Report> {
    let spec = runtime_spec(runtime).ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "unsupported engine runtime '{}' (supported: {})",
            runtime,
            supported_runtime_names().join(", ")
        )
    })?;

    let _engine_lock = crate::lifecycle::acquire("engine", Duration::from_secs(20)).await?;
    let (model_id, extra) = resolve_model_id_for_runtime(spec, preset);
    let bind_host = resolve_bind_host(spec, bind_host)?;

    let home = std::env::var("HOME")?;
    let cache_dir = PathBuf::from(&home).join(".cache/tnk");
    fs::create_dir_all(&cache_dir).await?;

    let log_stdout = cache_dir.join(spec.log_stdout);
    let log_stderr = cache_dir.join(spec.log_stderr);
    let pid_file = cache_dir.join(spec.pid_file_name);

    let mut existing_pids: Vec<(EngineRuntimeSpec, u32)> = Vec::new();
    for runtime_spec in SUPPORTED_RUNTIMES {
        for pid in list_runtime_pids(runtime_spec).await {
            existing_pids.push((runtime_spec, pid));
        }
    }

    if !existing_pids.is_empty() {
        eprintln!("warning: found running engine process(es), stopping before start");
        for (running_spec, pid) in existing_pids {
            stop_pid(running_spec, pid).await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    fs::remove_file(&pid_file).await.ok();

    if foreground {
        crate::ui::log_info(&format!("{} starting on {}:{}", spec.name, bind_host, port));

        let mut child = AsyncCommand::new(spec.executable);
        child
            .args(runtime_args(spec, &model_id, &bind_host, port, &extra))
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());

        let mut child = child.spawn()?;
        let child_pid = child.id().unwrap_or_default();

        let active_model_path = cache_dir.join(spec.active_preset_file);
        fs::write(&active_model_path, &model_id).await?;

        let mut sigterm = signal(SignalKind::terminate())?;
        let mut sigint = signal(SignalKind::interrupt())?;
        let mut shutdown_requested = false;

        loop {
            tokio::select! {
                maybe_status = child.wait() => {
                    let status = maybe_status?;
                    if !status.success() {
                        eprintln!("error: server exited with code {}", status);
                    }
                    return Ok(());
                }
                _ = sigterm.recv() => {
                    if child_pid != 0 {
                        if !shutdown_requested {
                            crate::ui::log_info(&format!(
                                "forwarding SIGTERM to {} pid {}",
                                spec.name, child_pid
                            ));
                            kill_runtime_target(child_pid, libc::SIGTERM);
                            shutdown_requested = true;
                        } else {
                            eprintln!("warning: second signal received, forwarding SIGKILL to {} pid {}", spec.name, child_pid);
                            kill_runtime_target(child_pid, libc::SIGKILL);
                        }
                    }
                }
                _ = sigint.recv() => {
                    if child_pid != 0 {
                        if !shutdown_requested {
                            crate::ui::log_info(&format!(
                                "forwarding SIGINT to {} pid {}",
                                spec.name, child_pid
                            ));
                            kill_runtime_target(child_pid, libc::SIGTERM);
                            shutdown_requested = true;
                        } else {
                            eprintln!("warning: second signal received, forwarding SIGKILL to {} pid {}", spec.name, child_pid);
                            kill_runtime_target(child_pid, libc::SIGKILL);
                        }
                    }
                }
            }
        }
    }

    eprintln!(
        "{} starting (background) on {}:{}",
        spec.name, bind_host, port
    );

    let stdout_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_stdout)?;
    let stderr_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_stderr)?;

    let mut cmd = build_command(spec, &model_id, &bind_host, port, &extra);
    cmd.stdout(stdout_file).stderr(stderr_file);

    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::from_raw_os_error(last_errno()));
            }
            Ok(())
        });
    }

    match cmd.spawn() {
        Ok(c) => {
            let pid = c.id();
            fs::write(&pid_file, pid.to_string()).await?;
            let active_model_path = cache_dir.join(spec.active_preset_file);
            fs::write(&active_model_path, &model_id).await?;
            crate::ui::log_info(&format!("started pid {}", pid));
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(color_eyre::eyre::eyre!(
            "{} executable not found in PATH (expected command: '{}')",
            spec.name,
            spec.executable
        )),
        Err(e) => Err(e.into()),
    }
}

pub async fn stop(runtime: &str) -> Result<(), color_eyre::Report> {
    let spec = runtime_spec(runtime).ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "unsupported engine runtime '{}' (supported: {})",
            runtime,
            supported_runtime_names().join(", ")
        )
    })?;

    let _engine_lock = crate::lifecycle::acquire("engine", Duration::from_secs(20)).await?;
    let home = std::env::var("HOME")?;
    let cache_dir = PathBuf::from(&home).join(".cache/tnk");
    let pid_file = cache_dir.join(spec.pid_file_name);

    let mut target_pids = Vec::new();
    if pid_file.exists()
        && let Ok(pid_bytes) = fs::read_to_string(&pid_file).await
        && let Ok(pid) = pid_bytes.trim().parse::<u32>()
    {
        if is_runtime_pid(spec, pid).await {
            target_pids.push(pid);
        } else {
            eprintln!(
                "warning: stale pid file for non-{} process {}, removing",
                spec.name, pid
            );
        }
    }

    for pid in list_runtime_pids(spec).await {
        if !target_pids.contains(&pid) {
            target_pids.push(pid);
        }
    }

    if target_pids.is_empty() {
        fs::remove_file(&pid_file).await.ok();
        return Ok(());
    }

    for pid in target_pids {
        stop_pid(spec, pid).await;
    }

    fs::remove_file(&pid_file).await.ok();
    Ok(())
}

pub async fn stop_all() -> Result<(), color_eyre::Report> {
    for spec in SUPPORTED_RUNTIMES {
        stop(spec.name).await?;
    }
    Ok(())
}

async fn stop_pid(spec: EngineRuntimeSpec, pid: u32) {
    if !is_runtime_pid(spec, pid).await {
        return;
    }

    crate::ui::log_info(&format!("stopping {} pid {}", spec.name, pid));
    kill_runtime_target(pid, libc::SIGTERM);

    let mut died = false;
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        if !is_runtime_pid(spec, pid).await {
            died = true;
            break;
        }
    }

    if died {
        crate::ui::log_info(&format!("stopped {} pid {}", spec.name, pid));
    } else {
        eprintln!(
            "warning: sigterm failed for {} pid {}, escalating to sigkill",
            spec.name, pid
        );
        kill_runtime_target(pid, libc::SIGKILL);
        crate::ui::log_info(&format!("killed {} pid {}", spec.name, pid));
    }
}

pub async fn status(output: crate::OutputFormat) -> Result<(), color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let cache_dir = PathBuf::from(&home).join(".cache/tnk");
    let server_port = config::load()
        .ok()
        .and_then(|cfg| cfg.server_port)
        .unwrap_or(8080);

    let mut runtimes = Vec::new();
    for spec in SUPPORTED_RUNTIMES {
        let active_model_from_cache = fs::read_to_string(cache_dir.join(spec.active_preset_file))
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let running = is_running_for_runtime(spec).await;
        let active_model = if active_model_from_cache.is_empty() && running {
            let from_process = detect_running_model_for_runtime(spec)
                .await
                .unwrap_or_default();
            if !from_process.is_empty() {
                from_process
            } else {
                model::poll_loaded_model("127.0.0.1", server_port, 1, 0.0)
                    .await
                    .unwrap_or_default()
            }
        } else {
            active_model_from_cache
        };
        let configured = !active_model.is_empty() || running;
        runtimes.push((spec, active_model, running, configured));
    }

    let any_running = runtimes.iter().any(|(_, _, running, _)| *running);
    let any_model = runtimes.iter().any(|(_, _, _, configured)| *configured);

    let overall_state = if !any_model {
        "not_configured"
    } else if any_running {
        "running"
    } else {
        "configured_stopped"
    };

    if output == crate::OutputFormat::Json || output == crate::OutputFormat::Ndjson {
        let mut runtimes_payload = serde_json::Map::new();
        for (spec, active_model, running, _) in &runtimes {
            runtimes_payload.insert(
                spec.name.to_string(),
                serde_json::json!({
                    "model": if active_model.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(active_model.clone()) },
                    "server_running": *running,
                }),
            );
        }

        let payload = serde_json::json!({
            "state": overall_state,
            "runtimes": runtimes_payload,
        });
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    if !any_model {
        eprintln!("tnk: not configured");
    } else if any_running {
        eprintln!("tnk: running");
    } else {
        eprintln!("tnk: configured, stopped");
    }

    for (spec, active_model, running, configured) in &runtimes {
        print_runtime_status(spec.name, active_model, *running, *configured);
    }

    match selected_sandbox_runtime() {
        crate::sandbox::Runtime::Container => {
            let services_container = "tnk-services";
            let searxng_container = "tnk-searxng";
            let container_items = AsyncCommand::new("container")
                .args(["list", "--all", "--format", "json"])
                .output()
                .await
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout).ok())
                .unwrap_or_default();

            let mut services_status: Option<&str> = None;
            for item in &container_items {
                let id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        item.get("configuration")
                            .and_then(|v| v.get("id"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or_default();
                if id != services_container {
                    continue;
                }
                let state = item
                    .get("status")
                    .and_then(|v| v.get("state"))
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("state").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");
                services_status = Some(if state.eq_ignore_ascii_case("running") {
                    "running"
                } else {
                    "stopped"
                });
                break;
            }

            if let Some(status) = services_status {
                let provision_output = AsyncCommand::new("container")
                    .args([
                        "exec",
                        services_container,
                        "bash",
                        "-c",
                        "test -f $HOME/mcp-stdio.sh && test -f $HOME/.local/lib/node_modules/mcp-searxng/dist/cli.js",
                    ])
                    .output()
                    .await
                    .ok();

                let mcp_state = match (status, provision_output) {
                    ("running", Some(out)) if out.status.success() => "running",
                    ("running", _) => "degraded",
                    _ => "stopped",
                };
                print_status_row("mcp (container)", mcp_state, "");
            }

            let mut searxng_status: Option<&str> = None;
            for item in &container_items {
                let id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        item.get("configuration")
                            .and_then(|v| v.get("id"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or_default();
                if id != searxng_container {
                    continue;
                }
                let state = item
                    .get("status")
                    .and_then(|v| v.get("state"))
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("state").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");
                searxng_status = Some(if state.eq_ignore_ascii_case("running") {
                    "running"
                } else {
                    "stopped"
                });
                break;
            }

            if let Some(status) = searxng_status {
                print_status_row("searxng (container)", status, "");
            }

            for (token, status) in &list_container_sandboxes().await {
                let label = format!("sandbox (container) {}", token);
                print_status_row(&label, status, "");
            }
        }
        crate::sandbox::Runtime::Lima => {
            let services_running = AsyncCommand::new("limactl")
                .args(["list", "--format", "{{.Status}}", "tnk-services"])
                .output()
                .await
                .ok()
                .filter(|out| out.status.success())
                .map(|out| {
                    String::from_utf8_lossy(&out.stdout)
                        .trim()
                        .eq_ignore_ascii_case("running")
                })
                .unwrap_or(false);
            let mcp_state = if services_running {
                "running"
            } else {
                "stopped"
            };
            print_status_row("mcp (lima)", mcp_state, "");
            print_status_row("searxng (lima)", mcp_state, "");

            for (token, status) in &list_lima_sandboxes().await {
                let label = format!("sandbox (lima) {}", token);
                print_status_row(&label, status, "");
            }
        }
    }

    Ok(())
}

const LABEL_WIDTH: usize = 18;

fn print_status_row(label: &str, state: &str, detail: &str) {
    if detail.is_empty() {
        eprintln!("  {:<width$}  {}", label, state, width = LABEL_WIDTH);
    } else {
        eprintln!(
            "  {:<width$}  {}  {}",
            label,
            state,
            detail,
            width = LABEL_WIDTH
        );
    }
}

fn print_runtime_status(runtime: &str, model_id: &str, is_running: bool, configured: bool) {
    let label = format!("engine {}", runtime);
    if !configured {
        print_status_row(&label, "stopped", "");
    } else if is_running {
        let detail = if model_id.is_empty() { "" } else { model_id };
        print_status_row(&label, "running", detail);
    } else if model_id.is_empty() {
        print_status_row(&label, "stopped", "");
    } else {
        let detail = format!("last: {}", model_id);
        print_status_row(&label, "stopped", &detail);
    }
}

pub async fn print_status() -> Result<(), color_eyre::Report> {
    let server_port = config::load()
        .ok()
        .and_then(|cfg| cfg.server_port)
        .unwrap_or(8080);

    if let Some(spec) = running_runtime().await {
        let model = detect_running_model_for_runtime(spec)
            .await
            .unwrap_or_default();
        let detail = if model.is_empty() {
            model::poll_loaded_model("127.0.0.1", server_port, 1, 0.0)
                .await
                .unwrap_or_default()
        } else {
            model
        };
        print_status_row("engine", "running", &detail);
    }

    match selected_sandbox_runtime() {
        crate::sandbox::Runtime::Container => {
            let container_items = AsyncCommand::new("container")
                .args(["list", "--all", "--format", "json"])
                .output()
                .await
                .ok()
                .filter(|out| out.status.success())
                .and_then(|out| serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout).ok())
                .unwrap_or_default();

            let mcp_row = container_items.iter().find(|item| {
                let id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        item.get("configuration")
                            .and_then(|v| v.get("id"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or_default();
                id == "tnk-services"
            });

            if let Some(item) = mcp_row {
                let state = item
                    .get("status")
                    .and_then(|v| v.get("state"))
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("state").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");
                let mcp_state = if state.eq_ignore_ascii_case("running") {
                    let provision_output = AsyncCommand::new("container")
                        .args([
                            "exec", "tnk-services", "bash", "-c",
                            "test -f $HOME/mcp-stdio.sh && test -f $HOME/.local/lib/node_modules/mcp-searxng/dist/cli.js",
                        ])
                        .output()
                        .await
                        .ok();
                    if provision_output
                        .map(|out| out.status.success())
                        .unwrap_or(false)
                    {
                        "running"
                    } else {
                        "degraded"
                    }
                } else {
                    "stopped"
                };
                if mcp_state != "stopped" {
                    print_status_row("mcp (container)", mcp_state, "");
                }
            }

            let searxng_row = container_items.iter().find(|item| {
                let id = item
                    .get("id")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        item.get("configuration")
                            .and_then(|v| v.get("id"))
                            .and_then(|v| v.as_str())
                    })
                    .unwrap_or_default();
                id == "tnk-searxng"
            });

            if let Some(item) = searxng_row {
                let state = item
                    .get("status")
                    .and_then(|v| v.get("state"))
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("state").and_then(|v| v.as_str()))
                    .unwrap_or("unknown");
                if state.eq_ignore_ascii_case("running") {
                    print_status_row("searxng (container)", "running", "");
                }
            }

            let active_sandboxes = list_container_sandboxes().await;
            for (token, status) in &active_sandboxes {
                if status.eq_ignore_ascii_case("running") {
                    print_status_row(&format!("sandbox (container) {}", token), "running", "");
                }
            }
        }
        crate::sandbox::Runtime::Lima => {
            let services_running = AsyncCommand::new("limactl")
                .args(["list", "--format", "{{.Status}}", "tnk-services"])
                .output()
                .await
                .ok()
                .filter(|out| out.status.success())
                .map(|out| {
                    String::from_utf8_lossy(&out.stdout)
                        .trim()
                        .eq_ignore_ascii_case("running")
                })
                .unwrap_or(false);
            if services_running {
                print_status_row("mcp (lima)", "running", "");
                print_status_row("searxng (lima)", "running", "");
            }

            let active_sandboxes = list_lima_sandboxes().await;
            for (token, status) in &active_sandboxes {
                if status.eq_ignore_ascii_case("running") {
                    print_status_row(&format!("sandbox (lima) {}", token), "running", "");
                }
            }
        }
    }

    Ok(())
}

pub fn presets_for_runtime(
    runtime: &str,
    output: crate::OutputFormat,
) -> Result<(), color_eyre::Report> {
    let spec = runtime_spec(runtime).ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "unsupported engine runtime '{}' (supported: {})",
            runtime,
            supported_runtime_names().join(", ")
        )
    })?;

    let presets: Vec<PresetSpec> = discover_presets()?
        .into_iter()
        .filter(|p| p.runtime.as_deref().unwrap_or(spec.name) == spec.name)
        .collect();

    if output == crate::OutputFormat::Json {
        let payload: Vec<serde_json::Value> = presets
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "model": p.model,
                    "runtime": p.runtime.as_deref().unwrap_or(spec.name),
                    "extra": p.extra,
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&payload)?);
        return Ok(());
    }

    if output == crate::OutputFormat::Ndjson {
        for p in &presets {
            let payload = serde_json::json!({
                "name": p.name,
                "model": p.model,
                "runtime": p.runtime.as_deref().unwrap_or(spec.name),
                "extra": p.extra,
            });
            println!("{}", serde_json::to_string(&payload)?);
        }
        return Ok(());
    }

    for p in &presets {
        if p.extra.is_empty() {
            eprintln!("{}  {}", p.name, p.model);
        } else {
            eprintln!("{}  {}  [{}]", p.name, p.model, p.extra.join(" "));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::extract_model_from_args;

    #[test]
    fn extracts_model_from_flag_pair() {
        let args = "llama-server --model /tmp/model.gguf --host 0.0.0.0";
        assert_eq!(
            extract_model_from_args(args).as_deref(),
            Some("/tmp/model.gguf")
        );
    }

    #[test]
    fn extracts_model_from_flag_assignment() {
        let args = "llama-server --model=/tmp/model.gguf --port 8080";
        assert_eq!(
            extract_model_from_args(args).as_deref(),
            Some("/tmp/model.gguf")
        );
    }

    #[test]
    fn extracts_vllm_model_from_serve_positional() {
        let args = "vllm-mlx serve mlx-community/Qwen3.6-35B-A3B-4bit --port 8080";
        assert_eq!(
            extract_model_from_args(args).as_deref(),
            Some("mlx-community/Qwen3.6-35B-A3B-4bit")
        );
    }
}
