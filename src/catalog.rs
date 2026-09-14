use std::path::{Path, PathBuf};
use tokio::fs;

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub manifest_path: Option<PathBuf>,
}

pub async fn list_profiles(config_dir: &Path) -> Result<Vec<Profile>, color_eyre::Report> {
    let mut profiles = Vec::new();

    let provision_dir = config_dir.join("sandbox.d/provision.d");
    if provision_dir.is_dir() {
        let mut entries = fs::read_dir(&provision_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "sh") {
                let name = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                profiles.push(Profile {
                    manifest_path: resolve_manifest(config_dir, &name),
                    name,
                });
            }
        }
    }

    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(profiles)
}

pub fn resolve_manifest(config_dir: &Path, profile_name: &str) -> Option<PathBuf> {
    let manifests_dir = config_dir.join("sandbox.d/manifests");
    let profile_specific = manifests_dir.join(format!("{}.yaml", profile_name));
    if profile_specific.is_file() {
        return Some(profile_specific);
    }
    let base = manifests_dir.join("base.yaml");
    if base.is_file() {
        return Some(base);
    }
    crate::ui::log_warn(&format!(
        "no manifest for profile '{}' and base.yaml is missing",
        profile_name
    ));
    None
}
