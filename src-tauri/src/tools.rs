//! Installs and locates yt-dlp and deno. Both are downloaded from their official
//! GitHub releases into the app's local data folder, and yt-dlp is checked
//! against its published SHA2-256SUMS before anything is written to disk.

use std::{
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Manager};
use tokio::process::Command;

const YT_DLP_BASE: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download/";
const DENO_BASE: &str = "https://github.com/denoland/deno/releases/latest/download/";

pub struct ToolPaths {
    pub yt_dlp: PathBuf,
    pub deno: PathBuf,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolsStatus {
    pub yt_dlp: Option<String>,
    pub deno: Option<String>,
}

pub fn tools_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_local_data_dir()
        .map(|dir| dir.join("bin"))
        .map_err(|e| e.to_string())
}

/// Paths to the installed tools, or an error asking the user to install them.
pub fn installed(app: &AppHandle) -> Result<ToolPaths, String> {
    let dir = tools_dir(app)?;
    let paths = ToolPaths {
        yt_dlp: yt_dlp_exe(&dir),
        deno: deno_exe(&dir),
    };
    if !paths.yt_dlp.is_file() || !paths.deno.is_file() {
        return Err("yt-dlp and deno are not installed yet. Click 'Update tools'.".into());
    }
    Ok(paths)
}

/// Version lines for the installed tools, or None where a tool is missing.
pub async fn status(app: &AppHandle) -> ToolsStatus {
    let Ok(dir) = tools_dir(app) else {
        return ToolsStatus { yt_dlp: None, deno: None };
    };
    ToolsStatus {
        yt_dlp: version(&yt_dlp_exe(&dir), "--version").await,
        deno: version(&deno_exe(&dir), "--version").await,
    }
}

async fn version(path: &Path, flag: &str) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    let output = Command::new(path).arg(flag).output().await.ok()?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(|line| line.trim().to_string())
}

fn yt_dlp_exe(dir: &Path) -> PathBuf {
    dir.join(format!("yt-dlp{}", std::env::consts::EXE_SUFFIX))
}

fn deno_exe(dir: &Path) -> PathBuf {
    dir.join(format!("deno{}", std::env::consts::EXE_SUFFIX))
}

/// Release asset names for this platform.
struct Assets {
    yt_dlp: &'static str,
    deno_triple: &'static str,
}

fn assets() -> Result<Assets, String> {
    let (yt_dlp, deno_triple) = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => ("yt-dlp_linux", "x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => ("yt-dlp_linux_aarch64", "aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => ("yt-dlp_macos", "aarch64-apple-darwin"),
        ("macos", "x86_64") => ("yt-dlp_macos", "x86_64-apple-darwin"),
        ("windows", "x86_64") => ("yt-dlp.exe", "x86_64-pc-windows-msvc"),
        (os, arch) => return Err(format!("no yt-dlp/deno builds for {os} {arch}")),
    };
    Ok(Assets { yt_dlp, deno_triple })
}

/// Downloads the latest yt-dlp and deno into `dir`. `report` receives progress messages.
pub async fn install(dir: &Path, report: impl Fn(&str)) -> Result<(), String> {
    let assets = assets()?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("delikan/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;

    report("Downloading yt-dlp...");
    let sums = fetch_text(&client, &format!("{YT_DLP_BASE}SHA2-256SUMS")).await?;
    let expected = expected_hash(&sums, assets.yt_dlp)
        .ok_or_else(|| format!("no checksum listed for {}", assets.yt_dlp))?;
    let bytes = fetch(&client, &format!("{YT_DLP_BASE}{}", assets.yt_dlp)).await?;
    verify(&bytes, &expected, assets.yt_dlp)?;
    write_executable(&yt_dlp_exe(dir), &bytes)?;
    report("yt-dlp installed");

    report("Downloading deno...");
    let zip_name = format!("deno-{}.zip", assets.deno_triple);
    let sum_text = fetch_text(&client, &format!("{DENO_BASE}{zip_name}.sha256sum")).await?;
    let expected = sum_text
        .split_whitespace()
        .next()
        .ok_or("empty deno checksum file")?
        .to_lowercase();
    let zip_bytes = fetch(&client, &format!("{DENO_BASE}{zip_name}")).await?;
    verify(&zip_bytes, &expected, &zip_name)?;
    let deno_bytes = extract_deno(&zip_bytes)?;
    write_executable(&deno_exe(dir), &deno_bytes)?;
    report("deno installed");

    Ok(())
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    log::info!("GET {url}");
    let response = client
        .get(url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("download failed: {e}"))?;
    response
        .bytes()
        .await
        .map(|b| b.to_vec())
        .map_err(|e| format!("download failed: {e}"))
}

async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String, String> {
    fetch(client, url)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Finds `file`'s hash in a `<hash>  <name>` listing such as SHA2-256SUMS.
fn expected_hash(listing: &str, file: &str) -> Option<String> {
    listing.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (name == file).then(|| hash.to_lowercase())
    })
}

fn verify(bytes: &[u8], expected: &str, name: &str) -> Result<(), String> {
    let actual: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    if actual == expected {
        log::info!("{name}: checksum ok");
        Ok(())
    } else {
        Err(format!("checksum mismatch for {name}; download discarded"))
    }
}

fn extract_deno(zip_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut archive = zip::ZipArchive::new(Cursor::new(zip_bytes)).map_err(|e| e.to_string())?;
    let name = format!("deno{}", std::env::consts::EXE_SUFFIX);
    let mut file = archive
        .by_name(&name)
        .map_err(|e| format!("{name} not found in deno archive: {e}"))?;
    let mut out = Vec::new();
    file.read_to_end(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Writes to a temp file and renames it into place, so a failed download never
/// leaves a half-written tool behind.
fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("download");
    std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}
