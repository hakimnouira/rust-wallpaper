/**
 * RustWallpaper Companion - Background Service Worker
 * Handles context menus, desktop notifications, loopback API communication,
 * connection health monitoring, and smart retry logic.
 */

const API_BASE = "http://127.0.0.1:24242";
const API_TIMEOUT_MS = 4000;
const HEALTH_CHECK_INTERVAL_MS = 15000;
const MAX_RETRY_ATTEMPTS = 2;

// --- Connection Health State ---
let daemonOnline = false;
let lastHealthCheck = 0;
let pendingHealthCheck = false;
let authToken = null;

async function getAuthToken() {
  if (authToken) return authToken;
  try {
    const res = await fetch(`${API_BASE}/api/auth/token`);
    if (res.ok) {
      const data = await res.json();
      if (data && data.token) {
        authToken = data.token;
        return authToken;
      }
    }
  } catch (e) {
    // Daemon offline
  }
  return null;
}

// --- IDM-Style Network Media Sniffing ---
// Captures video stream URLs from Pinterest (v1.pinimg.com), direct .mp4/.webm, etc.
const tabMediaStreams = new Map(); // tabId -> Array of { url, isPinterest, timestamp, type }

if (chrome.webRequest && chrome.webRequest.onBeforeRequest) {
  chrome.webRequest.onBeforeRequest.addListener(
    (details) => {
      if (!details.url || details.tabId < 0) return;
      const url = details.url;
      const lower = url.toLowerCase();

      // Filter for video CDN URLs and direct video streams
      const isPinterest = lower.includes("pinimg.com/videos/");
      const isVideoFormat =
        lower.includes(".mp4") || lower.includes(".webm") || lower.includes(".m3u8");
      const isTikTok = lower.includes("tiktokcdn.com") || lower.includes("tiktokv.com");
      const isReddit = lower.includes("v.redd.it/");
      const isInstagram = lower.includes("cdninstagram.com");

      if (isPinterest || isVideoFormat || isTikTok || isReddit || isInstagram) {
        let cleanUrl = url;

        // Convert Pinterest HLS playlist to direct 720p MP4 if possible
        if (cleanUrl.includes("/hls/") && cleanUrl.includes(".m3u8")) {
          cleanUrl = cleanUrl.replace("/hls/", "/mc/720p/").replace(".m3u8", ".mp4");
        }

        // Strip query params for cleaner URLs (some CDNs use signed URLs that expire)
        let streamType = "video";
        if (isPinterest) streamType = "pinterest";
        else if (isTikTok) streamType = "tiktok";
        else if (isReddit) streamType = "reddit";
        else if (isInstagram) streamType = "instagram";

        let streams = tabMediaStreams.get(details.tabId);
        if (!streams) {
          streams = [];
          tabMediaStreams.set(details.tabId, streams);
        }

        // Avoid duplicates
        if (!streams.some((item) => item.url === cleanUrl)) {
          streams.unshift({ url: cleanUrl, type: streamType, timestamp: Date.now() });
          if (streams.length > 25) streams.pop();

          // Notify the active content script in this tab
          chrome.tabs
            .sendMessage(details.tabId, {
              action: "MEDIA_STREAM_DETECTED",
              mediaUrl: cleanUrl,
              type: streamType,
            })
            .catch(() => {});
        }
      }
    },
    {
      urls: [
        "*://*.pinimg.com/videos/*",
        "*://*.tiktokcdn.com/*",
        "*://*.tiktokv.com/*",
        "*://*.cdninstagram.com/*",
        "*://v.redd.it/*",
        "*://*/*.mp4*",
        "*://*/*.webm*",
        "*://*/*.m3u8*",
      ],
    }
  );

  chrome.tabs.onRemoved.addListener((tabId) => {
    tabMediaStreams.delete(tabId);
  });
}

// Setup context menus
chrome.runtime.onInstalled.addListener(() => {
  setupContextMenus();
  // Check daemon health on install
  checkDaemonHealth();
});

chrome.runtime.onStartup.addListener(() => {
  setupContextMenus();
  checkDaemonHealth();
});

function setupContextMenus() {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: "rust-wallpaper-root",
      title: "RustWallpaper: Set as Live Wallpaper",
      contexts: ["video", "image", "link"]
    });

    chrome.contextMenus.create({
      id: "set-media-wallpaper",
      parentId: "rust-wallpaper-root",
      title: "Apply to Desktop Wallpaper",
      contexts: ["video", "image", "link"]
    });

    chrome.contextMenus.create({
      id: "set-media-wallpaper-primary",
      parentId: "rust-wallpaper-root",
      title: "Apply to Primary Monitor Only",
      contexts: ["video", "image", "link"]
    });
  });
}

// --- Connection Health Monitoring ---
async function checkDaemonHealth() {
  if (pendingHealthCheck) return daemonOnline;
  pendingHealthCheck = true;

  try {
    const controller = new AbortController();
    const timeoutId = setTimeout(() => controller.abort(), API_TIMEOUT_MS);

    const resp = await fetch(`${API_BASE}/api/status`, {
      signal: controller.signal,
    });
    clearTimeout(timeoutId);

    daemonOnline = resp.ok;
    lastHealthCheck = Date.now();
  } catch (err) {
    daemonOnline = false;
  } finally {
    pendingHealthCheck = false;
  }

  // Broadcast health status to all content scripts and popup
  chrome.runtime.sendMessage({
    action: "HEALTH_UPDATE",
    online: daemonOnline,
  }).catch(() => {});

  return daemonOnline;
}

// Periodic health check
setInterval(checkDaemonHealth, HEALTH_CHECK_INTERVAL_MS);

// Handle context menu clicks
chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  let mediaUrl =
    (info.linkUrl && /\/pin\//i.test(info.linkUrl) ? info.linkUrl : null) ||
    info.srcUrl ||
    info.linkUrl;
  if (!mediaUrl && info.pageUrl) {
    mediaUrl = info.pageUrl;
  }

  if (!mediaUrl) {
    notifyUser("No Media Detected", "Could not determine a direct media URL from this page element.");
    return;
  }

  let monitor = null;
  if (info.menuItemId === "set-media-wallpaper-primary") {
    monitor = "primary";
  } else {
    const stored = await chrome.storage.local.get(["targetMonitor"]);
    monitor = stored.targetMonitor || null;
  }

  const title = tab ? tab.title : "Web Wallpaper";
  await sendWallpaperWithRetry(mediaUrl, title, monitor);
});

// Handle incoming messages from content scripts and popup
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message.action === "GET_CAPTURED_MEDIA") {
    const tabId = sender.tab ? sender.tab.id : null;
    const media = tabId ? (tabMediaStreams.get(tabId) || []) : [];
    sendResponse({ success: true, media, daemonOnline });
    return false;
  }

  if (message.action === "CHECK_STATUS") {
    (async () => {
      // Use cached health first, then refresh
      const online = await checkDaemonHealth();
      if (online) {
        try {
          const resp = await fetch(`${API_BASE}/api/status`);
          if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
          const data = await resp.json();
          sendResponse({ success: true, data, daemonOnline: true });
        } catch (err) {
          sendResponse({ success: false, error: err.message, daemonOnline: false });
        }
      } else {
        sendResponse({
          success: false,
          error: "RustWallpaper daemon is not running on 127.0.0.1:24242",
          daemonOnline: false,
        });
      }
    })();
    return true;
  }

  if (message.action === "CHECK_HEALTH") {
    (async () => {
      const online = await checkDaemonHealth();
      sendResponse({ online });
    })();
    return true;
  }

  if (message.action === "SET_WALLPAPER") {
    (async () => {
      let targetUrl = message.url;
      const tabId = sender.tab ? sender.tab.id : null;

      // For pin pages, prefer server-side resolution of original pin metadata over the
      // potentially low-resolution rendition captured from playback.

      // If the URL is a blob: URL, try to get captured streams instead
      if (targetUrl && targetUrl.startsWith("blob:")) {
        const captured = tabId ? (tabMediaStreams.get(tabId) || []) : [];
        if (captured.length > 0) {
          targetUrl = captured[0].url;
        } else {
          sendResponse({
            success: false,
            error: "This site uses blob: URLs for video playback. Try right-clicking the video and using 'Set as Wallpaper' from the context menu instead.",
          });
          return;
        }
      }

      const stored = await chrome.storage.local.get(["targetMonitor"]);
      const monitor = message.monitor || stored.targetMonitor || null;
      const result = await sendWallpaperWithRetry(
        targetUrl,
        message.title || "Web Video Wallpaper",
        monitor
      );
      sendResponse(result);
    })();
    return true;
  }

  if (message.action === "SEND_ACTION") {
    (async () => {
      try {
        for (let attempt = 0; attempt < 2; attempt++) {
          const controller = new AbortController();
          const timeoutId = setTimeout(() => controller.abort(), API_TIMEOUT_MS);
          const token = await getAuthToken();
          const headers = { "Content-Type": "application/json" };
          if (token) headers["Authorization"] = `Bearer ${token}`;

          const payload = { ...(message.payload || {}) };
          if (typeof payload.monitor === "string" && payload.monitor.startsWith("hw:")) {
            payload.monitor = null;
          }

          const resp = await fetch(`${API_BASE}/api/action`, {
            method: "POST",
            headers,
            body: JSON.stringify(payload),
            signal: controller.signal,
          });
          clearTimeout(timeoutId);
          if (resp.status === 401 && attempt === 0) {
            authToken = null;
            continue;
          }
          const data = await resp.json();
          sendResponse({ success: resp.ok, data });
          return;
        }
      } catch (err) {
        // Check if it's a timeout/network error
        const isOffline = err.name === "AbortError" || err.message.includes("Failed to fetch");
        sendResponse({
          success: false,
          error: isOffline
            ? "RustWallpaper daemon is offline. Please start the RustWallpaper desktop app."
            : err.message,
          daemonOffline: isOffline,
        });
      }
    })();
    return true;
  }
});

/**
 * Sends a wallpaper URL to RustWallpaper daemon with automatic retry on transient failures.
 */
async function sendWallpaperWithRetry(url, title, monitor) {
  // Check daemon health first
  if (!daemonOnline && Date.now() - lastHealthCheck > 5000) {
    await checkDaemonHealth();
  }

  if (!daemonOnline) {
    notifyUser(
      "RustWallpaper Not Running",
      "Please start the RustWallpaper desktop app, then try again."
    );
    return {
      success: false,
      error: "RustWallpaper daemon is not running. Please start the desktop app.",
      daemonOffline: true,
    };
  }

  notifyUser("Downloading Wallpaper...", "Sending live media to RustWallpaper engine.");

  let lastError = null;

  for (let attempt = 0; attempt <= MAX_RETRY_ATTEMPTS; attempt++) {
    let requestTimeoutId;
    try {
      const cleanMonitor =
        monitor && monitor !== "all" && !(typeof monitor === "string" && monitor.startsWith("hw:"))
          ? monitor
          : null;
      const payload = {
        url: url,
        title: title || "Web Wallpaper",
        monitor: cleanMonitor,
        apply: true
      };

      const controller = new AbortController();
      // Pinterest pin resolution plus large original files can take longer than a small
      // preview download. Give the local service enough time to resolve and inspect media.
      requestTimeoutId = setTimeout(() => controller.abort(), 180000);

      const token = await getAuthToken();
      const headers = { "Content-Type": "application/json" };
      if (token) headers["Authorization"] = `Bearer ${token}`;

      const response = await fetch(`${API_BASE}/api/wallpaper`, {
        method: "POST",
        headers,
        body: JSON.stringify(payload),
        signal: controller.signal,
      });

      clearTimeout(requestTimeoutId);
      requestTimeoutId = undefined;

      if (response.status === 401 && attempt === 0) {
        authToken = null;
        continue;
      }

      if (!response.ok) {
        const errData = await response.json().catch(() => ({}));
        const msg = errData.message || `Server responded with ${response.status}`;
        const downloaded = errData.downloaded === true;
        const mediaType = errData.media_type === "image" ? "Image" : "Video";
        const resolution = errData.width && errData.height
          ? `${errData.width} × ${errData.height}`
          : "Resolution unavailable";

        // An apply error is returned after the file was already downloaded/imported;
        // never retry that POST or it can create duplicate library entries.
        if (downloaded) {
          saveToHistory({
            url,
            resolvedSource: errData.source || null,
            title: errData.title || title,
            mediaType: errData.media_type || "video",
            resolution,
            width: errData.width || null,
            height: errData.height || null,
            timestamp: Date.now(),
          });
          notifyUser(
            "Wallpaper Downloaded (Not Applied)",
            `${mediaType} · ${resolution}. ${msg}`
          );
          return {
            success: false,
            error: msg,
            downloaded: true,
            data: { ...errData, resolution },
          };
        }

        // Don't retry on client errors (4xx) — the URL or request is invalid
        if (response.status >= 400 && response.status < 500) {
          notifyUser("Failed to Download Wallpaper", msg);
          return { success: false, error: msg };
        }
        // Server errors (5xx) — retry only when no download/import completed
        lastError = msg;
        continue;
      }

      const resData = await response.json();
      const dimensions = resData.resolution || "Resolution unavailable";
      const mediaType = resData.media_type === "image" ? "Image" : "Video";
      const resultSummary = `${mediaType} · ${dimensions}`;
      const applied = resData.applied === true;
      notifyUser(
        applied ? "Wallpaper Applied! ✨" : "Wallpaper Downloaded",
        `${resultSummary}${applied ? (monitor ? ` · ${monitor}` : " · All monitors") : " · Saved to library"}.`
      );

      // Store the resolved media and actual dimensions for useful history/replay.
      saveToHistory({
        // Keep the original web URL so history actions can resolve/download it again.
        url,
        resolvedSource: resData.source || null,
        title: resData.title || title,
        mediaType: resData.media_type || "video",
        resolution: resData.resolution || null,
        width: resData.width || null,
        height: resData.height || null,
        timestamp: Date.now()
      });

      return {
        success: true,
        data: resData,
        message: applied ? `${resultSummary} applied successfully.` : `${resultSummary} downloaded to the library.`,
      };
    } catch (err) {
      if (requestTimeoutId) clearTimeout(requestTimeoutId);
      const isNetworkError =
        err.name === "AbortError" ||
        err.message.includes("Failed to fetch") ||
        err.message.includes("NetworkError");

      if (err.name === "AbortError") {
        // Do not retry an aborted POST: the daemon may still be finishing the import,
        // so automatic retries could create duplicate library entries.
        return {
          success: false,
          error: "The wallpaper request timed out. Check the library before trying again to avoid a duplicate download.",
          timedOut: true,
        };
      }

      if (isNetworkError) {
        // Daemon went down during request — no point retrying
        daemonOnline = false;
        notifyUser(
          "RustWallpaper Not Reachable",
          "The desktop app stopped responding. Make sure RustWallpaper.exe is running."
        );
        return {
          success: false,
          error: "RustWallpaper daemon became unreachable during download.",
          daemonOffline: true,
        };
      }
      lastError = err.message;
    }
  }

  notifyUser("Failed to Apply Wallpaper", lastError || "Unknown error after retries.");
  return { success: false, error: lastError || "Failed after multiple attempts." };
}

function notifyUser(title, message) {
  try {
    chrome.notifications.create({
      type: "basic",
      iconUrl: "icons/icon-128.png",
      title: title,
      message: message,
      priority: 1
    });
  } catch (_) {
    // Notifications might be silenced in quiet mode
  }
}

async function saveToHistory(item) {
  try {
    const data = await chrome.storage.local.get(["wallpaperHistory"]);
    let history = data.wallpaperHistory || [];
    history.unshift(item);
    if (history.length > 20) {
      history = history.slice(0, 20);
    }
    await chrome.storage.local.set({ wallpaperHistory: history });
  } catch (_) {}
}
