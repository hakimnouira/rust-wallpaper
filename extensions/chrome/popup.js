/**
 * RustWallpaper Companion - Popup Dashboard Script
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

  // Load saved monitor preference
  const saved = await chrome.storage.local.get(["targetMonitor"]);
  const savedMonitor = saved.targetMonitor || "all";

  // Check Engine Status
  try {
    const res = await chrome.runtime.sendMessage({ action: "CHECK_STATUS" });
    if (res && res.success && res.data) {
      statusBadge.classList.add("online");
      statusText.textContent = "Engine Ready";

      // Populate monitors
      if (Array.isArray(res.data.monitors) && res.data.monitors.length > 0) {
        monitorSelect.innerHTML = `<option value="all">All Monitors (Synchronized)</option>`;
        res.data.monitors.forEach((m) => {
          const opt = document.createElement("option");
          opt.value = m.name || m.id || m;
          const label = m.name ? `${m.name} (${m.width || 0}x${m.height || 0})` : m;
          opt.textContent = label;
          monitorSelect.appendChild(opt);
        });

        if (savedMonitor) {
          monitorSelect.value = savedMonitor;
        }
      }
    } else {
      statusBadge.classList.remove("online");
      statusText.textContent = "Engine Offline";
    }
  } catch (err) {
    statusBadge.classList.remove("online");
    statusText.textContent = "Engine Offline";
  }

  // Monitor select change listener
  monitorSelect.addEventListener("change", () => {
    chrome.storage.local.set({ targetMonitor: monitorSelect.value });
  });

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
        setFeedback("Wallpaper successfully applied! 🎉", "success");
        urlInput.value = "";
        loadHistory();
      } else {
        const err = (res && res.error) ? res.error : "Failed to apply wallpaper.";
        setFeedback(err, "error");
      }
    } catch (err) {
      setFeedback("Error contacting RustWallpaper bridge.", "error");
    } finally {
      applyBtn.disabled = false;
    }
  });

  // Pause & Resume buttons
  pauseBtn.addEventListener("click", async () => {
    const selectedMonitor = monitorSelect.value === "all" ? null : monitorSelect.value;
    await chrome.runtime.sendMessage({
      action: "SEND_ACTION",
      payload: { action: "pause", monitor: selectedMonitor }
    });
    setFeedback("Paused desktop playback.", "success");
  });

  resumeBtn.addEventListener("click", async () => {
    const selectedMonitor = monitorSelect.value === "all" ? null : monitorSelect.value;
    await chrome.runtime.sendMessage({
      action: "SEND_ACTION",
      payload: { action: "resume", monitor: selectedMonitor }
    });
    setFeedback("Resumed desktop playback.", "success");
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
        <span class="history-url">${item.title || item.url}</span>
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
