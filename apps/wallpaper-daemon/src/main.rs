#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> anyhow::Result<()> {
    // The guard lives in `run_daemon` so standalone, `RustWallpaper.exe --daemon`
    // and embedded daemon launches all share the same single-instance behavior.
    wallpaper_daemon::run_daemon()
}
