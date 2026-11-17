# RustWallpaper Companion - Browser Extension

The **RustWallpaper Companion** allows you to send animated live wallpapers, HTML5 web videos (mp4, webm), GIFs, and images from your web browser directly to your **RustWallpaper** desktop engine with a single click.

---

## 🚀 Features

- **Floating Hover Button**: Seamlessly hovers over any `<video>` element on supported sites (Reddit, YouTube, Mixkit, Pixabay, Pexels, wallpaper sites, etc.) with a `[ 🖼 Set as Wallpaper ]` badge.
- **Context Menu Integration**: Right-click any video, image, or link and choose:
  - `RustWallpaper: Set as Live Wallpaper` -> `Apply to Desktop Wallpaper`
  - `RustWallpaper: Set as Live Wallpaper` -> `Apply to Primary Monitor Only`
- **Dashboard Popup**:
  - Live engine status monitoring (`🟢 Engine Ready` / `🔴 Engine Offline`).
  - Dynamic monitor selection (Synchronized All Monitors, Display 1, Display 2, etc.).
  - Direct URL input box for instant downloads.
  - Quick Playback Controls: Pause and Resume live wallpapers across monitors.
  - Recent wallpaper history for one-click re-applying.

---

## 📦 How to Install (Google Chrome, Microsoft Edge, Brave, Opera)

1. Open your browser and navigate to the extensions management page:
   - **Google Chrome**: `chrome://extensions`
   - **Microsoft Edge**: `edge://extensions`
   - **Brave**: `brave://extensions`
2. Enable **Developer mode** (toggle switch in top-right or top-left corner).
3. Click the **"Load unpacked"** button.
4. Select the directory:
   ```
   <repo-root>\extensions\chrome
   ```
   (or from the distribution folder `dist\extension`)
5. The **RustWallpaper Companion** extension will now appear in your browser toolbar!

---

## 🛠 How It Works

1. **RustWallpaper Desktop Engine** runs a secure loopback HTTP server bound strictly to `127.0.0.1:24242`.
2. When you trigger "Set as Wallpaper" from the browser:
   - The extension makes an authenticated localhost POST request to `http://127.0.0.1:24242/api/wallpaper`.
   - The engine asynchronously downloads the media to your local wallpaper cache directory (`%APPDATA%\wallpaper-engine-rs\wallpapers\online`).
   - The engine imports the media into your live wallpaper library and instantly attaches it as the background for the targeted monitor.
3. Everything happens locally with zero external telemetry or cloud dependencies.

---

## 🧪 Testing

1. Start `RustWallpaper.exe` (or `wallpaper-daemon.exe`).
2. Open any web video in your browser (e.g. any live wallpaper on Reddit or video sharing site).
3. Hover over the video and click **"Set as Wallpaper"**, or right-click the video and select **RustWallpaper: Set as Live Wallpaper**.
4. Check your Windows desktop to see the new live wallpaper running smoothly.
