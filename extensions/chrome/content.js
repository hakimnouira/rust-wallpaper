/**
 * RustWallpaper Companion - Content Script
 * Injects a floating "Set as Wallpaper" button over HTML5 video elements.
 * Handles SPA navigation, debounced scanning, and connection-aware error states.
 */

(function () {
  const BADGE_CLASS = "rust-wallpaper-overlay-btn";
  const CONTAINER_CLASS = "rust-wallpaper-wrapper-relative";

  // --- Connection state (updated from background) ---
  let daemonOnline = false;

  chrome.runtime.onMessage.addListener((message) => {
    if (message.action === "HEALTH_UPDATE") {
      const wasOffline = !daemonOnline;
      daemonOnline = message.online;
      // If daemon just came back online, re-scan to update button states
      if (wasOffline && daemonOnline) {
        scanAll();
      }
    }
  });

  // Check initial health
  chrome.runtime.sendMessage({ action: "CHECK_HEALTH" }).then((resp) => {
    if (resp) daemonOnline = resp.online;
    scanAll();
  }).catch(() => {
    scanAll();
  });

  function extractBestVideoUrl(video) {
    let direct = video.currentSrc || video.src || (video.querySelector("source")?.src);
    // On Pinterest, scope to the enclosing pin card link first; only fall back to
    // window.location.href when the video is in the main closeup container.
    if (window.location.hostname.includes("pinterest.")) {
      const directLink = video.closest('a[href*="/pin/"]');
      if (directLink && directLink.href) return directLink.href;
      const pinCard = video.closest(
        '[data-test-id="pin"], [data-test-id="pinWrapper"], [data-grid-item="true"], div[role="listitem"]'
      );
      const cardLink = pinCard?.querySelector('a[href*="/pin/"]');
      if (cardLink && cardLink.href) return cardLink.href;
      if (
        window.location.pathname.includes("/pin/") &&
        video.closest('[data-test-id="pin-closeup"], [data-test-id="closeup-body"], [data-test-id="story-pin-closeup"]')
      ) {
        return window.location.href;
      }
    }

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
    if (
      window.location.hostname.includes("pinterest.") &&
      video.closest('[data-test-id="pin"], [data-test-id="pinWrapper"], [data-grid-item="true"], div[role="listitem"]')
    ) {
      return;
    }
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

      // If blob: URL, warn the user
      if (mediaUrl.startsWith("blob:")) {
        showStatus(btn, "Blob URL - try context menu", false);
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
          showStatus(btn, successLabel(response.data), true);
        } else if (response && response.daemonOffline) {
          showStatus(btn, "App Offline", false);
          // Update connection state
          daemonOnline = false;
        } else {
          showStatus(btn, failureLabel(response), false);
        }
      } catch (err) {
        if (err && err.message && err.message.includes("Could not establish connection")) {
          showStatus(btn, "Extension reloading...", false);
        } else {
          showStatus(btn, "Offline", false);
        }
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

    const originalText = "Set as Wallpaper";
    label.textContent = text;

    if (success === true) {
      btn.classList.add("success");
    } else if (success === false) {
      btn.classList.add("error");
    }

    const duration = success === true ? 2500 : 3500;
    setTimeout(() => {
      label.textContent = originalText;
      btn.classList.remove("success", "error");
    }, duration);
  }

  // --- Steam Workshop Integration ---
  function extractSteamWorkshopMedia() {
    // 1. Check onclick ShowEnlargedImagePreview
    const enlargedLink = document.querySelector('a[onclick*="ShowEnlargedImagePreview"]');
    if (enlargedLink) {
      const onclickAttr = enlargedLink.getAttribute("onclick") || "";
      const match = onclickAttr.match(/ShowEnlargedImagePreview\(\s*'([^']+)'/);
      if (match && match[1]) {
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
            showStatus(overlayBtn, successLabel(response.data), true);
          } else if (response && response.daemonOffline) {
            showStatus(overlayBtn, "App Offline", false);
            daemonOnline = false;
          } else {
            showStatus(overlayBtn, failureLabel(response), false);
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
              showStatus(steamBtn, successLabel(response.data), true);
            } else if (response && response.daemonOffline) {
              showStatus(steamBtn, "App Offline", false);
              daemonOnline = false;
            } else {
              showStatus(steamBtn, failureLabel(response), false);
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
          } else if (response && response.daemonOffline) {
            showStatus(cardBtn, "Off", false);
            daemonOnline = false;
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

  // --- Pinterest Integration ---
  const capturedMediaUrls = new Set();

  async function refreshCapturedMedia() {
    try {
      const response = await chrome.runtime.sendMessage({ action: "GET_CAPTURED_MEDIA" });
      if (response && response.media && Array.isArray(response.media)) {
        for (const item of response.media) {
          if (item && item.url) capturedMediaUrls.add(item.url);
        }
      }
    } catch (_) {}
  }

  chrome.runtime.onMessage.addListener((message) => {
    if (message.action === "MEDIA_STREAM_DETECTED" && message.mediaUrl) {
      capturedMediaUrls.add(message.mediaUrl);
      scanPinterest();
    }
  });

  function extractPinterestMedia(cardOrContainer) {
    // Strictly scope extraction to this specific card so we never grab media
    // from another pin or related-pin stream on the same page.

    // 1. Direct <video> element inside this card
    const video =
      cardOrContainer.querySelector("video") ||
      (cardOrContainer.tagName === "VIDEO" ? cardOrContainer : null);
    if (video) {
      const src = video.currentSrc || video.src || video.querySelector("source")?.src;
      if (
        src &&
        !src.startsWith("blob:") &&
        (src.includes(".mp4") || src.includes(".webm") || src.includes("pinimg.com"))
      ) {
        return src;
      }

      // 1b. Check poster thumbnail and convert to progressive 720p MP4
      const poster = video.getAttribute("poster") || video.poster;
      if (poster && (poster.includes("pinimg.com/videos/thumbnails/") || poster.includes("/videos/thumbnails/"))) {
        const mp4Url = poster
          .replace("i.pinimg.com", "v1.pinimg.com")
          .replace("/thumbnails/originals/", "/mc/720p/")
          .replace("/thumbnails/mc/720p/", "/mc/720p/")
          .replace("/thumbnails/", "/mc/720p/")
          .replace(/\.0000000\.(jpg|png)$/i, ".mp4")
          .replace(/\.(jpg|png)$/i, ".mp4");
        return mp4Url;
      }
    }

    // 2. Pin link inside this card
    const link =
      cardOrContainer.querySelector('a[href*="/pin/"]') ||
      cardOrContainer.closest('a[href*="/pin/"]');
    if (link && link.href) {
      return link.href;
    }

    // 3. High-resolution / original Pinterest image or animated GIF inside this card
    const img = cardOrContainer.querySelector('img[src*="pinimg.com"]');
    if (img) {
      const imgSrc = img.currentSrc || img.src || "";
      if (imgSrc.includes("pinimg.com")) {
        const origUrl = imgSrc
          .replace(/\/(?:236x|474x|564x|736x|1200x)\//, "/originals/")
          .replace(/\/[0-9]+x[0-9]*(_RS)?\//, "/originals/");
        if (origUrl.includes("/originals/")) {
          return origUrl;
        }
        return imgSrc;
      }
    }

    return null;
  }

  function formatAppliedMedia(data) {
    if (!data) return "resolution unknown";
    const kind = data.media_type === "image" ? "Image" : "Video";
    return `${kind} ${data.resolution || "· resolution unknown"}`;
  }

  function successLabel(data) {
    const media = formatAppliedMedia(data);
    return data?.applied === false ? `${media} · Saved` : `${media} ✨`;
  }

  function failureLabel(response) {
    if (response?.downloaded) {
      return `${formatAppliedMedia(response.data)} · Not applied`;
    }
    return response?.error || "Failed";
  }

  function cleanPinterestTitle(raw) {
    if (!raw) return "Pinterest Wallpaper";
    return raw
      .replace(/\s*\|\s*Pinterest\s*$/i, "")
      .replace(/\s*on\s*Pinterest\s*$/i, "")
      .trim();
  }

  function scanPinterest() {
    if (!window.location.hostname.includes("pinterest.")) return;

    // 1. Pin Detail / Closeup Page Action Button
    if (window.location.pathname.includes("/pin/")) {
      const actionContainer = document.querySelector(
        '[data-test-id="pin-action-buttons"], [data-test-id="closeup-action-bar"], [data-test-id="pin-closeup"]'
      );
      if (actionContainer && !document.getElementById("RustWallpaperPinterestBtn")) {
        const pBtn = document.createElement("button");
        pBtn.id = "RustWallpaperPinterestBtn";
        pBtn.className = "rust-wp-pinterest-action-btn";
        pBtn.type = "button";
        pBtn.innerHTML = `<span>🖥️ Set as Wallpaper</span>`;

        pBtn.addEventListener("click", async (e) => {
          e.preventDefault();
          e.stopPropagation();

          await refreshCapturedMedia();
          // Send the pin page itself so the daemon can choose the highest-quality
          // original rendition instead of the current feed/preview stream.
          const mediaUrl = window.location.href;
          const rawTitle = document.querySelector("h1")?.textContent?.trim() || document.title;
          const title = cleanPinterestTitle(rawTitle);

          showStatus(pBtn, "Sending…", null);

          try {
            const response = await chrome.runtime.sendMessage({
              action: "SET_WALLPAPER",
              url: mediaUrl || window.location.href,
              title,
            });

            if (response && response.success) {
              showStatus(pBtn, successLabel(response.data), true);
            } else if (response && response.daemonOffline) {
              showStatus(pBtn, "App Offline", false);
              daemonOnline = false;
            } else {
              showStatus(pBtn, failureLabel(response), false);
            }
          } catch (_) {
            showStatus(pBtn, "Offline", false);
          }
        });

        actionContainer.appendChild(pBtn);
      }
    }

    // 2. Video Pins in Feed / Grid
    const pinCards = document.querySelectorAll(
      '[data-test-id="pin"], [data-test-id="pinWrapper"], [data-grid-item="true"], div[role="listitem"]'
    );

    pinCards.forEach((card) => {
      if (!card.matches('[data-test-id="pin"]') && card.querySelector('[data-test-id="pin"]')) {
        return;
      }
      const hasMedia = card.querySelector("video, img[src*='pinimg.com']");
      if (!hasMedia) return;

      if (card.dataset.rustWallpaperAttached) return;
      card.dataset.rustWallpaperAttached = "true";

      const cardPos = window.getComputedStyle(card).position;
      if (cardPos === "static") {
        card.classList.add(CONTAINER_CLASS);
      }

      const cardBtn = document.createElement("button");
      cardBtn.className = BADGE_CLASS;
      cardBtn.type = "button";
      cardBtn.title = "Set this Pinterest image or video as desktop wallpaper";
      cardBtn.innerHTML = `
        <svg class="rust-wp-icon" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
          <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
          <line x1="8" y1="21" x2="16" y2="21"></line>
          <line x1="12" y1="17" x2="12" y2="21"></line>
        </svg>
        <span class="rust-wp-label">Set as Wallpaper</span>
      `;

      cardBtn.addEventListener("click", async (e) => {
        e.stopPropagation();
        e.preventDefault();

        const pinLink =
          card.querySelector('a[href*="/pin/"]') ||
          card.closest('a[href*="/pin/"]');
        const mediaUrl = extractPinterestMedia(card);
        const targetUrl = (pinLink && pinLink.href) || mediaUrl;
        if (!targetUrl) {
          showStatus(cardBtn, "No Pin URL", false);
          return;
        }

        const candidateTitle =
          card.querySelector('img[alt]')?.getAttribute("alt")?.trim() ||
          card.querySelector("h1, h2, h3")?.textContent?.trim() ||
          "";
        const rawTitle =
          candidateTitle && !/^set as wallpaper$/i.test(candidateTitle)
            ? candidateTitle
            : "Pinterest Wallpaper";
        const title = cleanPinterestTitle(rawTitle);

        showStatus(cardBtn, "Sending…", null);

        try {
          const response = await chrome.runtime.sendMessage({
            action: "SET_WALLPAPER",
            url: targetUrl,
            title,
          });

          if (response && response.success) {
            showStatus(cardBtn, successLabel(response.data), true);
          } else if (response && response.daemonOffline) {
            showStatus(cardBtn, "App Offline", false);
            daemonOnline = false;
          } else {
            showStatus(cardBtn, failureLabel(response), false);
          }
        } catch (_) {
          showStatus(cardBtn, "Offline", false);
        }
      });

      card.appendChild(cardBtn);

      let hideTimer = null;
      const show = () => {
        if (hideTimer) clearTimeout(hideTimer);
        cardBtn.classList.add("visible");
      };
      const hide = () => {
        hideTimer = setTimeout(() => {
          cardBtn.classList.remove("visible");
        }, 800);
      };

      card.addEventListener("mouseenter", show);
      card.addEventListener("mouseleave", hide);
      cardBtn.addEventListener("mouseenter", show);
      cardBtn.addEventListener("mouseleave", hide);
    });
  }

  // --- TikTok Integration ---
  function scanTikTok() {
    if (!window.location.hostname.includes("tiktok.com")) return;
    const containers = document.querySelectorAll(
      'div[data-e2e="feed-video"], div[data-e2e="recommend-list-item-container"], .tiktok-video-container, div[data-e2e="item-video"]'
    );
    containers.forEach((box) => {
      const video = box.querySelector("video");
      if (video) attachBadgeToVideo(video);
    });
  }

  // --- Instagram Reels Integration ---
  function scanInstagram() {
    if (!window.location.hostname.includes("instagram.com")) return;
    const videos = document.querySelectorAll("article video, div[role='dialog'] video, video");
    videos.forEach(attachBadgeToVideo);
  }

  // --- Reddit Video Integration ---
  function scanReddit() {
    if (!window.location.hostname.includes("reddit.com")) return;
    const players = document.querySelectorAll(
      "shreddit-player, div[data-testid='post-container'] video, video"
    );
    players.forEach((p) => {
      const video = p.tagName === "VIDEO" ? p : p.querySelector("video");
      if (video) attachBadgeToVideo(video);
    });
  }

  // --- Generic Image Integration (PNG, JPG, WebP, BMP, GIF) ---
  function extractBestImageUrl(img) {
    const link = img.closest("a[href]");
    if (link && link.href && /\.(png|jpe?g|webp|bmp|gif)(\?.*)?$/i.test(link.href)) {
      return link.href;
    }
    return img.currentSrc || img.src || "";
  }

  function cleanImageTitle(img, mediaUrl) {
    const alt = img.getAttribute("alt")?.trim() || "";
    if (alt && !/^set as wallpaper$/i.test(alt)) {
      return alt;
    }
    try {
      const pathname = new URL(mediaUrl).pathname;
      const file = pathname.split("/").pop() || "";
      const stem = file.replace(/\.[^.]+$/, "").replace(/[_-]+/g, " ").trim();
      if (stem) return stem;
    } catch (_) {}
    const docTitle = document.title?.trim() || "";
    if (docTitle && !/^set as wallpaper$/i.test(docTitle)) {
      return docTitle;
    }
    return "Web Image Wallpaper";
  }

  function attachBadgeToImage(img) {
    if (img.dataset.rustWallpaperAttached) return;
    if (img.closest(`.${BADGE_CLASS}`)) return;

    // Pinterest pin cards and Steam Workshop previews already have dedicated handlers
    if (
      window.location.hostname.includes("pinterest.") &&
      img.closest('[data-test-id="pin"], [data-test-id="pinWrapper"], [data-grid-item="true"], div[role="listitem"]')
    ) {
      return;
    }
    if (
      window.location.hostname.includes("steamcommunity.com") &&
      img.closest("#highlight_player_area, .workshopItemPreviewImageEnlargeableContainer, .workshopItem, .workshop_showcase, .ugc")
    ) {
      return;
    }

    const rawSrc = extractBestImageUrl(img);
    if (!/^https?:\/\//i.test(rawSrc)) return;
    const pathPart = rawSrc.split("?")[0].toLowerCase();
    if (pathPart.endsWith(".svg")) return;

    const isStandaloneImageTab =
      typeof document.contentType === "string" && document.contentType.startsWith("image/");

    if (!img.complete) {
      if (!img.dataset.rustWallpaperLoadHooked) {
        img.dataset.rustWallpaperLoadHooked = "true";
        img.addEventListener("load", () => attachBadgeToImage(img), { once: true });
      }
      return;
    }

    const rect = img.getBoundingClientRect();
    if (!isStandaloneImageTab && (rect.width < 320 || rect.height < 200)) {
      return;
    }

    const parent = img.parentElement;
    if (!parent) return;

    img.dataset.rustWallpaperAttached = "true";

    const parentPos = window.getComputedStyle(parent).position;
    if (parentPos === "static") {
      parent.classList.add(CONTAINER_CLASS);
    }

    const btn = document.createElement("button");
    btn.className = isStandaloneImageTab ? `${BADGE_CLASS} visible` : BADGE_CLASS;
    btn.type = "button";
    btn.title = "Download and apply this image as your desktop wallpaper";
    btn.innerHTML = `
      <svg class="rust-wp-icon" viewBox="0 0 24 24" width="16" height="16" fill="none" stroke="currentColor" stroke-width="2">
        <rect x="2" y="3" width="20" height="14" rx="2" ry="2"></rect>
        <line x1="8" y1="21" x2="16" y2="21"></line>
        <line x1="12" y1="17" x2="12" y2="21"></line>
      </svg>
      <span class="rust-wp-label">Set as Wallpaper</span>
    `;

    btn.addEventListener("click", async (e) => {
      e.stopPropagation();
      e.preventDefault();

      const mediaUrl = extractBestImageUrl(img);
      if (!mediaUrl || !/^https?:\/\//i.test(mediaUrl)) {
        showStatus(btn, "No URL", false);
        return;
      }

      const title = cleanImageTitle(img, mediaUrl);
      showStatus(btn, "Sending...", null);

      try {
        const response = await chrome.runtime.sendMessage({
          action: "SET_WALLPAPER",
          url: mediaUrl,
          title
        });

        if (response && response.success) {
          showStatus(btn, successLabel(response.data), true);
        } else if (response && response.daemonOffline) {
          showStatus(btn, "App Offline", false);
          daemonOnline = false;
        } else {
          showStatus(btn, failureLabel(response), false);
        }
      } catch (err) {
        if (err && err.message && err.message.includes("Could not establish connection")) {
          showStatus(btn, "Extension reloading...", false);
        } else {
          showStatus(btn, "Offline", false);
        }
      }
    });

    parent.appendChild(btn);

    let hideTimer = null;
    const show = () => {
      if (hideTimer) clearTimeout(hideTimer);
      btn.classList.add("visible");
    };
    const hide = () => {
      if (isStandaloneImageTab) return;
      hideTimer = setTimeout(() => {
        btn.classList.remove("visible");
      }, 800);
    };

    img.addEventListener("mouseenter", show);
    img.addEventListener("mouseleave", hide);
    btn.addEventListener("mouseenter", show);
    btn.addEventListener("mouseleave", hide);
  }

  function scanImages() {
    const images = document.querySelectorAll("img");
    images.forEach(attachBadgeToImage);
  }

  // --- Scan All (debounced) ---
  function scanAll() {
    const videos = document.querySelectorAll("video");
    videos.forEach(attachBadgeToVideo);
    scanImages();
    scanSteamWorkshop();
    scanPinterest();
    scanTikTok();
    scanInstagram();
    scanReddit();
  }

  // Debounced MutationObserver to avoid performance hits on heavy SPA sites
  let scanTimer = null;
  let lastUrl = window.location.href;

  function debouncedScan() {
    if (scanTimer) clearTimeout(scanTimer);
    scanTimer = setTimeout(() => {
      scanAll();
    }, 300);
  }

  // Detect SPA navigation (pushState/replaceState/popstate)
  function checkUrlChange() {
    if (window.location.href !== lastUrl) {
      lastUrl = window.location.href;
      // URL changed — SPA navigation occurred
      // Wait a moment for DOM to update, then re-scan
      setTimeout(scanAll, 500);
    }
  }

  // Hook into history API for SPA navigation detection
  const origPushState = history.pushState;
  const origReplaceState = history.replaceState;
  history.pushState = function () {
    origPushState.apply(history, arguments);
    setTimeout(checkUrlChange, 100);
  };
  history.replaceState = function () {
    origReplaceState.apply(history, arguments);
    setTimeout(checkUrlChange, 100);
  };
  window.addEventListener("popstate", () => {
    setTimeout(checkUrlChange, 100);
  });

  // MutationObserver for dynamic websites (debounced for performance)
  const observer = new MutationObserver(() => {
    debouncedScan();
  });

  scanAll();
  observer.observe(document.body, { childList: true, subtree: true });

  // Also poll for URL changes every 2s (catches SPA routes that don't trigger our hooks)
  setInterval(checkUrlChange, 2000);
})();
