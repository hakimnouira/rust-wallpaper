# Installing RustWallpaper

> **Online Guide:** [hakimnouira.github.io/rust-wallpaper/install.html](https://hakimnouira.github.io/rust-wallpaper/install.html)

## Requirements

- **OS:** Windows 10 or Windows 11 (64-bit)
- **Graphics:** GPU with DXVA2 / D3D11VA hardware video decoding and OpenGL support

---

## Quick Install (Recommended)

1. Download **`RustWallpaper-0.1.0-Setup.exe`** from [GitHub Releases](https://github.com/hakimnouira/rust-wallpaper/releases/latest).
2. Double-click **`RustWallpaper-0.1.0-Setup.exe`**, choose your install folder (defaults to `%LOCALAPPDATA%\Programs\RustWallpaper`), and finish the wizard.
3. Launch **Rust Wallpaper** from the Start Menu or Desktop shortcut.

### Portable Mode

Download `RustWallpaper-0.1.0-windows-x64.zip` from [GitHub Releases](https://github.com/hakimnouira/rust-wallpaper/releases/latest), extract it anywhere, and run **`RustWallpaper.exe`** directly without modifying Start Menu shortcuts or system-wide settings.

---

## Browser Companion Extension (Chrome, Edge, Brave)

1. Open **RustWallpaper → Settings → Browser Companion Integration**.
2. Enable **Integrate with web browsers**, or click **Auto-Configure Shortcuts** / **Launch Browser with Extension**.
3. Alternatively, open `chrome://extensions` (or `edge://extensions`), enable **Developer mode**, and click **Load unpacked** on `%LOCALAPPDATA%\RustWallpaper\extension`.

---

## Build from Source

Requires Rust 1.78+, Visual Studio C++ Build Tools (or MinGW-w64), and `mpv.exe` / `ffmpeg.exe` in `%USERPROFILE%\.mpv`.

```powershell
git clone https://github.com/hakimnouira/rust-wallpaper.git
cd rust-wallpaper
powershell -ExecutionPolicy Bypass -File .\scripts\build-production.ps1
```

All compiled binaries, bundled MPV/FFmpeg, signed extension (`chrome.crx`), and `RustWallpaper-0.1.0-windows-x64.zip` are written to `dist\`.

---

## Shortcuts & Data Paths

| Item | Value |
|---|---|
| Next Wallpaper | `Alt + W` |
| Mute Audio | `Alt + M` |
| Hide to Tray / Refresh | `Esc` / `F5` |
| Database, Thumbnails & Logs | `%LOCALAPPDATA%\RustWallpaper\` |
