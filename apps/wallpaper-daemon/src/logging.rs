//! Persistent file logging shared by the daemon and desktop UI.
//!
//! Installed builds run detached, so stderr is not visible to users. Logs are
//! written under the per-user application data directory and rotate daily.

use std::path::PathBuf;

use tracing_appender::rolling::Rotation;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Per-user application data directory used for the library and settings.
pub fn data_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .map(|path| path.join("RustWallpaper"))
}

/// Directory containing daemon and UI logs. The directory may not exist until
/// logging is initialized.
pub fn logs_dir() -> Option<PathBuf> {
    data_dir().map(|path| path.join("logs"))
}

/// Initializes daily file logging for `component` (`daemon` or `ui`).
///
/// Console logging is kept for development and terminal launches. If the log
/// directory cannot be created the subscriber still initializes on stderr, so
/// startup never fails solely because logging is unavailable.
pub fn init(component: &str, console: bool) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = logs_dir().and_then(|directory| {
        std::fs::create_dir_all(&directory).ok()?;
        let appender = tracing_appender::rolling::Builder::new()
            .rotation(Rotation::DAILY)
            .filename_prefix(component)
            .max_log_files(14)
            .build(directory)
            .ok()?;
        Some(fmt::layer().with_writer(appender).with_ansi(false))
    });

    match (file_layer, console) {
        (Some(file), true) => {
            let console = fmt::layer().with_writer(std::io::stderr);
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(file)
                .with(console)
                .try_init();
        }
        (Some(file), false) => {
            let _ = tracing_subscriber::registry()
                .with(filter)
                .with(file)
                .try_init();
        }
        (None, true) => {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
        (None, false) => {}
    }
}
