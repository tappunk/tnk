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

use std::path::{Path, PathBuf};
use std::process::Command;

pub struct InitCommands {
    pub git_url: Option<String>,
    pub force: bool,
}

const MANAGED_DIRS: &[&str] = &["clients", "sandbox.d", "provider.d"];

pub fn run(cmd: InitCommands) -> Result<(), color_eyre::Report> {
    let config_dir = get_config_dir()?;

    if cmd.force {
        crate::ui::log_info("overwriting existing configs");
    } else if config_dir.exists() {
        let entries: Vec<_> = std::fs::read_dir(&config_dir)?
            .filter_map(|e| e.ok())
            .collect();

        if !entries.is_empty() {
            return Ok(());
        }
    }

    let repo_url = cmd
        .git_url
        .clone()
        .unwrap_or_else(|| "https://github.com/tappunk/tnk-specs.git".to_string());

    let tmp_dir = std::env::temp_dir().join(format!(
        "tnk-init-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    stage_specs_source(&repo_url, &tmp_dir)?;

    if let Some(parent) = config_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if cmd.force && config_dir.exists() {
        sync_managed_dirs(&tmp_dir, &config_dir)?;
    } else {
        std::fs::rename(&tmp_dir, &config_dir)
            .map_err(|e| color_eyre::eyre::eyre!("failed to install configs: {}", e))?;
    }

    let home = std::env::var("HOME").ok();
    if let Some(home) = home {
        let tnk_toml = PathBuf::from(&home).join(".config/tnk/tnk.toml");
        if !tnk_toml.exists() {
            crate::config::init_config(false)?;
        }
    }

    crate::ui::log_info("installed");

    Ok(())
}

fn stage_specs_source(repo_url: &str, dst: &Path) -> Result<(), color_eyre::Report> {
    let local_src = Path::new(repo_url);
    if local_src.is_dir() {
        crate::ui::log_info(&format!(
            "staging tnk-specs from local directory {}",
            local_src.display()
        ));
        copy_dir_contents(local_src, dst)?;
        return Ok(());
    }

    crate::ui::log_info(&format!("cloning tnk-specs into {}", dst.display()));

    let status = Command::new("git")
        .args(["clone", "--depth", "1", repo_url])
        .arg(dst)
        .status()?;

    if !status.success() {
        let _ = std::fs::remove_dir_all(dst);
        eprintln!("error: failed to clone tnk-specs");
        std::process::exit(1);
    }

    Ok(())
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), color_eyre::Report> {
    std::fs::create_dir_all(dst)?;

    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }

        let dst_path = dst.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_contents(&src_path, &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }

    Ok(())
}

fn sync_managed_dirs(src: &Path, dst: &Path) -> Result<(), color_eyre::Report> {
    for dir_name in MANAGED_DIRS {
        let src_path = src.join(dir_name);
        let dst_path = dst.join(dir_name);

        if src_path.exists() {
            if dst_path.exists() {
                remove_dir_all(&dst_path)?;
            }
            std::fs::rename(&src_path, &dst_path)
                .map_err(|e| color_eyre::eyre::eyre!("failed to sync {}: {}", dir_name, e))?;
        }
    }
    Ok(())
}

fn get_config_dir() -> Result<PathBuf, color_eyre::Report> {
    let home = dirs::home_dir()
        .ok_or_else(|| color_eyre::eyre::eyre!("could not determine home directory"))?;
    Ok(home.join(".config/tnk"))
}

fn remove_dir_all(path: &Path) -> Result<(), color_eyre::Report> {
    std::fs::remove_dir_all(path)?;
    Ok(())
}
