/**
 * RustWallpaper Companion - Background Service Worker
 * Handles context menus, desktop notifications, and loopback API communication.
 */

const API_BASE = "http://127.0.0.1:24242";

// Setup context menus
chrome.runtime.onInstalled.addListener(() => {
  setupContextMenus();
});

chrome.runtime.onStartup.addListener(() => {
  setupContextMenus();
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

// Handle context menu clicks
chrome.contextMenus.onClicked.addListener(async (info, tab) => {
  let mediaUrl = info.srcUrl || info.linkUrl;
  if (!mediaUrl && info.pageUrl) {
    mediaUrl = info.pageUrl;
  }

  if (!mediaUrl) {
    notifyUser("No Media Detected", "Could not determine direct media URL.");
    return;
  }

  let monitor = null;
  if (info.menuItemId === "set-media-wallpaper-primary") {
    monitor = "primary";
  } else {
    // Check saved preferred monitor
    const stored = await chrome.storage.local.get(["targetMonitor"]);
    monitor = stored.targetMonitor || null;
  }

  const title = tab ? tab.title : "Web Wallpaper";
  await sendWallpaperToDaemon(mediaUrl, title, monitor);
});

// Handle incoming messages from content scripts and popup
chrome.runtime.onMessage.addListener((message, sender, sendResponse) => {
  if (message.action === "SET_WALLPAPER") {
    (async () => {
      const stored = await chrome.storage.local.get(["targetMonitor"]);
      const monitor = message.monitor || stored.targetMonitor || null;
      const result = await sendWallpaperToDaemon(message.url, message.title || "Web Video Wallpaper", monitor);
      sendResponse(result);
    })();
    return true; // Keep message channel open for async response
  }

  if (message.action === "CHECK_STATUS") {
    (async () => {
      try {
        const resp = await fetch(`${API_BASE}/api/status`);
        if (!resp.ok) {
          throw new Error(`HTTP ${resp.status}`);
        }
        const data = await resp.json();
        sendResponse({ success: true, data });
      } catch (err) {
        sendResponse({ success: false, error: err.message });
      }
    })();
    return true;
  }

  if (message.action === "SEND_ACTION") {
    (async () => {
      try {
        const resp = await fetch(`${API_BASE}/api/action`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(message.payload || {})
        });
        const data = await resp.json();
        sendResponse({ success: resp.ok, data });
      } catch (err) {
        sendResponse({ success: false, error: err.message });
      }
    })();
    return true;
  }
});

/**
 * Sends a wallpaper URL to RustWallpaper daemon via localhost API
 */
async function sendWallpaperToDaemon(url, title, monitor) {
  notifyUser("Downloading Wallpaper...", "Sending live media to RustWallpaper engine.");

  try {
    const payload = {
      url: url,
      title: title || "Web Wallpaper",
      monitor: (monitor && monitor !== "all") ? monitor : null,
      apply: true
    };

    const response = await fetch(`${API_BASE}/api/wallpaper`, {
      method: "POST",
      headers: {
        "Content-Type": "application/json"
      },
      body: JSON.stringify(payload)
    });

    if (!response.ok) {
      const errData = await response.json().catch(() => ({}));
      const msg = errData.message || `Server responded with ${response.status}`;
      notifyUser("Failed to Apply Wallpaper", msg);
      return { success: false, error: msg };
    }

    const resData = await response.json();
    notifyUser("Wallpaper Applied! ✨", `Successfully loaded on desktop${monitor ? ` (${monitor})` : ""}.`);
    
    // Save to history
    saveToHistory({
      url,
      title,
      timestamp: Date.now()
    });

    return { success: true, data: resData };
  } catch (err) {
    notifyUser("RustWallpaper Not Reachable", "Make sure RustWallpaper.exe is running on your PC.");
    return { success: false, error: err.message };
  }
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
