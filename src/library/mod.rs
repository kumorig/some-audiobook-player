//! Library domain types and audiobook metadata extraction.

pub mod db;
pub mod meta;
pub mod scan;

use std::path::PathBuf;
use std::time::Duration;

/// A single chapter within a book.
#[derive(Debug, Clone)]
pub struct Chapter {
    /// Zero-based position in the book.
    pub index: usize,
    pub title: String,
    /// Offset from the start of the book where this chapter begins.
    pub start: Duration,
}

/// Cover artwork extracted from a file.
#[derive(Debug, Clone)]
pub struct Cover {
    pub data: Vec<u8>,
    /// `true` for PNG, `false` for JPEG (the two formats iTunes-style art uses).
    pub is_png: bool,
}

/// Everything we know about a book from its file, independent of playback.
#[derive(Debug, Clone, Default)]
pub struct BookMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    /// The narrator/reader, taken from the composer tag (audiobooks put it there).
    pub reader: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub chapters: Vec<Chapter>,
    pub cover: Option<Cover>,
}

impl BookMeta {
    /// A human-friendly display title, falling back through album then filename.
    pub fn display_title(&self) -> &str {
        self.title
            .as_deref()
            .or(self.album.as_deref())
            .unwrap_or("Unknown book")
    }
}

/// A book as stored in the library database: metadata plus persistent state
/// (resume position, when it was added/last played).
#[derive(Debug, Clone)]
pub struct Book {
    pub id: i64,
    pub path: PathBuf,
    pub title: String,
    pub author: Option<String>,
    /// The narrator/reader (from the composer tag), if known.
    pub reader: Option<String>,
    pub duration: Duration,
    pub chapter_count: usize,
    pub cover: Option<Cover>,
    /// Resume position from the start of the book.
    pub position: Duration,
    pub finished: bool,
    /// Remembered per-book playback speed.
    pub speed: f32,
    /// True when this book is a folder of separate audio files (one per
    /// chapter) rather than a single file. `path` is then the folder.
    pub is_multi: bool,
    pub added_at: i64,
    pub last_played: Option<i64>,
}

/// One member file of a multi-file book (played back to back).
#[derive(Debug, Clone)]
pub struct Track {
    pub idx: usize,
    pub path: PathBuf,
    /// Offset of this track from the start of the book.
    pub start: Duration,
    pub duration: Duration,
    pub title: String,
}

/// A saved position within a book.
#[derive(Debug, Clone)]
pub struct Bookmark {
    pub id: i64,
    pub book_id: i64,
    pub position: Duration,
    pub label: String,
}

/// A persistent "My History" entry: a rating + note + saved cover for a book,
/// kept independently of the library so it survives the files being removed.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: i64,
    /// Stable key linking back to a library book (its path), if it still exists.
    pub book_key: String,
    pub title: String,
    pub author: Option<String>,
    pub reader: Option<String>,
    /// A copy of the cover artwork, saved so it outlives the source files.
    pub cover: Option<Cover>,
    /// User rating, 0..=5 stars (None if never rated).
    pub rating: Option<u8>,
    pub note: String,
    /// When the book was first played (Unix epoch seconds).
    pub first_played: Option<i64>,
    /// When the book was last played (Unix epoch seconds) — the "date listened".
    pub last_played: Option<i64>,
    pub finished: bool,
    /// True when the user added this entry by hand rather than by listening.
    pub added_manually: bool,
}

impl Book {
    /// Fraction played in 0.0..=1.0 (0 if duration unknown).
    pub fn progress_fraction(&self) -> f32 {
        let total = self.duration.as_secs_f32();
        if total <= 0.0 {
            0.0
        } else {
            (self.position.as_secs_f32() / total).clamp(0.0, 1.0)
        }
    }
}
