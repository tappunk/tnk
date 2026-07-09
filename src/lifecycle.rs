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

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::task::spawn_blocking;

pub struct RuntimeLock {
    path: PathBuf,
}

impl Drop for RuntimeLock {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

pub fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

fn lock_path(name: &str) -> Result<PathBuf, color_eyre::Report> {
    let home = std::env::var("HOME")?;
    Ok(PathBuf::from(home)
        .join(".cache/tnk")
        .join(format!("{}.lock", name)))
}

pub async fn acquire(name: &str, timeout: Duration) -> Result<RuntimeLock, color_eyre::Report> {
    let path = lock_path(name)?;
    let parent = path
        .parent()
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid lock path"))?;
    let parent = parent.to_path_buf();
    fs::create_dir_all(&parent).await?;
    spawn_blocking(move || {
        std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))
    })
    .await
    .map_err(|e| color_eyre::eyre::eyre!("set cache dir permissions: {e}"))?
    .ok();

    let pid = std::process::id();
    let started = std::time::Instant::now();
    let mut first_wait_log = true;

    loop {
        let open_result = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .await;

        match open_result {
            Ok(mut file) => {
                file.write_all(pid.to_string().as_bytes()).await?;
                file.flush().await?;
                drop(file);
                let lock_path = path.clone();
                spawn_blocking(move || {
                    std::fs::set_permissions(&lock_path, std::fs::Permissions::from_mode(0o600))
                })
                .await
                .map_err(|e| color_eyre::eyre::eyre!("set lock file permissions: {e}"))?
                .ok();
                return Ok(RuntimeLock { path });
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let stale = match fs::read_to_string(&path).await {
                    Ok(s) => match s.trim().parse::<u32>() {
                        Ok(holder_pid) => !is_process_alive(holder_pid),
                        Err(_) => true,
                    },
                    Err(_) => true,
                };

                if stale {
                    fs::remove_file(&path).await.ok();
                    continue;
                }

                if started.elapsed() >= timeout {
                    return Err(color_eyre::eyre::eyre!(
                        "timed out waiting for lifecycle lock '{}'",
                        name
                    ));
                }

                if first_wait_log {
                    crate::ui::log_info(&format!("waiting for lifecycle lock '{}'", name));
                    first_wait_log = false;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(err) => return Err(err.into()),
        }
    }
}
