use anyhow::Context;
use tracing::{info, warn};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    info!("Rust Wallpaper daemon starting");
    info!("Protocol/workspace scaffold initialized");

    match wallpaper_windows::enumerate_monitors() {
        Ok(monitors) => {
            info!(count = monitors.len(), "Detected display monitors");
            for monitor in monitors {
                info!(
                    device = %monitor.device_name,
                    x = monitor.x,
                    y = monitor.y,
                    width = monitor.width,
                    height = monitor.height,
                    primary = monitor.is_primary,
                    "Monitor"
                );
            }
        }
        Err(error) => {
            #[cfg(windows)]
            return Err(error).context("failed to enumerate Windows monitors");

            #[cfg(not(windows))]
            warn!(%error, "Native monitor discovery is unavailable on this platform");
        }
    }

    info!("No wallpaper player is started in this milestone.");
    Ok(())
}
