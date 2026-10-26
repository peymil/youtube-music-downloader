# Delikan - YouTube Playlist Downloader

Desktop playlist downloader built with [Tauri 2](https://tauri.app). The UI is plain HTML/CSS/JS; the backend is Rust, which runs `yt-dlp` and `deno` as bundled sidecars.

## Layout
- `src/` - frontend (`index.html`, `main.js`, `styles.css`)
- `src-tauri/src/main.rs` - backend: playlist listing, download loop, retries, cancel
- `src-tauri/binaries/` - sidecar binaries (not in git, see setup)
- `src-tauri/tauri.conf.json` - app config and sidecar list
- `main.py` - the previous Python/CustomTkinter version (no longer built)

## Setup
1. Install the [Tauri prerequisites](https://tauri.app/start/prerequisites/) for your OS (on Linux: WebKitGTK 4.1).
2. Install Node.js and Rust, then:
   ```
   npm install
   ```
3. Fetch the sidecar binaries into `src-tauri/binaries/`, named with the target triple:
   - `yt-dlp-x86_64-unknown-linux-gnu` from https://github.com/yt-dlp/yt-dlp/releases (`yt-dlp_linux`)
   - `yt-dlp-x86_64-pc-windows-msvc.exe` from the same releases (`yt-dlp.exe`)
   - `deno-x86_64-unknown-linux-gnu` and `deno-x86_64-pc-windows-msvc.exe` from https://deno.land (or copy the ones in `bin/`)
   - Make the Linux binaries executable: `chmod +x src-tauri/binaries/*-unknown-linux-gnu`

## Run
```
npm run tauri dev
```

## Build
```
npm run tauri build
```
Output goes to `src-tauri/target/release/bundle/`.

## Requirements at runtime
- `ffmpeg` on your `PATH` is needed for merging video and audio (same as before when `bin/ffmpeg` was missing).

## Features
- Playlist or single-video URL fetch with item list
- Download with per-file and total progress
- Audio-only toggle (original format), quality selector (best/1080p/.../360p)
- Cancel, log console, custom output folder
- Retries on rate limiting (waits 3 / 6 / 9 minutes), skips unavailable videos
- Pauses between downloads (3-8 s) to avoid YouTube throttling
