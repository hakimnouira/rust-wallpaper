/**
 * RustWallpaper Companion - Content Script
 * Injects a floating "Set as Wallpaper" button over HTML5 video elements.
 */

(function () {
  const BADGE_CLASS = "rust-wallpaper-overlay-btn";
  const CONTAINER_CLASS = "rust-wallpaper-wrapper-relative";

  function extractBestVideoUrl(video) {
    let direct = video.currentSrc || video.src || (video.querySelector("source")?.src);
    if (direct && !direct.startsWith("blob:") && (direct.includes(".mp4") || direct.includes(".webm") || direct.includes("cdn.pixabay.com"))) {
      return direct;
    }

    // Try schema.org JSON-LD (Pixabay and other rich media video sites)
    try {
      const scripts = document.querySelectorAll('script[type="application/ld+json"]');
      for (const script of scripts) {
        const text = script.textContent || "";
        if (text.includes("contentUrl")) {
          const data = JSON.parse(text);
          if (data.contentUrl) return data.contentUrl;
          if (Array.isArray(data["@graph"])) {
            for (const item of data["@graph"]) {
              if (item.contentUrl) return item.contentUrl;
            }
          }
        }
      }
    } catch (_) {}

    // Try OpenGraph / Twitter meta video tags
    const ogVideo = document.querySelector('meta[property="og:video"], meta[property="og:video:secure_url"], meta[name="twitter:player:stream"]');
    if (ogVideo && ogVideo.content && !ogVideo.content.startsWith("blob:")) {
      return ogVideo.content;
    }

    // Return direct if available (non-blob)
    if (direct && !direct.startsWith("blob:")) {
      return direct;
    }

    // Fall back to current page URL (RustWallpaper daemon will resolve it automatically)
    return window.location.href;
  }

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

      const mediaUrl = extractBestVideoUrl(video);
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
    const label = btn.querySelector(".rust-wp-label") || btn.querySelector("span") || btn;
    if (!label) return;

    const originalText = label.textContent;
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

  // --- Steam Workshop Integration ---
  function extractSteamWorkshopMedia() {
    // 1. Check onclick ShowEnlargedImagePreview
    const enlargedLink = document.querySelector('a[onclick*="ShowEnlargedImagePreview"]');
    if (enlargedLink) {
      const onclickAttr = enlargedLink.getAttribute("onclick") || "";
      const match = onclickAttr.match(/ShowEnlargedImagePreview\(\s*'([^']+)'/);
      if (match && match[1]) {
        // Strip query params to get full raw image / animated gif
        return match[1].split("?")[0];
      }
    }

    // 2. Check previewImage element
    const previewImg = document.getElementById("previewImage");
    if (previewImg && previewImg.src) {
      return previewImg.src.split("?")[0];
    }

    // 3. Check HTML5 video player if item has a preview video
    const video = document.querySelector("#highlight_player_area video, .highlight_player_item video");
    if (video) {
      const src = video.currentSrc || video.src || video.querySelector("source")?.src;
      if (src && !src.startsWith("blob:")) return src;
    }

    // 4. Check og:image meta tag
    const ogImg = document.querySelector('meta[property="og:image"]');
    if (ogImg && ogImg.content && ogImg.content.includes("steamusercontent")) {
      return ogImg.content.split("?")[0];
    }

    // Fall back to current workshop page URL
    return window.location.href;
  }

  function scanSteamWorkshop() {
    if (!window.location.hostname.includes("steamcommunity.com")) return;

    // 1. Steam Workshop Item Detail Page
    const highlightArea = document.getElementById("highlight_player_area") ||
      document.querySelector(".workshopItemPreviewImageEnlargeableContainer");

    if (highlightArea && !highlightArea.dataset.rustWallpaperAttached) {
      highlightArea.dataset.rustWallpaperAttached = "true";
      highlightArea.classList.add(CONTAINER_CLASS);

      const overlayBtn = document.createElement("button");
      overlayBtn.className = BADGE_CLASS + " visible";
      overlayBtn.type = "button";
      overlayBtn.title = "Set this Steam Workshop item as desktop wallpaper";
      overlayBtn.innerHTML = `
        <svg class="rust-wp-icon" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
          <line x1="8" y1="21" x2="16" y2="21"></line>
          <line x1="12" y1="17" x2="12" y2="21"></line>
        </svg>
        <span class="rust-wp-label">Set as Wallpaper</span>
      `;

      overlayBtn.addEventListener("click", async (e) => {
        e.stopPropagation();
        e.preventDefault();

        const mediaUrl = extractSteamWorkshopMedia();
        const title = document.querySelector(".workshopItemTitle")?.textContent?.trim() || document.title;

        showStatus(overlayBtn, "Sending...", null);

        try {
          const response = await chrome.runtime.sendMessage({
            action: "SET_WALLPAPER",
            url: mediaUrl,
            title
          });

          if (response && response.success) {
            showStatus(overlayBtn, "Applied! ✨", true);
          } else {
            showStatus(overlayBtn, "Failed", false);
          }
        } catch (_) {
          showStatus(overlayBtn, "Offline", false);
        }
      });

      highlightArea.appendChild(overlayBtn);

      // Also add native-style button next to Subscribe button
      const subscribeArea = document.querySelector(".game_area_purchase_game, #ItemControls .workshopItemRatings");
      if (subscribeArea && !document.getElementById("RustWallpaperSteamBtn")) {
        const steamBtn = document.createElement("a");
        steamBtn.id = "RustWallpaperSteamBtn";
        steamBtn.className = "rust-wp-steam-action-btn";
        steamBtn.innerHTML = `<span>🖥️ Set as Wallpaper</span>`;
        steamBtn.addEventListener("click", async (e) => {
          e.preventDefault();
          const mediaUrl = extractSteamWorkshopMedia();
          const title = document.querySelector(".workshopItemTitle")?.textContent?.trim() || document.title;

          showStatus(steamBtn, "Sending…", null);

          try {
            const response = await chrome.runtime.sendMessage({
              action: "SET_WALLPAPER",
              url: mediaUrl,
              title
            });

            if (response && response.success) {
              showStatus(steamBtn, "Applied! ✨", true);
            } else {
              showStatus(steamBtn, "Failed", false);
            }
          } catch (_) {
            showStatus(steamBtn, "Offline", false);
          }
        });

        subscribeArea.appendChild(steamBtn);
      }
    }

    // 2. Steam Workshop Browse / Grid Cards
    const workshopItems = document.querySelectorAll(".workshopItem, .workshop_showcase, .ugc");
    workshopItems.forEach((card) => {
      if (card.dataset.rustWallpaperAttached) return;
      card.dataset.rustWallpaperAttached = "true";

      const link = card.querySelector("a[href*='filedetails/?id=']") || card.closest("a");
      if (!link) return;

      const holder = card.querySelector(".workshopItemPreviewHolder, .workshopItemPreviewImage") || card;
      const holderPos = window.getComputedStyle(holder).position;
      if (holderPos === "static") {
        holder.classList.add(CONTAINER_CLASS);
      }

      const cardBtn = document.createElement("button");
      cardBtn.className = BADGE_CLASS;
      cardBtn.type = "button";
      cardBtn.title = "Set this workshop item as wallpaper";
      cardBtn.innerHTML = `
        <svg class="rust-wp-icon" viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
          <line x1="8" y1="21" x2="16" y2="21"></line>
          <line x1="12" y1="17" x2="12" y2="21"></line>
        </svg>
        <span class="rust-wp-label">Set</span>
      `;

      cardBtn.addEventListener("click", async (e) => {
        e.stopPropagation();
        e.preventDefault();

        const itemUrl = link.href;
        const title = card.querySelector(".workshopItemTitle, .ugc_title")?.textContent?.trim() || "Steam Workshop Item";

        showStatus(cardBtn, "…", null);

        try {
          const response = await chrome.runtime.sendMessage({
            action: "SET_WALLPAPER",
            url: itemUrl,
            title
          });

          if (response && response.success) {
            showStatus(cardBtn, "✨", true);
          } else {
            showStatus(cardBtn, "Err", false);
          }
        } catch (_) {
          showStatus(cardBtn, "Off", false);
        }
      });

      holder.appendChild(cardBtn);
    });
  }

  // Scan for existing videos and Steam items
  function scanAll() {
    const videos = document.querySelectorAll("video");
    videos.forEach(attachBadgeToVideo);
    scanSteamWorkshop();
  }

  // MutationObserver for dynamic websites (YouTube, Reddit, Steam Workshop, etc.)
  const observer = new MutationObserver(() => {
    scanAll();
  });

  scanAll();
  observer.observe(document.body, { childList: true, subtree: true });
})();
