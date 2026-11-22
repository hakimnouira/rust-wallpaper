//! Built-in interactive HTML5, WebGL, and Canvas wallpapers.
//!
//! Provides self-contained, offline-capable interactive widgets such as
//! Digital Clock HUDs, Matrix Digital Rain, 3D Hyperspace Starfield,
//! and Glowing Audio Visualizers that run inside the embedded WebPlayer.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Metadata and source template for a built-in interactive HTML5 wallpaper.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveWidget {
    pub id: String,
    pub title: String,
    pub description: String,
    pub category: String,
    pub badge: String,
    pub html_content: String,
}

/// Returns the catalog of built-in interactive HTML5/Canvas live wallpapers.
pub fn get_builtin_widgets() -> Vec<InteractiveWidget> {
    vec![
        InteractiveWidget {
            id: "widget-cyber-clock".to_string(),
            title: "Cyber Digital Clock & HUD".to_string(),
            description: "Futuristic glowing digital clock with live date, pulsing radar ring, and floating ambient dust particles.".to_string(),
            category: "Clock & HUD".to_string(),
            badge: "🕒 Live HUD".to_string(),
            html_content: CYBER_CLOCK_HTML.to_string(),
        },
        InteractiveWidget {
            id: "widget-matrix-rain".to_string(),
            title: "Matrix Digital Code Rain".to_string(),
            description: "Classic phosphorescent green falling matrix characters on deep OLED black. High performance 60FPS canvas.".to_string(),
            category: "Retro & Sci-Fi".to_string(),
            badge: "🟢 Matrix 60FPS".to_string(),
            html_content: MATRIX_RAIN_HTML.to_string(),
        },
        InteractiveWidget {
            id: "widget-starfield".to_string(),
            title: "3D Hyperspace Warp Starfield".to_string(),
            description: "Deep space warp simulation with 1,200 stars flying past the screen in full 3D perspective.".to_string(),
            category: "Space & Stars".to_string(),
            badge: "🌌 3D Warp".to_string(),
            html_content: STARFIELD_HTML.to_string(),
        },
        InteractiveWidget {
            id: "widget-audio-visualizer".to_string(),
            title: "Neon Wave Spectrum Visualizer".to_string(),
            description: "Pulsing audio frequency wavebars with cyan-to-magenta glowing gradients and ambient background waves.".to_string(),
            category: "Music & Beats".to_string(),
            badge: "🎵 Visualizer".to_string(),
            html_content: AUDIO_VISUALIZER_HTML.to_string(),
        },
    ]
}

/// Ensures the interactive widget HTML file exists on disk in LocalAppData and returns its canonical path.
pub fn ensure_widget_file(widget: &InteractiveWidget) -> std::io::Result<PathBuf> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData\RustWallpaper"));

    let dir = local_app_data.join("RustWallpaper").join("widgets");
    fs::create_dir_all(&dir)?;

    let path = dir.join(format!("{}.html", widget.id));
    fs::write(&path, &widget.html_content)?;
    Ok(path)
}

const CYBER_CLOCK_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Cyber Clock HUD</title>
<style>
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    background: radial-gradient(circle at 50% 50%, #0d1117 0%, #05070a 100%);
    color: #e2e8f0;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", sans-serif;
    height: 100vh;
    overflow: hidden;
    display: flex;
    align-items: center;
    justify-content: center;
    user-select: none;
  }
  #particles { position: absolute; top: 0; left: 0; width: 100%; height: 100%; pointer-events: none; }
  .hud-container {
    position: relative;
    z-index: 10;
    text-align: center;
    background: rgba(15, 23, 42, 0.45);
    border: 1px solid rgba(56, 189, 248, 0.2);
    border-radius: 24px;
    padding: 48px 64px;
    backdrop-filter: blur(16px);
    box-shadow: 0 0 50px rgba(14, 165, 233, 0.15), inset 0 0 20px rgba(56, 189, 248, 0.05);
  }
  .tag {
    font-size: 13px;
    font-weight: 700;
    letter-spacing: 4px;
    text-transform: uppercase;
    color: #38bdf8;
    margin-bottom: 12px;
    opacity: 0.9;
  }
  .time {
    font-size: 88px;
    font-weight: 800;
    font-variant-numeric: tabular-nums;
    letter-spacing: 2px;
    color: #ffffff;
    text-shadow: 0 0 30px rgba(56, 189, 248, 0.6), 0 0 60px rgba(56, 189, 248, 0.2);
    line-height: 1;
    margin-bottom: 14px;
  }
  .seconds {
    font-size: 42px;
    color: #38bdf8;
    font-weight: 600;
  }
  .date {
    font-size: 18px;
    font-weight: 500;
    letter-spacing: 2px;
    color: #94a3b8;
    text-transform: uppercase;
  }
  .status-bar {
    margin-top: 24px;
    display: flex;
    justify-content: center;
    gap: 20px;
    font-size: 11px;
    letter-spacing: 1.5px;
    color: #64748b;
  }
  .status-item { display: flex; align-items: center; gap: 6px; }
  .pulse { width: 8px; height: 8px; border-radius: 50%; background: #10b981; box-shadow: 0 0 8px #10b981; animation: blink 2s infinite; }
  @keyframes blink { 0%, 100% { opacity: 1; } 50% { opacity: 0.3; } }
</style>
</head>
<body>
<canvas id="particles"></canvas>
<div class="hud-container">
  <div class="tag">SYSTEM MONITOR · LIVE DESKTOP</div>
  <div class="time"><span id="hours">00</span>:<span id="minutes">00</span><span class="seconds">:<span id="seconds">00</span></span></div>
  <div class="date" id="date">LOADING DATE…</div>
  <div class="status-bar">
    <div class="status-item"><span class="pulse"></span> LIVE ENGINE ACTIVE</div>
    <div class="status-item">GPU ACCELERATED</div>
    <div class="status-item">60 FPS</div>
  </div>
</div>
<script>
  function updateTime() {
    const now = new Date();
    const h = String(now.getHours()).padStart(2, '0');
    const m = String(now.getMinutes()).padStart(2, '0');
    const s = String(now.getSeconds()).padStart(2, '0');
    document.getElementById('hours').textContent = h;
    document.getElementById('minutes').textContent = m;
    document.getElementById('seconds').textContent = s;
    const options = { weekday: 'long', year: 'numeric', month: 'long', day: 'numeric' };
    document.getElementById('date').textContent = now.toLocaleDateString(undefined, options);
  }
  setInterval(updateTime, 1000);
  updateTime();

  const canvas = document.getElementById('particles');
  const ctx = canvas.getContext('2d');
  let width, height;
  function resize() {
    width = canvas.width = window.innerWidth;
    height = canvas.height = window.innerHeight;
  }
  window.addEventListener('resize', resize);
  resize();

  const dots = [];
  for (let i = 0; i < 70; i++) {
    dots.push({
      x: Math.random() * width,
      y: Math.random() * height,
      vx: (Math.random() - 0.5) * 0.4,
      vy: (Math.random() - 0.5) * 0.4,
      r: Math.random() * 2 + 1,
      alpha: Math.random() * 0.5 + 0.2
    });
  }

  let rafId = null;
  function render() {
    if (document.hidden) {
      rafId = null;
      return;
    }
    ctx.clearRect(0, 0, width, height);
    for (let d of dots) {
      d.x += d.vx;
      d.y += d.vy;
      if (d.x < 0) d.x = width;
      if (d.x > width) d.x = 0;
      if (d.y < 0) d.y = height;
      if (d.y > height) d.y = 0;
      ctx.beginPath();
      ctx.arc(d.x, d.y, d.r, 0, Math.PI * 2);
      ctx.fillStyle = `rgba(56, 189, 248, ${d.alpha})`;
      ctx.fill();
    }
    rafId = requestAnimationFrame(render);
  }
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && !rafId) {
      updateTime();
      rafId = requestAnimationFrame(render);
    }
  });
  render();
</script>
</body>
</html>"#;

const MATRIX_RAIN_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Matrix Digital Rain</title>
<style>
  * { margin: 0; padding: 0; }
  body { background: #000000; overflow: hidden; }
  canvas { display: block; position: absolute; top: 0; left: 0; }
</style>
</head>
<body>
<canvas id="matrix"></canvas>
<script>
  const canvas = document.getElementById('matrix');
  const ctx = canvas.getContext('2d');
  let width = canvas.width = window.innerWidth;
  let height = canvas.height = window.innerHeight;

  window.addEventListener('resize', () => {
    width = canvas.width = window.innerWidth;
    height = canvas.height = window.innerHeight;
  });

  const chars = 'アイウエオカキクケコサシスセソタチツテトナニヌネノハヒフヘホマミムメモヤユヨラリルレロワヲン0123456789ABCDEF';
  const fontSize = 16;
  let columns = Math.floor(width / fontSize);
  let drops = Array(columns).fill(1);

  function draw() {
    if (document.hidden) return;
    ctx.fillStyle = 'rgba(0, 0, 0, 0.05)';
    ctx.fillRect(0, 0, width, height);

    ctx.font = fontSize + 'px monospace';

    for (let i = 0; i < drops.length; i++) {
      const text = chars.charAt(Math.floor(Math.random() * chars.length));
      const x = i * fontSize;
      const y = drops[i] * fontSize;

      ctx.fillStyle = '#ffffff';
      ctx.fillText(text, x, y);

      ctx.fillStyle = '#00ff66';
      ctx.fillText(text, x, y - fontSize);

      if (y > height && Math.random() > 0.975) {
        drops[i] = 0;
      }
      drops[i]++;
    }
  }

  let timerId = setInterval(draw, 33);
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      if (timerId !== null) {
        clearInterval(timerId);
        timerId = null;
      }
    } else if (timerId === null) {
      timerId = setInterval(draw, 33);
    }
  });
</script>
</body>
</html>"#;

const STARFIELD_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>3D Hyperspace Warp Starfield</title>
<style>
  * { margin: 0; padding: 0; }
  body { background: #020205; overflow: hidden; }
  canvas { display: block; position: absolute; top: 0; left: 0; }
</style>
</head>
<body>
<canvas id="stars"></canvas>
<script>
  const canvas = document.getElementById('stars');
  const ctx = canvas.getContext('2d');
  let width = canvas.width = window.innerWidth;
  let height = canvas.height = window.innerHeight;

  window.addEventListener('resize', () => {
    width = canvas.width = window.innerWidth;
    height = canvas.height = window.innerHeight;
  });

  const numStars = 1000;
  const stars = [];
  for (let i = 0; i < numStars; i++) {
    stars.push({
      x: (Math.random() - 0.5) * 2000,
      y: (Math.random() - 0.5) * 2000,
      z: Math.random() * 1000,
      pz: 1000
    });
  }

  const speed = 6;
  let starRafId = null;
  function update() {
    if (document.hidden) {
      starRafId = null;
      return;
    }
    ctx.fillStyle = 'rgba(2, 2, 5, 0.35)';
    ctx.fillRect(0, 0, width, height);

    const cx = width / 2;
    const cy = height / 2;

    for (let s of stars) {
      s.pz = s.z;
      s.z -= speed;
      if (s.z <= 0) {
        s.z = 1000;
        s.pz = 1000;
        s.x = (Math.random() - 0.5) * 2000;
        s.y = (Math.random() - 0.5) * 2000;
      }

      const k = 250 / s.z;
      const px = s.x * k + cx;
      const py = s.y * k + cy;

      const pk = 250 / s.pz;
      const ppx = s.x * pk + cx;
      const ppy = s.y * pk + cy;

      const size = Math.max(0.5, (1 - s.z / 1000) * 3);
      const alpha = Math.min(1, (1 - s.z / 1000) * 1.5);

      ctx.beginPath();
      ctx.moveTo(ppx, ppy);
      ctx.lineTo(px, py);
      ctx.strokeStyle = `rgba(180, 220, 255, ${alpha})`;
      ctx.lineWidth = size;
      ctx.stroke();
    }
    starRafId = requestAnimationFrame(update);
  }
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && !starRafId) {
      starRafId = requestAnimationFrame(update);
    }
  });
  update();
</script>
</body>
</html>"#;

const AUDIO_VISUALIZER_HTML: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Neon Spectrum Wave Visualizer</title>
<style>
  * { margin: 0; padding: 0; }
  body { background: radial-gradient(circle at 50% 60%, #110d1e 0%, #06040a 100%); overflow: hidden; }
  canvas { display: block; position: absolute; top: 0; left: 0; }
</style>
</head>
<body>
<canvas id="viz"></canvas>
<script>
  const canvas = document.getElementById('viz');
  const ctx = canvas.getContext('2d');
  let width = canvas.width = window.innerWidth;
  let height = canvas.height = window.innerHeight;

  window.addEventListener('resize', () => {
    width = canvas.width = window.innerWidth;
    height = canvas.height = window.innerHeight;
  });

  const bars = 64;
  let phase = 0;
  let audioData = null;
  let hasRealAudio = false;

  // Web Audio API & Loopback capture
  if (navigator.mediaDevices && navigator.mediaDevices.getUserMedia) {
    navigator.mediaDevices.getUserMedia({ audio: true }).then(stream => {
      const audioCtx = new (window.AudioContext || window.webkitAudioContext)();
      const source = audioCtx.createMediaStreamSource(stream);
      const analyser = audioCtx.createAnalyser();
      analyser.fftSize = 128;
      analyser.smoothingTimeConstant = 0.82;
      source.connect(analyser);
      audioData = new Uint8Array(analyser.frequencyBinCount);
      let sampleRafId = null;
      function sampleAudio() {
        if (document.hidden) {
          sampleRafId = null;
          return;
        }
        analyser.getByteFrequencyData(audioData);
        let sum = 0;
        for (let j = 0; j < audioData.length; j++) sum += audioData[j];
        hasRealAudio = (sum / audioData.length) > 2.0;
        sampleRafId = requestAnimationFrame(sampleAudio);
      }
      document.addEventListener('visibilitychange', () => {
        if (!document.hidden && !sampleRafId) {
          sampleRafId = requestAnimationFrame(sampleAudio);
        }
      });
      sampleAudio();
    }).catch(() => {
      // Graceful fallback to fluid organic harmonic synthesis
    });
  }

  let vizRafId = null;
  function render() {
    if (document.hidden) {
      vizRafId = null;
      return;
    }
    ctx.clearRect(0, 0, width, height);

    const barWidth = (width / bars) * 0.72;
    const spacing = (width / bars) * 0.28;
    const baseY = height * 0.75;
    phase += 0.035;

    for (let i = 0; i < bars; i++) {
      let amp = 0;
      if (hasRealAudio && audioData) {
        const binIndex = Math.floor((i / bars) * audioData.length);
        amp = (audioData[binIndex] || 0) / 255.0;
      } else {
        const freq1 = Math.sin(phase + i * 0.14) * 0.45;
        const freq2 = Math.cos(phase * 1.6 + i * 0.28) * 0.35;
        const freq3 = Math.sin(phase * 0.7 - i * 0.08) * 0.2;
        amp = Math.abs(freq1 + freq2 + freq3);
      }

      const h = Math.max(10, amp * (height * 0.52));
      const x = i * (barWidth + spacing) + spacing / 2;
      const y = baseY - h;

      const grad = ctx.createLinearGradient(0, y, 0, baseY);
      grad.addColorStop(0, '#06b6d4');
      grad.addColorStop(0.5, '#8b5cf6');
      grad.addColorStop(1, '#ec4899');

      ctx.fillStyle = grad;
      ctx.shadowColor = '#8b5cf6';
      ctx.shadowBlur = 16;

      ctx.beginPath();
      ctx.roundRect(x, y, barWidth, h, [4, 4, 0, 0]);
      ctx.fill();

      // Mirror reflection below
      const refGrad = ctx.createLinearGradient(0, baseY, 0, baseY + h * 0.32);
      refGrad.addColorStop(0, 'rgba(139, 92, 246, 0.3)');
      refGrad.addColorStop(1, 'rgba(139, 92, 246, 0)');
      ctx.fillStyle = refGrad;
      ctx.shadowBlur = 0;
      ctx.fillRect(x, baseY + 2, barWidth, h * 0.32);
    }

    vizRafId = requestAnimationFrame(render);
  }
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && !vizRafId) {
      vizRafId = requestAnimationFrame(render);
    }
  });
  render();
</script>
</body>
</html>"#;
