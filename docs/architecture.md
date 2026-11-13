# Architecture Notes & Desktop Integration

## Design Principles

1. Keep Windows APIs strictly behind `wallpaper-windows`; domain types in `wallpaper-core` and protocol contracts in `wallpaper-protocol` must compile on non-Windows platforms.
2. Keep the control UI separate from wallpaper renderer processes.
3. Use a versioned, serializable IPC contract.
4. Treat each player as a supervised child process with explicit start, pause, resume, and stop behavior.
5. Release GPU, decoder, and browser resources when players stop.
6. Do not assume a single desktop-host window or static monitor topology.

## Process Model

- **wallpaper-daemon**: owns application lifecycle, display enumeration, desktop host orchestration, player supervision, and IPC.
- **video-player**: isolated hardware video decoding and presentation process embedding MPV.
- **desktop-ui** (future milestone): library, settings, import, and user commands.
- **web-player** (future milestone): WebView2 content in a separate worker process.
- **watchdog** (future milestone): restart policy and health reporting.

---

## Video Wallpaper Engine (Milestone 3)

### 1. MPV Process Architecture & HWND Embedding
- **Isolated Process Execution**: Playback runs via an external `mpv.exe` child process rather than in-process decoding libraries, isolating decoder crashes or memory leaks from the main daemon.
- **Native Embedding via `--wid`**: MPV renders directly into the target wallpaper window via `--wid=<HWND>`, utilizing Direct3D 11 (`d3d11va`) without creating standalone foreground or taskbar windows.
- **Desktop Window Compatibility**: Borderless wallpaper windows are created with `WS_POPUP | WS_VISIBLE | WS_CLIPSIBLINGS` and reparented via Win32 `SetParent` into `WorkerW`. This permits MPV's DirectX swapchain to bind correctly while remaining layered strictly behind desktop icons (`HWND_BOTTOM`).

### 2. Bidirectional Named Pipe IPC
- **Dedicated Named Pipe**: Each MPV process connects through a unique Windows Named Pipe (`\\.\pipe\rust-wallpaper-mpv-<UUID>`).
- **Background Reader Thread**: A background thread continuously consumes line-delimited JSON messages.
- **Request ID Matching**: Synchronous command replies are matched against pending requests via `request_id`, allowing unsolicited asynchronous events (`seek`, `playback-restart`) to pass through without desynchronizing state.
- **Supported Controls**: Pause, resume, volume adjustment (0-100), mute/unmute, stop, and active hardware decoder queries.

### 3. Hardware Decoding & Telemetry
- Supports `HardwareAcceleration::Auto`, `D3d11va`, `Dxva2`, and `Disabled` (software fallback).
- Active decoder query via MPV IPC (`hwdec-current`) dynamically detects and logs hardware acceleration status (e.g., `d3d11va` on AMD Radeon RX 6700 XT).
- Memory working set telemetry sampled via Win32 `GetProcessMemoryInfo`.

### 4. Kernel-Level Anti-Orphan Process Supervision
- Every spawned MPV child process is immediately registered with a Windows Job Object configured with:
  ```rust
  JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
  ```
- If the daemon crashes, is forcefully killed, or exits, the Windows kernel unconditionally and immediately terminates all MPV processes, guaranteeing zero orphaned player processes.

### 5. Persistent SQLite Wallpaper Library (`wallpaper-core`)
- SQLite storage backed by `rusqlite` (`library.db`) with `wal` journal mode.
- Stores wallpaper metadata: UUID, title, source file path, duration, resolution, thumbnail path, and playback configuration.

---

## Windows Desktop Integration (Milestone 2)

### 1. Research: Documented APIs vs Undocumented Shell Internals

| API / Mechanism | Status | Purpose & Behavior |
| :--- | :--- | :--- |
| `FindWindowW` | **Documented** | Locates top-level windows by class name (e.g. `"Progman"`) or title. |
| `FindWindowExW` | **Documented** | Traverses child and sibling windows across Z-order (e.g. locating `SHELLDLL_DefView` or child `WorkerW`). |
| `EnumWindows` | **Documented** | Enumerates all top-level windows on the caller's desktop thread. |
| `SendMessageTimeoutW` | **Documented** | Synchronously dispatches a message to a window with timeout protection, preventing hangs if Explorer is unresponsive. |
| `SetWindowPos` | **Documented** | Adjusts window bounds, visibility, and Z-order (`HWND_BOTTOM`, `SWP_NOACTIVATE`, `SWP_SHOWWINDOW`). |
| `DestroyWindow` / GDI Cleanup | **Documented** | Tears down the HWND and frees GDI brushes during shutdown. |
| **Message `0x052C`** | **Undocumented** | Custom Explorer/Progman message (`WM_SPAWN_WORKER`). Forces Windows Explorer to separate desktop drawing by spawning a `WorkerW` background layer directly behind `SHELLDLL_DefView`. |
| **`WorkerW` Layering** | **Undocumented** | Windows Explorer creates multiple `WorkerW` windows for task switching and wallpaper transitions. Exactly one `WorkerW` holds the desktop icon container (`SHELLDLL_DefView`); its immediately subsequent sibling in the top-level Z-order acts as the background wallpaper canvas. |

### 2. Desktop Host Discovery (`desktop_host.rs`)

Desktop host discovery locates the precise parent window for wallpaper embedding:

1. **Desktop Station Verification**: Ensures the thread is bound to the interactive desktop (`default` on `WinSta0`), handling cases where the process is spawned by background services or automated test runners.
2. **Locate `Progman`**: Identifies `Program Manager` (`FindWindowW(w!("Progman"), ...)`).
3. **Trigger WorkerW Spawning**: Sends message `0x052C` (`WPARAM(0x0000000D), LPARAM(0x00000001)`) to `Progman` with a 1000ms timeout.
4. **Top-Level Window Enumeration**:
   - Enumerates all top-level windows.
   - Inspects each window for `SHELLDLL_DefView` (the icon list container).
   - Collects diagnostic information on all candidate `WorkerW` windows.
5. **Sibling WorkerW Identification**:
   - Obtains the container window holding `SHELLDLL_DefView`.
   - Queries `FindWindowExW(HWND::default(), shell_container, w!("WorkerW"), PCWSTR::null())`.
   - The resulting `WorkerW` is verified with `IsWindow` and selected as the wallpaper host.
   - If not found, falls back to an unoccupied `WorkerW` or `Progman`.

### 3. Native Wallpaper Windows (`window.rs`)

Each display monitor receives a dedicated native Win32 child window:

- **Window Styles**: `WS_CHILD | WS_VISIBLE | WS_CLIPSIBLINGS`.
- **Extended Styles**: `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` (prevents appearing in Alt-Tab and prevents focus stealing).
- **Z-Order Placement**: Pinned to `HWND_BOTTOM` behind desktop shortcuts.
- **Focus Preservation**: Responds to `WM_MOUSEACTIVATE` with `MA_NOACTIVATE` and `WM_SETFOCUS` with `0`, ensuring desktop mouse clicks never steal keyboard focus from active user applications.
- **Flicker-Free Rendering**: Handles `WM_ERASEBKGND` with `1`, rendering background color and monitor telemetry via GDI in `WM_PAINT`.
- **Cleanup on Drop**: Invokes `DestroyWindow`, deletes allocated GDI brushes, and invalidates the host rect to ensure clean redrawing.

### 4. Multi-Monitor Orchestration (`manager.rs`)

- **Screen-to-Client Mapping**: Maps virtual monitor coordinates $(x, y)$ to host-relative client coordinates:
  $$\text{client}_x = \text{monitor}_x - \text{host}_{\text{left}}$$
  $$\text{client}_y = \text{monitor}_y - \text{host}_{\text{top}}$$
  Correctly handles negative coordinates, non-standard monitor layouts, and disparate display resolutions.
- **Dynamic Reconfiguration**: The daemon message loop periodically calls `WallpaperManager::refresh()` to detect display topology changes, resizing existing windows and attaching new ones without daemon restarts.
- **Explorer Crash / Restart Recovery**: `refresh()` detects when `host.is_valid()` returns false (e.g. `explorer.exe` killed or restarted), automatically destroying stale handles, re-discovering the newly spawned `WorkerW`, and restoring wallpaper windows.

---

## Manual Windows Integration Test Instructions

To verify desktop attachment manually on Windows 10 or 11:

1. **Standard Launch**:
   ```powershell
   cargo run -p wallpaper-daemon
   ```
2. **Visual Inspection**:
   - Confirm that each connected display renders a clean solid background with monitor diagnostics text (e.g. `Monitor: \\.\DISPLAY1 (2048 x 1152)`).
   - Confirm that desktop icons remain visible and clickable on top of the wallpaper window.
   - Confirm that clicking on desktop icons or the wallpaper does not cause windows to flicker or steal keyboard focus from running applications.
   - Confirm that the Windows Taskbar remains fully functional and visible in front of the wallpaper.
3. **Multi-Monitor Verification**:
   - Drag a window across multiple monitors to ensure borders and scaling match monitor boundaries.
   - In Windows Display Settings, toggle display resolution or reposition a display to verify automatic geometry synchronization.
4. **Explorer Restart Test**:
   - Open Task Manager, locate `Windows Explorer`, and click **Restart** (or run `Stop-Process -Name explorer -Force`).
   - Observe that the daemon logs detection of the invalid host, runs discovery, and re-attaches cleanly.
5. **Clean Shutdown**:
   - Press `Ctrl+C` in the daemon terminal.
   - Confirm that the daemon exits cleanly and the desktop returns to the default Windows background without orphaned windows.
