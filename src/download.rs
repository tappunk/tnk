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

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tokio_stream::StreamExt;

use crate::OutputFormat;

#[derive(Debug, Clone)]
pub struct HfUrl {
    pub repo_id: String,
    pub revision: String,
    pub path_in_repo: String,
    pub is_folder: bool,
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub is_lfs: bool,
}

#[derive(Debug, Clone)]
pub struct DownloadJob {
    pub file: FileEntry,
    pub local_path: PathBuf,
    pub new: bool,
    pub total_files: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileResult {
    pub path: String,
    pub size: u64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadResult {
    pub repo_id: String,
    pub revision: String,
    pub target_dir: String,
    pub files: Vec<FileResult>,
}

fn hf_base_url() -> String {
    std::env::var("HF_ENDPOINT").unwrap_or_else(|_| "https://huggingface.co".to_string())
}

fn is_path_traversal(segment: &str) -> bool {
    segment == ".." || segment.starts_with("../")
}

#[must_use]
fn is_invalid_repo_path(path: &str) -> bool {
    path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|seg| {
            seg.is_empty()
                || is_path_traversal(seg)
                || !seg
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '.' || c == '-' || c == '_' || c == '/')
        })
}

pub fn normalize_url(input: &str) -> Result<HfUrl, color_eyre::Report> {
    let trimmed = input.trim();

    if trimmed.starts_with("hf://") {
        parse_hf_uri(trimmed)
    } else if trimmed.starts_with("https://huggingface.co/")
        || trimmed.starts_with("http://huggingface.co/")
    {
        parse_browser_url(trimmed)
    } else {
        parse_plain_repo_id(trimmed)
    }
}

fn parse_hf_uri(input: &str) -> Result<HfUrl, color_eyre::Report> {
    let without_scheme = input.strip_prefix("hf://").unwrap();

    let (repo_and_rev, path_in_repo) = if let Some(idx) = split_path_from_repo_rev(without_scheme) {
        idx
    } else {
        (without_scheme, "")
    };

    let (repo_id, revision) = if let Some(at_idx) = repo_and_rev.find('@') {
        (&repo_and_rev[..at_idx], &repo_and_rev[at_idx + 1..])
    } else {
        (repo_and_rev, "main")
    };

    let repo_id = repo_id.trim_end_matches('/');
    let revision = revision.trim_end_matches('/');
    let path_in_repo = path_in_repo.trim_start_matches('/').trim_end_matches('/');

    if repo_id.is_empty() {
        return Err(color_eyre::eyre::eyre!("hf:// URI: missing repository ID"));
    }
    if !repo_id.contains('/') {
        return Err(color_eyre::eyre::eyre!(
            "hf:// URI: repository ID must be 'namespace/name', got '{}'",
            repo_id
        ));
    }
    if !revision.is_empty() && is_path_traversal(revision) {
        return Err(color_eyre::eyre::eyre!(
            "hf:// URI: invalid revision '{}'",
            revision
        ));
    }
    if !path_in_repo.is_empty() && is_invalid_repo_path(path_in_repo) {
        return Err(color_eyre::eyre::eyre!(
            "hf:// URI: invalid path '{}'",
            path_in_repo
        ));
    }

    Ok(HfUrl {
        repo_id: repo_id.to_string(),
        revision: if revision.is_empty() {
            "main".to_string()
        } else {
            revision.to_string()
        },
        path_in_repo: path_in_repo.to_string(),
        is_folder: path_in_repo.is_empty(),
    })
}

fn split_path_from_repo_rev(s: &str) -> Option<(&str, &str)> {
    let after_at = if let Some(at_idx) = s.find('@') {
        s[at_idx + 1..].find('/').map(|i| at_idx + 1 + i)
    } else {
        None
    };

    if let Some(pos) = after_at {
        Some((&s[..pos], &s[pos + 1..]))
    } else {
        None
    }
}

fn parse_browser_url(input: &str) -> Result<HfUrl, color_eyre::Report> {
    let host_stripped = input
        .strip_prefix("https://huggingface.co/")
        .or_else(|| input.strip_prefix("http://huggingface.co/"))
        .ok_or_else(|| color_eyre::eyre::eyre!("invalid Hugging Face URL"))?;

    let parts: Vec<&str> = host_stripped.split('/').collect();
    if parts.len() < 3 {
        return Err(color_eyre::eyre::eyre!(
            "invalid Hugging Face URL: too few path segments"
        ));
    }

    let mut offset = 0;
    if parts[0] == "datasets" || parts[0] == "spaces" {
        return Err(color_eyre::eyre::eyre!(
            "unsupported URL type: '{}' is not a model repository",
            parts[0]
        ));
    }
    if parts[0] == "models" {
        offset = 1;
    }

    let namespace = parts[offset];
    let repo_name = parts[offset + 1];
    let kind = if offset + 2 < parts.len() {
        parts[offset + 2]
    } else {
        ""
    };

    let mut it = parts[offset + 3..].iter();

    match kind {
        "blob" => {
            let revision = it
                .next()
                .copied()
                .ok_or_else(|| color_eyre::eyre::eyre!("blob URL missing revision"))?;
            let path: Vec<&str> = it.copied().collect();
            let path = if path.is_empty() {
                return Err(color_eyre::eyre::eyre!("blob URL missing file path"));
            } else {
                path.join("/")
            };

            if is_path_traversal(&path) || is_path_traversal(revision) {
                return Err(color_eyre::eyre::eyre!(
                    "invalid path or revision in blob URL"
                ));
            }

            Ok(HfUrl {
                repo_id: format!("{}/{}", namespace, repo_name),
                revision: revision.to_string(),
                path_in_repo: path,
                is_folder: false,
            })
        }
        "tree" => {
            let revision = it.next().copied().unwrap_or("main");
            let path: Vec<&str> = it.copied().collect();
            let path = path.join("/");

            if is_path_traversal(&path) || is_path_traversal(revision) {
                return Err(color_eyre::eyre::eyre!(
                    "invalid path or revision in tree URL"
                ));
            }

            Ok(HfUrl {
                repo_id: format!("{}/{}", namespace, repo_name),
                revision: revision.to_string(),
                path_in_repo: path,
                is_folder: true,
            })
        }
        "resolve" => Err(color_eyre::eyre::eyre!(
            "resolve URLs are not supported as input; use blob or tree URLs"
        )),
        _ => Ok(HfUrl {
            repo_id: format!("{}/{}", namespace, repo_name),
            revision: "main".to_string(),
            path_in_repo: String::new(),
            is_folder: true,
        }),
    }
}

fn parse_plain_repo_id(input: &str) -> Result<HfUrl, color_eyre::Report> {
    if !input.contains('/') {
        return Err(color_eyre::eyre::eyre!(
            "repository ID must be 'namespace/name', got '{}'",
            input
        ));
    }

    if is_invalid_repo_path(input) {
        return Err(color_eyre::eyre::eyre!(
            "invalid repository ID '{}': path traversal detected",
            input
        ));
    }

    Ok(HfUrl {
        repo_id: input.to_string(),
        revision: "main".to_string(),
        path_in_repo: String::new(),
        is_folder: true,
    })
}

pub fn resolve_target_dir(repo_id: &str, model_dir: &str) -> PathBuf {
    PathBuf::from(model_dir).join(repo_id)
}

pub async fn list_files(url: &HfUrl) -> Result<Vec<FileEntry>, color_eyre::Report> {
    let base = hf_base_url();
    let tree_url = format!(
        "{}/api/models/{}/tree/{}",
        base.trim_end_matches('/'),
        url.repo_id,
        url.revision
    );

    let client = reqwest::Client::new();
    let resp = client
        .get(&tree_url)
        .header("User-Agent", "tnk/0.1.7")
        .send()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to query tree API: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        return Err(match status.as_u16() {
            401 => color_eyre::eyre::eyre!(
                "authentication required for '{}'. This may be a gated repository.",
                url.repo_id
            ),
            403 => color_eyre::eyre::eyre!(
                "access denied for '{}'. This may be a gated repository.",
                url.repo_id
            ),
            404 => color_eyre::eyre::eyre!("repository '{}' not found", url.repo_id),
            _ => {
                color_eyre::eyre::eyre!("tree API returned status {} for '{}'", status, url.repo_id)
            }
        });
    }

    let entries: Vec<TreeEntry> = resp
        .json()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("failed to parse tree API response: {}", e))?;

    let mut result = Vec::new();

    for entry in entries {
        if entry.entry_type != "file" {
            continue;
        }

        if !url.path_in_repo.is_empty() {
            let prefix = if url.path_in_repo.ends_with('/') {
                url.path_in_repo.clone()
            } else {
                format!("{}/", url.path_in_repo)
            };
            if entry.path != url.path_in_repo && !entry.path.starts_with(&prefix) {
                continue;
            }
        }

        let size = if let Some(lfs) = &entry.lfs {
            lfs.size as u64
        } else {
            entry.size as u64
        };
        let is_lfs = entry.lfs.is_some();

        result.push(FileEntry {
            path: entry.path,
            size,
            is_lfs,
        });
    }

    if result.is_empty() && !url.path_in_repo.is_empty() {
        return Err(color_eyre::eyre::eyre!(
            "path '{}' not found in '{}@{}'",
            url.path_in_repo,
            url.repo_id,
            url.revision
        ));
    }

    Ok(result)
}

#[derive(serde::Deserialize, Debug)]
struct TreeEntry {
    #[serde(rename = "type")]
    entry_type: String,
    path: String,
    size: i64,
    #[serde(default)]
    lfs: Option<LfsMeta>,
}

#[derive(serde::Deserialize, Debug)]
struct LfsMeta {
    size: i64,
}

pub async fn build_download_jobs(
    target_dir: &Path,
    entries: &[FileEntry],
    force: bool,
) -> Result<Vec<DownloadJob>, color_eyre::Report> {
    let mut jobs = Vec::new();

    for entry in entries.iter() {
        let local = target_dir.join(&entry.path);
        let is_new = if force {
            true
        } else {
            match tokio::fs::metadata(&local).await {
                Ok(meta) => meta.len() != entry.size,
                Err(_) => true,
            }
        };

        jobs.push(DownloadJob {
            file: entry.clone(),
            local_path: local,
            new: is_new,
            total_files: entries.len(),
        });
    }

    Ok(jobs)
}

fn make_tmp_path(local: &Path) -> PathBuf {
    PathBuf::from(format!("{}.tmp", local.to_string_lossy()))
}

pub async fn download_file(
    job: &DownloadJob,
    repo_id: &str,
    revision: &str,
    output: OutputFormat,
) -> Result<FileResult, color_eyre::Report> {
    let base = hf_base_url();
    let resolve_url = format!(
        "{}/{}/resolve/{}/{}",
        base.trim_end_matches('/'),
        repo_id,
        revision,
        urlencoding_path(&job.file.path)
    );

    let tmp_path = make_tmp_path(&job.local_path);

    let start_offset = if job.new {
        0u64
    } else {
        match tokio::fs::metadata(&tmp_path).await {
            Ok(meta) => meta.len(),
            Err(_) => 0,
        }
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| color_eyre::eyre::eyre!("failed to build HTTP client: {}", e))?;

    let resp = if start_offset > 0 {
        client
            .get(&resolve_url)
            .header("User-Agent", "tnk/0.1.7")
            .header("Range", format!("bytes={}-", start_offset))
            .send()
            .await
            .map_err(|e| color_eyre::eyre::eyre!("request failed: {}", e))?
    } else {
        client
            .get(&resolve_url)
            .header("User-Agent", "tnk/0.1.7")
            .send()
            .await
            .map_err(|e| color_eyre::eyre::eyre!("request failed: {}", e))?
    };

    let status = resp.status();
    if !status.is_success() && status.as_u16() != 206 {
        return Err(match status.as_u16() {
            401 => color_eyre::eyre::eyre!(
                "authentication required for '{}'. This may be a gated repository.",
                repo_id
            ),
            403 => color_eyre::eyre::eyre!("access denied for '{}'", repo_id),
            404 => color_eyre::eyre::eyre!(
                "file '{}' not found in '{}@{}'",
                job.file.path,
                repo_id,
                revision
            ),
            _ => {
                color_eyre::eyre::eyre!("download failed for '{}': HTTP {}", job.file.path, status)
            }
        });
    }

    let expected_size = job.file.size;

    tokio::fs::create_dir_all(tmp_path.parent().unwrap()).await?;

    let mut file = if start_offset > 0 {
        tokio::fs::OpenOptions::new()
            .write(true)
            .append(true)
            .open(&tmp_path)
            .await?
    } else {
        tokio::fs::File::create(&tmp_path).await?
    };
    let mut stream = resp.bytes_stream();
    let mut downloaded: u64 = start_offset;
    let start_time = std::time::Instant::now();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|e| {
            color_eyre::eyre::eyre!("download stream error for '{}': {}", job.file.path, e)
        })?;
        file.write_all(&chunk).await.map_err(|e| {
            let kind = e.kind();
            if kind == std::io::ErrorKind::StorageFull {
                color_eyre::eyre::eyre!(
                    "disk full while writing '{}' — delete temporary files and retry",
                    job.file.path
                )
            } else {
                color_eyre::eyre::eyre!("failed to write '{}': {}", job.file.path, e)
            }
        })?;
        downloaded += chunk.len() as u64;

        let is_tty = crate::ui::is_human_output(output);
        if is_tty && crate::ui::is_verbose() {
            let elapsed = start_time.elapsed().as_secs_f64();
            let speed = if elapsed > 0.0 {
                (downloaded as f64 / elapsed) as u64
            } else {
                0
            };
            let progress = if expected_size > 0 {
                (downloaded as f64 / expected_size as f64) * 100.0
            } else {
                100.0
            };

            let bar_width = 10;
            let filled = (progress / 100.0 * bar_width as f64) as u64;
            let bar: String = (0..bar_width)
                .map(|i| if i < filled { '#' } else { '-' })
                .collect();

            eprintln!(
                "\rinfo: {} {} {:.1}% [{}] {:.1} MB/s",
                format_file_name(&job.file.path),
                format_size(downloaded),
                progress,
                bar,
                speed as f64 / 1_048_576.0,
            );
            std::io::stderr().flush().ok();
        }
    }

    file.flush().await?;
    drop(file);

    let actual_size = tokio::fs::metadata(&tmp_path).await?.len();

    if actual_size != expected_size {
        eprintln!(
            "\nwarning: size mismatch for '{}' (expected {}, got {}) — cleaning up partial file",
            job.file.path, expected_size, actual_size
        );
        tokio::fs::remove_file(&tmp_path).await.ok();
        return Ok(FileResult {
            path: job.file.path.clone(),
            size: expected_size,
            status: "error".to_string(),
            local_path: Some(job.local_path.to_string_lossy().to_string()),
        });
    }

    tokio::fs::rename(&tmp_path, &job.local_path).await?;

    Ok(FileResult {
        path: job.file.path.clone(),
        size: expected_size,
        status: "downloaded".to_string(),
        local_path: Some(job.local_path.to_string_lossy().to_string()),
    })
}

fn urlencoding_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    for c in path.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '/' | '~' => {
                result.push(c);
            }
            ' ' => result.push_str("%20"),
            _ => {
                let bytes = c.to_string().into_bytes();
                for b in bytes {
                    result.push_str(&format!("%{:02X}", b));
                }
            }
        }
    }
    result
}

pub async fn download_all(
    jobs: Vec<DownloadJob>,
    repo_id: &str,
    revision: &str,
    workers: usize,
    output: OutputFormat,
) -> Result<DownloadResult, color_eyre::Report> {
    let mut results = Vec::new();
    let semaphore = Arc::new(tokio::sync::Semaphore::new(workers));
    let is_tty = crate::ui::is_human_output(output);
    let total_downloaded = Arc::new(Mutex::new(0u64));

    let mut tasks = Vec::new();

    for job in jobs {
        if !job.new {
            if is_tty {
                eprintln!(
                    "info: {} {} cached",
                    format_file_name(&job.file.path),
                    format_size(job.file.size),
                );
            }
            results.push(FileResult {
                path: job.file.path.clone(),
                size: job.file.size,
                status: "cached".to_string(),
                local_path: Some(job.local_path.to_string_lossy().to_string()),
            });
            continue;
        }

        let repo_id = repo_id.to_string();
        let revision = revision.to_string();
        let total_downloaded = total_downloaded.clone();

        if is_tty {
            eprintln!(
                "info: {} {} downloading...",
                format_file_name(&job.file.path),
                format_size(job.file.size),
            );
        }

        let sem = semaphore.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            let result = download_file(&job, &repo_id, &revision, output).await;
            if let Ok(ref r) = result
                && r.status == "downloaded"
            {
                let mut guard = total_downloaded.lock().await;
                *guard += r.size;
            }
            (job.file.path.clone(), result)
        });

        tasks.push(handle);
    }

    for handle in tasks {
        match handle.await {
            Ok((_path, Ok(result))) => {
                results.push(result);
            }
            Ok((path, Err(e))) => {
                let path_clone = path.clone();
                results.push(FileResult {
                    path,
                    size: 0,
                    status: "error".to_string(),
                    local_path: None,
                });
                eprintln!("error: download failed for '{}': {}", path_clone, e);
            }
            Err(e) => {
                eprintln!("error: task panicked: {}", e);
                results.push(FileResult {
                    path: String::new(),
                    size: 0,
                    status: "error".to_string(),
                    local_path: None,
                });
            }
        }
    }

    let downloaded_count = results.iter().filter(|r| r.status == "cached").count();
    let new_count = results.iter().filter(|r| r.status == "downloaded").count();
    let total_bytes = *total_downloaded.lock().await;

    if is_tty && (downloaded_count > 0 || new_count > 0) {
        eprintln!();
        if new_count > 0 {
            eprintln!(
                "info: downloaded {} file{} ({}), {} cached",
                new_count,
                if new_count > 1 { "s" } else { "" },
                format_size(total_bytes),
                downloaded_count,
            );
        } else {
            eprintln!(
                "info: all {} file{} already cached",
                downloaded_count,
                if downloaded_count > 1 { "s" } else { "" }
            );
        }
    }

    Ok(DownloadResult {
        repo_id: repo_id.to_string(),
        revision: revision.to_string(),
        target_dir: String::new(),
        files: results,
    })
}

pub async fn dry_run_cmd(
    url: &HfUrl,
    target_dir: &Path,
) -> Result<DownloadResult, color_eyre::Report> {
    let entries = list_files(url).await?;

    if entries.is_empty() {
        return Err(color_eyre::eyre::eyre!("no files to download"));
    }

    let mut files = Vec::new();
    let mut total_size: u64 = 0;
    let mut new_count = 0;
    let mut cached_count = 0;

    for entry in &entries {
        let local = target_dir.join(&entry.path);
        let (status, is_cached) = match tokio::fs::metadata(&local).await {
            Ok(meta) if meta.len() == entry.size => ("cached".to_string(), true),
            Ok(_) => ("redownload".to_string(), false),
            Err(_) => ("new".to_string(), false),
        };

        if is_cached {
            cached_count += 1;
        } else {
            new_count += 1;
            total_size += entry.size;
        }

        files.push(FileResult {
            path: entry.path.clone(),
            size: entry.size,
            status,
            local_path: if is_cached {
                Some(local.to_string_lossy().to_string())
            } else {
                None
            },
        });
    }

    crate::ui::log_info(&format!(
        "{} file{}",
        entries.len(),
        if entries.len() > 1 { "s" } else { "" }
    ));
    if new_count > 0 {
        crate::ui::log_info(&format!(
            "  {} new, {} cached, {} total",
            new_count,
            cached_count,
            format_size(total_size)
        ));
    } else {
        crate::ui::log_info(&format!("  all {} cached", cached_count));
    }

    for file in &files {
        let status_label = match file.status.as_str() {
            "cached" => "cached",
            "new" => "new",
            "redownload" => "redownload",
            _ => &file.status,
        };
        crate::ui::log_info(&format!(
            "  {} {}",
            format_file_name(&file.path),
            status_label,
        ));
    }

    Ok(DownloadResult {
        repo_id: url.repo_id.clone(),
        revision: url.revision.clone(),
        target_dir: target_dir.to_string_lossy().to_string(),
        files,
    })
}

fn format_file_name(path: &str) -> String {
    if let Some(pos) = path.rfind('/') {
        path[pos + 1..].to_string()
    } else {
        path.to_string()
    }
}

fn format_size(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1} GiB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.1} KB", bytes as f64 / 1_024.0)
    } else {
        format!("{} B", bytes)
    }
}

pub async fn run(
    url_input: String,
    output: OutputFormat,
    dry_run: bool,
    revision: Option<String>,
    workers: usize,
    force: bool,
) -> Result<(), color_eyre::Report> {
    let mut url = normalize_url(&url_input)?;

    if let Some(rev) = revision {
        url.revision = rev;
    }

    let cfg = crate::config::load().await?;
    let resolved = crate::config::ResolvedConfig::resolve(&cfg)?;
    let target_dir = resolve_target_dir(&url.repo_id, &resolved.model_dir);

    crate::ui::log_info(&format!(
        "listing files for {}@{}",
        url.repo_id, url.revision
    ));

    if dry_run {
        let result = dry_run_cmd(&url, &target_dir).await?;
        print_result(&result, output);
        return Ok(());
    }

    let entries = list_files(&url).await?;

    if entries.is_empty() {
        return Err(color_eyre::eyre::eyre!("no files to download"));
    }

    crate::ui::log_info(&format!("target directory: {}", target_dir.display()));

    tokio::fs::create_dir_all(&target_dir).await?;

    let jobs = build_download_jobs(&target_dir, &entries, force).await?;

    let mut result = download_all(jobs, &url.repo_id, &url.revision, workers, output).await?;
    result.target_dir = target_dir.to_string_lossy().to_string();

    if !crate::ui::is_human_output(output) {
        print_result(&result, output);
    } else {
        println!("{}", target_dir.display());
    }

    Ok(())
}

fn print_result(result: &DownloadResult, output: OutputFormat) {
    match output {
        OutputFormat::Text => {
            println!("{}", result.target_dir);
        }
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(result).unwrap_or_default();
            println!("{}", json);
        }
        OutputFormat::Ndjson => {
            for file in &result.files {
                let json = serde_json::to_string(file).unwrap_or_default();
                println!("{}", json);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_blob_url() {
        let url = normalize_url("https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/blob/main/Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf").unwrap();
        assert_eq!(url.repo_id, "unsloth/Qwen3.6-35B-A3B-GGUF");
        assert_eq!(url.revision, "main");
        assert_eq!(url.path_in_repo, "Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf");
        assert!(!url.is_folder);
    }

    #[test]
    fn normalize_tree_url() {
        let url =
            normalize_url("https://huggingface.co/mlx-community/Qwen3.5-9B-MLX-4bit/tree/main")
                .unwrap();
        assert_eq!(url.repo_id, "mlx-community/Qwen3.5-9B-MLX-4bit");
        assert_eq!(url.revision, "main");
        assert_eq!(url.path_in_repo, "");
        assert!(url.is_folder);
    }

    #[test]
    fn normalize_tree_url_with_subdir() {
        let url =
            normalize_url("https://huggingface.co/unsloth/Qwen3.6-35B-A3B-GGUF/tree/main/BF16")
                .unwrap();
        assert_eq!(url.repo_id, "unsloth/Qwen3.6-35B-A3B-GGUF");
        assert_eq!(url.revision, "main");
        assert_eq!(url.path_in_repo, "BF16");
        assert!(url.is_folder);
    }

    #[test]
    fn normalize_plain_repo_id() {
        let url = normalize_url("unsloth/Qwen3.6-35B-A3B-GGUF").unwrap();
        assert_eq!(url.repo_id, "unsloth/Qwen3.6-35B-A3B-GGUF");
        assert_eq!(url.revision, "main");
        assert_eq!(url.path_in_repo, "");
        assert!(url.is_folder);
    }

    #[test]
    fn normalize_hf_uri() {
        let url =
            normalize_url("hf://unsloth/Qwen3.6-35B-A3B-GGUF@main/Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf")
                .unwrap();
        assert_eq!(url.repo_id, "unsloth/Qwen3.6-35B-A3B-GGUF");
        assert_eq!(url.revision, "main");
        assert_eq!(url.path_in_repo, "Qwen3.6-35B-A3B-UD-Q4_K_XL.gguf");
        assert!(!url.is_folder);
    }

    #[test]
    fn reject_dataset_url() {
        assert!(
            normalize_url(
                "https://huggingface.co/datasets/HuggingFaceH4/ultrachat_200k/blob/main/data.json"
            )
            .is_err()
        );
    }

    #[test]
    fn reject_malformed_url() {
        assert!(normalize_url("https://example.com/something").is_err());
    }

    #[test]
    fn test_resolve_target_dir() {
        assert_eq!(
            resolve_target_dir("unsloth/Qwen3.6-35B-A3B-GGUF", "/models"),
            PathBuf::from("/models/unsloth/Qwen3.6-35B-A3B-GGUF")
        );
    }

    #[test]
    fn validate_path_no_traversal() {
        assert!(normalize_url("https://huggingface.co/x/y/blob/main/../etc/passwd").is_err());
    }

    #[test]
    fn format_size_human_readable() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1_048_576), "1.0 MiB");
        assert_eq!(format_size(1_073_741_824), "1.0 GiB");
    }

    #[test]
    fn format_file_name_strips_directory() {
        assert_eq!(
            format_file_name("model-0001.safetensors"),
            "model-0001.safetensors"
        );
        assert_eq!(
            format_file_name("subdir/model-0001.safetensors"),
            "model-0001.safetensors"
        );
        assert_eq!(format_file_name("a/b/c/file.gguf"), "file.gguf");
    }
}
