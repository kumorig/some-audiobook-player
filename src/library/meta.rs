//! Metadata extraction. For `.m4b`/`.m4a` we use `mp4ameta`, which reads
//! iTunes-style tags plus both chapter representations (chapter track and
//! chapter list) that symphonia itself does not expose. For other formats
//! (mp3/flac/ogg/wav) we fall back to `lofty` for tags, cover, and duration.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use lofty::prelude::{Accessor, AudioFile, ItemKey, TaggedFileExt};

use super::{BookMeta, Chapter, Cover};

/// MP4-family extensions handled by `mp4ameta` (with chapter support).
const MP4_EXTS: &[&str] = &["m4b", "m4a", "mp4"];

/// Read metadata from any supported audiobook file, dispatching by extension.
pub fn read(path: &Path) -> Result<BookMeta> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    if MP4_EXTS.contains(&ext.as_str()) {
        read_m4b(path)
    } else {
        read_general(path)
    }
}

/// Read tags/cover/duration for non-MP4 formats via `lofty`. These typically
/// carry no chapter markers, so the book plays as a single span.
fn read_general(path: &Path) -> Result<BookMeta> {
    let file = lofty::read_from_path(path)
        .with_context(|| format!("reading metadata from {}", path.display()))?;
    let duration = Some(file.properties().duration());
    let tag = file.primary_tag().or_else(|| file.first_tag());

    let (title, author, reader, album, cover) = match tag {
        Some(tag) => {
            let cover = tag.pictures().first().map(|p| Cover {
                data: p.data().to_vec(),
                is_png: matches!(p.mime_type(), Some(lofty::picture::MimeType::Png)),
            });
            (
                tag.title().map(|s| s.to_string()),
                tag.artist().map(|s| s.to_string()),
                tag.get_string(ItemKey::Composer).map(|s| s.to_string()),
                tag.album().map(|s| s.to_string()),
                cover,
            )
        }
        None => (None, None, None, None, None),
    };

    Ok(BookMeta {
        title,
        author,
        reader,
        album,
        duration,
        chapters: Vec::new(),
        cover,
    })
}

/// Read metadata (tags, chapters, cover) from an MP4-family audiobook file.
pub fn read_m4b(path: &Path) -> Result<BookMeta> {
    let tag = mp4ameta::Tag::read_from_path(path)
        .with_context(|| format!("reading MP4 metadata from {}", path.display()))?;

    // Prefer the chapter track (real per-chapter timestamps); fall back to the
    // chapter list. On our reference file both hold the same 33 entries.
    let raw = {
        let track = tag.chapter_track();
        if track.is_empty() {
            tag.chapter_list()
        } else {
            track
        }
    };
    let chapters = raw
        .iter()
        .enumerate()
        .map(|(index, ch)| Chapter {
            index,
            title: ch.title.clone(),
            start: ch.start,
        })
        .collect();

    let cover = tag.images().next().map(|(_ident, img)| Cover {
        data: img.data.to_vec(),
        is_png: matches!(img.fmt, mp4ameta::ImgFmt::Png),
    });

    Ok(BookMeta {
        title: tag.title().map(str::to_owned),
        author: tag.artist().map(str::to_owned),
        reader: tag.composer().map(str::to_owned),
        album: tag.album().map(str::to_owned),
        duration: Some(tag.duration()),
        chapters,
        cover,
    })
}

/// Title + duration for one member file of a multi-file book. Falls back to the
/// filename when the file has no title tag.
pub fn track_info(path: &Path) -> Result<(String, Duration)> {
    let file = lofty::read_from_path(path)
        .with_context(|| format!("reading {}", path.display()))?;
    let duration = file.properties().duration();
    let title = file
        .primary_tag()
        .or_else(|| file.first_tag())
        .and_then(|t| t.title().map(|s| s.to_string()))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Track")
                .to_string()
        });
    Ok((title, duration))
}

/// Look for a standalone cover image in a book folder (`cover.*`/`folder.*`
/// preferred, otherwise the first image found).
pub fn folder_cover(dir: &Path) -> Option<Cover> {
    let mut images: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
                Some("jpg" | "jpeg" | "png")
            )
        })
        .collect();
    images.sort();
    images.sort_by_key(|p| {
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
        u8::from(stem != "cover" && stem != "folder")
    });
    let path = images.first()?;
    let data = std::fs::read(path).ok()?;
    let is_png = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("png"));
    Some(Cover { data, is_png })
}

/// Given a playback position, return the index of the chapter containing it.
pub fn chapter_at(chapters: &[Chapter], pos: Duration) -> Option<usize> {
    chapters
        .iter()
        .rev()
        .find(|c| c.start <= pos)
        .map(|c| c.index)
}
