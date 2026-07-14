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

use tokio::process::Command as AsyncCommand;

async fn discover_lima_sandboxes() -> Vec<String> {
    let output = AsyncCommand::new("limactl")
        .args(["list", "--format", "{{.Name}}"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success());

    match output {
        Some(out) => String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .filter(|id| {
                id.starts_with("tnk-")
                    && *id != "tnk-services"
                    && *id != "tnk-searxng"
                    && *id != "tnk-config"
            })
            .map(ToString::to_string)
            .collect(),
        None => Vec::new(),
    }
}

async fn stop_lima(name: String) {
    let status = AsyncCommand::new("limactl")
        .args(["stop", "--force", &name])
        .output()
        .await;

    match status {
        Ok(out) if out.status.success() => crate::ui::log_info(&format!("stopped {}", name)),
        Ok(_) | Err(_) => eprintln!("warning: failed to stop {}", name),
    }
}

async fn stop_engine() {
    let mut had_any = false;

    if crate::engine::is_running().await {
        had_any = true;
        if let Err(err) = crate::engine::stop_all().await {
            eprintln!("warning: failed to stop inference engine: {}", err);
        }
    }

    if had_any {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if crate::engine::is_running().await {
            eprintln!("warning: inference engine still running after stop request; retrying");
            if let Err(err) = crate::engine::stop_all().await {
                eprintln!("warning: second stop_all attempt failed: {}", err);
            }
        }
    }
}

pub async fn run(_timeout_secs: Option<u64>, dry_run: bool) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping shutdown actions");
        return Ok(());
    }

    let _lock =
        crate::lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20)).await?;

    for instance in discover_lima_sandboxes().await {
        stop_lima(instance).await;
    }
    crate::services::stop(false).await?;

    stop_engine().await;
    crate::ui::log_info("shutdown complete");
    Ok(())
}
