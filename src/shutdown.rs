use tokio::process::Command as AsyncCommand;

use crate::ui;

async fn discover_lima_sandboxes() -> Vec<String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        AsyncCommand::new("limactl")
            .args(["list", "--format", "{{.Name}}"])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .filter(|o| o.status.success());

    match output {
        Some(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|id| id.starts_with("tnk-") && *id != "tnk-config")
            .map(ToString::to_string)
            .collect(),
        None => Vec::new(),
    }
}

async fn stop_lima(name: String, grace_secs: u64) {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        AsyncCommand::new("limactl")
            .args(["list", "--format", "{{.Status}}", &name])
            .output(),
    )
    .await
    .ok()
    .and_then(Result::ok);

    let is_running = output
        .as_ref()
        .map(|out| {
            if out.status.success() {
                String::from_utf8_lossy(&out.stdout)
                    .trim()
                    .eq_ignore_ascii_case("running")
            } else {
                false
            }
        })
        .unwrap_or(false);

    if !is_running {
        ui::log_info(&format!("already stopped {}", name));
        return;
    }

    let graceful = tokio::time::timeout(
        std::time::Duration::from_secs(grace_secs),
        AsyncCommand::new("limactl").args(["stop", &name]).output(),
    )
    .await;

    let graceful_ok = match graceful {
        Ok(Ok(output)) => output.status.success(),
        Ok(Err(_)) | Err(_) => false,
    };

    if graceful_ok {
        ui::log_info(&format!("stopped {}", name));
        return;
    }

    eprintln!(
        "warning: graceful stop for '{}' did not succeed, escalating to force",
        name
    );
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

    let force = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        AsyncCommand::new("limactl")
            .args(["stop", "--force", &name])
            .output(),
    )
    .await;

    match force {
        Ok(Ok(output)) if output.status.success() => {
            ui::log_info(&format!("stopped {}", name));
        }
        Ok(Ok(_)) | Ok(Err(_)) | Err(_) => {
            eprintln!("warning: failed to stop {}", name);
        }
    }
}

pub async fn run(timeout_secs: Option<u64>, dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping shutdown actions");
        return Ok(());
    }

    let grace_secs = timeout_secs.unwrap_or(60);
    let _lima_lock =
        crate::lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20)).await?;
    for instance in discover_lima_sandboxes().await {
        stop_lima(instance, grace_secs).await;
    }

    crate::ui::log_info("shutdown complete");
    Ok(())
}
