use std::path::{Path, PathBuf};
use std::process::Command;

pub struct InitCommands {
    pub git_url: Option<String>,
    pub force: bool,
}

const MANAGED_DIRS: &[&str] = &["sandbox.d"];

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

    let tmp_dir = tempfile::tempdir()
        .map_err(|e| color_eyre::eyre::eyre!("failed to create temp dir: {}", e))?;

    stage_specs_source(&repo_url, tmp_dir.path())?;

    std::fs::create_dir_all(&config_dir)?;
    sync_managed_dirs(tmp_dir.path(), &config_dir)?;

    let tnk_toml = config_dir.join("tnk.toml");
    if !tnk_toml.exists() {
        let src_toml = tmp_dir.path().join("tnk.toml");
        if src_toml.exists() {
            std::fs::copy(&src_toml, &tnk_toml)
                .map_err(|e| color_eyre::eyre::eyre!("failed to install tnk.toml: {}", e))?;
        } else {
            crate::config::init_config(false)?;
        }
    }

    let specs_rev = read_specs_rev(tmp_dir.path());
    if specs_rev != "local" {
        std::fs::write(config_dir.join(".specs_rev"), format!("{}\n", specs_rev))?;
    }

    crate::ui::log_info("installed");

    Ok(())
}

fn read_specs_rev(src: &Path) -> String {
    if src.join(".git").exists()
        && let Ok(output) = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .current_dir(src)
            .output()
        && let Ok(rev) = String::from_utf8(output.stdout)
        && let Some(rev) = rev.trim().get(..7)
    {
        return format!("git:{}", rev);
    }
    "local".to_string()
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
        return Err(color_eyre::eyre::eyre!("failed to clone tnk-specs"));
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
            if let Err(_e) = std::fs::rename(&src_path, &dst_path) {
                copy_dir_contents(&src_path, &dst_path)
                    .map_err(|e| color_eyre::eyre::eyre!("failed to sync {}: {}", dir_name, e))?;
                let _ = std::fs::remove_dir_all(&src_path);
            }
        }
    }
    Ok(())
}

fn get_config_dir() -> Result<PathBuf, color_eyre::Report> {
    let home = std::env::var("HOME")
        .map_err(|_| color_eyre::eyre::eyre!("could not determine home directory"))?;
    Ok(PathBuf::from(home).join(".config/tnk"))
}

fn remove_dir_all(path: &Path) -> Result<(), color_eyre::Report> {
    std::fs::remove_dir_all(path)?;
    Ok(())
}
