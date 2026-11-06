use rusqlite::{params, Connection, OptionalExtension};
use std::{path::Path, sync::Mutex};

pub struct Db(Mutex<Connection>);

#[derive(Clone, Debug)]
pub struct FileRow {
    pub id: String,
    pub filename: String,
    pub size: i64,
    pub content_type: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub max_downloads: Option<i64>,
    pub downloads: i64,
}

impl FileRow {
    pub fn downloads_left(&self) -> Option<i64> {
        self.max_downloads.map(|m| (m - self.downloads).max(0))
    }
}

const COLS: &str = "id, filename, size, content_type, created_at, expires_at, max_downloads, downloads";

fn row(r: &rusqlite::Row) -> rusqlite::Result<FileRow> {
    Ok(FileRow {
        id: r.get(0)?,
        filename: r.get(1)?,
        size: r.get(2)?,
        content_type: r.get(3)?,
        created_at: r.get(4)?,
        expires_at: r.get(5)?,
        max_downloads: r.get(6)?,
        downloads: r.get(7)?,
    })
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS files (
                id            TEXT PRIMARY KEY,
                filename      TEXT NOT NULL,
                size          INTEGER NOT NULL,
                content_type  TEXT NOT NULL,
                created_at    INTEGER NOT NULL,
                expires_at    INTEGER NOT NULL,
                max_downloads INTEGER,
                downloads     INTEGER NOT NULL DEFAULT 0,
                delete_hash   TEXT NOT NULL
             );
             CREATE INDEX IF NOT EXISTS files_expires ON files(expires_at);",
        )?;
        Ok(Db(Mutex::new(conn)))
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Returns false if the id is already taken.
    pub fn insert(&self, f: &FileRow, delete_hash: &str) -> rusqlite::Result<bool> {
        let res = self.conn().execute(
            "INSERT INTO files (id, filename, size, content_type, created_at, expires_at, max_downloads, downloads, delete_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8)",
            params![f.id, f.filename, f.size, f.content_type, f.created_at, f.expires_at, f.max_downloads, delete_hash],
        );
        match res {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Fetches a file that is still available (not expired, downloads left).
    pub fn get_active(&self, id: &str, filename: &str) -> rusqlite::Result<Option<FileRow>> {
        self.conn()
            .query_row(
                &format!(
                    "SELECT {COLS} FROM files WHERE id = ?1 AND filename = ?2 AND expires_at > ?3
                     AND (max_downloads IS NULL OR downloads < max_downloads)"
                ),
                params![id, filename, now()],
                row,
            )
            .optional()
    }

    /// Atomically counts one download. Returns the row after the increment,
    /// or None if the file is gone / expired / exhausted.
    pub fn take_download(&self, id: &str, filename: &str) -> rusqlite::Result<Option<FileRow>> {
        self.conn()
            .query_row(
                &format!(
                    "UPDATE files SET downloads = downloads + 1
                     WHERE id = ?1 AND filename = ?2 AND expires_at > ?3
                       AND (max_downloads IS NULL OR downloads < max_downloads)
                     RETURNING {COLS}"
                ),
                params![id, filename, now()],
                row,
            )
            .optional()
    }

    pub fn delete_with_hash(&self, id: &str, filename: &str, delete_hash: &str) -> rusqlite::Result<bool> {
        let n = self.conn().execute(
            "DELETE FROM files WHERE id = ?1 AND filename = ?2 AND delete_hash = ?3",
            params![id, filename, delete_hash],
        )?;
        Ok(n > 0)
    }

    /// True if this is the delete token of a stored file (used to explain a GET on the delete link).
    pub fn is_delete_link(&self, id: &str, filename: &str, delete_hash: &str) -> rusqlite::Result<bool> {
        self.conn()
            .query_row(
                "SELECT 1 FROM files WHERE id = ?1 AND filename = ?2 AND delete_hash = ?3",
                params![id, filename, delete_hash],
                |_| Ok(()),
            )
            .optional()
            .map(|o| o.is_some())
    }

    pub fn delete(&self, id: &str) -> rusqlite::Result<()> {
        self.conn().execute("DELETE FROM files WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Removes expired or exhausted rows and returns their ids.
    pub fn purge_dead(&self) -> rusqlite::Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "DELETE FROM files WHERE expires_at <= ?1
               OR (max_downloads IS NOT NULL AND downloads >= max_downloads)
             RETURNING id",
        )?;
        let ids = stmt.query_map(params![now()], |r| r.get(0))?.collect();
        ids
    }

    pub fn exists(&self, id: &str) -> rusqlite::Result<bool> {
        self.conn()
            .query_row("SELECT 1 FROM files WHERE id = ?1", params![id], |_| Ok(()))
            .optional()
            .map(|o| o.is_some())
    }
}
