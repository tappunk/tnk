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

use std::time::Duration;

pub const DEFAULT_CONTEXT_WINDOW: u32 = 131072;

fn value_as_u32(value: &serde_json::Value) -> Option<u32> {
    if let Some(v) = value.as_u64() {
        return u32::try_from(v).ok();
    }
    if let Some(v) = value.as_i64()
        && v >= 0
    {
        return u32::try_from(v as u64).ok();
    }
    if let Some(v) = value.as_str()
        && let Ok(parsed) = v.parse::<u32>()
    {
        return Some(parsed);
    }
    None
}

fn extract_ctx_window(item: &serde_json::Value) -> Option<u32> {
    let top_level_keys = [
        "context_window",
        "context_length",
        "max_context_length",
        "input_token_limit",
        "max_input_tokens",
        "n_ctx",
    ];
    for key in top_level_keys {
        if let Some(value) = item.get(key)
            && let Some(parsed) = value_as_u32(value)
        {
            return Some(parsed);
        }
    }

    if let Some(meta) = item.get("meta") {
        let meta_keys = [
            "n_ctx",
            "context_window",
            "context_length",
            "max_context_length",
        ];
        for key in meta_keys {
            if let Some(value) = meta.get(key)
                && let Some(parsed) = value_as_u32(value)
            {
                return Some(parsed);
            }
        }
    }

    if let Some(limits) = item.get("limits") {
        if let Some(value) = limits.get("context")
            && let Some(parsed) = value_as_u32(value)
        {
            return Some(parsed);
        }
        if let Some(value) = limits.get("input")
            && let Some(parsed) = value_as_u32(value)
        {
            return Some(parsed);
        }
    }

    if let Some(capabilities) = item.get("capabilities") {
        let capability_keys = ["context_window", "max_context_length", "input_token_limit"];
        for key in capability_keys {
            if let Some(value) = capabilities.get(key)
                && let Some(parsed) = value_as_u32(value)
            {
                return Some(parsed);
            }
        }
    }

    None
}

pub async fn verify_health(host: &str, port: u16) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    let url = format!("http://{}:{}/health", host, port);
    client.get(&url).send().await.is_ok()
}

pub async fn get_ctx_window(host: &str, port: u16) -> Result<u32, color_eyre::Report> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;

    let url = format!("http://{}:{}/v1/models", host, port);
    let response = client.get(&url).send().await?;
    let json: serde_json::Value = response.json().await?;

    if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
        for item in data {
            if let Some(ctx) = extract_ctx_window(item) {
                return Ok(ctx);
            }
        }
    }

    if let Some(ctx) = extract_ctx_window(&json) {
        return Ok(ctx);
    }

    Ok(DEFAULT_CONTEXT_WINDOW)
}

pub async fn poll_loaded_model(
    host: &str,
    port: u16,
    max_retries: u32,
    interval_secs: f32,
) -> Result<String, color_eyre::Report> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()?;

    let url = format!("http://{}:{}/v1/models", host, port);

    for i in 1..=max_retries {
        if let Ok(response) = client.get(&url).send().await
            && let Ok(json) = response.json::<serde_json::Value>().await
            && let Some(data) = json.get("data").and_then(|d| d.as_array())
        {
            for item in data {
                if let Some(id) = item.get("id") {
                    return Ok(id.as_str().unwrap_or("").to_string());
                }
            }
        }

        if i % 5 == 0 {
            crate::ui::log_info(&format!("waiting for loaded model ({}/{})", i, max_retries));
        }

        tokio::time::sleep(Duration::from_secs_f32(interval_secs)).await;
    }

    Err(color_eyre::eyre::eyre!(
        "timeout: could not get loaded model from inference server"
    ))
}
