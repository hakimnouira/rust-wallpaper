/**
 * RustWallpaper Companion - Popup Dashboard Script
 * Enhanced with connection health indicator and actionable error states.
 */

document.addEventListener("DOMContentLoaded", async () => {
  const statusBadge = document.getElementById("statusBadge");
  const statusText = document.getElementById("statusText");
  const monitorSelect = document.getElementById("monitorSelect");
  const urlInput = document.getElementById("urlInput");
  const applyBtn = document.getElementById("applyBtn");
  const pauseBtn = document.getElementById("pauseBtn");
  const resumeBtn = document.getElementById("resumeBtn");
  const feedback = document.getElementById("feedback");
  const historyList = document.getElementById("historyList");
  const healthDot = document.getElementById("healthDot");
  const healthText = document.getElementById("healthText");

  // Load saved monitor preference (clear stale hardware-ID values from older extension builds)
  const saved = await chrome.storage.local.get(["targetMonitor"]);
  let savedMonitor = saved.targetMonitor || "all";
  if (typeof savedMonitor === "string" && savedMonitor.startsWith("hw:")) {
    savedMonitor = "all";
    chrome.storage.local.set({ targetMonitor: "all" });
  }

  // Check Engine Status
  async function checkStatus() {
    try {
      const res = await chrome.runtime.sendMessage({ action: "CHECK_STATUS" });
      if (res && res.success && res.data) {
        statusBadge.classList.add("online");
        statusText.textContent = "Engine Ready";
        updateHealthIndicator(true);

        // Populate monitors
        if (Array.isArray(res.data.monitors) && res.data.monitors.length > 0) {
          monitorSelect.innerHTML = `<option value="all">All Monitors (Synchronized)</option>`;
          res.data.monitors.forEach((m) => {
            const opt = document.createElement("option");
            const devName = m.device_name || m.name || (typeof m === "string" ? m : null);
            opt.value = devName || "all";
            if (devName) {
              const shortName = devName.replace(/^\\\\\.\\/, "");
              const primaryTag = m.is_primary ? " (Primary)" : "";
              opt.textContent = `${shortName}${primaryTag} (${m.width || 0}x${m.height || 0})`;
            } else {
              opt.textContent = String(m);
            }
            monitorSelect.appendChild(opt);
          });

          if (savedMonitor) {
            monitorSelect.value = savedMonitor;
          }
        }
      } else {
        statusBadge.classList.remove("online");
        statusText.textContent = "Engine Offline";
        updateHealthIndicator(false, res ? res.error : null);
      }
    } catch (err) {
      statusBadge.classList.remove("online");
      statusText.textContent = "Engine Offline";
      updateHealthIndicator(false, "Extension is reconnecting...");
    }
  }

  function updateHealthIndicator(online, errorMsg) {
    if (online) {
      healthDot.className = "health-dot online";
      healthText.textContent = "Connected to RustWallpaper desktop engine";
      healthText.className = "health-text online";
    } else {
      healthDot.className = "health-dot offline";
      if (errorMsg && errorMsg.includes("not running")) {
        healthText.textContent = "Desktop app not running — please start RustWallpaper.exe";
      } else if (errorMsg) {
        healthText.textContent = errorMsg;
      } else {
        healthText.textContent = "Cannot reach desktop engine on 127.0.0.1:24242";
      }
      healthText.className = "health-text offline";
    }
  }

  // Initial status check
  await checkStatus();

  // Listen for health updates from background
  chrome.runtime.onMessage.addListener((message) => {
    if (message.action === "HEALTH_UPDATE") {
      updateHealthIndicator(message.online);
      if (message.online) {
        statusBadge.classList.add("online");
        statusText.textContent = "Engine Ready";
      } else {
        statusBadge.classList.remove("online");
        statusText.textContent = "Engine Offline";
      }
    }
  });

  // Monitor select change listener
  monitorSelect.addEventListener("change", () => {
    chrome.storage.local.set({ targetMonitor: monitorSelect.value });
  });

  // Auto-detect active tab video/page URL
  try {
    const [activeTab] = await chrome.tabs.query({ active: true, currentWindow: true });
    if (activeTab && activeTab.url && (activeTab.url.startsWith("http://") || activeTab.url.startsWith("https://"))) {
      if (activeTab.url.includes("/videos/") || activeTab.url.includes(".mp4") || activeTab.url.includes(".webm")) {
        urlInput.value = activeTab.url;
      }
    }
  } catch (_) {}

  // Apply button listener
  applyBtn.addEventListener("click", async () => {
    const url = urlInput.value.trim();
    if (!url) {
      setFeedback("Please enter a valid media URL.", "error");
      return;
    }

    setFeedback("Downloading and applying...", "");
    applyBtn.disabled = true;

    try {
      const selectedMonitor = monitorSelect.value === "all" ? null : monitorSelect.value;
      const res = await chrome.runtime.sendMessage({
        action: "SET_WALLPAPER",
        url: url,
        title: "Manual Web Wallpaper",
        monitor: selectedMonitor
      });

      if (res && res.success) {
        const data = res.data || {};
        const kind = data.media_type === "image" ? "Image" : "Video";
        const resolution = data.resolution || "resolution unavailable";
        setFeedback(
          data.applied === false
            ? `${kind} downloaded · ${resolution} (saved to library, not applied)`
            : `${kind} applied · ${resolution} 🎉`,
          "success"
        );
        urlInput.value = "";
        loadHistory();
      } else if (res && res.downloaded) {
        const data = res.data || {};
        const kind = data.media_type === "image" ? "Image" : "Video";
        const resolution = data.resolution || "resolution unavailable";
        setFeedback(`${kind} downloaded · ${resolution}, but could not be applied: ${res.error || "Unknown error"}`, "error");
        loadHistory();
      } else if (res && res.daemonOffline) {
        setFeedback("RustWallpaper desktop app is not running. Please start it and try again.", "error");
        updateHealthIndicator(false, res.error);
      } else {
        const err = (res && res.error) ? res.error : "Failed to apply wallpaper.";
        setFeedback(err, "error");
      }
    } catch (err) {
      setFeedback("Error contacting RustWallpaper bridge. The extension may be reloading.", "error");
    } finally {
      applyBtn.disabled = false;
    }
  });

  // Pause & Resume buttons
  pauseBtn.addEventListener("click", async () => {
    const selectedMonitor = monitorSelect.value === "all" ? null : monitorSelect.value;
    const res = await chrome.runtime.sendMessage({
      action: "SEND_ACTION",
      payload: { action: "pause", monitor: selectedMonitor }
    });
    if (res && res.success) {
      setFeedback("Paused desktop playback.", "success");
    } else if (res && res.daemonOffline) {
      setFeedback("Cannot pause — desktop app is offline.", "error");
    } else {
      setFeedback("Failed to pause playback.", "error");
    }
  });

  resumeBtn.addEventListener("click", async () => {
    const selectedMonitor = monitorSelect.value === "all" ? null : monitorSelect.value;
    const res = await chrome.runtime.sendMessage({
      action: "SEND_ACTION",
      payload: { action: "resume", monitor: selectedMonitor }
    });
    if (res && res.success) {
      setFeedback("Resumed desktop playback.", "success");
    } else if (res && res.daemonOffline) {
      setFeedback("Cannot resume — desktop app is offline.", "error");
    } else {
      setFeedback("Failed to resume playback.", "error");
    }
  });

  function setFeedback(text, type) {
    feedback.textContent = text;
    feedback.className = "msg-feedback " + (type || "");
  }

  // Load and render history
  async function loadHistory() {
    const data = await chrome.storage.local.get(["wallpaperHistory"]);
    const list = data.wallpaperHistory || [];
    if (list.length === 0) {
      historyList.innerHTML = `<li class="history-item" style="cursor: default; color: #475569;">No recent wallpapers</li>`;
      return;
    }

    historyList.innerHTML = "";
    list.slice(0, 5).forEach((item) => {
      const li = document.createElement("li");
      li.className = "history-item";
      li.title = item.url;
      li.innerHTML = `
        <span class="history-url">${item.title || item.url}${item.resolution ? ` · ${item.resolution}` : ""}</span>
        <span style="font-size: 10px; opacity: 0.6;">Apply</span>
      `;
      li.addEventListener("click", () => {
        urlInput.value = item.url;
        applyBtn.click();
      });
      historyList.appendChild(li);
    });
  }

  loadHistory();
});
