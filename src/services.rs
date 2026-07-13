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
use std::path::PathBuf;

use std::time::{Duration, Instant};

use crate::lifecycle;
use crate::sandbox::container_utils::{self, ContainerListItem};
use tokio::fs;
use tokio::process::Command;
use tokio::task::spawn_blocking;

fn container_matches_id(item: &ContainerListItem, container_id: &str) -> bool {
    item.id() == Some(container_id)
}

const SEARXNG_CONFIG_REV: &str = "v3";

async fn container_exec(
    label: &str,
    args: &[&str],
) -> Result<std::process::ExitStatus, color_eyre::Report> {
    let label = label.to_owned();
    let args: Vec<String> = args.iter().map(|&s| s.to_owned()).collect();
    let res =
        spawn_blocking(move || std::process::Command::new("container").args(&args).status()).await;
    match res {
        Ok(inner) => inner.map_err(|e| color_eyre::eyre::eyre!("{label}: {e}")),
        Err(e) => Err(color_eyre::eyre::eyre!("{label}: {e}")),
    }
}

async fn container_output(
    label: &str,
    args: &[&str],
) -> Result<std::process::Output, color_eyre::Report> {
    let label = label.to_owned();
    let args: Vec<String> = args.iter().map(|&s| s.to_owned()).collect();
    let res =
        spawn_blocking(move || std::process::Command::new("container").args(&args).output()).await;
    match res {
        Ok(inner) => inner.map_err(|e| color_eyre::eyre::eyre!("{label}: {e}")),
        Err(e) => Err(color_eyre::eyre::eyre!("{label}: {e}")),
    }
}

async fn generate_searxng_secret() -> Result<String, color_eyre::Report> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0_u8; 32];
    let mut source = tokio::fs::File::open("/dev/urandom").await?;
    use tokio::io::AsyncReadExt;
    source.read_exact(&mut bytes).await?;

    let secret: String = bytes
        .iter()
        .map(|byte| ALPHABET[usize::from(*byte) % ALPHABET.len()] as char)
        .collect();
    Ok(secret)
}

async fn resolve_host_gateway() -> Result<String, color_eyre::Report> {
    if let Ok(cfg) = crate::config::load().await
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
    container_utils::discover_container_gateway()
        .await
        .ok_or_else(|| {
            color_eyre::eyre::eyre!(
                "could not determine container host gateway; set TNK_CONTAINER_HOST_GATEWAY or container_host_gateway in config"
            )
        })
}

fn resolve_services_runtime(
    runtime_flag: Option<String>,
) -> Result<crate::sandbox::Runtime, color_eyre::Report> {
    let cfg = crate::config::load_blocking()?;
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

    let _lock = lifecycle::acquire("services-runtime", Duration::from_secs(20)).await?;
    ensure_runtime_exclusive_container().await?;

    let container_id = "tnk-services";
    let searxng_container_id = "tnk-searxng";
    let home = std::env::var("HOME")?;
    let searxng_settings_path = ensure_searxng_settings(&home).await?;

    if is_container_exists_any(searxng_container_id).await
        && !container_has_label(searxng_container_id, "tnk.config-rev", SEARXNG_CONFIG_REV).await
    {
        crate::ui::log_info("recreating tnk-searxng container for updated config");
        let delete_status = container_exec(
            "recreate searxng",
            &["delete", "--force", searxng_container_id],
        )
        .await?;
        if !delete_status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to recreate tnk-searxng container"
            ));
        }
    }

    if !is_container_exists_any(searxng_container_id).await {
        crate::ui::log_info("creating tnk-searxng container");
        let settings_mount = format!(
            "{}:/etc/searxng/settings.yml",
            searxng_settings_path.to_string_lossy()
        );
        let searxng_secret = generate_searxng_secret().await?;
        let cache_dir = PathBuf::from(&home).join(".cache/tnk");
        tokio::fs::create_dir_all(&cache_dir).await?;
        let secret_path = cache_dir.join("searxng-secret");
        if !secret_path.exists() {
            fs::write(&secret_path, format!("SEARXNG_SECRET={}\n", searxng_secret)).await?;
        }
        let secret_mount = format!("{}:/run/secrets/searxng-secret:ro", secret_path.display());
        let status = container_exec(
            "create searxng",
            &[
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
                "--volume",
                &secret_mount,
                "--env-file",
                "/run/secrets/searxng-secret",
                "docker.io/searxng/searxng:latest",
            ],
        )
        .await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to create tnk-searxng container"
            ));
        }
    }

    if !is_container_running(searxng_container_id).await {
        crate::ui::log_info("starting tnk-searxng container");
        let status = container_exec("start searxng", &["start", searxng_container_id]).await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to start tnk-searxng container"
            ));
        }
    }

    if !is_container_exists_any(container_id).await {
        crate::ui::log_info("creating tnk-services container");
        let status = container_exec(
            "create services",
            &[
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
            ],
        )
        .await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to create tnk-services container"
            ));
        }
    }

    if !is_container_running(container_id).await {
        crate::ui::log_info("starting tnk-services container");
        let status = container_exec("start services", &["start", container_id]).await?;
        if !status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to start tnk-services container"
            ));
        }
    } else {
        crate::ui::log_info("tnk-services container already running");
        return Ok(());
    }

    ensure_services_runtime_baseline(container_id).await?;

    if !is_container_provisioned(container_id).await {
        let host_gateway = resolve_host_gateway().await?;
        let searxng_url = format!("http://{}:18766", host_gateway);

        let home = home.clone();
        let provision_result: Result<(), color_eyre::Report> =
            tokio::time::timeout(Duration::from_secs(600), spawn_blocking(move || {
                let provision_script = PathBuf::from(&home)
                    .join(".config/tnk/sandbox.d/container/provision.d/tnk-services.sh");
                if !provision_script.is_file() {
                    return Err(color_eyre::eyre::eyre!(
                        "services provision script not found at {}; run `tnk init --force`",
                        provision_script.display()
                    ));
                }

                let provision_lib =
                    PathBuf::from(&home).join(".config/tnk/sandbox.d/container/provision.d/lib");

                let mut cp_cmd = std::process::Command::new("container");
                cp_cmd.args([
                    "copy",
                    provision_script.to_str().ok_or_else(|| {
                        color_eyre::eyre::eyre!("provision script path contains invalid UTF-8")
                    })?,
                    &format!("{}:/tmp/tnk-services.sh", container_id),
                ]);
                if !cp_cmd.status()?.success() {
                    return Err(color_eyre::eyre::eyre!(
                        "failed to copy provision script into services container"
                    ));
                }

                if provision_lib.is_dir() {
                    let mut cp_lib_cmd = std::process::Command::new("container");
                    cp_lib_cmd.args([
                        "copy",
                        provision_lib.to_str().ok_or_else(|| {
                            color_eyre::eyre::eyre!("provision lib path contains invalid UTF-8")
                        })?,
                        &format!("{}:/tmp", container_id),
                    ]);
                    if !cp_lib_cmd.status()?.success() {
                        return Err(color_eyre::eyre::eyre!(
                            "failed to copy provision library into services container"
                        ));
                    }
                }

                let mut provision_cmd = std::process::Command::new("container");
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
                Ok(())
            }))
            .await
            .map_err(|_| color_eyre::eyre::eyre!("provision timed out after 600s"))?
            .map_err(|e| color_eyre::eyre::eyre!("provision task error: {}", e))?;
        provision_result?;
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
    if !is_container_exists(container_id).await && !is_container_exists(searxng_container_id).await
    {
        return Ok(());
    }

    if is_container_exists(container_id).await {
        let output = container_output("stop services", &["stop", container_id])
            .await
            .ok();

        match output {
            Some(out) if out.status.success() => {
                crate::ui::log_info(&format!("stopped {}", container_id));
            }
            Some(_) => {
                eprintln!("warning: failed to stop {}", container_id);
            }
            None => {
                eprintln!("warning: failed to stop {}", container_id);
            }
        }
    }

    if is_container_exists(searxng_container_id).await {
        let output = container_output("stop searxng", &["stop", searxng_container_id])
            .await
            .ok();

        match output {
            Some(out) if out.status.success() => {
                crate::ui::log_info(&format!("stopped {}", searxng_container_id));
            }
            Some(_) => {
                eprintln!("warning: failed to stop {}", searxng_container_id);
            }
            None => {
                eprintln!("warning: failed to stop {}", searxng_container_id);
            }
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

    if !is_container_exists(container_id).await && !is_container_exists(searxng_container_id).await
    {
        return Ok(());
    }

    let status = if is_container_running(container_id).await {
        "running"
    } else {
        "stopped"
    };
    let searxng_status = if is_container_exists(searxng_container_id).await {
        Some(if is_container_running(searxng_container_id).await {
            "running"
        } else {
            "stopped"
        })
    } else {
        None
    };
    let provisioned =
        is_container_running(container_id).await && is_container_provisioned(container_id).await;

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

async fn lima_services_exist_any() -> bool {
    lima_instance_exists("tnk-services").await || lima_instance_exists("tnk-searxng").await
}

async fn container_services_exist_any() -> bool {
    is_container_exists_any("tnk-services").await || is_container_exists_any("tnk-searxng").await
}

async fn ensure_runtime_exclusive_container() -> Result<(), color_eyre::Report> {
    if lima_services_exist_any().await {
        return Err(color_eyre::eyre::eyre!(
            "lima services are present; switch to lima runtime or delete lima services first"
        ));
    }
    Ok(())
}

async fn ensure_runtime_exclusive_lima() -> Result<(), color_eyre::Report> {
    if container_services_exist_any().await {
        return Err(color_eyre::eyre::eyre!(
            "container services are present; switch to container runtime or delete container services first"
        ));
    }
    Ok(())
}

async fn limactl_output(args: &[&str]) -> Result<std::process::Output, color_eyre::Report> {
    let output = Command::new("limactl").args(args).output().await?;
    if crate::ui::is_verbose() {
        use std::io::Write;
        let _ = std::io::stderr().write_all(&output.stdout);
        let _ = std::io::stderr().write_all(&output.stderr);
    }
    Ok(output)
}

async fn limactl_run_or_err(args: &[&str], context: &str) -> Result<(), color_eyre::Report> {
    let output = tokio::time::timeout(Duration::from_secs(300), limactl_output(args)).await;
    let output = match output {
        Ok(result) => result?,
        Err(_) => {
            return Err(color_eyre::eyre::eyre!("{}: timed out after 300s", context));
        }
    };

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

async fn lima_instance_exists(id: &str) -> bool {
    let Some(items) = Command::new("limactl")
        .args(["list", "--format", "{{.Name}}"])
        .output()
        .await
        .ok()
    else {
        return false;
    };
    if !items.status.success() {
        return false;
    }
    String::from_utf8_lossy(&items.stdout)
        .lines()
        .any(|line| line.trim() == id)
}

async fn lima_instance_running(id: &str) -> bool {
    let output = Command::new("limactl")
        .args(["list", "--format", "{{.Status}}", id])
        .output()
        .await;
    output
        .ok()
        .map(|out| {
            if out.status.success() {
                String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .eq_ignore_ascii_case("running")
            } else {
                false
            }
        })
        .unwrap_or(false)
}

fn lima_services_template() -> String {
    let provision = crate::sandbox::shared::BASELINE_PROVISION_SCRIPT
        .lines()
        .map(|line| format!("      {line}\n"))
        .collect::<String>();
    format!(
        "\
base: template:default
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
{provision}portForwards:
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
    )
}

async fn wait_for_lima_user(
    id: &str,
    user: &str,
    timeout: Duration,
) -> Result<(), color_eyre::Report> {
    let started = Instant::now();
    loop {
        let check =
            limactl_output(&["shell", id, "--", "bash", "-lc", &format!("id -u {}", user)]).await;
        if matches!(check, Ok(out) if out.status.success()) {
            return Ok(());
        }

        if started.elapsed() >= timeout {
            return Err(color_eyre::eyre::eyre!(
                "timed out waiting for lima user '{}' in instance '{}' after {}s",
                user,
                id,
                timeout.as_secs()
            ));
        }

        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn ensure_lima_services_instance() -> Result<(), color_eyre::Report> {
    let id = "tnk-services";
    if !lima_instance_exists(id).await {
        eprintln!(
            "info: creating services instance '{}' (this can take a few minutes)",
            id
        );
        let home = std::env::var("HOME")?;
        let template_path = PathBuf::from(home)
            .join(".cache/tnk/lima")
            .join("tnk-services.yaml");
        if let Some(parent) = template_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&template_path, lima_services_template()).await?;

        let template_arg = template_path.to_string_lossy().to_string();
        limactl_run_or_err(
            &["--tty=false", "start", "--name", id, &template_arg],
            "failed to create/start services instance",
        )
        .await?;
        eprintln!("info: services instance '{}' is running", id);
        eprintln!("info: waiting for baseline provisioning to create user 'tnk'");
        wait_for_lima_user(id, "tnk", Duration::from_secs(180)).await?;
        return Ok(());
    }

    if !lima_instance_running(id).await {
        eprintln!("info: starting existing services instance '{}'", id);
        limactl_run_or_err(
            &["--tty=false", "start", id],
            "failed to start services instance",
        )
        .await?;
        eprintln!("info: services instance '{}' is running", id);
    }

    eprintln!("info: waiting for baseline provisioning to create user 'tnk'");
    wait_for_lima_user(id, "tnk", Duration::from_secs(180)).await?;

    Ok(())
}

async fn provision_lima_services_instance() -> Result<(), color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let script =
        PathBuf::from(&home).join(".config/tnk/sandbox.d/container/provision.d/tnk-services.sh");
    let searxng_secret = generate_searxng_secret().await?;
    let run_searxng = format!(
        "cat >/tmp/tnk-searxng-settings.yml <<'EOF'\nuse_default_settings: true\nsearch:\n  formats:\n    - html\n    - json\nserver:\n  limiter: false\nEOF\nnerdctl rm -f tnk-searxng >/dev/null 2>&1 || true\nnerdctl run -d --name tnk-searxng -p 127.0.0.1:18766:8080 -e SEARXNG_SECRET={} -v /tmp/tnk-searxng-settings.yml:/etc/searxng/settings.yml:ro docker.io/searxng/searxng:latest >/dev/null 2>&1 || true",
        searxng_secret
    );

    eprintln!("info: provisioning services instance 'tnk-services'");
    let script_arg = script.to_string_lossy().to_string();
    eprintln!("info: copying provision script into services instance");
    limactl_run_or_err(
        &["copy", &script_arg, "tnk-services:/tmp/tnk-services.sh"],
        "failed to copy services provision script into services instance",
    )
    .await?;

    eprintln!("info: starting searxng inside services instance");
    let start_searxng = tokio::time::timeout(
        Duration::from_secs(120),
        limactl_output(&["shell", "tnk-services", "--", "bash", "-lc", &run_searxng]),
    )
    .await;
    match start_searxng {
        Ok(Ok(out)) if out.status.success() => {}
        Ok(Ok(_)) | Ok(Err(_)) => {
            eprintln!("warning: failed to start searxng in services instance");
        }
        Err(_) => {
            eprintln!("warning: timed out starting searxng in services instance");
        }
    }

    eprintln!("info: running tnk-services provision script inside services instance");
    limactl_run_or_err(
        &[
            "shell",
            "tnk-services",
            "--",
            "bash",
            "-lc",
            "sudo -u tnk env TNK_SEARXNG_URL=http://127.0.0.1:18766 bash /tmp/tnk-services.sh",
        ],
        "failed to provision services instance",
    )
    .await?;

    Ok(())
}

async fn start_lima(dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services start");
        return Ok(());
    }
    eprintln!("info: services machine: acquiring lifecycle lock");
    let _lock = lifecycle::acquire("services-runtime", Duration::from_secs(20)).await?;
    eprintln!("info: services machine: checking runtime exclusivity");
    ensure_runtime_exclusive_lima().await?;
    ensure_lima_services_instance().await?;
    provision_lima_services_instance().await?;
    crate::ui::log_info("searxng:  http://127.0.0.1:18766 (browser access)");
    crate::ui::log_info("mcp:      stdio bridge via limactl shell tnk-services");
    Ok(())
}

async fn stop_lima(dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services stop");
        return Ok(());
    }
    if !lima_instance_exists("tnk-services").await {
        return Ok(());
    }
    let id = "tnk-services";
    let graceful = tokio::time::timeout(
        Duration::from_secs(60),
        limactl_output(&["stop", id]),
    )
    .await;

    let graceful_ok = match graceful {
        Ok(Ok(output)) => output.status.success(),
        Ok(Err(_)) | Err(_) => false,
    };

    if !graceful_ok && lima_instance_running(id).await {
        eprintln!(
            "warning: graceful stop for '{}' did not succeed, escalating to force stop",
            id
        );
        limactl_run_or_err(
            &["stop", "--force", id],
            "failed to stop services instance",
        )
        .await?;
    }
    Ok(())
}

async fn is_lima_services_provisioned() -> bool {
    let output = limactl_output(&[
        "shell",
        "tnk-services",
        "--",
        "bash",
        "-lc",
        crate::sandbox::shared::PROVISION_STATE_CHECK,
    ])
    .await;

    matches!(output, Ok(out) if out.status.success())
}

async fn status_lima(output: crate::OutputFormat) -> Result<(), color_eyre::Report> {
    let exists = lima_instance_exists("tnk-services").await;
    if !exists {
        return Ok(());
    }
    let running = lima_instance_running("tnk-services").await;
    let status = if running { "running" } else { "stopped" };
    let searxng_status = if running { "running" } else { "stopped" };
    let provisioned = running && is_lima_services_provisioned().await;

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

async fn delete_lima_instance(id: &str) -> Result<(), color_eyre::Report> {
    if !lima_instance_exists(id).await {
        return Ok(());
    }
    limactl_run_or_err(
        &["delete", "--force", id],
        &format!("failed to delete lima instance '{}'", id),
    )
    .await?;
    Ok(())
}

async fn delete_lima(force: bool, dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping services delete");
        return Ok(());
    }
    if !force && !std::io::stdout().is_terminal() {
        return Err(color_eyre::eyre::eyre!(
            "terminal required for deletion, use --yes"
        ));
    }
    delete_lima_instance("tnk-services").await?;
    delete_lima_instance("tnk-searxng").await?;
    Ok(())
}

async fn is_container_exists(container_id: &str) -> bool {
    let Some(items) = container_utils::container_list_all().await else {
        return false;
    };
    items.iter().any(|item| {
        container_matches_id(item, container_id)
            && item.label("tnk.managed").is_some_and(|v| v == "true")
    })
}

async fn is_container_exists_any(container_id: &str) -> bool {
    let Some(items) = container_utils::container_list_all().await else {
        return false;
    };
    items
        .iter()
        .any(|item| container_matches_id(item, container_id))
}

async fn is_container_running(container_id: &str) -> bool {
    let Some(items) = container_utils::container_list_all().await else {
        return false;
    };
    items.iter().any(|item| {
        container_matches_id(item, container_id) && item.status_state() == Some("running")
    })
}

async fn container_has_label(container_id: &str, key: &str, expected: &str) -> bool {
    let Some(items) = container_utils::container_list_all().await else {
        return false;
    };
    items
        .iter()
        .any(|item| container_matches_id(item, container_id) && item.label(key) == Some(expected))
}

async fn ensure_searxng_settings(home: &str) -> Result<PathBuf, color_eyre::Report> {
    let settings_dir = PathBuf::from(home).join(".cache/tnk/searxng");
    tokio::fs::create_dir_all(&settings_dir).await?;
    let settings_path = settings_dir.join("settings.yml");

    let settings = "use_default_settings: true\nsearch:\n  formats:\n    - html\n    - json\nserver:\n  limiter: false\n";
    if !tokio::fs::try_exists(&settings_path).await? {
        tokio::fs::write(&settings_path, settings).await?;
    }

    Ok(settings_path)
}

async fn is_container_provisioned(container_id: &str) -> bool {
    let output = Command::new("container")
        .args(["exec", container_id])
        .arg("bash")
        .arg("-c")
        .arg(crate::sandbox::shared::PROVISION_STATE_CHECK)
        .output()
        .await;
    output.ok().map(|out| out.status.success()).unwrap_or(false)
}

async fn ensure_services_runtime_baseline(container_id: &str) -> Result<(), color_eyre::Report> {
    let marker = "/var/lib/tnk/services-baseline-v2";
    let marker_check = Command::new("container")
        .args([
            "exec",
            container_id,
            "sh",
            "-lc",
            &format!("test -f {}", marker),
        ])
        .status()
        .await?;
    if marker_check.success() {
        return Ok(());
    }

    crate::ui::log_info("installing tnk-services runtime dependencies");
    let deps_status = tokio::time::timeout(
        Duration::from_secs(300),
        Command::new("container")
            .args([
                "exec",
                container_id,
                "sh",
                "-lc",
                "apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq bash ca-certificates curl nodejs npm sudo && if ! id -u tnk >/dev/null 2>&1; then useradd -m -s /bin/bash tnk; fi && usermod -aG sudo tnk && install -d -m 755 /etc/sudoers.d && printf 'tnk ALL=(ALL) NOPASSWD:ALL\\n' >/etc/sudoers.d/tnk && chmod 0440 /etc/sudoers.d/tnk && mkdir -p /home/tnk/.local && chown -R tnk:tnk /home/tnk",
            ])
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status(),
    )
    .await
    .map_err(|_| {
        color_eyre::eyre::eyre!("dependency install timed out after 300s")
    })??;
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
        .status()
        .await?;
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

    if !is_container_exists(container_id).await && !is_container_exists(searxng_container_id).await
    {
        return Ok(());
    }

    if !force && !std::io::stdout().is_terminal() {
        return Err(color_eyre::eyre::eyre!(
            "terminal required for deletion, use --yes"
        ));
    }

    if is_container_exists(container_id).await {
        crate::ui::log_info("deleting tnk-services container");

        let delete_status =
            container_output("delete services", &["delete", "--force", container_id]).await?;

        if !delete_status.status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to delete tnk-services container"
            ));
        }

        crate::ui::log_info(&format!("deleted {}", container_id));
    }

    if is_container_exists(searxng_container_id).await {
        crate::ui::log_info("deleting tnk-searxng container");
        let delete_status = container_output(
            "delete searxng",
            &["delete", "--force", searxng_container_id],
        )
        .await?;
        if !delete_status.status.success() {
            return Err(color_eyre::eyre::eyre!(
                "failed to delete tnk-searxng container"
            ));
        }
        crate::ui::log_info(&format!("deleted {}", searxng_container_id));
    }

    let home2 = home.clone();
    let searxng_cache_dir = PathBuf::from(&home).join(".cache/tnk/searxng");
    if searxng_cache_dir.exists() {
        fs::remove_dir_all(&searxng_cache_dir).await?;
        crate::ui::log_info(&format!("removed {}", searxng_cache_dir.display()));
    }

    let secret_path = PathBuf::from(home2).join(".cache/tnk/searxng-secret");
    if secret_path.exists() {
        fs::remove_file(&secret_path).await?;
        crate::ui::log_info(&format!("removed {}", secret_path.display()));
    }

    Ok(())
}
