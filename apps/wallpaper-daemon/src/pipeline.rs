//! Background processing worker and pipeline for video metadata and thumbnail extraction.

use crate::metadata::extract_video_metadata_and_thumbnail;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::thread;
use tracing::{error, info, warn};
use wallpaper_core::{ProcessingStatus, WallpaperId, WallpaperLibrary};

/// A job submitted to the background extraction queue.
#[derive(Debug, Clone)]
pub struct ExtractionJob {
    pub wallpaper_id: WallpaperId,
    pub source: PathBuf,
}

/// Scans the thumbnail directory and deletes any thumbnail file whose ID does not belong to an active wallpaper.
pub fn cleanup_orphaned_thumbnails(library: &WallpaperLibrary, thumb_dir: &Path) {
    if !thumb_dir.is_dir() {
        return;
    }
    let existing_wallpapers = library.list().unwrap_or_default();
    let known_ids: std::collections::HashSet<String> = existing_wallpapers
        .iter()
        .map(|w| w.id.to_string())
        .collect();

    if let Ok(entries) = std::fs::read_dir(thumb_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.eq_ignore_ascii_case("jpg"))
                    .unwrap_or(false)
            {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if !known_ids.contains(stem) {
                        info!(thumbnail = %path.display(), "Pruning orphaned thumbnail during startup cleanup");
                        let _ = std::fs::remove_file(&path);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::Arc;
    use std::sync::Barrier;
    use std::sync::Mutex;

    #[derive(Clone)]
    pub struct TestSync {
        pub extraction_done: Arc<Barrier>,
        pub delete_done: Arc<Barrier>,
    }

    static TEST_SYNC: Mutex<Option<TestSync>> = Mutex::new(None);

    pub fn set_test_sync(sync: Option<TestSync>) {
        *TEST_SYNC.lock().unwrap() = sync;
    }

    pub fn wait_test_sync() {
        let opt = TEST_SYNC.lock().unwrap().clone();
        if let Some(sync) = opt {
            sync.extraction_done.wait();
            sync.delete_done.wait();
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SubmitError {
    #[error("metadata processing queue is full")]
    QueueFull,
    #[error("metadata processing worker is unavailable")]
    Disconnected,
}

pub struct ExtractionPipeline {
    sender: SyncSender<ExtractionJob>,
    thumbnail_dir: PathBuf,
}

impl ExtractionPipeline {
    /// Starts the background extraction worker thread.
    pub fn start(library: WallpaperLibrary, thumbnail_dir: PathBuf) -> Self {
        let (sender, receiver) = sync_channel::<ExtractionJob>(128);

        let worker_lib = library.clone();
        let worker_dir = thumbnail_dir.clone();

        thread::Builder::new()
            .name("thumbnail-pipeline-worker".into())
            .spawn(move || {
                Self::worker_loop(receiver, worker_lib, worker_dir);
            })
            .expect("failed to spawn thumbnail worker thread");

        Self {
            sender,
            thumbnail_dir,
        }
    }

    #[cfg(test)]
    fn with_sender(sender: SyncSender<ExtractionJob>) -> Self {
        Self {
            sender,
            thumbnail_dir: PathBuf::new(),
        }
    }

    /// Submits a wallpaper for background processing.
    pub fn submit(&self, wallpaper_id: WallpaperId, source: PathBuf) -> Result<(), SubmitError> {
        match self.sender.try_send(ExtractionJob {
            wallpaper_id,
            source,
        }) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(SubmitError::QueueFull),
            Err(TrySendError::Disconnected(_)) => Err(SubmitError::Disconnected),
        }
    }

    /// Path where the thumbnail for a wallpaper is saved.
    pub fn thumbnail_path(&self, wallpaper_id: WallpaperId) -> PathBuf {
        self.thumbnail_dir.join(format!("{}.jpg", wallpaper_id))
    }

    fn worker_loop(
        receiver: Receiver<ExtractionJob>,
        library: WallpaperLibrary,
        thumbnail_dir: PathBuf,
    ) {
        info!("Thumbnail and metadata pipeline worker thread started");

        while let Ok(job) = receiver.recv() {
            let wallpaper_id = job.wallpaper_id;
            let source_path = job.source;
            let thumb_path = thumbnail_dir.join(format!("{}.jpg", wallpaper_id));

            // Verify wallpaper still exists before launching expensive extraction
            match library.get(wallpaper_id) {
                Ok(Some(_)) => {}
                _ => {
                    info!(%wallpaper_id, "Wallpaper was deleted before extraction started; skipping job");
                    continue;
                }
            }

            // Set state to Processing
            if let Err(e) =
                library.set_processing_state(wallpaper_id, ProcessingStatus::Processing, None)
            {
                warn!(%wallpaper_id, %e, "Failed to update processing state to Processing");
            }

            info!(%wallpaper_id, source = %source_path.display(), "Extracting metadata and thumbnail");

            match extract_video_metadata_and_thumbnail(&source_path, &thumb_path) {
                Ok(meta) => {
                    #[cfg(test)]
                    test_hooks::wait_test_sync();

                    if let Ok(Some(existing)) = library.get(wallpaper_id) {
                        let mut updated_meta = existing.metadata;
                        updated_meta.duration_secs = meta.duration_secs;
                        updated_meta.width = meta.width;
                        updated_meta.height = meta.height;
                        updated_meta.aspect_ratio = meta.aspect_ratio;
                        updated_meta.fps = meta.fps;
                        updated_meta.codec = meta.codec;
                        updated_meta.file_size = meta.file_size;
                        updated_meta.resolution_category = meta.resolution_category;
                        updated_meta.thumbnail_path = Some(meta.thumbnail_path);
                        updated_meta.source_mtime = meta.source_mtime;
                        updated_meta.processing_state = ProcessingStatus::Completed;
                        updated_meta.processing_error = None;

                        match library.update_metadata(wallpaper_id, &updated_meta) {
                            Ok(true) => {
                                info!(
                                    %wallpaper_id,
                                    width = ?updated_meta.width,
                                    height = ?updated_meta.height,
                                    resolution = ?updated_meta.resolution_category,
                                    duration = ?updated_meta.duration_secs,
                                    "Metadata and thumbnail extraction completed successfully"
                                );
                            }
                            Ok(false) => {
                                // Wallpaper was deleted concurrently before metadata write!
                                info!(
                                    %wallpaper_id,
                                    "Wallpaper was deleted concurrently before metadata write; pruning orphan thumbnail"
                                );
                                if thumb_path.is_file() {
                                    let _ = std::fs::remove_file(&thumb_path);
                                }
                            }
                            Err(e) => {
                                error!(%wallpaper_id, %e, "Failed to persist extracted metadata to database");
                                if thumb_path.is_file() {
                                    let _ = std::fs::remove_file(&thumb_path);
                                }
                            }
                        }
                    } else {
                        // Wallpaper was deleted while extraction was running.
                        // Prune the newly written thumbnail immediately to prevent orphaned files.
                        info!(%wallpaper_id, "Wallpaper was deleted during extraction; pruning orphan thumbnail");
                        if thumb_path.is_file() {
                            let _ = std::fs::remove_file(&thumb_path);
                        }
                    }
                }
                Err(err_msg) => {
                    warn!(
                        %wallpaper_id,
                        error = %err_msg,
                        "Metadata or thumbnail extraction failed"
                    );
                    // If file was deleted, no need to update state
                    if let Ok(Some(_)) = library.get(wallpaper_id) {
                        if let Err(e) = library.set_processing_state(
                            wallpaper_id,
                            ProcessingStatus::Failed,
                            Some(&err_msg),
                        ) {
                            error!(%wallpaper_id, %e, "Failed to persist failure state to database");
                        }
                    }
                    if thumb_path.is_file() {
                        let _ = std::fs::remove_file(&thumb_path);
                    }
                }
            }
        }

        info!("Thumbnail and metadata pipeline worker thread exiting");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;
    use std::time::{Duration, Instant};

    #[test]
    fn submit_reports_full_queue() {
        let (sender, _receiver) = sync_channel(0);
        let pipeline = ExtractionPipeline::with_sender(sender);
        assert_eq!(
            pipeline.submit(WallpaperId::new(), PathBuf::from("pending.mp4")),
            Err(SubmitError::QueueFull)
        );
    }

    #[test]
    fn submit_reports_disconnected_worker() {
        let (sender, receiver) = sync_channel(1);
        drop(receiver);
        let pipeline = ExtractionPipeline::with_sender(sender);
        assert_eq!(
            pipeline.submit(WallpaperId::new(), PathBuf::from("pending.mp4")),
            Err(SubmitError::Disconnected)
        );
    }

    #[test]
    fn test_pipeline_end_to_end_valid_video() {
        let video_path = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !video_path.exists() {
            return;
        }

        let temp_dir = std::env::temp_dir().join(format!("test_pipe_{}", uuid::Uuid::new_v4()));
        let thumb_dir = temp_dir.join("thumbnails");
        std::fs::create_dir_all(&thumb_dir).unwrap();

        let library = WallpaperLibrary::in_memory().unwrap();
        let pipeline = ExtractionPipeline::start(library.clone(), thumb_dir.clone());

        let wp = library
            .import_video(&video_path, Some("Nature Video"), None)
            .unwrap();
        assert_eq!(wp.metadata.processing_state, ProcessingStatus::Pending);

        pipeline.submit(wp.id, video_path).unwrap();

        // Wait for background worker up to 5s
        let start = Instant::now();
        let mut completed = false;
        while start.elapsed() < Duration::from_secs(5) {
            if let Ok(Some(item)) = library.get(wp.id) {
                if item.metadata.processing_state == ProcessingStatus::Completed {
                    assert_eq!(item.metadata.width, Some(1920));
                    assert_eq!(item.metadata.height, Some(1080));
                    assert_eq!(
                        item.metadata.resolution_category.as_deref(),
                        Some("1080p FHD")
                    );
                    assert!(item.metadata.thumbnail_path.is_some());
                    let thumb_file = PathBuf::from(item.metadata.thumbnail_path.unwrap());
                    assert!(thumb_file.is_file());
                    assert!(std::fs::metadata(&thumb_file).unwrap().len() > 0);
                    completed = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(completed, "Pipeline did not complete extraction in time");
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_pipeline_corrupted_file_marks_failed() {
        let temp_dir = std::env::temp_dir().join(format!("test_corrupt_{}", uuid::Uuid::new_v4()));
        let thumb_dir = temp_dir.join("thumbnails");
        std::fs::create_dir_all(&thumb_dir).unwrap();

        let corrupt_video = temp_dir.join("corrupt.mp4");
        std::fs::write(&corrupt_video, b"NOT A VALID MP4 FILE HEADER").unwrap();

        let library = WallpaperLibrary::in_memory().unwrap();
        let pipeline = ExtractionPipeline::start(library.clone(), thumb_dir.clone());

        let wp = library
            .import_video(&corrupt_video, Some("Corrupt Video"), None)
            .unwrap();
        pipeline.submit(wp.id, corrupt_video).unwrap();

        let start = Instant::now();
        let mut failed = false;
        while start.elapsed() < Duration::from_secs(15) {
            if let Ok(Some(item)) = library.get(wp.id) {
                if item.metadata.processing_state == ProcessingStatus::Failed {
                    assert!(item.metadata.processing_error.is_some());
                    failed = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(failed, "Pipeline did not mark corrupt file as Failed");
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_pipeline_restart_recovery() {
        let video_path = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !video_path.exists() {
            return;
        }

        let temp_dir = std::env::temp_dir().join(format!("test_recovery_{}", uuid::Uuid::new_v4()));
        let thumb_dir = temp_dir.join("thumbnails");
        std::fs::create_dir_all(&thumb_dir).unwrap();

        let library = WallpaperLibrary::in_memory().unwrap();
        let wp = library
            .import_video(&video_path, Some("Nature Video"), None)
            .unwrap();

        // Simulate daemon start: create pipeline and replay recovery
        let pipeline = ExtractionPipeline::start(library.clone(), thumb_dir.clone());
        for item in library.list().unwrap() {
            if item.metadata.is_processing()
                || item
                    .metadata
                    .thumbnail_path
                    .as_ref()
                    .map_or(true, |p| !std::path::Path::new(p).exists())
            {
                pipeline
                    .submit(item.id, PathBuf::from(&item.metadata.source))
                    .unwrap();
            }
        }

        let start = Instant::now();
        let mut recovered = false;
        while start.elapsed() < Duration::from_secs(5) {
            if let Ok(Some(item)) = library.get(wp.id) {
                if item.metadata.processing_state == ProcessingStatus::Completed {
                    recovered = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        assert!(recovered, "Startup recovery did not complete extraction");
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_concurrent_deletion_during_extraction_prunes_thumbnail() {
        let video_path = PathBuf::from("assets/wallpapers/sample-nature.mp4");
        if !video_path.exists() {
            return;
        }

        let temp_dir = std::env::temp_dir().join(format!("test_del_{}", uuid::Uuid::new_v4()));
        let thumb_dir = temp_dir.join("thumbnails");
        std::fs::create_dir_all(&thumb_dir).unwrap();

        let library = WallpaperLibrary::in_memory().unwrap();

        // Setup synchronization barriers
        let sync = test_hooks::TestSync {
            extraction_done: std::sync::Arc::new(std::sync::Barrier::new(2)),
            delete_done: std::sync::Arc::new(std::sync::Barrier::new(2)),
        };
        test_hooks::set_test_sync(Some(sync.clone()));

        let pipeline = ExtractionPipeline::start(library.clone(), thumb_dir.clone());

        let wp = library
            .import_video(&video_path, Some("To Be Deleted"), None)
            .unwrap();
        let expected_thumb = thumb_dir.join(format!("{}.jpg", wp.id));

        // Submit extraction job
        pipeline.submit(wp.id, video_path).unwrap();

        // Wait until thumbnail is generated on disk and worker is paused right before metadata write
        sync.extraction_done.wait();

        // Assert thumbnail is currently present on disk
        assert!(
            expected_thumb.is_file(),
            "Thumbnail must have been extracted and written before database update"
        );

        // Delete the wallpaper concurrently from the library
        let deleted = library.delete(wp.id).unwrap();
        assert!(deleted, "Wallpaper must be deleted");

        // Allow worker to proceed with atomic update
        sync.delete_done.wait();

        // Wait for worker to finish processing and prune the thumbnail
        let start = Instant::now();
        let mut pruned = false;
        while start.elapsed() < Duration::from_secs(3) {
            if !expected_thumb.exists() {
                pruned = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        test_hooks::set_test_sync(None);
        assert!(
            pruned,
            "Orphaned thumbnail should have been atomically pruned when wallpaper was deleted"
        );
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
