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

use std::io::IsTerminal;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn has_label(item: &serde_json::Value, key: &str, expected: &str) -> bool {
    item.get("configuration")
        .and_then(|v| v.get("labels"))
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .is_some_and(|v| v == expected)
}

fn container_matches_id(item: &serde_json::Value, container_id: &str) -> bool {
    item.get("id")
        .or_else(|| item.get("ID"))
        .or_else(|| item.get("Id"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            item.get("configuration")
                .or_else(|| item.get("Configuration"))
                .or_else(|| item.get("config"))
                .or_else(|| item.get("Config"))
                .and_then(|v| v.get("id").or_else(|| v.get("ID")).or_else(|| v.get("Id")))
                .and_then(|v| v.as_str())
        })
        == Some(container_id)
}

const SEARXNG_CONFIG_REV: &str = "v3";

fn generate_searxng_secret() -> Result<String, color_eyre::Report> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0_u8; 32];
    let mut source = std::fs::File::open("/dev/urandom")?;
    source.read_exact(&mut bytes)?;

    let secret: String = bytes
        .iter()
        .map(|byte| ALPHABET[usize::from(*byte) % ALPHABET.len()] as char)
        .collect();
    Ok(secret)
}

fn discover_container_gateway() -> Option<String> {
    let output = Command::new("container")
        .args(["network", "list", "--format", "json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let entries = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout).ok()?;
    for entry in entries {
        let candidates = [
            entry.get("gateway"),
            entry.get("Gateway"),
            entry.get("status").and_then(|v| v.get("ipv4Gateway")),
            entry.get("status").and_then(|v| v.get("ipv6Gateway")),
            entry.get("status").and_then(|v| v.get("gateway")),
            entry.get("Status").and_then(|v| v.get("IPv4Gateway")),
            entry.get("Status").and_then(|v| v.get("IPv6Gateway")),
            entry.get("Status").and_then(|v| v.get("Gateway")),
            entry.get("ipam").and_then(|v| v.get("gateway")),
            entry.get("IPAM").and_then(|v| v.get("Gateway")),
            entry
                .get("subnets")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("gateway")),
            entry
                .get("Subnets")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("Gateway")),
        ];

        for candidate in candidates.into_iter().flatten() {
            if let Some(ip) = candidate.as_str()
                && !ip.trim().is_empty()
            {
                return Some(ip.trim().to_string());
            }
        }
    }

    None
}

fn resolve_host_gateway() -> Result<String, color_eyre::Report> {
    if let Ok(cfg) = crate::config::load()
        && let Some(configured) = cfg.container_host_gateway
    {
        let host = configured.trim().to_string();
        if host.is_empty() {
            return Err(color_eyre::eyre::eyre!(
                "container_host_gateway is empty in config"
            ));
        }
        return Ok(host);
    }
    if let Ok(env_host) = std::env::var("TNK_CONTAINER_HOST_GATEWAY")
        && !env_host.trim().is_empty()
    {
        return Ok(env_host.trim().to_string());
    }
    discover_container_gateway().ok_or_else(|| {
        color_eyre::eyre::eyre!(
            "could not determine container host gateway; set TNK_CONTAINER_HOST_GATEWAY or container_host_gateway in config"
        )
    })
}

fn resolve_services_runtime(
    runtime_flag: Option<String>,
) -> Result<crate::sandbox::Runtime, color_eyre::Report> {
    let cfg = crate::config::load()?;
    crate::sandbox::resolve_runtime(runtime_flag, cfg.default_sandbox_runtime)
}

pub async fn run(action: crate::ServicesCommands) -> Result<(), color_eyre::Report> {
    match action {
        crate::ServicesCommands::Start { dry_run, runtime } => start(dry_run, runtime).await?,
        crate::ServicesCommands::Stop { dry_run, runtime } => stop(dry_run, runtime).await?,
        crate::ServicesCommands::Status { output, runtime } => status(output, runtime).await?,
        crate::ServicesCommands::Restart { dry_run, runtime } => restart(dry_run, runtime).await?,
        crate::ServicesCommands::Delete {
            yes,
            dry_run,
            runtime,
        } => delete(yes, dry_run, runtime).await?,
    }
    Ok(())
}

pub async fn start(dry_run: bool, runtime_flag: Option<String>) -> Result<(), color_eyre::Report> {
    match resolve_services_runtime(runtime_flag)? {
        crate::sandbox::Runtime::Container => start_container(dry_run).await,
        crate::sandbox::Runtime::Lima => start_lima(dry_run).await,
    }
}

async fn start_container(dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services start");
        return Ok(());
    }

    ensure_runtime_exclusive_container()?;

    let container_id = "tnk-services";
    let searxng_container_id = "tnk-searxng";
    let home = std::env::var("HOME")?;
    let searxng_settings_path = ensure_searxng_settings(&home)?;

    if is_container_exists_any(searxng_container_id)
        && !container_has_label(searxng_container_id, "tnk.config-rev", SEARXNG_CONFIG_REV)
    {
        crate::ui::log_info("recreating tnk-searxng container for updated config");
        let delete_status = Command::new("container")
            .args(["delete", "--force", searxng_container_id])
            .status()?;
        if !delete_status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to recreate tnk-searxng container"
            ));
        }
    }

    if !is_container_exists_any(searxng_container_id) {
        crate::ui::log_info("creating tnk-searxng container");
        let settings_mount = format!(
            "{}:/etc/searxng/settings.yml",
            searxng_settings_path.to_string_lossy()
        );
        let searxng_secret = generate_searxng_secret()?;
        let status = Command::new("container")
            .args([
                "create",
                "--name",
                searxng_container_id,
                "--detach",
                "--label",
                "tnk.managed=true",
                "--label",
                "tnk.owner=services",
                "--label",
                "tnk.config-rev=v3",
                "--publish",
                "18766:8080",
                "--volume",
                &settings_mount,
                "--env",
            ])
            .arg(format!("SEARXNG_SECRET={}", searxng_secret))
            .arg("docker.io/searxng/searxng:latest")
            .status()?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to create tnk-searxng container"
            ));
        }
    }

    if !is_container_running(searxng_container_id) {
        crate::ui::log_info("starting tnk-searxng container");
        let status = Command::new("container")
            .args(["start", searxng_container_id])
            .status()?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to start tnk-searxng container"
            ));
        }
    }

    if !is_container_exists_any(container_id) {
        crate::ui::log_info("creating tnk-services container");
        let status = Command::new("container")
            .args([
                "create",
                "--name",
                container_id,
                "--detach",
                "--label",
                "tnk.managed=true",
                "--label",
                "tnk.owner=services",
                "--publish",
                "127.0.0.1:18765:18765",
                "--workdir",
                "/tmp",
                "debian:13-slim",
                "sh",
                "-lc",
                "while true; do sleep 3600; done",
            ])
            .status()?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to create tnk-services container"
            ));
        }
    }

    if !is_container_running(container_id) {
        crate::ui::log_info("starting tnk-services container");
        let status = Command::new("container")
            .args(["start", container_id])
            .status()?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to start tnk-services container"
            ));
        }
    } else {
        crate::ui::log_info("tnk-services container already running");
        return Ok(());
    }

    ensure_services_runtime_baseline(container_id)?;

    if !is_container_provisioned(container_id) {
        let host_gateway = resolve_host_gateway()?;
        let searxng_url = format!("http://{}:18766", host_gateway);

        let mut cp_cmd = Command::new("container");
        cp_cmd.args([
            "copy",
            &format!(
                "{}/.config/tnk/sandbox.d/container/provision.d/tnk-services.sh",
                home
            ),
            &format!("{}:/tmp/tnk-services.sh", container_id),
        ]);
        if !cp_cmd.status()?.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to copy provision script into services container"
            ));
        }

        let mut cp_lib_cmd = Command::new("container");
        cp_lib_cmd.args([
            "copy",
            &format!("{}/.config/tnk/sandbox.d/container/provision.d/lib", home),
            &format!("{}:/tmp", container_id),
        ]);
        if !cp_lib_cmd.status()?.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to copy provision library into services container"
            ));
        }

        let mut provision_cmd = Command::new("container");
        provision_cmd.args([
            "exec",
            "--env",
            &format!("TNK_SEARXNG_URL={}", searxng_url),
            "--user",
            "tnk",
            container_id,
            "bash",
            "/tmp/tnk-services.sh",
        ]);
        if provision_cmd.status()?.success() {
            crate::ui::log_info("tnk-services container provisioned");
        } else {
            return Err(color_eyre::eyre::eyre!(
                "tnk-services container provisioning failed"
            ));
        }
    }

    crate::ui::log_info("searxng:  http://127.0.0.1:18766 (browser access)");
    crate::ui::log_info("mcp:      stdio bridge via tnk-services over exec");

    Ok(())
}

pub async fn stop(dry_run: bool, runtime_flag: Option<String>) -> Result<(), color_eyre::Report> {
    match resolve_services_runtime(runtime_flag)? {
        crate::sandbox::Runtime::Container => stop_container(dry_run).await,
        crate::sandbox::Runtime::Lima => stop_lima(dry_run).await,
    }
}

async fn stop_container(dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services stop");
        return Ok(());
    }

    let container_id = "tnk-services";
    let searxng_container_id = "tnk-searxng";
    if !is_container_exists(container_id) && !is_container_exists(searxng_container_id) {
        return Ok(());
    }

    if is_container_exists(container_id) {
        let output = Command::new("container")
            .args(["stop", container_id])
            .output()
            .ok();

        match output {
            Some(out) if out.status.success() => {
                crate::ui::log_info(&format!("stopped {}", container_id))
            }
            Some(_) | None => eprintln!("warning: failed to stop {}", container_id),
        }
    }

    if is_container_exists(searxng_container_id) {
        let output = Command::new("container")
            .args(["stop", searxng_container_id])
            .output()
            .ok();

        match output {
            Some(out) if out.status.success() => {
                crate::ui::log_info(&format!("stopped {}", searxng_container_id))
            }
            Some(_) | None => eprintln!("warning: failed to stop {}", searxng_container_id),
        }
    }

    Ok(())
}

pub async fn status(
    output: crate::OutputFormat,
    runtime_flag: Option<String>,
) -> Result<(), color_eyre::Report> {
    match resolve_services_runtime(runtime_flag)? {
        crate::sandbox::Runtime::Container => status_container(output).await,
        crate::sandbox::Runtime::Lima => status_lima(output).await,
    }
}

async fn status_container(output: crate::OutputFormat) -> Result<(), color_eyre::Report> {
    let container_id = "tnk-services";
    let searxng_container_id = "tnk-searxng";

    if !is_container_exists(container_id) && !is_container_exists(searxng_container_id) {
        return Ok(());
    }

    let status = if is_container_running(container_id) {
        "running"
    } else {
        "stopped"
    };
    let searxng_status = if is_container_exists(searxng_container_id) {
        Some(if is_container_running(searxng_container_id) {
            "running"
        } else {
            "stopped"
        })
    } else {
        None
    };
    let provisioned = is_container_running(container_id) && is_container_provisioned(container_id);

    match output {
        crate::OutputFormat::Text => {
            eprintln!("tnk-services (container): {}", status);
            eprintln!(
                "tnk-searxng (container): {}",
                searxng_status.unwrap_or("missing")
            );
            eprintln!("provisioned: {}", if provisioned { "yes" } else { "no" });
        }
        crate::OutputFormat::Json | crate::OutputFormat::Ndjson => {
            let payload = serde_json::json!({
                "name": container_id,
                "runtime": "container",
                "status": status,
                "searxng": searxng_status,
                "provisioned": provisioned
            });
            println!("{}", serde_json::to_string(&payload)?);
        }
    }

    Ok(())
}

pub async fn restart(
    dry_run: bool,
    runtime_flag: Option<String>,
) -> Result<(), color_eyre::Report> {
    match resolve_services_runtime(runtime_flag)? {
        crate::sandbox::Runtime::Container => restart_container(dry_run).await,
        crate::sandbox::Runtime::Lima => restart_lima(dry_run).await,
    }
}

async fn restart_container(dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services restart");
        return Ok(());
    }
    stop_container(false).await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    start_container(false).await?;
    Ok(())
}

fn lima_services_exist_any() -> bool {
    lima_instance_exists("tnk-services") || lima_instance_exists("tnk-searxng")
}

fn container_services_exist_any() -> bool {
    is_container_exists_any("tnk-services") || is_container_exists_any("tnk-searxng")
}

fn ensure_runtime_exclusive_container() -> Result<(), color_eyre::Report> {
    if lima_services_exist_any() {
        return Err(color_eyre::eyre::eyre!(
            "lima services are present; switch to lima runtime or delete lima services first"
        ));
    }
    Ok(())
}

fn ensure_runtime_exclusive_lima() -> Result<(), color_eyre::Report> {
    if container_services_exist_any() {
        return Err(color_eyre::eyre::eyre!(
            "container services are present; switch to container runtime or delete container services first"
        ));
    }
    Ok(())
}

fn run_limactl(args: &[&str]) -> Result<std::process::Output, color_eyre::Report> {
    let output = Command::new("limactl").args(args).output()?;
    if crate::ui::is_verbose() {
        use std::io::Write;
        let _ = std::io::stderr().write_all(&output.stdout);
        let _ = std::io::stderr().write_all(&output.stderr);
    }
    Ok(output)
}

fn run_limactl_or_err(args: &[&str], context: &str) -> Result<(), color_eyre::Report> {
    let output = run_limactl(args)?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(color_eyre::eyre::eyre!(
        "{}: {}",
        context,
        stderr.lines().take(6).collect::<Vec<_>>().join("\n")
    ))
}

fn lima_instance_exists(id: &str) -> bool {
    let output = Command::new("limactl")
        .args(["list", "--format", "{{.Name}}"])
        .output()
        .ok();
    match output {
        Some(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .any(|line| line.trim() == id),
        _ => false,
    }
}

fn lima_instance_running(id: &str) -> bool {
    let output = Command::new("limactl")
        .args(["list", "--format", "{{.Status}}", id])
        .output()
        .ok();
    match output {
        Some(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .eq_ignore_ascii_case("running"),
        _ => false,
    }
}

fn lima_services_template() -> String {
    "base: template:default
vmType: vz
arch: aarch64
cpus: 2
memory: 4GiB
disk: 20GiB
mounts: []
hostResolver:
  enabled: true
provision:
  - mode: system
    script: |
      #!/usr/bin/env bash
      set -eu
      export DEBIAN_FRONTEND=noninteractive
      apt-get update -qq
      apt-get install -y -qq bash ca-certificates curl nodejs npm sudo
      id -u tnk >/dev/null 2>&1 || useradd -m -s /bin/bash tnk
      usermod -aG sudo tnk
      install -d -m 755 /etc/sudoers.d
      printf 'tnk ALL=(ALL) NOPASSWD:ALL\\n' >/etc/sudoers.d/tnk
      chmod 0440 /etc/sudoers.d/tnk
portForwards:
  - guestIP: 127.0.0.1
    guestPort: 18766
    hostIP: 127.0.0.1
    hostPort: 18766
  - guestIP: 127.0.0.1
    guestPort: 18765
    hostIP: 127.0.0.1
    hostPort: 18765
ssh:
  loadDotSSHPubKeys: false
"
    .to_string()
}

fn ensure_lima_services_instance() -> Result<(), color_eyre::Report> {
    let id = "tnk-services";
    if !lima_instance_exists(id) {
        let home = std::env::var("HOME")?;
        let template_path = PathBuf::from(home)
            .join(".cache/tnk/lima")
            .join("tnk-services.yaml");
        if let Some(parent) = template_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&template_path, lima_services_template())?;

        let template_arg = template_path.to_string_lossy().to_string();
        run_limactl_or_err(
            &["--tty=false", "start", "--name", id, &template_arg],
            "failed to create/start lima services instance",
        )?;
        return Ok(());
    }

    if !lima_instance_running(id) {
        run_limactl_or_err(
            &["--tty=false", "start", id],
            "failed to start lima services instance",
        )?;
    }

    Ok(())
}

fn provision_lima_services_instance() -> Result<(), color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let script =
        PathBuf::from(&home).join(".config/tnk/sandbox.d/container/provision.d/tnk-services.sh");
    let searxng_secret = generate_searxng_secret()?;
    let run_searxng = format!(
        "cat >/tmp/tnk-searxng-settings.yml <<'EOF'\nuse_default_settings: true\nsearch:\n  formats:\n    - html\n    - json\nserver:\n  limiter: false\nEOF\nnerdctl rm -f tnk-searxng >/dev/null 2>&1 || true\nnerdctl run -d --name tnk-searxng -p 127.0.0.1:18766:8080 -e SEARXNG_SECRET={} -v /tmp/tnk-searxng-settings.yml:/etc/searxng/settings.yml:ro docker.io/searxng/searxng:latest >/dev/null 2>&1 || true",
        searxng_secret
    );

    let script_arg = script.to_string_lossy().to_string();
    run_limactl_or_err(
        &["copy", &script_arg, "tnk-services:/tmp/tnk-services.sh"],
        "failed to copy services provision script into lima instance",
    )?;

    let start_searxng = run_limactl(&["shell", "tnk-services", "--", "bash", "-lc", &run_searxng])?;
    if !start_searxng.status.success() {
        eprintln!("warning: failed to start searxng in lima services instance");
    }

    run_limactl_or_err(
        &[
            "shell",
            "tnk-services",
            "--",
            "bash",
            "-lc",
            "sudo -u tnk env TNK_SEARXNG_URL=http://127.0.0.1:18766 bash /tmp/tnk-services.sh",
        ],
        "failed to provision lima services instance",
    )?;

    Ok(())
}

async fn start_lima(dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services start");
        return Ok(());
    }
    ensure_runtime_exclusive_lima()?;
    ensure_lima_services_instance()?;
    provision_lima_services_instance()?;
    crate::ui::log_info("searxng:  http://127.0.0.1:18766 (browser access)");
    crate::ui::log_info("mcp:      stdio bridge via limactl shell tnk-services");
    Ok(())
}

async fn stop_lima(dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services stop");
        return Ok(());
    }
    if !lima_instance_exists("tnk-services") {
        return Ok(());
    }
    run_limactl_or_err(
        &["stop", "--force", "tnk-services"],
        "failed to stop lima services instance",
    )?;
    Ok(())
}

fn is_lima_services_provisioned() -> bool {
    let output = run_limactl(&[
        "shell",
        "tnk-services",
        "--",
        "bash",
        "-lc",
        "sudo -u tnk bash -lc 'test -f $HOME/mcp-stdio.sh && test -f $HOME/.local/lib/node_modules/mcp-searxng/dist/cli.js'",
    ]);

    matches!(output, Ok(out) if out.status.success())
}

async fn status_lima(output: crate::OutputFormat) -> Result<(), color_eyre::Report> {
    let exists = lima_instance_exists("tnk-services");
    if !exists {
        return Ok(());
    }
    let running = lima_instance_running("tnk-services");
    let status = if running { "running" } else { "stopped" };
    let searxng_status = if running { "running" } else { "stopped" };
    let provisioned = running && is_lima_services_provisioned();

    match output {
        crate::OutputFormat::Text => {
            eprintln!("tnk-services (lima): {}", status);
            eprintln!("tnk-searxng (lima): {}", searxng_status);
            eprintln!("provisioned: {}", if provisioned { "yes" } else { "no" });
        }
        crate::OutputFormat::Json | crate::OutputFormat::Ndjson => {
            let payload = serde_json::json!({
                "name": "tnk-services",
                "runtime": "lima",
                "status": status,
                "searxng": searxng_status,
                "provisioned": provisioned
            });
            println!("{}", serde_json::to_string(&payload)?);
        }
    }
    Ok(())
}

async fn restart_lima(dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services restart");
        return Ok(());
    }
    stop_lima(false).await?;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    start_lima(false).await
}

fn delete_lima_instance(id: &str) -> Result<(), color_eyre::Report> {
    if !lima_instance_exists(id) {
        return Ok(());
    }
    run_limactl_or_err(
        &["delete", "--force", id],
        &format!("failed to delete lima instance '{}'", id),
    )?;
    Ok(())
}

async fn delete_lima(force: bool, dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services delete");
        return Ok(());
    }
    if !force && !std::io::stdout().is_terminal() {
        eprintln!("error: terminal required for deletion, use --yes");
        std::process::exit(77);
    }
    delete_lima_instance("tnk-services")?;
    delete_lima_instance("tnk-searxng")?;
    Ok(())
}

fn is_container_exists(container_id: &str) -> bool {
    let output = Command::new("container")
        .args(["list", "--all", "--format", "json"])
        .output()
        .ok()
        .filter(|o| o.status.success());

    match output {
        Some(out) => serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
            .ok()
            .is_some_and(|items| {
                items.iter().any(|item| {
                    container_matches_id(item, container_id)
                        && has_label(item, "tnk.managed", "true")
                })
            }),
        None => false,
    }
}

fn is_container_exists_any(container_id: &str) -> bool {
    let output = Command::new("container")
        .args(["list", "--all", "--format", "json"])
        .output()
        .ok()
        .filter(|o| o.status.success());

    match output {
        Some(out) => serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
            .ok()
            .is_some_and(|items| {
                items
                    .iter()
                    .any(|item| container_matches_id(item, container_id))
            }),
        None => false,
    }
}

fn is_container_running(container_id: &str) -> bool {
    let output = Command::new("container")
        .args(["list", "--all", "--format", "json"])
        .output()
        .ok();

    match output {
        Some(out) if out.status.success() => {
            serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
                .ok()
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        let id_matches = container_matches_id(item, container_id);
                        let state = item
                            .get("status")
                            .or_else(|| item.get("Status"))
                            .and_then(|v| v.get("state"))
                            .or_else(|| {
                                item.get("status")
                                    .or_else(|| item.get("Status"))
                                    .and_then(|v| v.get("State"))
                            })
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                item.get("state")
                                    .or_else(|| item.get("State"))
                                    .and_then(|v| v.as_str())
                            });
                        id_matches && state == Some("running")
                    })
                })
        }
        Some(_) | None => false,
    }
}

fn container_has_label(container_id: &str, key: &str, expected: &str) -> bool {
    let output = Command::new("container")
        .args(["list", "--all", "--format", "json"])
        .output()
        .ok();

    match output {
        Some(out) if out.status.success() => {
            serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
                .ok()
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        let label = item
                            .get("configuration")
                            .and_then(|v| v.get("labels"))
                            .and_then(|v| v.get(key))
                            .and_then(|v| v.as_str());
                        container_matches_id(item, container_id) && label == Some(expected)
                    })
                })
        }
        Some(_) | None => false,
    }
}

fn ensure_searxng_settings(home: &str) -> Result<PathBuf, color_eyre::Report> {
    let settings_dir = PathBuf::from(home).join(".cache/tnk/searxng");
    std::fs::create_dir_all(&settings_dir)?;
    let settings_path = settings_dir.join("settings.yml");

    let settings = "use_default_settings: true\nsearch:\n  formats:\n    - html\n    - json\nserver:\n  limiter: false\n";
    std::fs::write(&settings_path, settings)?;

    Ok(settings_path)
}

fn is_container_provisioned(container_id: &str) -> bool {
    let output = Command::new("container")
        .args(["exec", container_id])
        .arg("bash")
        .arg("-c")
        .arg("test -f $HOME/mcp-stdio.sh && test -f $HOME/.local/lib/node_modules/mcp-searxng/dist/cli.js")
        .output()
        .ok();

    match output {
        Some(out) => out.status.success(),
        None => false,
    }
}

fn ensure_services_runtime_baseline(container_id: &str) -> Result<(), color_eyre::Report> {
    let marker = "/var/lib/tnk/services-baseline-v2";
    let marker_check = Command::new("container")
        .args([
            "exec",
            container_id,
            "sh",
            "-lc",
            &format!("test -f {}", marker),
        ])
        .status()?;
    if marker_check.success() {
        return Ok(());
    }

    crate::ui::log_info("installing tnk-services runtime dependencies");
    let deps_status = Command::new("container")
        .args([
            "exec",
            container_id,
            "sh",
            "-lc",
            "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq bash ca-certificates curl nodejs npm sudo && if ! id -u tnk >/dev/null 2>&1; then useradd -m -s /bin/bash tnk; fi && usermod -aG sudo tnk && install -d -m 755 /etc/sudoers.d && printf 'tnk ALL=(ALL) NOPASSWD:ALL\\n' >/etc/sudoers.d/tnk && chmod 0440 /etc/sudoers.d/tnk && mkdir -p /home/tnk/.local && chown -R tnk:tnk /home/tnk",
        ])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()?;
    if !deps_status.success() {
        return Err(color_eyre::eyre::eyre!(
            "failed to install tnk-services container dependencies"
        ));
    }

    let marker_status = Command::new("container")
        .args([
            "exec",
            container_id,
            "sh",
            "-lc",
            &format!("mkdir -p /var/lib/tnk && touch {}", marker),
        ])
        .status()?;
    if !marker_status.success() {
        eprintln!("warning: failed to persist services baseline marker");
    }

    Ok(())
}

pub async fn delete(
    force: bool,
    dry_run: bool,
    runtime_flag: Option<String>,
) -> Result<(), color_eyre::Report> {
    match resolve_services_runtime(runtime_flag)? {
        crate::sandbox::Runtime::Container => delete_container(force, dry_run).await,
        crate::sandbox::Runtime::Lima => delete_lima(force, dry_run).await,
    }
}

async fn delete_container(force: bool, dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services delete");
        return Ok(());
    }
    let container_id = "tnk-services";
    let searxng_container_id = "tnk-searxng";
    let home = std::env::var("HOME")?;

    if !is_container_exists(container_id) && !is_container_exists(searxng_container_id) {
        return Ok(());
    }

    if !force && !std::io::stdout().is_terminal() {
        eprintln!("error: terminal required for deletion, use --yes");
        std::process::exit(77);
    }

    if is_container_exists(container_id) {
        crate::ui::log_info("deleting tnk-services container");

        let delete_status = Command::new("container")
            .args(["delete", "--force", container_id])
            .output()?;

        if !delete_status.status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to delete tnk-services container"
            ));
        }

        crate::ui::log_info(&format!("deleted {}", container_id));
    }

    if is_container_exists(searxng_container_id) {
        crate::ui::log_info("deleting tnk-searxng container");
        let delete_status = Command::new("container")
            .args(["delete", "--force", searxng_container_id])
            .output()?;
        if !delete_status.status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to delete tnk-searxng container"
            ));
        }
        crate::ui::log_info(&format!("deleted {}", searxng_container_id));
    }

    let searxng_cache_dir = PathBuf::from(home).join(".cache/tnk/searxng");
    if searxng_cache_dir.exists() {
        std::fs::remove_dir_all(&searxng_cache_dir)?;
        crate::ui::log_info(&format!("removed {}", searxng_cache_dir.display()));
    }

    Ok(())
}
