"""Delikan - YouTube playlist downloader with GUI (yt-dlp bundled as dependency)."""
import os
import sys
import threading
import queue
import time
import traceback
from pathlib import Path

import customtkinter as ctk
import yt_dlp

# VS Code/other node tooling injects NODE_OPTIONS which breaks the bundled deno
os.environ.pop("NODE_OPTIONS", None)
os.environ.pop("NODE_PATH", None)


def resource_path(rel: str) -> str:
    """Path that works both from source and inside a PyInstaller bundle."""
    base = getattr(sys, "_MEIPASS", Path(__file__).parent)
    return str(Path(base) / rel)


def default_download_dir() -> str:
    return str(Path.home() / "Downloads" / "delikan")


def ffmpeg_dir() -> str | None:
    p = resource_path("bin")
    return p if os.path.isdir(p) else None


def deno_path() -> str | None:
    p = resource_path("bin/deno")
    return p if os.path.isfile(p) else None


class QueueHandler:
    """Forwards yt-dlp progress messages to the GUI thread via a queue."""

    def __init__(self, q: queue.Queue):
        self.q = q

    def debug(self, msg):
        pass

    def warning(self, msg):
        self.q.put(("warn", str(msg)))

    def error(self, msg):
        self.q.put(("error", str(msg)))


class App(ctk.CTk):
    def __init__(self):
        super().__init__()
        self.title("Delikan - Playlist Downloader")
        self.geometry("640x480")
        self.minsize(560, 420)
        ctk.set_appearance_mode("system")
        ctk.set_default_color_theme("blue")

        self.q: queue.Queue = queue.Queue()
        self.worker: threading.Thread | None = None
        self.cancel_flag = False
        self.total_items = 0
        self.done_indices: set = set()

        self.grid_columnconfigure(0, weight=1)
        self.grid_rowconfigure(4, weight=1)

        # --- URL row ---
        top = ctk.CTkFrame(self, fg_color="transparent")
        top.grid(row=0, column=0, padx=16, pady=(16, 8), sticky="ew")
        top.grid_columnconfigure(0, weight=1)
        self.url_entry = ctk.CTkEntry(top, placeholder_text="Playlist URL")
        self.url_entry.grid(row=0, column=0, sticky="ew")
        self.fetch_btn = ctk.CTkButton(top, text="Fetch", width=90, command=self.fetch)
        self.fetch_btn.grid(row=0, column=1, padx=(8, 0))

        # --- Options row ---
        opts = ctk.CTkFrame(self, fg_color="transparent")
        opts.grid(row=1, column=0, padx=16, pady=4, sticky="ew")
        opts.grid_columnconfigure(1, weight=1)
        ctk.CTkLabel(opts, text="Save to:").grid(row=0, column=0, padx=(0, 6))
        self.dir_entry = ctk.CTkEntry(opts)
        self.dir_entry.insert(0, default_download_dir())
        self.dir_entry.grid(row=0, column=1, sticky="ew")
        self.browse_btn = ctk.CTkButton(opts, text="Browse", width=80, command=self.browse)
        self.browse_btn.grid(row=0, column=2, padx=(8, 0))

        fmt = ctk.CTkFrame(self, fg_color="transparent")
        fmt.grid(row=2, column=0, padx=16, pady=4, sticky="ew")
        self.audio_only = ctk.CTkSwitch(fmt, text="Audio only (original format)")
        self.audio_only.grid(row=0, column=0, padx=0)
        self.quality_box = ctk.CTkComboBox(
            fmt, values=["best", "1080p", "720p", "480p", "360p"], width=110
        )
        self.quality_box.set("best")
        self.quality_box.grid(row=0, column=1, padx=(16, 0))

        # --- Current file progress (top) ---
        cur = ctk.CTkFrame(self, fg_color="transparent")
        cur.grid(row=3, column=0, padx=16, pady=4, sticky="ew")
        cur.grid_columnconfigure(0, weight=1)
        self.current_title = ctk.CTkLabel(cur, text="Current:", anchor="w")
        self.current_title.grid(row=0, column=0, columnspan=2, sticky="ew")
        self.current_progress = ctk.CTkProgressBar(cur)
        self.current_progress.set(0)
        self.current_progress.grid(row=1, column=0, sticky="ew", pady=(2, 0))
        self.current_pct = ctk.CTkLabel(cur, text="0%", width=44, anchor="e")
        self.current_pct.grid(row=1, column=1, padx=(6, 0), pady=(2, 0))

        # --- Log box ---
        self.log = ctk.CTkTextbox(self, wrap="none")
        self.log.grid(row=4, column=0, padx=16, pady=8, sticky="nsew")
        self.log.configure(state="disabled")

        # --- Action row (total progress + buttons) ---
        bottom = ctk.CTkFrame(self, fg_color="transparent")
        bottom.grid(row=5, column=0, padx=16, pady=(0, 16), sticky="ew")
        bottom.grid_columnconfigure(0, weight=1)
        self.total_progress = ctk.CTkProgressBar(bottom)
        self.total_progress.set(0)
        self.total_progress.grid(row=0, column=0, sticky="ew", padx=(0, 8))
        self.total_pct = ctk.CTkLabel(bottom, text="0%", width=44, anchor="e")
        self.total_pct.grid(row=0, column=1, padx=(0, 8))
        self.download_btn = ctk.CTkButton(bottom, text="Download", width=110, command=self.download)
        self.download_btn.grid(row=0, column=2)
        self.cancel_btn = ctk.CTkButton(
            bottom, text="Cancel", width=80, fg_color="#8a3a3a", hover_color="#a84a4a",
            command=self.cancel, state="disabled"
        )
        self.cancel_btn.grid(row=0, column=3, padx=(8, 0))

        self.status = ctk.CTkLabel(self, text="Ready", anchor="w")
        self.status.grid(row=6, column=0, padx=16, pady=(0, 8), sticky="ew")

        self.after(100, self.poll_queue)

    # ---------- helpers ----------
    def log_line(self, text: str):
        self.log.configure(state="normal")
        self.log.insert("end", text + "\n")
        self.log.see("end")
        self.log.configure(state="disabled")

    def set_busy(self, busy: bool):
        self.download_btn.configure(state="disabled" if busy else "normal")
        self.fetch_btn.configure(state="disabled" if busy else "normal")
        self.cancel_btn.configure(state="normal" if busy else "disabled")

    # ---------- actions ----------
    def browse(self):
        d = ctk.filedialog.askdirectory(initialdir=self.dir_entry.get())
        if d:
            self.dir_entry.delete(0, "end")
            self.dir_entry.insert(0, d)

    def base_opts(self, flat=False):
        ff = ffmpeg_dir()
        opts = {
            "logger": QueueHandler(self.q),
            "noprogress": True,
            "ignoreerrors": True,
            "retries": 5,
            "sleep_interval_requests": 1,
            "sleep_interval": 3,
            "max_sleep_interval": 8,
            "http_chunk_size": 10 * 1024 * 1024,
            "concurrent_fragment_downloads": 1,
            "quiet": True,
            "outtmpl": {"default": "%(artist,uploader,channel)s - %(title)s.%(ext)s"},
            "paths": {"home": self.dir_entry.get().strip() or default_download_dir()},
        }
        if ff:
            opts["ffmpeg_location"] = ff
        deno = deno_path()
        if deno:
            opts["js_runtimes"] = {"deno": {"path": deno}}
            opts["remote_components"] = ["ejs:github"]
        if flat:
            opts["extract_flat"] = "in_playlist"
        return opts

    def fetch(self):
        url = self.url_entry.get().strip()
        if not url:
            self.status.configure(text="Enter a playlist URL first.")
            return
        self.set_busy(True)
        self.status.configure(text="Fetching playlist info...")

        def work():
            try:
                with yt_dlp.YoutubeDL(self.base_opts(flat=True)) as ydl:
                    info = ydl.extract_info(url, download=False)
                    entries = info.get("entries") or []
                    title = info.get("title", "unknown")
                    self.q.put(("info", f"Playlist: {title} ({len(entries)} items)"))
                    for i, e in enumerate(entries, 1):
                        if e:
                            self.q.put(("info", f"  {i:3d}. {e.get('title', '?')}"))
                    self.q.put(("done", f"Fetched {len(entries)} items"))
            except Exception as e:
                self.q.put(("error", str(e)))
            finally:
                self.q.put(("idle", None))

        threading.Thread(target=work, daemon=True).start()

    def download(self):
        url = self.url_entry.get().strip()
        if not url:
            self.status.configure(text="Enter a playlist URL first.")
            return
        Path(self.dir_entry.get().strip() or default_download_dir()).mkdir(parents=True, exist_ok=True)
        self.set_busy(True)
        self.cancel_flag = False
        self.current_progress.set(0)
        self.current_pct.configure(text="0%")
        self.current_title.configure(text="Current:", anchor="w")
        self.total_progress.set(0)
        self.total_pct.configure(text="0%")
        self.done_indices = set()
        self.status.configure(text="Downloading...")
        audio = self.audio_only.get()
        quality = self.quality_box.get()

        def work():
            opts = self.base_opts()
            opts["progress_hooks"] = [self.progress_hook]
            opts["ignoreerrors"] = False  # we handle per-video errors ourselves
            opts.pop("extract_flat", None)
            if audio:
                opts.update({
                    "format": "bestaudio/best",
                })
            else:
                height = None if quality == "best" else int(quality.rstrip("p"))
                fmt = "bestvideo*+bestaudio/best" if height is None else \
                    f"bestvideo[height<={height}]+bestaudio/best[height<={height}]/best"
                opts.update({
                    "format": fmt,
                    "merge_output_format": "mp4",
                })
            RATE_LIMIT_PAT = ("403", "forbidden", "rate limit", "sign in to confirm",
                              "too many requests", "throttl")
            UNAVAIL_PAT = ("unavailable", "private video", "removed by", "does not exist",
                           "age-restricted", "geo-restricted", "not available")
            RETRY_WAIT_SECS = 180

            def is_rate_limited(msg):
                m = msg.lower()
                return any(p in m for p in RATE_LIMIT_PAT)

            def is_unavailable(msg):
                m = msg.lower()
                return any(p in m for p in UNAVAIL_PAT)

            try:
                # determine playlist size and entries for the total progress bar
                with yt_dlp.YoutubeDL(self.base_opts(flat=True)) as ydl:
                    info = ydl.extract_info(url, download=False)
                    if info and info.get("entries") is not None:
                        entries = [e for e in info["entries"] if e]
                    else:
                        entries = [{"url": url, "id": info.get("id")} if info else None]
                        entries = [e for e in entries if e]
                self.total_items = len(entries)
                self.q.put(("total_items", len(entries)))

                # download each item individually so failures are handled per-video
                ydl = yt_dlp.YoutubeDL(opts)
                for i, e in enumerate(entries, 1):
                    if self.cancel_flag:
                        break
                    vurl = e.get("url") or e.get("webpage_url")
                    if not vurl and e.get("id"):
                        vurl = f"https://www.youtube.com/watch?v={e['id']}"
                    if not vurl:
                        continue
                    self.current_idx = i
                    attempts = 0
                    while True:
                        if self.cancel_flag:
                            break
                        try:
                            ydl.download([vurl])
                            self.q.put(("file_done", i))
                            break
                        except yt_dlp.utils.DownloadCancelled:
                            break
                        except yt_dlp.utils.DownloadError as err:
                            msg = str(err)
                            attempts += 1
                            if is_rate_limited(msg) and attempts <= 3:
                                wait = 180 * attempts
                                self.q.put(("warn", f"Rate limited on item {i}. "
                                            f"Waiting {wait}s before retry "
                                            f"({attempts}/3)..."))
                                # cancellable wait
                                slept = 0
                                while slept < wait and not self.cancel_flag:
                                    time.sleep(1)
                                    slept += 1
                                    if slept % 10 == 0:
                                        self.q.put(("status",
                                                    f"Waiting {wait - slept}s (retry {attempts}/3)"))
                                continue
                            if is_unavailable(msg):
                                self.q.put(("warn", f"Skipping item {i}: video is "
                                            f"really unavailable ({msg[:120]})"))
                                break
                            self.q.put(("error", f"Item {i} failed: {msg[:200]}"))
                            if attempts > 3:
                                self.q.put(("warn", f"Giving up on item {i}"))
                                break
                            time.sleep(5)
                if self.cancel_flag:
                    self.q.put(("done", "Cancelled"))
                else:
                    self.q.put(("done", f"Download finished ({len(self.done_indices)}/{len(entries)})"))
            except Exception as e:
                if self.cancel_flag:
                    self.q.put(("done", "Cancelled"))
                else:
                    self.q.put(("error", str(e)))
            finally:
                self.q.put(("idle", None))

        self.worker = threading.Thread(target=work, daemon=True)
        self.worker.start()

    def cancel(self):
        self.cancel_flag = True
        self.q.put(("warn", "Cancelling... (finishes current file)"))

    def progress_hook(self, d):
        if self.cancel_flag:
            raise yt_dlp.utils.DownloadCancelled()
        info = d.get("info_dict") or {}
        idx = info.get("playlist_index")
        if d["status"] == "downloading":
            total = d.get("total_bytes") or d.get("total_bytes_estimate")
            got = d.get("downloaded_bytes", 0)
            if total:
                self.q.put(("progress", got / total))
            self.q.put(("title", info.get("title") or d.get("filename", "downloading...")))
        elif d["status"] == "finished":
            if idx is not None:
                self.q.put(("file_done", idx))
            self.q.put(("status", f"Finished: {d.get('filename', '')}"))

    # ---------- queue pump ----------
    def poll_queue(self):
        try:
            while True:
                kind, msg = self.q.get_nowait()
                if kind == "progress":
                    self.current_progress.set(msg)
                    self.current_pct.configure(text=f"{msg * 100:.0f}%")
                elif kind == "title":
                    t = str(msg)
                    self.current_title.configure(text=("Current: " + t)[:80], anchor="w")
                elif kind == "file_done":
                    self.done_indices.add(msg)
                    if self.total_items:
                        frac = min(1.0, len(self.done_indices) / self.total_items)
                        self.total_progress.set(frac)
                        self.total_pct.configure(text=f"{frac * 100:.0f}%")
                        self.status.configure(
                            text=f"Completed {len(self.done_indices)}/{self.total_items}")
                elif kind == "total_items":
                    self.status.configure(text=f"Playlist: {msg} items")
                elif kind == "status":
                    self.status.configure(text=str(msg)[-120:])
                elif kind == "info":
                    self.log_line(str(msg))
                elif kind == "warn":
                    self.log_line("WARN: " + str(msg))
                elif kind == "error":
                    self.log_line("ERROR: " + str(msg))
                elif kind == "done":
                    self.log_line(str(msg))
                    self.status.configure(text=str(msg))
                elif kind == "idle":
                    self.set_busy(False)
        except queue.Empty:
            pass
        self.after(100, self.poll_queue)


def main():
    app = App()
    app.mainloop()


if __name__ == "__main__":
    main()
