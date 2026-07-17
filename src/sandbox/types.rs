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

use std::path::PathBuf;

use tokio::fs;
use tokio::io::AsyncWriteExt;

pub struct TerminalStateGuard {
    fds: Vec<(i32, libc::termios)>,
}

impl TerminalStateGuard {
    pub fn capture() -> Self {
        let mut fds = Vec::new();

        for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            let is_tty = unsafe { libc::isatty(fd) } == 1;
            if !is_tty {
                continue;
            }

            let mut termios: libc::termios = unsafe {
                // termios on aarch64-darwin has no padding or special init requirements;
                // zeroed() produces a valid representation that tcgetattr overwrites on success.
                std::mem::zeroed()
            };
            let ok = unsafe { libc::tcgetattr(fd, &mut termios) } == 0;
            if ok {
                fds.push((fd, termios));
            }
        }

        Self { fds }
    }
}

impl Drop for TerminalStateGuard {
    fn drop(&mut self) {
        for (fd, termios) in &self.fds {
            let _ = unsafe { libc::tcsetattr(*fd, libc::TCSANOW, termios) };
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuditLogger {
    pub path: PathBuf,
}

impl AuditLogger {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub async fn write_event(&self, event: serde_json::Value) -> Result<(), color_eyre::Report> {
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        let line = format!("{}\n", serde_json::to_string(&event)?);
        file.write_all(line.as_bytes()).await?;
        Ok(())
    }
}

pub async fn resolve_audit_logger(
    audit_log: Option<String>,
    id: &str,
) -> Result<Option<AuditLogger>, color_eyre::Report> {
    let Some(path_str) = audit_log else {
        return Ok(None);
    };

    let path = if path_str.trim().is_empty() {
        let home = std::env::var("HOME")?;
        let audit_dir = PathBuf::from(home).join(".cache/tnk/audit");
        fs::create_dir_all(&audit_dir).await?;
        let ts = crate::sandbox::shared::now_unix_seconds();
        audit_dir.join(format!("{}-{}.ndjson", ts, id))
    } else {
        PathBuf::from(path_str)
    };

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).await?;
    }

    Ok(Some(AuditLogger::new(path)))
}

pub fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn validate_script_name(name: &str) -> Result<(), color_eyre::Report> {
    if name.is_empty() {
        return Err(color_eyre::eyre::eyre!("invalid profile name: empty"));
    }
    if name
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        return Err(color_eyre::eyre::eyre!(
            "invalid profile name: unsupported characters"
        ));
    }
    Ok(())
}

pub fn validate_engine_runtime(runtime: &str) -> Result<(), color_eyre::Report> {
    if runtime.is_empty() {
        return Err(color_eyre::eyre::eyre!("invalid runtime: empty"));
    }
    if runtime
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    {
        return Err(color_eyre::eyre::eyre!(
            "invalid runtime: unsupported characters"
        ));
    }
    Ok(())
}

pub fn validate_env_value(value: &str, field: &str) -> Result<(), color_eyre::Report> {
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return Err(color_eyre::eyre::eyre!(
            "invalid value for {}: contains control characters",
            field
        ));
    }
    Ok(())
}

pub fn validate_model_name(name: &str) -> Result<(), color_eyre::Report> {
    if name.is_empty() {
        return Err(color_eyre::eyre::eyre!("invalid model name: empty"));
    }
    if name
        .chars()
        .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/')))
    {
        return Err(color_eyre::eyre::eyre!(
            "invalid model name: unsupported characters"
        ));
    }
    Ok(())
}
