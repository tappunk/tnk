use std::path::PathBuf;
use std::process::Stdio;

use tokio::process::Command;

async fn list_lima_instances() -> Result<Vec<String>, color_eyre::Report> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        Command::new("limactl")
            .args(["list", "--format", "{{.Name}}"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| color_eyre::eyre::eyre!("limactl list timed out after 15s"))??;
    if !output.status.success() {
        return Ok(vec![]);
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

async fn check_config() -> Result<(), color_eyre::Report> {
    let cfg = crate::config::load().await?;
    cfg.print_resolved();
    eprintln!("ok: config loaded");
    Ok(())
}

fn check_locks_dir() -> Result<(), color_eyre::Report> {
    let home = std::env::var("HOME")?;
    let lock_dir = PathBuf::from(home).join(".cache/tnk");
    if lock_dir.exists() {
        eprintln!("ok: runtime cache directory exists");
    } else {
        crate::ui::log_info("runtime cache directory not present yet");
    }
    Ok(())
}

async fn check_managed_instances() -> Result<(), color_eyre::Report> {
    let items = list_lima_instances().await?;
    let count = items.iter().filter(|id| id.starts_with("tnk-")).count();
    crate::ui::log_info(&format!("managed lima instances detected: {}", count));
    Ok(())
}

pub async fn run() -> Result<(), color_eyre::Report> {
    eprintln!("tnk doctor");

    check_config().await?;
    check_locks_dir()?;
    check_managed_instances().await?;

    eprintln!("ok: diagnostics completed");
    Ok(())
}
