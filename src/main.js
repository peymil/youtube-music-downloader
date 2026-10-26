const { invoke, Channel } = window.__TAURI__.core;
const { open } = window.__TAURI__.dialog;
const { downloadDir, join } = window.__TAURI__.path;

const $ = (id) => document.getElementById(id);
const ui = {
  url: $("url"),
  dir: $("dir"),
  fetch: $("fetch"),
  browse: $("browse"),
  tools: $("tools"),
  download: $("download"),
  cancel: $("cancel"),
  log: $("log"),
  status: $("status"),
  currentTitle: $("current-title"),
  currentProgress: $("current-progress"),
  currentPct: $("current-pct"),
  totalProgress: $("total-progress"),
  totalPct: $("total-pct"),
  errorCount: $("error-count"),
};

let totalItems = 0;
let doneCount = 0;
let errorCount = 0;

function log(level, message) {
  const prefix = level === "info" ? "" : `${level.toUpperCase()}: `;
  ui.log.textContent += prefix + message + "\n";
  ui.log.scrollTop = ui.log.scrollHeight;
}

function setStatus(text) {
  ui.status.textContent = text;
}

function setBusy(busy) {
  ui.download.disabled = busy;
  ui.fetch.disabled = busy;
  ui.cancel.disabled = !busy;
}

async function installTools() {
  ui.tools.disabled = true;
  ui.download.disabled = true;
  ui.fetch.disabled = true;
  setStatus("Updating yt-dlp and deno...");

  const onEvent = new Channel();
  onEvent.onmessage = handleEvent;
  try {
    await invoke("install_tools", { onEvent });
    const status = await invoke("tools_status");
    log("info", `Ready: yt-dlp ${status.ytDlp}, ${status.deno}`);
    setStatus("Tools ready");
  } catch (err) {
    log("error", String(err));
    setStatus("Tool install failed");
  } finally {
    ui.tools.disabled = false;
    ui.download.disabled = false;
    ui.fetch.disabled = false;
  }
}

// Installs the tools on first launch, when they are missing.
async function ensureTools() {
  const status = await invoke("tools_status");
  if (status.ytDlp && status.deno) {
    log("info", `Ready: yt-dlp ${status.ytDlp}, ${status.deno}`);
  } else {
    await installTools();
  }
}

function setCurrent(title, fraction) {
  ui.currentTitle.textContent = "Current: " + title;
  ui.currentProgress.value = fraction;
  ui.currentPct.textContent = Math.round(fraction * 100) + "%";
}

// Skipped (failed) items count as processed, so the bar reaches the total.
function updateTotal() {
  const processed = doneCount + errorCount;
  const fraction = totalItems ? Math.min(1, processed / totalItems) : 0;
  ui.totalProgress.value = fraction;
  ui.totalPct.textContent = `${processed}/${totalItems}`;
  ui.errorCount.textContent = `Errors: ${errorCount}`;
  ui.errorCount.classList.toggle("has-errors", errorCount > 0);
  if (totalItems) {
    setStatus(`Processed ${processed}/${totalItems}`);
  }
}

function handleEvent(ev) {
  switch (ev.event) {
    case "total":
      totalItems = ev.items;
      doneCount = 0;
      errorCount = 0;
      updateTotal();
      setStatus(`Playlist: ${totalItems} items`);
      break;
    case "progress":
      setCurrent(ev.title, ev.fraction);
      break;
    case "fileDone":
      doneCount += 1;
      updateTotal();
      break;
    case "itemFailed":
      errorCount += 1;
      updateTotal();
      break;
    case "log":
      log(ev.level, ev.message);
      break;
    case "status":
      setStatus(ev.message);
      break;
    case "done":
      log("info", ev.message);
      setStatus(ev.message);
      break;
  }
}

async function fetchPlaylist() {
  const url = ui.url.value.trim();
  if (!url) {
    setStatus("Enter a playlist URL first.");
    return;
  }
  setBusy(true);
  setStatus("Fetching playlist info...");
  try {
    const playlist = await invoke("fetch_playlist", { url });
    log("info", `Playlist: ${playlist.title} (${playlist.entries.length} items)`);
    for (const entry of playlist.entries) {
      log("info", `${String(entry.index).padStart(3)}. ${entry.title}`);
    }
    setStatus(`Fetched ${playlist.entries.length} items`);
  } catch (err) {
    log("error", String(err));
    setStatus("Fetch failed");
  } finally {
    setBusy(false);
  }
}

async function download() {
  const url = ui.url.value.trim();
  if (!url) {
    setStatus("Enter a playlist URL first.");
    return;
  }
  setBusy(true);
  totalItems = 0;
  doneCount = 0;
  errorCount = 0;
  setCurrent("", 0);
  updateTotal();
  setStatus("Downloading...");

  const onEvent = new Channel();
  onEvent.onmessage = handleEvent;

  try {
    await invoke("start_download", {
      url,
      dir: ui.dir.value.trim(),
      onEvent,
    });
  } catch (err) {
    log("error", String(err));
    setStatus(String(err));
  } finally {
    setBusy(false);
  }
}

async function browse() {
  const selected = await open({ directory: true, defaultPath: ui.dir.value || undefined });
  if (selected) {
    ui.dir.value = selected;
  }
}

async function init() {
  ui.dir.value = await join(await downloadDir(), "delikan");

  ui.fetch.addEventListener("click", fetchPlaylist);
  ui.download.addEventListener("click", download);
  ui.browse.addEventListener("click", browse);
  ui.tools.addEventListener("click", installTools);
  ui.cancel.addEventListener("click", () => {
    invoke("cancel_download");
    log("warn", "Cancelling...");
    setStatus("Cancelling...");
  });
  ui.url.addEventListener("keydown", (e) => {
    if (e.key === "Enter") fetchPlaylist();
  });

  await ensureTools();
}

init();
