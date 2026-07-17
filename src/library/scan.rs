//! Filesystem import scanner.
//!
//! Walks a storage root, finds audiobook files, and imports them into the
//! library. Crucially, files that can't be imported are collected with the
//! *reason* they failed — the thing Cozy's vague "some files could not be
//! imported" never told you.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use walkdir::WalkDir;

use super::db::Database;
use super::{Track, meta};

/// Audiobook extensions we import.
pub const AUDIO_EXTS: &[&str] = &["m4b", "m4a", "mp4", "mp3", "flac", "ogg", "oga", "wav"];

/// Extensions that are always their own single-file book (they carry internal
/// chapters), never grouped into a multi-file book.
const STANDALONE_EXTS: &[&str] = &["m4b", "mp4"];

/// A folder needs at least this many audio files to be treated as one
/// multi-file book (fewer, and each file is imported on its own).
const MIN_MULTI_FILES: usize = 2;

/// Outcome of importing a storage location.
#[derive(Debug, Default)]
pub struct ImportReport {
    /// Newly imported (or refreshed) books.
    pub imported: usize,
    /// Files already present in the library, left untouched.
    pub skipped_existing: usize,
    /// Files that looked like audiobooks but could not be read, with the reason.
    pub failed: Vec<(PathBuf, String)>,
}

impl ImportReport {
    pub fn summary(&self) -> String {
        let mut s = format!("{} imported", self.imported);
        if self.skipped_existing > 0 {
            s += &format!(", {} already in library", self.skipped_existing);
        }
        if !self.failed.is_empty() {
            s += &format!(", {} failed", self.failed.len());
        }
        s
    }
}

fn ext_of(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase())
}

fn is_audiobook(path: &Path) -> bool {
    ext_of(path).is_some_and(|e| AUDIO_EXTS.contains(&e.as_str()))
}

fn is_standalone(path: &Path) -> bool {
    ext_of(path).is_some_and(|e| STANDALONE_EXTS.contains(&e.as_str()))
}

/// Recursively scan `root` and import books found, grouping folders of loose
/// audio files into single multi-file books.
pub fn scan_and_import(db: &Database, root: &Path) -> ImportReport {
    let mut report = ImportReport::default();

    // Group audio files by their containing directory.
    let mut by_dir: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for entry in WalkDir::new(root).follow_links(true).into_iter().flatten() {
        let path = entry.path();
        if entry.file_type().is_file() && is_audiobook(path) {
            if let Some(parent) = path.parent() {
                by_dir.entry(parent.to_path_buf()).or_default().push(path.to_path_buf());
            }
        }
    }

    for (dir, mut files) in by_dir {
        files.sort();
        // Standalone files (m4b/mp4) are always their own book.
        let (standalone, parts): (Vec<_>, Vec<_>) =
            files.into_iter().partition(|p| is_standalone(p));
        for f in standalone {
            import_single(db, &f, &mut report);
        }
        if parts.len() >= MIN_MULTI_FILES {
            import_multi(db, &dir, &parts, &mut report);
        } else {
            for f in parts {
                import_single(db, &f, &mut report);
            }
        }
    }

    report
}

fn import_single(db: &Database, path: &Path, report: &mut ImportReport) {
    match db.book_path_exists(path) {
        Ok(true) => report.skipped_existing += 1,
        Ok(false) => match import_file(db, path) {
            Ok(_) => report.imported += 1,
            Err(e) => report.failed.push((path.to_path_buf(), format!("{e:#}"))),
        },
        Err(e) => report.failed.push((path.to_path_buf(), format!("{e:#}"))),
    }
}

/// Import a folder of loose audio files as one multi-file book (each file a
/// chapter, in filename order).
fn import_multi(db: &Database, dir: &Path, parts: &[PathBuf], report: &mut ImportReport) {
    // Clean up any stale per-file entries (e.g. imported before this folder was
    // recognised as one multi-file book).
    for p in parts {
        let _ = db.remove_single_file_book(p);
    }

    match db.book_path_exists(dir) {
        Ok(true) => {
            report.skipped_existing += 1;
            return;
        }
        Ok(false) => {}
        Err(e) => {
            report.failed.push((dir.to_path_buf(), format!("{e:#}")));
            return;
        }
    }

    // Book-level metadata from the first readable file.
    let header = parts.iter().find_map(|p| meta::read(p).ok());
    let cover = header
        .as_ref()
        .and_then(|h| h.cover.clone())
        .or_else(|| meta::folder_cover(dir));

    // Build the ordered track list with cumulative offsets.
    let mut tracks = Vec::new();
    let mut start = Duration::ZERO;
    for path in parts {
        match meta::track_info(path) {
            Ok((title, duration)) => {
                tracks.push(Track {
                    idx: tracks.len(),
                    path: path.clone(),
                    start,
                    duration,
                    title,
                });
                start += duration;
            }
            Err(e) => report.failed.push((path.clone(), format!("{e:#}"))),
        }
    }
    if tracks.is_empty() {
        return;
    }

    let title = header
        .as_ref()
        .and_then(|h| h.album.clone().or_else(|| h.title.clone()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            dir.file_name().and_then(|s| s.to_str()).unwrap_or("Audiobook").to_string()
        });

    let book_meta = super::BookMeta {
        title: Some(title),
        author: header.as_ref().and_then(|h| h.author.clone()),
        reader: header.as_ref().and_then(|h| h.reader.clone()),
        album: header.as_ref().and_then(|h| h.album.clone()),
        duration: Some(start),
        chapters: Vec::new(),
        cover,
    };

    match db.upsert_multi_book(dir, &book_meta, &tracks) {
        Ok(_) => report.imported += 1,
        Err(e) => report.failed.push((dir.to_path_buf(), format!("{e:#}"))),
    }
}

/// Read one file's metadata and insert/refresh it as a single-file book,
/// returning its id. Used by the folder scanner and the "Open file…" action.
pub fn import_file(db: &Database, path: &Path) -> Result<i64> {
    let meta = meta::read(path)?;
    db.upsert_book(path, &meta)
}
