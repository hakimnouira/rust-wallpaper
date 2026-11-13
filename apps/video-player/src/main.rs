use anyhow::{bail, Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::info;
use wallpaper_core::{HardwareAcceleration, PlaybackConfig};
use wallpaper_windows::{MpvLaunchConfig, MpvPlayer};

fn print_usage() {
    println!(
        r#"
Usage: video-player [OPTIONS] --source <PATH> --wid <HWND>

Options:
  --source <PATH>      Path to the video file (.mp4, .webm, etc.)
  --wid <HWND>         Native Windows HWND to embed playback into (hex or decimal)
  --hwdec <MODE>       Hardware acceleration: auto, d3d11va, dxva2, no (default: auto)
  --loop               Enable loop playback (default: enabled)
  --no-loop            Disable loop playback
  --mute               Start in muted state
  --unmute             Start with audio enabled (default)
  --volume <0-100>     Set initial volume percentage (default: 100)
  --duration <SECS>    Run for N seconds then exit cleanly (useful for automated testing)
  --help               Display this help message
"#
    );
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();
    if args.contains(&"--help".to_string()) || args.len() < 2 {
        print_usage();
        return Ok(());
    }

    let mut source: Option<PathBuf> = None;
    let mut wid: Option<isize> = None;
    let mut hwdec = HardwareAcceleration::Auto;
    let mut loop_playback = true;
    let mut mute = false;
    let mut volume = 100u32;
    let mut duration_limit: Option<Duration> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--source" => {
                i += 1;
                if i < args.len() {
                    source = Some(PathBuf::from(&args[i]));
                }
            }
            "--wid" => {
                i += 1;
                if i < args.len() {
                    let s = &args[i];
                    let val = if s.starts_with("0x") || s.starts_with("0X") {
                        isize::from_str_radix(&s[2..], 16)
                            .context("invalid hexadecimal HWND format")?
                    } else {
                        s.parse::<isize>().context("invalid integer HWND format")?
                    };
                    wid = Some(val);
                }
            }
            "--hwdec" => {
                i += 1;
                if i < args.len() {
                    hwdec = match args[i].to_lowercase().as_str() {
                        "d3d11va" => HardwareAcceleration::D3d11va,
                        "dxva2" => HardwareAcceleration::Dxva2,
                        "no" | "disabled" => HardwareAcceleration::Disabled,
                        _ => HardwareAcceleration::Auto,
                    };
                }
            }
            "--loop" => loop_playback = true,
            "--no-loop" => loop_playback = false,
            "--mute" => mute = true,
            "--unmute" => mute = false,
            "--volume" => {
                i += 1;
                if i < args.len() {
                    volume = args[i].parse::<u32>().unwrap_or(100).min(100);
                }
            }
            "--duration" => {
                i += 1;
                if i < args.len() {
                    if let Ok(secs) = args[i].parse::<u64>() {
                        duration_limit = Some(Duration::from_secs(secs));
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }

    let video_path = match source {
        Some(p) => p,
        None => {
            bail!("missing required parameter: --source <PATH>");
        }
    };

    if !video_path.exists() {
        bail!("video file does not exist: {}", video_path.display());
    }

    let target_wid = match wid {
        Some(w) => w,
        None => {
            bail!("missing required parameter: --wid <HWND>");
        }
    };

    info!(
        video = %video_path.display(),
        wid = format_args!("0x{:X}", target_wid),
        hwdec = ?hwdec,
        loop_playback,
        mute,
        volume,
        "Starting standalone video player application"
    );

    let launch_cfg = MpvLaunchConfig {
        video_path,
        wid: target_wid,
        config: PlaybackConfig {
            loop_playback,
            mute,
            volume,
            hardware_acceleration: hwdec,
        },
    };

    let mut player = MpvPlayer::launch(launch_cfg).context("failed to launch embedded MPV")?;

    info!(
        active_decoder = %player.active_decoder(),
        state = ?player.state(),
        memory_bytes = player.get_memory_bytes(),
        "Video player running and embedded"
    );

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc_handler(move || {
        r.store(false, Ordering::SeqCst);
    });

    let start_time = Instant::now();
    let mut last_metrics = Instant::now();

    while running.load(Ordering::SeqCst) && player.is_alive() {
        if last_metrics.elapsed() >= Duration::from_secs(2) {
            info!(
                active_decoder = %player.active_decoder(),
                memory_mb = player.get_memory_bytes() / (1024 * 1024),
                "Player telemetry"
            );
            last_metrics = Instant::now();
        }

        if let Some(limit) = duration_limit {
            if start_time.elapsed() >= limit {
                info!("Scheduled duration limit reached. Stopping video player");
                break;
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    info!("Shutting down video player");
    player.stop()?;
    info!("Video player shutdown complete");

    Ok(())
}

fn ctrlc_handler<F: Fn() + Send + 'static>(handler: F) {
    #[cfg(windows)]
    {
        use std::sync::Mutex;
        static HANDLER: Mutex<Option<Box<dyn Fn() + Send>>> = Mutex::new(None);
        *HANDLER.lock().unwrap() = Some(Box::new(handler));

        unsafe extern "system" fn win_ctrl(_: u32) -> windows::Win32::Foundation::BOOL {
            if let Some(ref h) = *HANDLER.lock().unwrap() {
                h();
            }
            windows::Win32::Foundation::BOOL(1)
        }

        unsafe {
            let _ = windows::Win32::System::Console::SetConsoleCtrlHandler(Some(win_ctrl), true);
        }
    }
}
