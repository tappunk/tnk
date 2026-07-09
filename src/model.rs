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

use std::sync::OnceLock;
use std::time::Duration;

pub const DEFAULT_CONTEXT_WINDOW: u32 = 131072;

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn cached_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("reqwest client build should not fail")
    })
}

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
    let url = format!("http://{}:{}/health", host, port);
    cached_client().get(&url).send().await.is_ok()
}

pub async fn get_ctx_window(host: &str, port: u16) -> Result<u32, color_eyre::Report> {
    let url = format!("http://{}:{}/v1/models", host, port);
    let response = cached_client().get(&url).send().await?;
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
    interval_secs: f64,
) -> Result<Option<String>, color_eyre::Report> {
    let url = format!("http://{}:{}/v1/models", host, port);

    let mut json_parse_failures = 0;
    let mut last_content_type = String::from("unknown");

    for i in 1..=max_retries {
        let response = match cached_client().get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                crate::ui::log_warn(&format!("request failed (attempt {i}/{max_retries}): {e}"));
                tokio::time::sleep(Duration::from_secs_f64(interval_secs)).await;
                continue;
            }
        };

        if let Some(v) = response.headers().get(reqwest::header::CONTENT_TYPE)
            && let Ok(s) = v.to_str()
        {
            last_content_type = s.to_string();
        }

        let json = match response.json::<serde_json::Value>().await {
            Ok(j) => j,
            Err(_) => {
                json_parse_failures += 1;
                if json_parse_failures >= 3 {
                    crate::ui::log_warn(&format!(
                        "non-JSON response (content-type: {last_content_type}) on {json_parse_failures} consecutive attempts"
                    ));
                }
                tokio::time::sleep(Duration::from_secs_f64(interval_secs)).await;
                continue;
            }
        };

        if let Some(data) = json.get("data").and_then(|d| d.as_array()) {
            for item in data {
                if let Some(id) = item.get("id")
                    && let Some(model_id) = id.as_str()
                    && !model_id.is_empty()
                {
                    return Ok(Some(model_id.to_string()));
                }
            }
        }

        if i % 5 == 0 {
            crate::ui::log_info(&format!("waiting for loaded model ({}/{})", i, max_retries));
        }

        tokio::time::sleep(Duration::from_secs_f64(interval_secs)).await;
    }

    Err(color_eyre::eyre::eyre!(
        "timeout: could not get loaded model from inference server (content-type seen: {last_content_type})"
    ))
}
