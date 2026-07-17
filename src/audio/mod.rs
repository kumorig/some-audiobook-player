//! Playback engine.
//!
//! UI-agnostic wrapper over rodio. Audio flows through a custom
//! [`source::StretchSource`] that decodes the book and applies pitch-preserving
//! tempo change; rodio's `Player` provides play/pause/volume around it. Book
//! position and seeking go through the source's shared [`source::Controls`]
//! rather than rodio's own (output-based) position tracking, which a
//! time-stretched stream would otherwise make incorrect.

pub mod source;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::library::{BookMeta, meta};
use source::{Controls, StretchSource};

/// The book currently loaded into the engine.
pub struct LoadedBook {
    pub path: PathBuf,
    pub meta: BookMeta,
}

pub struct PlaybackEngine {
    /// Output device handle — must stay alive for audio to keep playing.
    _device: rodio::MixerDeviceSink,
    player: rodio::Player,
    current: Option<LoadedBook>,
    controls: Option<Arc<Controls>>,
    speed: f32,
}

impl PlaybackEngine {
    pub fn new() -> Result<Self> {
        let mut device = rodio::DeviceSinkBuilder::open_default_sink()
            .context("opening default audio output device")?;
        device.log_on_drop(false);
        let player = rodio::Player::connect_new(device.mixer());
        player.pause();
        Ok(Self {
            _device: device,
            player,
            current: None,
            controls: None,
            speed: 1.0,
        })
    }

    /// Load a single-file book (reads its own metadata/chapters).
    pub fn load_path(&mut self, path: &Path) -> Result<()> {
        let meta = meta::read(path)?;
        let duration = meta.duration.unwrap_or_default();
        self.load(vec![(path.to_path_buf(), duration)], path.to_path_buf(), meta)
    }

    /// Load a book from an ordered list of `(file, duration)` segments plus its
    /// metadata (chapters used for navigation). Replaces anything playing and
    /// starts paused. A single-file book passes a one-element list.
    pub fn load(
        &mut self,
        files: Vec<(PathBuf, Duration)>,
        path: PathBuf,
        meta: BookMeta,
    ) -> Result<()> {
        let (source, controls) = StretchSource::open_files(files)?;
        controls.set_speed(self.speed);

        self.player.clear();
        self.player.append(source);
        self.player.pause();

        self.controls = Some(controls);
        self.current = Some(LoadedBook { path, meta });
        Ok(())
    }

    pub fn current(&self) -> Option<&LoadedBook> {
        self.current.as_ref()
    }

    pub fn play(&self) {
        self.player.play();
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }

    pub fn toggle(&self) {
        if self.player.is_paused() {
            self.player.play();
        } else {
            self.player.pause();
        }
    }

    /// Current playback position from the start of the book.
    pub fn position(&self) -> Duration {
        self.controls
            .as_ref()
            .map(|c| c.position())
            .unwrap_or_default()
    }

    /// Seek to an absolute position within the book.
    pub fn seek(&self, pos: Duration) -> Result<()> {
        if let Some(c) = &self.controls {
            c.request_seek(pos);
        }
        Ok(())
    }

    /// Seek relative to the current position (negative to rewind), clamped to 0.
    pub fn seek_relative(&self, delta: i64) -> Result<()> {
        let cur = self.position().as_secs() as i64;
        let target = (cur + delta).max(0) as u64;
        self.seek(Duration::from_secs(target))
    }

    /// Index of the chapter currently playing, if any.
    pub fn current_chapter(&self) -> Option<usize> {
        let book = self.current.as_ref()?;
        meta::chapter_at(&book.meta.chapters, self.position())
    }

    /// Jump to the start of a chapter by index.
    pub fn seek_chapter(&self, index: usize) -> Result<()> {
        let book = self.current.as_ref().context("no book loaded")?;
        let ch = book
            .meta
            .chapters
            .get(index)
            .context("chapter index out of range")?;
        self.seek(ch.start)
    }

    pub fn next_chapter(&self) -> Result<()> {
        let cur = self.current_chapter().unwrap_or(0);
        self.seek_chapter(cur + 1)
    }

    pub fn prev_chapter(&self) -> Result<()> {
        let cur = self.current_chapter().unwrap_or(0);
        self.seek_chapter(cur.saturating_sub(1))
    }

    /// Playback speed (1.0 = normal), pitch-preserving.
    pub fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
        if let Some(c) = &self.controls {
            c.set_speed(speed);
        }
    }

    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Linear volume, 0.0..=1.0 (and above for gain).
    pub fn set_volume(&self, volume: f32) {
        self.player.set_volume(volume);
    }
}
