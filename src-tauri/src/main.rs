#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod tools;

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicBool, Ordering},
};

use serde::Serialize;
use serde_json::Value;
use tauri::{ipc::Channel, AppHandle, State};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    time::{sleep, Duration},
};

use tools::ToolPaths;

// Bot checks ("Sign in to confirm you're not a bot") are worth waiting out.
// Age gates are not: they need cookies, so waiting never fixes them.
const RATE_LIMIT_PATTERNS: [&str; 7] = [
    "403",
    "forbidden",
    "rate limit",
    "not a bot",
    "too many requests",
    "throttl",
    "429",
];
const UNAVAILABLE_PATTERNS: [&str; 11] = [
    "unavailable",
    "private video",
    "removed by",
    "does not exist",
    "age-restricted",
    "confirm your age",
    "geo-restricted",
    "not available",
    "available in your country",
    "not made this video available",
    "members",
];
const RATE_LIMIT_WAIT_SECS: u64 = 180;
const MAX_RATE_LIMIT_RETRIES: u32 = 3;
const MAX_ERROR_RETRIES: u32 = 3;
const ERROR_RETRY_DELAY_SECS: u64 = 5;
const CANCEL_POLL_MILLIS: u64 = 200;

#[derive(Default)]
struct Downloads {
    busy: AtomicBool,
    cancel: AtomicBool,
}

#[derive(Serialize)]
struct Entry {
    index: usize,
    title: String,
    url: String,
}

#[derive(Serialize)]
struct Playlist {
    title: String,
    entries: Vec<Entry>,
}

#[derive(Clone, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
enum DownloadEvent {
    Total { items: usize },
    Progress { index: usize, title: String, fraction: f64 },
    FileDone { index: usize },
    ItemFailed { index: usize },
    Log { level: &'static str, message: String },
    Status { message: String },
    Done { message: String },
}

/// Why a single video download did not succeed.
enum Failure {
    Cancelled,
    Failed(String),
}

/// Arguments shared by every yt-dlp invocation.
fn common_args(deno: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "--retries",
        "5",
        "--sleep-requests",
        "1",
        "--sleep-interval",
        "3",
        "--max-sleep-interval",
        "8",
        "--http-chunk-size",
        "10M",
        "--remote-components",
        "ejs:github",
    ]
    .map(String::from)
    .to_vec();

    args.push("--js-runtimes".into());
    args.push(format!("deno:{}", deno.display()));
    args
}

/// Keeps the console window from flashing up for each yt-dlp call on Windows.
fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = cmd;
    }
}

fn is_rate_limited(msg: &str) -> bool {
    let m = msg.to_lowercase();
    RATE_LIMIT_PATTERNS.iter().any(|p| m.contains(p))
}

fn is_unavailable(msg: &str) -> bool {
    let m = msg.to_lowercase();
    UNAVAILABLE_PATTERNS.iter().any(|p| m.contains(p))
}

fn video_url(entry: &Value) -> Option<String> {
    if let Some(url) = entry["url"].as_str().or(entry["webpage_url"].as_str()) {
        return Some(url.to_string());
    }
    entry["id"]
        .as_str()
        .map(|id| format!("https://www.youtube.com/watch?v={id}"))
}

/// Lists the items of a playlist (or a single video) without downloading them.
async fn list_items(tools: &ToolPaths, url: &str) -> Result<Playlist, String> {
    let mut cmd = Command::new(&tools.yt_dlp);
    cmd.args(common_args(&tools.deno))
        .args(["--flat-playlist", "--no-warnings", "-J", url]);
    hide_console(&mut cmd);
    let output = cmd.output().await.map_err(|e| e.to_string())?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        log::error!("list failed for {url}:\n{stderr}");
        return Err(stderr);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: Value = serde_json::from_str(&stdout).map_err(|e| {
        log::error!("could not parse yt-dlp JSON for {url}: {e}");
        e.to_string()
    })?;
    let title = json["title"].as_str().unwrap_or("unknown").to_string();

    let entries = match json["entries"].as_array() {
        Some(items) => items
            .iter()
            .filter(|e| !e.is_null())
            .enumerate()
            .map(|(i, e)| Entry {
                index: i + 1,
                title: e["title"].as_str().unwrap_or("?").to_string(),
                url: video_url(e).unwrap_or_default(),
            })
            .filter(|e| !e.url.is_empty())
            .collect(),
        None => vec![Entry {
            index: 1,
            title: title.clone(),
            url: url.to_string(),
        }],
    };

    Ok(Playlist { title, entries })
}

#[tauri::command]
async fn tools_status(app: AppHandle) -> tools::ToolsStatus {
    tools::status(&app).await
}

#[tauri::command]
async fn install_tools(
    app: AppHandle,
    state: State<'_, Downloads>,
    on_event: Channel<DownloadEvent>,
) -> Result<(), String> {
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err("Wait for the current download to finish".into());
    }
    log::info!("installing tools");

    let result = match tools::tools_dir(&app) {
        Ok(dir) => tools::install(&dir, |msg| log(&on_event, "info", msg)).await,
        Err(e) => Err(e),
    };
    if let Err(e) = &result {
        log::error!("tool install failed: {e}");
        log(&on_event, "error", e.clone());
    }

    state.busy.store(false, Ordering::SeqCst);
    result
}

#[tauri::command]
async fn fetch_playlist(app: AppHandle, url: String) -> Result<Playlist, String> {
    let tools = tools::installed(&app)?;
    list_items(&tools, url.trim()).await
}

#[tauri::command]
async fn start_download(
    app: AppHandle,
    state: State<'_, Downloads>,
    url: String,
    dir: String,
    on_event: Channel<DownloadEvent>,
) -> Result<(), String> {
    if state.busy.swap(true, Ordering::SeqCst) {
        return Err("A download is already running".into());
    }
    state.cancel.store(false, Ordering::SeqCst);
    log::info!("download started: url={url} dir={dir}");

    let result = run_download(&app, &state, url.trim(), &dir, &on_event).await;
    if let Err(e) = &result {
        log::error!("download aborted: {e}");
    }

    state.busy.store(false, Ordering::SeqCst);
    result
}

#[tauri::command]
fn cancel_download(state: State<'_, Downloads>) {
    state.cancel.store(true, Ordering::SeqCst);
}

fn emit(on_event: &Channel<DownloadEvent>, event: DownloadEvent) {
    let _ = on_event.send(event);
}

fn log(on_event: &Channel<DownloadEvent>, level: &'static str, message: impl Into<String>) {
    emit(on_event, DownloadEvent::Log { level, message: message.into() });
}

async fn run_download(
    app: &AppHandle,
    state: &Downloads,
    url: &str,
    dir: &str,
    on_event: &Channel<DownloadEvent>,
) -> Result<(), String> {
    let tools = tools::installed(app)?;
    let playlist = list_items(&tools, url).await?;
    let total = playlist.entries.len();
    log::info!("playlist '{}' has {total} items", playlist.title);
    emit(on_event, DownloadEvent::Total { items: total });

    let mut done: HashSet<usize> = HashSet::new();
    let mut failed: usize = 0;
    let home = if dir.is_empty() { "." } else { dir };

    for entry in &playlist.entries {
        if state.cancel.load(Ordering::SeqCst) {
            break;
        }
        let i = entry.index;
        let mut attempts: u32 = 0;

        loop {
            if state.cancel.load(Ordering::SeqCst) {
                break;
            }
            match download_one(state, on_event, entry, home, &tools).await {
                Ok(()) => {
                    log::info!("item {i} done: {}", entry.title);
                    done.insert(i);
                    emit(on_event, DownloadEvent::FileDone { index: i });
                    break;
                }
                Err(Failure::Cancelled) => {
                    log::info!("item {i} cancelled");
                    break;
                }
                Err(Failure::Failed(msg)) => {
                    attempts += 1;
                    log::warn!("item {i} attempt {attempts} failed: {msg}");

                    if is_rate_limited(&msg) && attempts <= MAX_RATE_LIMIT_RETRIES {
                        let wait = RATE_LIMIT_WAIT_SECS * attempts as u64;
                        log::warn!("item {i}: rate limited, waiting {wait}s (retry {attempts})");
                        log(
                            on_event,
                            "warn",
                            format!(
                                "Rate limited on item {i}. Waiting {wait}s before retry ({attempts}/{MAX_RATE_LIMIT_RETRIES})..."
                            ),
                        );
                        // Cancellable wait, reporting the countdown every 10 seconds.
                        let mut slept = 0;
                        while slept < wait && !state.cancel.load(Ordering::SeqCst) {
                            sleep(Duration::from_secs(1)).await;
                            slept += 1;
                            if slept % 10 == 0 {
                                emit(
                                    on_event,
                                    DownloadEvent::Status {
                                        message: format!(
                                            "Waiting {}s (retry {attempts}/{MAX_RATE_LIMIT_RETRIES})",
                                            wait - slept
                                        ),
                                    },
                                );
                            }
                        }
                        continue;
                    }

                    if is_unavailable(&msg) {
                        log(
                            on_event,
                            "warn",
                            format!("Skipping item {i}: video is really unavailable ({})", truncate(&msg, 120)),
                        );
                        failed += 1;
                        emit(on_event, DownloadEvent::ItemFailed { index: i });
                        break;
                    }

                    log(on_event, "error", format!("Item {i} failed: {}", truncate(&msg, 200)));
                    if attempts > MAX_ERROR_RETRIES {
                        log(on_event, "warn", format!("Giving up on item {i}"));
                        failed += 1;
                        emit(on_event, DownloadEvent::ItemFailed { index: i });
                        break;
                    }
                    sleep(Duration::from_secs(ERROR_RETRY_DELAY_SECS)).await;
                }
            }
        }
    }

    let downloaded = done.len();
    let message = if state.cancel.load(Ordering::SeqCst) {
        format!("Cancelled: {downloaded} downloaded, {failed} failed, {} not tried ({total} total)", total - downloaded - failed)
    } else {
        format!("Finished: {downloaded} downloaded, {failed} failed ({total} total)")
    };
    log::info!("{message}");
    emit(on_event, DownloadEvent::Done { message });
    Ok(())
}

/// Downloads one video in its own yt-dlp process so failures stay per-item.
async fn download_one(
    state: &Downloads,
    on_event: &Channel<DownloadEvent>,
    entry: &Entry,
    home: &str,
    tools: &ToolPaths,
) -> Result<(), Failure> {
    let mut args = common_args(&tools.deno);
    args.extend(
        [
            "--no-playlist",
            "--newline",
            "--progress-template",
            "download:PROG|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(info.title)s",
            "-f",
            "bestaudio/best",
            "-P",
            &format!("home:{home}"),
            "-o",
            "%(artist,uploader,channel)s - %(title)s.%(ext)s",
        ]
        .map(String::from),
    );
    args.push(entry.url.clone());
    log::debug!("yt-dlp args: {args:?}");

    let mut cmd = Command::new(&tools.yt_dlp);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_console(&mut cmd);
    let mut child = cmd.spawn().map_err(|e| Failure::Failed(e.to_string()))?;

    let stdout = child.stdout.take().expect("stdout is piped");
    let mut stderr_pipe = child.stderr.take().expect("stderr is piped");
    let stderr_task = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr_pipe.read_to_string(&mut text).await;
        text
    });

    let mut lines = BufReader::new(stdout).lines();
    let status = loop {
        tokio::select! {
            line = lines.next_line() => match line {
                Ok(Some(line)) => {
                    if let Some(progress) = line.trim().strip_prefix("PROG|") {
                        if let Some(event) = parse_progress(progress, entry) {
                            emit(on_event, event);
                        }
                    }
                }
                _ => break child.wait().await.map_err(|e| Failure::Failed(e.to_string()))?,
            },
            _ = sleep(Duration::from_millis(CANCEL_POLL_MILLIS)) => {
                if state.cancel.load(Ordering::SeqCst) {
                    let _ = child.kill().await;
                    break child.wait().await.map_err(|e| Failure::Failed(e.to_string()))?;
                }
            }
        }
    };
    let stderr = stderr_task.await.unwrap_or_default();

    if state.cancel.load(Ordering::SeqCst) {
        return Err(Failure::Cancelled);
    }
    if status.success() {
        return Ok(());
    }

    log::error!(
        "yt-dlp exited with {:?} for {}:\n{}",
        status.code(),
        entry.url,
        stderr.trim_end()
    );
    let message = stderr
        .lines()
        .rev()
        .find(|l| l.contains("ERROR"))
        .unwrap_or(stderr.trim())
        .to_string();
    Err(Failure::Failed(message))
}

/// Parses `downloaded|total|estimate|title` from the progress template.
fn parse_progress(progress: &str, entry: &Entry) -> Option<DownloadEvent> {
    let mut parts = progress.splitn(4, '|');
    let downloaded: f64 = parts.next()?.parse().ok()?;
    let total: Option<f64> = parts.next().and_then(|t| t.parse().ok());
    let estimate: Option<f64> = parts.next().and_then(|t| t.parse().ok());
    let title = parts.next().unwrap_or(&entry.title).to_string();

    let fraction = match total.or(estimate) {
        Some(size) if size > 0.0 => (downloaded / size).min(1.0),
        _ => 0.0,
    };
    Some(DownloadEvent::Progress { index: entry.index, title, fraction })
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

/// Debug builds write `logs/delikan.log` at the project root (Release builds log nothing).
#[cfg(debug_assertions)]
fn log_plugin() -> tauri::plugin::TauriPlugin<tauri::Wry> {
    use tauri_plugin_log::{Target, TargetKind};

    tauri_plugin_log::Builder::new()
        .level(log::LevelFilter::Debug)
        .clear_targets()
        .targets([
            Target::new(TargetKind::Stdout),
            Target::new(TargetKind::Folder {
                path: PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../logs")),
                file_name: Some("delikan".into()),
            }),
        ])
        .max_file_size(5_000_000)
        .build()
}

fn main() {
    let builder = tauri::Builder::default().plugin(tauri_plugin_dialog::init());

    #[cfg(debug_assertions)]
    let builder = builder.plugin(log_plugin());

    builder
        .setup(|_app| {
            log::info!("Delikan {} started", env!("CARGO_PKG_VERSION"));
            Ok(())
        })
        .manage(Downloads::default())
        .invoke_handler(tauri::generate_handler![
            tools_status,
            install_tools,
            fetch_playlist,
            start_download,
            cancel_download
        ])
        .run(tauri::generate_context!())
        .expect("error while running Delikan");
}
