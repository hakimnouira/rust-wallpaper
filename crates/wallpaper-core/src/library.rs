//! SQLite persistence for wallpaper metadata and library management.

use crate::{
    HardwareAcceleration, PlaybackConfig, Wallpaper, WallpaperId, WallpaperKind, WallpaperMetadata,
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
                thumbnail_path TEXT,
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

        conn.execute(
            r#"
            INSERT INTO wallpapers (
                id, title, source, kind, duration_secs, width, height, thumbnail_path,
                loop_playback, mute, volume, hwdec
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            ON CONFLICT(id) DO UPDATE SET
                title = excluded.title,
                source = excluded.source,
                kind = excluded.kind,
                duration_secs = excluded.duration_secs,
                width = excluded.width,
                height = excluded.height,
                thumbnail_path = excluded.thumbnail_path,
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
                wp.metadata.thumbnail_path,
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
            SELECT id, title, source, kind, duration_secs, width, height, thumbnail_path,
                   loop_playback, mute, volume, hwdec
            FROM wallpapers WHERE id = ?1;
            "#,
        )?;

        let result = stmt
            .query_row(params![id.to_string()], |row| {
                Ok(Self::row_to_wallpaper(row)?)
            })
            .optional()?;

        Ok(result)
    }

    /// Lists all wallpapers in the library.
    pub fn list(&self) -> Result<Vec<Wallpaper>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            r#"
            SELECT id, title, source, kind, duration_secs, width, height, thumbnail_path,
                   loop_playback, mute, volume, hwdec
            FROM wallpapers ORDER BY created_at DESC;
            "#,
        )?;

        let iter = stmt.query_map([], |row| Ok(Self::row_to_wallpaper(row)?))?;
        let mut wallpapers = Vec::new();
        for item in iter {
            wallpapers.push(item?);
        }
        Ok(wallpapers)
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

        if extension != "mp4" && extension != "webm" && extension != "mkv" && extension != "mov" {
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
        let thumbnail_path: Option<String> = row.get(7)?;
        let loop_playback: i32 = row.get(8)?;
        let mute: i32 = row.get(9)?;
        let volume: u32 = row.get(10)?;
        let hwdec_str: String = row.get(11)?;

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
                thumbnail_path,
            },
            config: PlaybackConfig {
                loop_playback: loop_playback != 0,
                mute: mute != 0,
                volume,
                hardware_acceleration: hwdec,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
