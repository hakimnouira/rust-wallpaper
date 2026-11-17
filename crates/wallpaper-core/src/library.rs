//! SQLite persistence for wallpaper metadata and library management.

use crate::{
    HardwareAcceleration, PlaybackConfig, ProcessingStatus, Wallpaper, WallpaperId, WallpaperKind,
    WallpaperMetadata,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("invalid wallpaper ID: {0}")]
    InvalidId(String),
    #[error("unsupported file format or extension: {0}")]
    UnsupportedFormat(String),
    #[error("file not found: {0}")]
    FileNotFound(String),
}

/// SQLite-backed persistent library for wallpapers.
#[derive(Clone)]
pub struct WallpaperLibrary {
    conn: Arc<Mutex<Connection>>,
}

impl WallpaperLibrary {
    /// Opens or creates a SQLite database at the specified path.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LibraryError> {
        let conn = Connection::open(path)?;
        let library = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        library.init_schema()?;
        Ok(library)
    }

    /// Creates an in-memory SQLite database (useful for unit tests).
    pub fn in_memory() -> Result<Self, LibraryError> {
        let conn = Connection::open_in_memory()?;
        let library = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        library.init_schema()?;
        Ok(library)
    }

    fn init_schema(&self) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS wallpapers (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                source TEXT NOT NULL,
                kind TEXT NOT NULL,
                duration_secs REAL,
                width INTEGER,
                height INTEGER,
                aspect_ratio REAL,
                fps REAL,
                codec TEXT,
                file_size INTEGER,
                resolution_category TEXT,
                processing_state TEXT NOT NULL DEFAULT 'completed',
                processing_error TEXT,
                thumbnail_path TEXT,
                source_mtime INTEGER,
                loop_playback INTEGER NOT NULL DEFAULT 1,
                mute INTEGER NOT NULL DEFAULT 1,
                volume INTEGER NOT NULL DEFAULT 100,
                hwdec TEXT NOT NULL DEFAULT 'auto',
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS engine_settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS monitor_assignments (
                device_name TEXT PRIMARY KEY,
                wallpaper_id TEXT NOT NULL,
                assigned_at TEXT NOT NULL DEFAULT (datetime('now')),
                FOREIGN KEY(wallpaper_id) REFERENCES wallpapers(id) ON DELETE CASCADE
            );
            "#,
        )?;

        // Non-destructive schema migration for pre-existing databases
        let mut pragma_stmt = conn.prepare("PRAGMA table_info(wallpapers);")?;
        let columns: Vec<String> = pragma_stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .collect();

        let migrations = [
            ("aspect_ratio", "ALTER TABLE wallpapers ADD COLUMN aspect_ratio REAL;"),
            ("fps", "ALTER TABLE wallpapers ADD COLUMN fps REAL;"),
            ("codec", "ALTER TABLE wallpapers ADD COLUMN codec TEXT;"),
            ("file_size", "ALTER TABLE wallpapers ADD COLUMN file_size INTEGER;"),
            ("resolution_category", "ALTER TABLE wallpapers ADD COLUMN resolution_category TEXT;"),
            (
                "processing_state",
                "ALTER TABLE wallpapers ADD COLUMN processing_state TEXT NOT NULL DEFAULT 'completed';",
            ),
            ("processing_error", "ALTER TABLE wallpapers ADD COLUMN processing_error TEXT;"),
            ("source_mtime", "ALTER TABLE wallpapers ADD COLUMN source_mtime INTEGER;"),
        ];

        for (col_name, sql) in migrations {
            if !columns.iter().any(|c| c.eq_ignore_ascii_case(col_name)) {
                conn.execute(sql, [])?;
            }
        }

        Ok(())
    }

    /// Sets a persistent configuration key-value pair.
    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO engine_settings (key, value, updated_at)
            VALUES (?1, ?2, datetime('now'))
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = datetime('now')
            "#,
            params![key, value],
        )?;
        Ok(())
    }

    /// Retrieves a persistent configuration setting by key.
    pub fn get_setting(&self, key: &str) -> Result<Option<String>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT value FROM engine_settings WHERE key = ?1")?;
        let result = stmt.query_row(params![key], |row| row.get(0)).optional()?;
        Ok(result)
    }

    /// Assigns a wallpaper to a specific monitor display device.
    pub fn set_monitor_assignment(
        &self,
        device_name: &str,
        wallpaper_id: WallpaperId,
    ) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO monitor_assignments (device_name, wallpaper_id, assigned_at)
            VALUES (?1, ?2, datetime('now'))
            ON CONFLICT(device_name) DO UPDATE SET
                wallpaper_id = excluded.wallpaper_id,
                assigned_at = datetime('now')
            "#,
            params![device_name, wallpaper_id.to_string()],
        )?;
        Ok(())
    }

    /// Retrieves the wallpaper assigned to a specific monitor device.
    pub fn get_monitor_assignment(
        &self,
        device_name: &str,
    ) -> Result<Option<WallpaperId>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT wallpaper_id FROM monitor_assignments WHERE device_name = ?1")?;
        let result: Option<String> = stmt
            .query_row(params![device_name], |row| row.get(0))
            .optional()?;
        match result {
            Some(id_str) => {
                let id = id_str.parse::<WallpaperId>().map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
                    )
                })?;
                Ok(Some(id))
            }
            None => Ok(None),
        }
    }

    /// Inserts or updates a wallpaper in the library.
    pub fn insert(&self, wp: &Wallpaper) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        let hwdec_str = match wp.config.hardware_acceleration {
            HardwareAcceleration::Auto => "auto",
            HardwareAcceleration::D3d11va => "d3d11va",
            HardwareAcceleration::Dxva2 => "dxva2",
            HardwareAcceleration::Disabled => "no",
        };

        let state_str = match wp.metadata.processing_state {
            ProcessingStatus::Pending => "pending",
            ProcessingStatus::Processing => "processing",
            ProcessingStatus::Completed => "completed",
            ProcessingStatus::Failed => "failed",
        };

        conn.execute(
            r#"
            INSERT INTO wallpapers (
                id, title, source, kind, duration_secs, width, height, aspect_ratio, fps, codec,
                file_size, resolution_category, processing_state, processing_error, thumbnail_path,
                source_mtime, loop_playback, mute, volume, hwdec
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                source = excluded.source,
                kind = excluded.kind,
                duration_secs = excluded.duration_secs,
                width = excluded.width,
                height = excluded.height,
                aspect_ratio = excluded.aspect_ratio,
                fps = excluded.fps,
                codec = excluded.codec,
                file_size = excluded.file_size,
                resolution_category = excluded.resolution_category,
                processing_state = excluded.processing_state,
                processing_error = excluded.processing_error,
                thumbnail_path = excluded.thumbnail_path,
                source_mtime = excluded.source_mtime,
                loop_playback = excluded.loop_playback,
                mute = excluded.mute,
                volume = excluded.volume,
                hwdec = excluded.hwdec;
            "#,
            params![
                wp.id.to_string(),
                wp.title(),
                wp.source(),
                match wp.kind {
                    WallpaperKind::Video => "video",
                    WallpaperKind::Web => "web",
                    WallpaperKind::Scene => "scene",
                },
                wp.metadata.duration_secs,
                wp.metadata.width,
                wp.metadata.height,
                wp.metadata.aspect_ratio,
                wp.metadata.fps,
                wp.metadata.codec,
                wp.metadata.file_size,
                wp.metadata.resolution_category,
                state_str,
                wp.metadata.processing_error,
                wp.metadata.thumbnail_path,
                wp.metadata.source_mtime,
                if wp.config.loop_playback { 1 } else { 0 },
                if wp.config.mute { 1 } else { 0 },
                wp.config.volume,
                hwdec_str,
            ],
        )?;
        Ok(())
    }

    /// Retrieves a wallpaper by ID.
    pub fn get(&self, id: WallpaperId) -> Result<Option<Wallpaper>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, title, source, kind, duration_secs, width, height, aspect_ratio, fps, codec,
                   file_size, resolution_category, processing_state, processing_error, thumbnail_path,
                   source_mtime, loop_playback, mute, volume, hwdec
            FROM wallpapers WHERE id = ?1;
            "#,
        )?;

        let result = stmt
            .query_row(params![id.to_string()], Self::row_to_wallpaper)
            .optional()?;

        Ok(result)
    }

    /// Lists all wallpapers in the library.
    pub fn list(&self) -> Result<Vec<Wallpaper>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, title, source, kind, duration_secs, width, height, aspect_ratio, fps, codec,
                   file_size, resolution_category, processing_state, processing_error, thumbnail_path,
                   source_mtime, loop_playback, mute, volume, hwdec
            FROM wallpapers ORDER BY created_at DESC;
            "#,
        )?;

        let iter = stmt.query_map([], Self::row_to_wallpaper)?;
        let mut wallpapers = Vec::new();
        for item in iter {
            wallpapers.push(item?);
        }
        Ok(wallpapers)
    }

    /// Lists wallpapers that are currently in pending or processing state.
    pub fn list_pending_or_processing(&self) -> Result<Vec<Wallpaper>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, title, source, kind, duration_secs, width, height, aspect_ratio, fps, codec,
                   file_size, resolution_category, processing_state, processing_error, thumbnail_path,
                   source_mtime, loop_playback, mute, volume, hwdec
            FROM wallpapers
            WHERE processing_state IN ('pending', 'processing')
            ORDER BY created_at ASC;
            "#,
        )?;

        let iter = stmt.query_map([], Self::row_to_wallpaper)?;
        let mut wallpapers = Vec::new();
        for item in iter {
            wallpapers.push(item?);
        }
        Ok(wallpapers)
    }

    /// Updates metadata and processing state for a wallpaper.
    /// Returns `Ok(true)` if the wallpaper row was updated, or `Ok(false)` if the wallpaper
    /// was deleted concurrently.
    pub fn update_metadata(
        &self,
        id: WallpaperId,
        metadata: &WallpaperMetadata,
    ) -> Result<bool, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let state_str = match metadata.processing_state {
            ProcessingStatus::Pending => "pending",
            ProcessingStatus::Processing => "processing",
            ProcessingStatus::Completed => "completed",
            ProcessingStatus::Failed => "failed",
        };

        let affected = conn.execute(
            r#"
            UPDATE wallpapers SET
                title = ?2,
                source = ?3,
                duration_secs = ?4,
                width = ?5,
                height = ?6,
                aspect_ratio = ?7,
                fps = ?8,
                codec = ?9,
                file_size = ?10,
                resolution_category = ?11,
                processing_state = ?12,
                processing_error = ?13,
                thumbnail_path = ?14,
                source_mtime = ?15
            WHERE id = ?1;
            "#,
            params![
                id.to_string(),
                metadata.title,
                metadata.source,
                metadata.duration_secs,
                metadata.width,
                metadata.height,
                metadata.aspect_ratio,
                metadata.fps,
                metadata.codec,
                metadata.file_size,
                metadata.resolution_category,
                state_str,
                metadata.processing_error,
                metadata.thumbnail_path,
                metadata.source_mtime,
            ],
        )?;
        Ok(affected > 0)
    }

    /// Updates only the processing state and optional error message.
    /// Returns `Ok(true)` if the wallpaper row was updated, or `Ok(false)` if the wallpaper was deleted.
    pub fn set_processing_state(
        &self,
        id: WallpaperId,
        state: ProcessingStatus,
        error: Option<&str>,
    ) -> Result<bool, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let state_str = match state {
            ProcessingStatus::Pending => "pending",
            ProcessingStatus::Processing => "processing",
            ProcessingStatus::Completed => "completed",
            ProcessingStatus::Failed => "failed",
        };

        let affected = conn.execute(
            r#"
            UPDATE wallpapers SET
                processing_state = ?2,
                processing_error = ?3
            WHERE id = ?1;
            "#,
            params![id.to_string(), state_str, error],
        )?;
        Ok(affected > 0)
    }

    /// Deletes a wallpaper by ID.
    pub fn delete(&self, id: WallpaperId) -> Result<bool, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let affected = conn.execute(
            "DELETE FROM wallpapers WHERE id = ?1;",
            params![id.to_string()],
        )?;
        Ok(affected > 0)
    }

    /// Imports a video file into the library with metadata.
    pub fn import_video(
        &self,
        path: impl AsRef<Path>,
        custom_title: Option<&str>,
        config: Option<PlaybackConfig>,
    ) -> Result<Wallpaper, LibraryError> {
        let path_ref = path.as_ref();
        if !path_ref.exists() {
            return Err(LibraryError::FileNotFound(path_ref.display().to_string()));
        }

        let extension = path_ref
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if !matches!(
            extension.as_str(),
            "mp4" | "webm" | "mkv" | "mov" | "gif" | "jpg" | "jpeg" | "png" | "webp" | "bmp"
        ) {
            return Err(LibraryError::UnsupportedFormat(extension));
        }

        let title = custom_title.map(|t| t.to_string()).unwrap_or_else(|| {
            path_ref
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Untitled Video")
                .to_string()
        });

        let source = path_ref.to_string_lossy().to_string();
        let mut wp = Wallpaper::new(title, source, WallpaperKind::Video);
        if let Ok(meta) = std::fs::metadata(path_ref) {
            wp.metadata.file_size = Some(meta.len());
            if let Ok(mtime) = meta.modified() {
                if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    wp.metadata.source_mtime = Some(duration.as_secs());
                }
            }
        }
        if let Some(cfg) = config {
            wp.config = cfg;
        }

        self.insert(&wp)?;
        Ok(wp)
    }

    fn row_to_wallpaper(row: &rusqlite::Row) -> Result<Wallpaper, rusqlite::Error> {
        let id_str: String = row.get(0)?;
        let title: String = row.get(1)?;
        let source: String = row.get(2)?;
        let kind_str: String = row.get(3)?;
        let duration_secs: Option<f64> = row.get(4)?;
        let width: Option<u32> = row.get(5)?;
        let height: Option<u32> = row.get(6)?;
        let aspect_ratio: Option<f64> = row.get(7)?;
        let fps: Option<f64> = row.get(8)?;
        let codec: Option<String> = row.get(9)?;
        let file_size: Option<u64> = row.get(10)?;
        let resolution_category: Option<String> = row.get(11)?;
        let processing_state_str: Option<String> = row.get(12)?;
        let processing_error: Option<String> = row.get(13)?;
        let thumbnail_path: Option<String> = row.get(14)?;
        let source_mtime: Option<u64> = row.get(15)?;
        let loop_playback: i32 = row.get(16)?;
        let mute: i32 = row.get(17)?;
        let volume: u32 = row.get(18)?;
        let hwdec_str: String = row.get(19)?;

        let id = id_str.parse::<WallpaperId>().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            )
        })?;

        let kind = match kind_str.as_str() {
            "web" => WallpaperKind::Web,
            "scene" => WallpaperKind::Scene,
            _ => WallpaperKind::Video,
        };

        let processing_state = match processing_state_str.as_deref() {
            Some("pending") => ProcessingStatus::Pending,
            Some("processing") => ProcessingStatus::Processing,
            Some("failed") => ProcessingStatus::Failed,
            _ => ProcessingStatus::Completed,
        };

        let hwdec = match hwdec_str.as_str() {
            "d3d11va" => HardwareAcceleration::D3d11va,
            "dxva2" => HardwareAcceleration::Dxva2,
            "no" => HardwareAcceleration::Disabled,
            _ => HardwareAcceleration::Auto,
        };

        Ok(Wallpaper {
            id,
            kind,
            metadata: WallpaperMetadata {
                title,
                source,
                duration_secs,
                width,
                height,
                aspect_ratio,
                fps,
                codec,
                file_size,
                resolution_category,
                processing_state,
                processing_error,
                thumbnail_path,
                source_mtime,
            },
            config: PlaybackConfig {
                loop_playback: loop_playback != 0,
                mute: mute != 0,
                volume,
                hardware_acceleration: hwdec,
                ..Default::default()
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::categorize_resolution;

    #[test]
    fn library_crud_operations() {
        let library = WallpaperLibrary::in_memory().unwrap();
        let wp = Wallpaper::new("Nature Loop", "wallpapers/nature.mp4", WallpaperKind::Video);

        // Insert
        library.insert(&wp).unwrap();

        // Get
        let retrieved = library.get(wp.id).unwrap().expect("wallpaper must exist");
        assert_eq!(retrieved.title(), "Nature Loop");
        assert_eq!(retrieved.source(), "wallpapers/nature.mp4");
        assert_eq!(retrieved.kind, WallpaperKind::Video);
        assert!(retrieved.config.loop_playback);
        assert!(retrieved.config.mute);

        // List
        let list = library.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, wp.id);

        // Delete
        let deleted = library.delete(wp.id).unwrap();
        assert!(deleted);
        assert!(library.get(wp.id).unwrap().is_none());

        // Engine settings
        library
            .set_setting("performance_profile", "performance")
            .unwrap();
        let setting = library.get_setting("performance_profile").unwrap();
        assert_eq!(setting.as_deref(), Some("performance"));

        // Monitor assignment
        let wp2 = Wallpaper::new("Abstract", "wallpapers/abstract.webm", WallpaperKind::Video);
        library.insert(&wp2).unwrap();
        library
            .set_monitor_assignment(r"\\.\DISPLAY1", wp2.id)
            .unwrap();
        let assigned = library.get_monitor_assignment(r"\\.\DISPLAY1").unwrap();
        assert_eq!(assigned, Some(wp2.id));
    }

    #[test]
    fn test_open_existing_database_non_destructive_migration() {
        let temp_dir = std::env::temp_dir();
        let db_file = temp_dir.join(format!("test_wp_lib_{}.db", uuid::Uuid::new_v4()));

        // 1. Create v1 database with only the wallpapers table
        {
            let conn = rusqlite::Connection::open(&db_file).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE wallpapers (
                    id TEXT PRIMARY KEY,
                    title TEXT NOT NULL,
                    source TEXT NOT NULL,
                    kind TEXT NOT NULL,
                    duration_secs REAL,
                    width INTEGER,
                    height INTEGER,
                    thumbnail_path TEXT,
                    loop_playback INTEGER NOT NULL DEFAULT 1,
                    mute INTEGER NOT NULL DEFAULT 1,
                    volume INTEGER NOT NULL DEFAULT 100,
                    hwdec TEXT NOT NULL DEFAULT 'auto',
                    created_at TEXT NOT NULL DEFAULT (datetime('now'))
                );
                INSERT INTO wallpapers (id, title, source, kind)
                VALUES ('f47ac10b-58cc-4372-a567-0e02b2c3d479', 'Existing V1 Wallpaper', 'video.mp4', 'video');
                "#,
            ).unwrap();
        }

        // 2. Open via WallpaperLibrary which executes IF NOT EXISTS migrations
        let lib = WallpaperLibrary::open(&db_file).unwrap();

        // 3. Verify original data was preserved
        let id = "f47ac10b-58cc-4372-a567-0e02b2c3d479"
            .parse::<WallpaperId>()
            .unwrap();
        let existing = lib.get(id).unwrap().expect("v1 data must be preserved");
        assert_eq!(existing.title(), "Existing V1 Wallpaper");

        // 4. Verify new tables are usable without conflicts
        lib.set_setting("migrated", "true").unwrap();
        assert_eq!(
            lib.get_setting("migrated").unwrap().as_deref(),
            Some("true")
        );

        lib.set_monitor_assignment(r"\\.\DISPLAY2", id).unwrap();
        assert_eq!(
            lib.get_monitor_assignment(r"\\.\DISPLAY2").unwrap(),
            Some(id)
        );

        // Cleanup
        let _ = std::fs::remove_file(db_file);
    }

    #[test]
    fn test_metadata_updates_and_processing_states() {
        let library = WallpaperLibrary::in_memory().unwrap();
        let wp = Wallpaper::new("Metadata Test", "video.mp4", WallpaperKind::Video);
        library.insert(&wp).unwrap();

        // Initially Pending
        let pending = library.list_pending_or_processing().unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, wp.id);
        assert_eq!(
            pending[0].metadata.processing_state,
            ProcessingStatus::Pending
        );

        // Update to Processing
        library
            .set_processing_state(wp.id, ProcessingStatus::Processing, None)
            .unwrap();
        let processing = library.get(wp.id).unwrap().unwrap();
        assert_eq!(
            processing.metadata.processing_state,
            ProcessingStatus::Processing
        );

        // Update with full completed metadata
        let mut meta = processing.metadata.clone();
        meta.duration_secs = Some(15.5);
        meta.width = Some(3840);
        meta.height = Some(2160);
        meta.aspect_ratio = Some(16.0 / 9.0);
        meta.fps = Some(60.0);
        meta.codec = Some("hevc".into());
        meta.file_size = Some(1024 * 1024 * 25);
        meta.resolution_category = Some(categorize_resolution(3840, 2160).into());
        meta.thumbnail_path = Some("C:\\AppData\\thumbnails\\test.jpg".into());
        meta.processing_state = ProcessingStatus::Completed;

        let updated = library.update_metadata(wp.id, &meta).unwrap();
        assert!(
            updated,
            "update_metadata on existing wallpaper should return true"
        );

        let completed = library.get(wp.id).unwrap().unwrap();
        assert_eq!(
            completed.metadata.processing_state,
            ProcessingStatus::Completed
        );
        assert_eq!(completed.metadata.duration_secs, Some(15.5));
        assert_eq!(completed.metadata.width, Some(3840));
        assert_eq!(completed.metadata.height, Some(2160));
        assert_eq!(completed.metadata.fps, Some(60.0));
        assert_eq!(completed.metadata.codec.as_deref(), Some("hevc"));
        assert_eq!(
            completed.metadata.resolution_category.as_deref(),
            Some("4K UHD")
        );
        assert_eq!(
            completed.metadata.thumbnail_path.as_deref(),
            Some("C:\\AppData\\thumbnails\\test.jpg")
        );

        // list_pending_or_processing should now be empty
        let pending_after = library.list_pending_or_processing().unwrap();
        assert!(pending_after.is_empty());

        // Test failure state
        library
            .set_processing_state(
                wp.id,
                ProcessingStatus::Failed,
                Some("Corrupted file header"),
            )
            .unwrap();
        let failed = library.get(wp.id).unwrap().unwrap();
        assert_eq!(failed.metadata.processing_state, ProcessingStatus::Failed);
        assert_eq!(
            failed.metadata.processing_error.as_deref(),
            Some("Corrupted file header")
        );
    }

    #[test]
    fn test_categorize_resolution_helper() {
        assert_eq!(categorize_resolution(7680, 4320), "8K UHD");
        assert_eq!(categorize_resolution(3840, 2160), "4K UHD");
        assert_eq!(categorize_resolution(2560, 1440), "1440p QHD");
        assert_eq!(categorize_resolution(1920, 1080), "1080p FHD");
        assert_eq!(categorize_resolution(1280, 720), "720p HD");
        assert_eq!(categorize_resolution(854, 480), "480p SD");
        assert_eq!(categorize_resolution(640, 360), "SD");
    }
}
