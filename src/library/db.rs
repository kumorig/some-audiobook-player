//! SQLite persistence for the library (books, resume positions, storage roots).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use super::{Book, BookMeta, Bookmark, Cover, HistoryEntry, Track};

/// How the library grid is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Title,
    Author,
    RecentlyAdded,
    RecentlyPlayed,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open (creating if needed) the library database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok();
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening library database at {}", path.display()))?;
        conn.pragma_update(None, "foreign_keys", "ON").ok();
        let db = Self { conn };
        db.migrate()?;
        Ok(db)
    }

    /// Open an in-memory database (used by tests).
    pub fn open_in_memory() -> Result<Self> {
        let db = Self {
            conn: Connection::open_in_memory()?,
        };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS storage_locations (
                id   INTEGER PRIMARY KEY,
                path TEXT NOT NULL UNIQUE
            );
            CREATE TABLE IF NOT EXISTS books (
                id            INTEGER PRIMARY KEY,
                path          TEXT NOT NULL UNIQUE,
                title         TEXT NOT NULL,
                author        TEXT,
                album         TEXT,
                duration_secs REAL NOT NULL DEFAULT 0,
                chapter_count INTEGER NOT NULL DEFAULT 0,
                cover         BLOB,
                cover_is_png  INTEGER NOT NULL DEFAULT 0,
                position_secs REAL NOT NULL DEFAULT 0,
                finished      INTEGER NOT NULL DEFAULT 0,
                added_at      INTEGER NOT NULL,
                last_played   INTEGER
            );
            CREATE TABLE IF NOT EXISTS bookmarks (
                id            INTEGER PRIMARY KEY,
                book_id       INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
                position_secs REAL NOT NULL,
                label         TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS tracks (
                book_id       INTEGER NOT NULL REFERENCES books(id) ON DELETE CASCADE,
                idx           INTEGER NOT NULL,
                path          TEXT NOT NULL,
                start_secs    REAL NOT NULL,
                duration_secs REAL NOT NULL,
                title         TEXT NOT NULL DEFAULT '',
                PRIMARY KEY (book_id, idx)
            );
            -- "My History": ratings/notes/cover kept independently of the library
            -- so they survive the underlying files being removed.
            CREATE TABLE IF NOT EXISTS history (
                id             INTEGER PRIMARY KEY,
                book_key       TEXT UNIQUE,
                title          TEXT NOT NULL,
                author         TEXT,
                reader         TEXT,
                cover          BLOB,
                cover_is_png   INTEGER NOT NULL DEFAULT 0,
                rating         INTEGER,
                note           TEXT NOT NULL DEFAULT '',
                first_played   INTEGER,
                last_played    INTEGER,
                finished       INTEGER NOT NULL DEFAULT 0,
                added_manually INTEGER NOT NULL DEFAULT 0
            );
            "#,
        )?;
        // Columns added after the initial release; ignore if already present.
        let _ = self
            .conn
            .execute("ALTER TABLE books ADD COLUMN speed REAL NOT NULL DEFAULT 1.0", []);
        let _ = self
            .conn
            .execute("ALTER TABLE books ADD COLUMN is_multi INTEGER NOT NULL DEFAULT 0", []);
        let _ = self
            .conn
            .execute("ALTER TABLE books ADD COLUMN reader TEXT", []);
        Ok(())
    }

    // ---- storage locations -------------------------------------------------

    pub fn add_storage_location(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO storage_locations (path) VALUES (?1)",
            params![path.to_string_lossy()],
        )?;
        Ok(())
    }

    pub fn remove_storage_location(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "DELETE FROM storage_locations WHERE path = ?1",
            params![path.to_string_lossy()],
        )?;
        Ok(())
    }

    pub fn storage_locations(&self) -> Result<Vec<PathBuf>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path FROM storage_locations ORDER BY path")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.filter_map(|r| r.ok()).map(PathBuf::from).collect())
    }

    // ---- settings (key/value) ---------------------------------------------

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", params![key], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) {
        let _ = self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        );
    }

    // ---- books -------------------------------------------------------------

    pub fn book_path_exists(&self, path: &Path) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM books WHERE path = ?1",
            params![path.to_string_lossy()],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Insert or refresh a book by path. Metadata is overwritten; resume state
    /// (`position`, `finished`, `added_at`) is preserved across re-imports.
    pub fn upsert_book(&self, path: &Path, meta: &BookMeta) -> Result<i64> {
        let (cover, cover_is_png) = match &meta.cover {
            Some(c) => (Some(c.data.clone()), c.is_png as i64),
            None => (None, 0),
        };
        self.conn.execute(
            r#"
            INSERT INTO books
                (path, title, author, reader, album, duration_secs, chapter_count,
                 cover, cover_is_png, added_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(path) DO UPDATE SET
                title         = excluded.title,
                author        = excluded.author,
                reader        = excluded.reader,
                album         = excluded.album,
                duration_secs = excluded.duration_secs,
                chapter_count = excluded.chapter_count,
                cover         = excluded.cover,
                cover_is_png  = excluded.cover_is_png
            "#,
            params![
                path.to_string_lossy(),
                meta.display_title(),
                meta.author,
                meta.reader,
                meta.album,
                meta.duration.unwrap_or_default().as_secs_f64(),
                meta.chapters.len() as i64,
                cover,
                cover_is_png,
                now_unix(),
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM books WHERE path = ?1",
            params![path.to_string_lossy()],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    /// Insert or refresh a multi-file book (a folder played as one book),
    /// replacing its track list. Resume state is preserved across re-imports.
    pub fn upsert_multi_book(
        &self,
        folder: &Path,
        meta: &BookMeta,
        tracks: &[Track],
    ) -> Result<i64> {
        let (cover, cover_is_png) = match &meta.cover {
            Some(c) => (Some(c.data.clone()), c.is_png as i64),
            None => (None, 0),
        };
        self.conn.execute(
            r#"
            INSERT INTO books
                (path, title, author, reader, album, duration_secs, chapter_count,
                 cover, cover_is_png, is_multi, added_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10)
            ON CONFLICT(path) DO UPDATE SET
                title = excluded.title, author = excluded.author,
                reader = excluded.reader, album = excluded.album,
                duration_secs = excluded.duration_secs,
                chapter_count = excluded.chapter_count, cover = excluded.cover,
                cover_is_png = excluded.cover_is_png, is_multi = 1
            "#,
            params![
                folder.to_string_lossy(),
                meta.display_title(),
                meta.author,
                meta.reader,
                meta.album,
                meta.duration.unwrap_or_default().as_secs_f64(),
                tracks.len() as i64,
                cover,
                cover_is_png,
                now_unix(),
            ],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM books WHERE path = ?1",
            params![folder.to_string_lossy()],
            |r| r.get(0),
        )?;
        self.conn
            .execute("DELETE FROM tracks WHERE book_id = ?1", params![id])?;
        for t in tracks {
            self.conn.execute(
                "INSERT INTO tracks (book_id, idx, path, start_secs, duration_secs, title) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    id,
                    t.idx as i64,
                    t.path.to_string_lossy(),
                    t.start.as_secs_f64(),
                    t.duration.as_secs_f64(),
                    t.title,
                ],
            )?;
        }
        Ok(id)
    }

    /// Ordered member tracks of a multi-file book.
    pub fn tracks(&self, book_id: i64) -> Result<Vec<Track>> {
        let mut stmt = self.conn.prepare(
            "SELECT idx, path, start_secs, duration_secs, title FROM tracks \
             WHERE book_id = ?1 ORDER BY idx",
        )?;
        let rows = stmt.query_map(params![book_id], |r| {
            Ok(Track {
                idx: r.get::<_, i64>(0)? as usize,
                path: PathBuf::from(r.get::<_, String>(1)?),
                start: Duration::from_secs_f64(r.get::<_, f64>(2)?.max(0.0)),
                duration: Duration::from_secs_f64(r.get::<_, f64>(3)?.max(0.0)),
                title: r.get(4)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn get_book(&self, id: i64) -> Result<Option<Book>> {
        let book = self
            .conn
            .query_row(
                &format!("SELECT {BOOK_COLS} FROM books WHERE id = ?1"),
                params![id],
                row_to_book,
            )
            .optional()?;
        Ok(book)
    }

    pub fn list_books(&self, sort: Sort) -> Result<Vec<Book>> {
        let order = match sort {
            Sort::Title => "title COLLATE NOCASE ASC",
            Sort::Author => "author COLLATE NOCASE ASC, title COLLATE NOCASE ASC",
            Sort::RecentlyAdded => "added_at DESC",
            Sort::RecentlyPlayed => "last_played DESC NULLS LAST, added_at DESC",
        };
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT {BOOK_COLS} FROM books ORDER BY {order}"))?;
        let rows = stmt.query_map([], row_to_book)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn update_position(&self, id: i64, position: Duration, finished: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE books SET position_secs = ?2, finished = ?3, last_played = ?4 WHERE id = ?1",
            params![id, position.as_secs_f64(), finished as i64, now_unix()],
        )?;
        Ok(())
    }

    pub fn update_speed(&self, id: i64, speed: f32) -> Result<()> {
        self.conn.execute(
            "UPDATE books SET speed = ?2 WHERE id = ?1",
            params![id, speed as f64],
        )?;
        Ok(())
    }

    pub fn remove_book(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM books WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Remove a single-file book by its exact file path (used to clean up
    /// per-file entries once a folder is recognised as one multi-file book).
    pub fn remove_single_file_book(&self, path: &Path) -> Result<()> {
        self.conn.execute(
            "DELETE FROM books WHERE path = ?1 AND is_multi = 0",
            params![path.to_string_lossy()],
        )?;
        Ok(())
    }

    // ---- bookmarks ---------------------------------------------------------

    pub fn add_bookmark(&self, book_id: i64, position: Duration, label: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO bookmarks (book_id, position_secs, label) VALUES (?1, ?2, ?3)",
            params![book_id, position.as_secs_f64(), label],
        )?;
        Ok(())
    }

    pub fn bookmarks(&self, book_id: i64) -> Result<Vec<Bookmark>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, book_id, position_secs, label FROM bookmarks \
             WHERE book_id = ?1 ORDER BY position_secs",
        )?;
        let rows = stmt.query_map(params![book_id], |r| {
            Ok(Bookmark {
                id: r.get(0)?,
                book_id: r.get(1)?,
                position: Duration::from_secs_f64(r.get::<_, f64>(2)?.max(0.0)),
                label: r.get(3)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn remove_bookmark(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM bookmarks WHERE id = ?1", params![id])?;
        Ok(())
    }

    // ---- history -----------------------------------------------------------

    /// Create or refresh the history entry for a book, stamping "last played"
    /// now. Preserves an existing rating/note/first-played; `finished` and
    /// `added_manually` are OR-ed so they only ever turn on.
    pub fn upsert_history_from_book(
        &self,
        book: &Book,
        finished: bool,
        manual: bool,
    ) -> Result<()> {
        let (cover, cover_is_png) = match &book.cover {
            Some(c) => (Some(c.data.clone()), c.is_png as i64),
            None => (None, 0),
        };
        let now = now_unix();
        self.conn.execute(
            r#"
            INSERT INTO history
                (book_key, title, author, reader, cover, cover_is_png,
                 first_played, last_played, finished, added_manually)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9)
            ON CONFLICT(book_key) DO UPDATE SET
                title          = excluded.title,
                author         = excluded.author,
                reader         = excluded.reader,
                cover          = excluded.cover,
                cover_is_png   = excluded.cover_is_png,
                last_played    = excluded.last_played,
                first_played   = COALESCE(history.first_played, excluded.first_played),
                finished       = history.finished | excluded.finished,
                added_manually = history.added_manually | excluded.added_manually
            "#,
            params![
                book.path.to_string_lossy(),
                book.title,
                book.author,
                book.reader,
                cover,
                cover_is_png,
                now,
                finished as i64,
                manual as i64,
            ],
        )?;
        Ok(())
    }

    pub fn set_history_rating(&self, id: i64, rating: Option<u8>) -> Result<()> {
        self.conn.execute(
            "UPDATE history SET rating = ?2 WHERE id = ?1",
            params![id, rating.map(|r| r as i64)],
        )?;
        Ok(())
    }

    pub fn set_history_note(&self, id: i64, note: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE history SET note = ?2 WHERE id = ?1",
            params![id, note],
        )?;
        Ok(())
    }

    /// Set the "date listened" (Unix epoch seconds) for a history entry.
    pub fn set_history_date(&self, id: i64, epoch: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE history SET last_played = ?2 WHERE id = ?1",
            params![id, epoch],
        )?;
        Ok(())
    }

    pub fn remove_history(&self, id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM history WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// All history entries, most recently listened first.
    pub fn list_history(&self) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {HISTORY_COLS} FROM history ORDER BY last_played DESC NULLS LAST, id DESC"
        ))?;
        let rows = stmt.query_map([], row_to_history)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// The history entry for a book path, if one exists.
    pub fn history_for_key(&self, key: &str) -> Result<Option<HistoryEntry>> {
        let entry = self
            .conn
            .query_row(
                &format!("SELECT {HISTORY_COLS} FROM history WHERE book_key = ?1"),
                params![key],
                row_to_history,
            )
            .optional()?;
        Ok(entry)
    }
}

/// Column list shared by all `SELECT`s that build a [`Book`], keeping the order
/// in sync with [`row_to_book`].
const BOOK_COLS: &str = "id, path, title, author, reader, duration_secs, chapter_count, \
     cover, cover_is_png, position_secs, finished, speed, is_multi, added_at, last_played";

fn row_to_book(r: &rusqlite::Row<'_>) -> rusqlite::Result<Book> {
    let cover_data: Option<Vec<u8>> = r.get("cover")?;
    let cover_is_png: i64 = r.get("cover_is_png")?;
    Ok(Book {
        id: r.get("id")?,
        path: PathBuf::from(r.get::<_, String>("path")?),
        title: r.get("title")?,
        author: r.get("author")?,
        reader: r.get("reader").ok(),
        duration: Duration::from_secs_f64(r.get::<_, f64>("duration_secs")?.max(0.0)),
        chapter_count: r.get::<_, i64>("chapter_count")? as usize,
        cover: cover_data.map(|data| Cover {
            data,
            is_png: cover_is_png != 0,
        }),
        position: Duration::from_secs_f64(r.get::<_, f64>("position_secs")?.max(0.0)),
        finished: r.get::<_, i64>("finished")? != 0,
        speed: r.get::<_, f64>("speed").unwrap_or(1.0) as f32,
        is_multi: r.get::<_, i64>("is_multi").unwrap_or(0) != 0,
        added_at: r.get("added_at")?,
        last_played: r.get("last_played")?,
    })
}

/// Column list shared by the history `SELECT`s, kept in sync with
/// [`row_to_history`].
const HISTORY_COLS: &str = "id, book_key, title, author, reader, cover, cover_is_png, \
     rating, note, first_played, last_played, finished, added_manually";

fn row_to_history(r: &rusqlite::Row<'_>) -> rusqlite::Result<HistoryEntry> {
    let cover_data: Option<Vec<u8>> = r.get("cover")?;
    let cover_is_png: i64 = r.get("cover_is_png")?;
    Ok(HistoryEntry {
        id: r.get("id")?,
        book_key: r.get::<_, Option<String>>("book_key")?.unwrap_or_default(),
        title: r.get("title")?,
        author: r.get("author")?,
        reader: r.get("reader")?,
        cover: cover_data.map(|data| Cover {
            data,
            is_png: cover_is_png != 0,
        }),
        rating: r.get::<_, Option<i64>>("rating")?.map(|v| v.clamp(0, 5) as u8),
        note: r.get("note")?,
        first_played: r.get("first_played")?,
        last_played: r.get("last_played")?,
        finished: r.get::<_, i64>("finished")? != 0,
        added_manually: r.get::<_, i64>("added_manually")? != 0,
    })
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_meta() -> BookMeta {
        BookMeta {
            title: Some("The Hobbit".into()),
            author: Some("J.R.R. Tolkien".into()),
            reader: Some("Rob Inglis".into()),
            album: None,
            duration: Some(Duration::from_secs(3600)),
            chapters: Vec::new(),
            cover: Some(Cover { data: vec![1, 2, 3], is_png: true }),
        }
    }

    #[test]
    fn reader_column_round_trips() {
        let db = Database::open_in_memory().unwrap();
        let id = db.upsert_book(Path::new("/books/hobbit.m4b"), &sample_meta()).unwrap();
        let book = db.get_book(id).unwrap().unwrap();
        assert_eq!(book.reader.as_deref(), Some("Rob Inglis"));
        assert_eq!(book.author.as_deref(), Some("J.R.R. Tolkien"));
    }

    #[test]
    fn history_crud_and_survives_book_removal() {
        let db = Database::open_in_memory().unwrap();
        let id = db.upsert_book(Path::new("/books/hobbit.m4b"), &sample_meta()).unwrap();
        let book = db.get_book(id).unwrap().unwrap();

        // Auto-add on finish, then rate + note it.
        db.upsert_history_from_book(&book, true, false).unwrap();
        let entry = db
            .history_for_key("/books/hobbit.m4b")
            .unwrap()
            .expect("history entry");
        assert!(entry.finished);
        assert_eq!(entry.title, "The Hobbit");
        assert_eq!(entry.reader.as_deref(), Some("Rob Inglis"));
        assert!(entry.cover.is_some(), "cover copied into history");

        db.set_history_rating(entry.id, Some(4)).unwrap();
        db.set_history_note(entry.id, "Loved it.").unwrap();

        // Removing the book must NOT remove the history entry.
        db.remove_book(id).unwrap();
        assert!(db.get_book(id).unwrap().is_none());

        let list = db.list_history().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].rating, Some(4));
        assert_eq!(list[0].note, "Loved it.");

        // Re-listening keeps the rating/note but bumps last_played + finished.
        db.upsert_history_from_book(&book, false, false).unwrap();
        let again = db.history_for_key("/books/hobbit.m4b").unwrap().unwrap();
        assert_eq!(again.rating, Some(4));
        assert_eq!(again.note, "Loved it.");
        assert!(again.finished, "finished stays set once true");

        db.remove_history(again.id).unwrap();
        assert!(db.list_history().unwrap().is_empty());
    }
}
