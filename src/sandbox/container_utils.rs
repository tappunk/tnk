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

use std::collections::HashMap;

use tokio::process::Command;

pub fn parse_gateway_from_route_output(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let tokens: Vec<&str> = trimmed.split_whitespace().collect();

        if let Some(idx) = tokens.iter().position(|t| *t == "via")
            && let Some(candidate) = tokens.get(idx + 1)
            && !candidate.trim().is_empty()
        {
            return Some(candidate.trim().to_string());
        }

        if let Some(idx) = tokens.iter().position(|t| *t == "gateway:")
            && let Some(candidate) = tokens.get(idx + 1)
            && !candidate.trim().is_empty()
        {
            return Some(candidate.trim().to_string());
        }
    }

    None
}

pub async fn discover_container_gateway() -> Option<String> {
    let output = tokio::process::Command::new("container")
        .args(["network", "list", "--format", "json"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let entries = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout).ok()?;
    for entry in entries {
        let candidates = [
            entry.get("gateway"),
            entry.get("Gateway"),
            entry.get("status").and_then(|v| v.get("ipv4Gateway")),
            entry.get("status").and_then(|v| v.get("ipv6Gateway")),
            entry.get("status").and_then(|v| v.get("gateway")),
            entry.get("Status").and_then(|v| v.get("IPv4Gateway")),
            entry.get("Status").and_then(|v| v.get("IPv6Gateway")),
            entry.get("Status").and_then(|v| v.get("Gateway")),
            entry.get("ipam").and_then(|v| v.get("gateway")),
            entry.get("IPAM").and_then(|v| v.get("Gateway")),
            entry
                .get("subnets")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("gateway")),
            entry
                .get("Subnets")
                .and_then(|v| v.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("Gateway")),
        ];

        for candidate in candidates.into_iter().flatten() {
            if let Some(ip) = candidate.as_str()
                && !ip.trim().is_empty()
            {
                return Some(ip.trim().to_string());
            }
        }
    }

    None
}

pub fn container_id_from_json(item: &serde_json::Value) -> Option<String> {
    item.get("id")
        .and_then(|v| v.as_str())
        .or_else(|| {
            item.get("configuration")
                .and_then(|v| v.get("id"))
                .and_then(|v| v.as_str())
        })
        .map(|v| v.to_string())
}

pub fn container_has_label_json(item: &serde_json::Value, key: &str, expected: &str) -> bool {
    item.get("configuration")
        .and_then(|v| v.get("labels"))
        .and_then(|v| v.get(key))
        .and_then(|v| v.as_str())
        .is_some_and(|v| v == expected)
}

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct ContainerConfiguration {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, alias = "ID", alias = "Id")]
    pub id_alias: Option<String>,
    #[serde(default)]
    pub labels: Option<HashMap<String, serde_json::Value>>,
    #[serde(default, alias = "Labels")]
    pub labels_alias: Option<HashMap<String, serde_json::Value>>,
}

#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct ContainerListItem {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default, alias = "ID", alias = "Id")]
    pub id_alias: Option<String>,
    #[serde(default)]
    pub status: Option<serde_json::Value>,
    #[serde(default, alias = "Status")]
    pub status_alias: Option<serde_json::Value>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default, alias = "State")]
    pub state_alias: Option<String>,
    #[serde(default)]
    pub configuration: Option<ContainerConfiguration>,
    #[serde(default, alias = "Configuration", alias = "config", alias = "Config")]
    pub configuration_alias: Option<ContainerConfiguration>,
}

impl ContainerListItem {
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref().or(self.id_alias.as_deref()).or_else(|| {
            self.configuration_ref()
                .and_then(|c| c.id.as_deref().or(c.id_alias.as_deref()))
        })
    }

    pub fn status_state(&self) -> Option<&str> {
        self.status
            .as_ref()
            .or(self.status_alias.as_ref())
            .and_then(|v| {
                if let Some(s) = v.as_str() {
                    return Some(s);
                }
                v.get("state")
                    .or_else(|| v.get("State"))
                    .and_then(|s| s.as_str())
            })
            .or(self.state.as_deref())
            .or(self.state_alias.as_deref())
    }

    pub fn label(&self, key: &str) -> Option<&str> {
        self.labels_ref()
            .and_then(|labels| labels.get(key))
            .and_then(|v| v.as_str())
    }

    pub fn has_profile_label(&self) -> bool {
        self.labels_ref()
            .is_some_and(|labels| labels.keys().any(|k| k.starts_with("tnk.profile.")))
    }

    fn configuration_ref(&self) -> Option<&ContainerConfiguration> {
        self.configuration
            .as_ref()
            .or(self.configuration_alias.as_ref())
    }

    fn labels_ref(&self) -> Option<&HashMap<String, serde_json::Value>> {
        self.configuration_ref()
            .and_then(|c| c.labels.as_ref().or(c.labels_alias.as_ref()))
    }
}

pub async fn container_list_all() -> Option<Vec<ContainerListItem>> {
    let output = Command::new("container")
        .args(["list", "--all", "--format", "json"])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }

    serde_json::from_slice::<Vec<ContainerListItem>>(&output.stdout).ok()
}
