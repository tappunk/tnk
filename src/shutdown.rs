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

fn selected_runtime() -> crate::sandbox::Runtime {
    crate::config::load()
        .ok()
        .and_then(|cfg| crate::sandbox::resolve_runtime(None, cfg.default_sandbox_runtime).ok())
        .unwrap_or_default()
}

async fn discover_sandbox_containers() -> Vec<String> {
    let output = AsyncCommand::new("container")
        .args(["list", "--all", "--format", "json"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success());

    match output {
        Some(out) => serde_json::from_slice::<Vec<serde_json::Value>>(&out.stdout)
            .ok()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        let id = item
                            .get("id")
                            .or_else(|| item.get("ID"))
                            .or_else(|| item.get("Id"))
                            .and_then(|v| v.as_str())
                            .or_else(|| {
                                item.get("configuration")
                                    .or_else(|| item.get("Configuration"))
                                    .or_else(|| item.get("config"))
                                    .or_else(|| item.get("Config"))
                                    .and_then(|v| {
                                        v.get("id").or_else(|| v.get("ID")).or_else(|| v.get("Id"))
                                    })
                                    .and_then(|v| v.as_str())
                            })?;
                        let labels = item
                            .get("configuration")
                            .or_else(|| item.get("Configuration"))
                            .or_else(|| item.get("config"))
                            .or_else(|| item.get("Config"))
                            .and_then(|v| v.get("labels").or_else(|| v.get("Labels")));
                        let managed = labels
                            .and_then(|v| v.get("tnk.managed"))
                            .and_then(|v| v.as_str())
                            .is_some_and(|v| v == "true");
                        let owner_project = labels
                            .and_then(|v| v.get("tnk.owner"))
                            .and_then(|v| v.as_str())
                            .is_some_and(|v| v == "project");
                        if id.starts_with("tnk-")
                            && id != "tnk-services"
                            && id != "tnk-searxng"
                            && managed
                            && owner_project
                        {
                            Some(id.to_string())
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default(),
        None => Vec::new(),
    }
}

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

async fn stop_container(name: String, timeout_secs: u64) {
    let status = AsyncCommand::new("container")
        .args(["stop", "--time", &timeout_secs.to_string(), &name])
        .output()
        .await;

    match status {
        Ok(out) if out.status.success() => crate::ui::log_info(&format!("stopped {}", name)),
        Ok(_) | Err(_) => eprintln!("warning: failed to stop {}", name),
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
    let default_runtime = crate::config::load()
        .ok()
        .and_then(|cfg| cfg.default_engine_runtime)
        .unwrap_or_else(|| "llama".to_string());

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
            if let Err(err) = crate::engine::stop(&default_runtime).await {
                eprintln!("warning: second stop attempt failed: {}", err);
            }
        }
    }
}

pub async fn run(
    timeout_secs: Option<u64>,
    _yes: bool,
    dry_run: bool,
) -> Result<(), color_eyre::Report> {
    if dry_run {
        crate::ui::log_info("dry run, skipping shutdown actions");
        return Ok(());
    }

    let timeout = timeout_secs.unwrap_or(30);
    let runtime = selected_runtime();

    match runtime {
        crate::sandbox::Runtime::Container => {
            let _lock = crate::lifecycle::acquire(
                "container-lifecycle",
                std::time::Duration::from_secs(20),
            )
            .await?;

            for container in discover_sandbox_containers().await {
                stop_container(container, timeout).await;
            }
            crate::services::stop(false, Some("container".to_string())).await?;
        }
        crate::sandbox::Runtime::Lima => {
            let _lock =
                crate::lifecycle::acquire("lima-lifecycle", std::time::Duration::from_secs(20))
                    .await?;

            for instance in discover_lima_sandboxes().await {
                stop_lima(instance).await;
            }
            crate::services::stop(false, Some("lima".to_string())).await?;
        }
    }

    stop_engine().await;
    crate::ui::log_info("shutdown complete");
    Ok(())
}
