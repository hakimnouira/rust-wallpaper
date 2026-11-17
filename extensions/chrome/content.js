/**
 * RustWallpaper Companion - Content Script
 * Injects a floating "Set as Wallpaper" button over HTML5 video elements.
 */

(function () {
  const BADGE_CLASS = "rust-wallpaper-overlay-btn";
  const CONTAINER_CLASS = "rust-wallpaper-wrapper-relative";

  function attachBadgeToVideo(video) {
    if (video.dataset.rustWallpaperAttached) return;
    video.dataset.rustWallpaperAttached = "true";

    // Create badge element
    const btn = document.createElement("button");
    btn.className = BADGE_CLASS;
    btn.type = "button";
    btn.title = "Apply this video as your live desktop wallpaper";
    btn.innerHTML = `
      <svg class="rust-wp-icon" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
        <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
        <line x1="8" y1="21" x2="16" y2="21"></line>
        <line x1="12" y1="17" x2="12" y2="21"></line>
      </svg>
      <span class="rust-wp-label">Set as Wallpaper</span>
    `;

    // Ensure parent has position: relative or absolute so button can be anchored
    const parent = video.parentElement;
    if (!parent) return;

    const parentPos = window.getComputedStyle(parent).position;
    if (parentPos === "static") {
      parent.classList.add(CONTAINER_CLASS);
    }

    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      e.preventDefault();

      const mediaUrl = video.currentSrc || video.src || (video.querySelector("source")?.src);
      if (!mediaUrl) {
        showStatus(btn, "No URL", false);
        return;
      }

      showStatus(btn, "Sending...", null);

      try {
        const response = await chrome.runtime.sendMessage({
          action: "SET_WALLPAPER",
          url: mediaUrl,
          title: document.title || "Live Web Video"
        });

        if (response && response.success) {
          showStatus(btn, "Applied! ✨", true);
        } else {
          showStatus(btn, "Failed", false);
        }
      } catch (err) {
        showStatus(btn, "Offline", false);
      }
    });

    parent.appendChild(btn);

    // Show button when hovering over video or button itself
    let hideTimer = null;
    const show = () => {
      if (hideTimer) clearTimeout(hideTimer);
      btn.classList.add("visible");
    };
    const hide = () => {
      hideTimer = setTimeout(() => {
        btn.classList.remove("visible");
      }, 800);
    };

    video.addEventListener("mouseenter", show);
    video.addEventListener("mouseleave", hide);
    btn.addEventListener("mouseenter", show);
    btn.addEventListener("mouseleave", hide);
  }

  function showStatus(btn, text, success) {
    const label = btn.querySelector(".rust-wp-label");
    if (!label) return;

    const originalText = "Set as Wallpaper";
    label.textContent = text;

    if (success === true) {
      btn.classList.add("success");
    } else if (success === false) {
      btn.classList.add("error");
    }

    setTimeout(() => {
      label.textContent = originalText;
      btn.classList.remove("success", "error");
    }, 2500);
  }

  // Scan for existing videos
  function scanVideos() {
    const videos = document.querySelectorAll("video");
    videos.forEach(attachBadgeToVideo);
  }

  // MutationObserver for dynamic websites (YouTube, Reddit, etc.)
  const observer = new MutationObserver(() => {
    scanVideos();
  });

  scanVideos();
  observer.observe(document.body, { childList: true, subtree: true });
})();
