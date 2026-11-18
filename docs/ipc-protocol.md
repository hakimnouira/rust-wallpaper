# Daemon IPC Protocol (v1)

## Transport

- Windows named pipe: `\\.\pipe\rust-wallpaper-ipc`
- Duplex byte stream with one UTF-8 JSON envelope per line.
- Maximum JSON frame: 1 MiB, excluding the terminating LF.
- The server rejects remote pipe clients and uses a bounded 64-request handoff queue.
- A response is correlated to its request by UUID and carries the same protocol version.
- The daemon is authoritative. The pipe worker never mutates runtime objects; it forwards requests to the daemon's main loop.

The server creates the pipe with a per-user SDDL and rejects clients from another
Windows session or user identity. Redirector/remote clients are rejected as well.
Client impersonation is used only for that identity check and is reverted before
command dispatch; commands still run through the daemon's main loop.

## Request envelope

```json
{
  "protocol_version": 1,
  "request_id": "a UUID",
  "command": "ping"
}
```

Commands with fields use the tagged JSON representation, for example:

```json
{
  "protocol_version": 1,
  "request_id": "a UUID",
  "command": "apply_wallpaper",
  "payload": {
    "wallpaper_id": "a wallpaper UUID",
    "monitor": "\\\\.\\DISPLAY1"
  }
}
```

## Implemented commands

| Command | Behavior |
| --- | --- |
| `ping` | Returns `pong`. |
| `list_monitors` | Returns connected monitor IDs, device names, geometry, and primary flag. |
| `list_wallpapers` | Returns the daemon's library entries. |
| `import_wallpaper` | Imports a supported local video path and optional display title. |
| `remove_wallpaper` | Removes a wallpaper record from the library by ID, provided no runtime assignment references its source. |
| `set_profile` / `get_profile` | Updates or reads the persisted Eco, Balanced or Performance runtime profile. |
| `get_playback_settings` / `set_playback_settings(PlaybackSettings)` | Reads or updates persistent playback automation: fullscreen pause mode (`never`, `exclusive_only`, `fullscreen`, `maximized_and_fullscreen`) and the lock / display-off / suspend pause flags. Overrides survive profile changes and daemon restarts. |
| `set_performance_settings` / `get_performance_settings` | Updates or reads the persisted FPS limit, RAM budget and scaling filter. Changes are applied to all active assignments. |
| `apply_wallpaper` | Applies a library video by ID to one monitor or all connected monitors. Web/scene entries are not playable yet. |
| `unassign_wallpaper` | Clears a monitor's user assignment and restores the runtime's default assignment policy. |
| `pause` / `resume` | Adds/removes the user-pause reason for one monitor or globally. |
| `set_volume` | Sets volume from 0 through 100 on one monitor or all active players. |
| `set_mute` | Sets mute state on one monitor or all active players. |
| `get_status` | Returns active player state, decoder, memory use, and suspension reasons. |
| `quit` | Acknowledges the request, then stops the daemon supervision loop and releases all wallpaper players. The desktop UI uses this for its tray-menu Quit action. |

`stop` is defined in the contract but deliberately returns `unsupported` until its behavior is reconciled with persistent monitor assignments and automatic recovery.

## Response envelope

Successful and error responses use:

```json
{
  "protocol_version": 1,
  "request_id": "the same UUID",
  "result": "pong"
}
```

Errors use a stable code plus a human-readable message. Version mismatches are rejected before command dispatch.

## Client behavior

The shared Windows client provides:
- connect to the daemon pipe;
- bounded exponential retry for initial connection;
- request/response correlation and protocol-version validation;
- bounded response frame reading.

The desktop UI should own a background connection actor, maintain a disconnected state when requests fail, reconnect with backoff, then refresh monitors and library state. It should not issue IPC calls directly from the immediate-mode UI render callback.

## Known follow-up work

- Add request cancellation for long-running metadata/download work.
- Add protocol-level integration tests on Windows for connection, disconnect, malformed frames, concurrent clients, and daemon restart.
- Define and implement stop semantics.
- Add library metadata mutation (tags/favorites) and per-wallpaper playback configuration commands.
- Add a Windows client/server smoke-test executable or test harness.
