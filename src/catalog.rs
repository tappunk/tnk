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
use tokio::fs;

const EXCLUDED_PROFILES: &[&str] = &["tnk-services"];

#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub manifest_path: Option<PathBuf>,
}

pub async fn list_profiles(config_dir: &Path) -> Result<Vec<Profile>, color_eyre::Report> {
    let mut profiles = Vec::new();

    let provision_dir = config_dir.join("sandbox.d/container/provision.d");
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
                if name.is_empty() || EXCLUDED_PROFILES.contains(&name.as_str()) {
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
    let manifests_dir = config_dir.join("sandbox.d/container/manifests");
    let profile_specific = manifests_dir.join(format!("{}.yaml", profile_name));
    if profile_specific.is_file() {
        return Some(profile_specific);
    }
    let base = manifests_dir.join("base-sandbox.yaml");
    if base.is_file() {
        crate::ui::log_info(&format!(
            "no manifest for profile '{}', falling back to base",
            profile_name
        ));
        Some(base)
    } else {
        crate::ui::log_warn(&format!(
            "no manifest for profile '{}' and base-sandbox.yaml is missing",
            profile_name
        ));
        None
    }
}
