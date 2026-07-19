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

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::lifecycle;
use async_trait::async_trait;
use tokio::process::Command;

#[async_trait]
pub trait ServicesBackend: Send + Sync {
    async fn start(&self, dry_run: bool) -> Result<(), color_eyre::Report>;
    async fn stop(&self, dry_run: bool) -> Result<(), color_eyre::Report>;
    async fn status(&self, output: crate::OutputFormat) -> Result<(), color_eyre::Report>;
    async fn restart(&self, dry_run: bool) -> Result<(), color_eyre::Report>;
    async fn delete(&self, force: bool, dry_run: bool) -> Result<(), color_eyre::Report>;
}

pub struct LimaServices;

#[async_trait]
impl ServicesBackend for LimaServices {
    async fn start(&self, dry_run: bool) -> Result<(), color_eyre::Report> {
        start_lima(dry_run).await
    }

    async fn stop(&self, dry_run: bool) -> Result<(), color_eyre::Report> {
        stop_lima(dry_run).await
    }

    async fn status(&self, output: crate::OutputFormat) -> Result<(), color_eyre::Report> {
        status_lima(output).await
    }

    async fn restart(&self, dry_run: bool) -> Result<(), color_eyre::Report> {
        restart_lima(dry_run).await
    }

    async fn delete(&self, force: bool, dry_run: bool) -> Result<(), color_eyre::Report> {
        delete_lima(force, dry_run).await
    }
}

pub async fn run(action: crate::ServicesCommands) -> Result<(), color_eyre::Report> {
    let backend = LimaServices;
    match action {
        crate::ServicesCommands::Start { dry_run } => backend.start(dry_run).await?,
        crate::ServicesCommands::Stop { dry_run } => backend.stop(dry_run).await?,
        crate::ServicesCommands::Status { output } => backend.status(output).await?,
        crate::ServicesCommands::Restart { dry_run } => backend.restart(dry_run).await?,
        crate::ServicesCommands::Delete { yes, dry_run } => backend.delete(yes, dry_run).await?,
    }
    Ok(())
}

pub async fn start(dry_run: bool) -> Result<(), color_eyre::Report> {
    LimaServices.start(dry_run).await
}

pub async fn stop(dry_run: bool) -> Result<(), color_eyre::Report> {
    LimaServices.stop(dry_run).await
}

pub async fn status(output: crate::OutputFormat) -> Result<(), color_eyre::Report> {
    LimaServices.status(output).await
}

pub async fn restart(dry_run: bool) -> Result<(), color_eyre::Report> {
    LimaServices.restart(dry_run).await
}

pub async fn delete(force: bool, dry_run: bool) -> Result<(), color_eyre::Report> {
    LimaServices.delete(force, dry_run).await
}

async fn generate_searxng_secret() -> Result<String, color_eyre::Report> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    const ALPHABET_LEN: u8 = ALPHABET.len() as u8;
    const REJECT_THRESHOLD: u16 = 256 - 256 % ALPHABET_LEN as u16;

    let mut source = tokio::fs::File::open("/dev/urandom").await?;
    use tokio::io::AsyncReadExt;

    let mut secret = String::with_capacity(32);
    loop {
        let mut byte = [0u8; 1];
        source.read_exact(&mut byte).await?;
        if u16::from(byte[0]) < REJECT_THRESHOLD {
            secret.push(ALPHABET[usize::from(byte[0] % ALPHABET_LEN)] as char);
        }
        if secret.len() == 32 {
            break;
        }
    }
    Ok(secret)
}

async fn limactl_output(args: &[&str]) -> Result<std::process::Output, color_eyre::Report> {
    let output = Command::new("limactl").args(args).output().await?;
    if crate::ui::is_verbose() {
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
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new("limactl")
            .args(["list", "--format", "{{.Name}}"])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok);

    let Some(items) = output else {
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
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new("limactl")
            .args(["list", "--format", "{{.Status}}", id])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok);
    output
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

async fn wait_for_services_ready(id: &str, timeout: Duration) -> Result<(), color_eyre::Report> {
    let started = Instant::now();
    loop {
        let check = tokio::time::timeout(
            Duration::from_secs(10),
            limactl_output(&["shell", id, "--", "bash", "-lc", "id -u"]),
        )
        .await;

        match check {
            Ok(Ok(out)) if out.status.success() => return Ok(()),
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {}
            Err(_) => {
                crate::ui::log_verbose(&format!(
                    "lima shell check timed out after 10s for instance '{}'",
                    id
                ));
            }
        }

        if started.elapsed() >= timeout {
            return Err(color_eyre::eyre::eyre!(
                "timed out waiting for lima instance '{}' to be ready after {}s",
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

        limactl_run_or_err(
            &[
                "--tty=false",
                "start",
                "--name",
                id,
                "--vm-type=vz",
                // Self-contained VM: no host filesystem mounts. SearXNG runs as a
                // nerdctl container pulled from Docker Hub; the provision script
                // is copied in once, then runs entirely inside the guest.
                "--mount-none",
                "--containerd=system",
                "--cpus=2",
                "--memory=2",
                "--port-forward=18765:18765",
                "--port-forward=18766:18766",
                "--set",
                ".ssh.loadDotSSHPubKeys = false",
                "template:ubuntu",
            ],
            "failed to create/start services instance",
        )
        .await?;

        eprintln!("info: services instance '{}' is running", id);
    } else if !lima_instance_running(id).await {
        eprintln!("info: starting existing services instance '{}'", id);
        limactl_run_or_err(
            &["--tty=false", "start", id],
            "failed to start services instance",
        )
        .await?;
        eprintln!("info: services instance '{}' is running", id);
    }

    wait_for_services_ready(id, Duration::from_secs(180)).await?;
    Ok(())
}

async fn provision_lima_services_instance() -> Result<(), color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let script = PathBuf::from(&home).join(".config/tnk/sandbox.d/provision.d/tnk-services.sh");
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
            "env TNK_SEARXNG_URL=http://127.0.0.1:18766 bash /tmp/tnk-services.sh",
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
    let graceful =
        tokio::time::timeout(Duration::from_secs(60), limactl_output(&["stop", id])).await;

    let graceful_ok = match graceful {
        Ok(Ok(output)) => output.status.success(),
        Ok(Err(_)) | Err(_) => false,
    };

    if !graceful_ok && lima_instance_running(id).await {
        eprintln!(
            "warning: graceful stop for '{}' did not succeed, escalating to force stop",
            id
        );
        limactl_run_or_err(&["stop", "--force", id], "failed to stop services instance").await?;
    }
    Ok(())
}

async fn status_lima(output: crate::OutputFormat) -> Result<(), color_eyre::Report> {
    let exists = lima_instance_exists("tnk-services").await;
    if !exists {
        return Ok(());
    }
    let running = lima_instance_running("tnk-services").await;
    let status = if running { "running" } else { "stopped" };
    let searxng_status = if running { "running" } else { "stopped" };

    match output {
        crate::OutputFormat::Text => {
            eprintln!("services (vm): {}", status);
            eprintln!("searxng (vm): {}", searxng_status);
        }
        crate::OutputFormat::Json | crate::OutputFormat::Ndjson => {
            let payload = serde_json::json!({
                "name": "tnk-services",
                "runtime": "lima",
                "status": status,
                "searxng": searxng_status
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
        crate::ui::exit_with(
            crate::ui::ExitCode::PermissionDenied,
            "terminal required for deletion, use --yes",
        );
    }
    delete_lima_instance("tnk-services").await?;
    delete_lima_instance("tnk-searxng").await?;
    Ok(())
}
