//! A rodio `Source` that plays one or more files back-to-back as a single book
//! and applies **pitch-preserving** tempo change (via `timestretch`), so
//! speeding up speech doesn't chipmunk it.
//!
//! rodio's own position tracking counts *output* samples, which at 1.5× no
//! longer equals the book position. So this source publishes the true book
//! position (from *input* frames consumed) through a shared [`Controls`] handle
//! that the engine reads, and it performs its own seeking. rodio's `Player` is
//! used only for play/pause/volume around it.
//!
//! Multi-file books assume all parts share a sample rate and channel count
//! (true for a normally-encoded audiobook); the first file sets both.

use std::fs::File;
use std::io::BufReader;
use std::num::{NonZeroU16, NonZeroU32};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rodio::Source;
use rodio::source::SeekError;
use timestretch::{StreamProcessor, StretchParams};

const NO_SEEK: i64 = -1;
const RELAXED: Ordering = Ordering::Relaxed;

type FileDecoder = rodio::Decoder<BufReader<File>>;

/// Shared, thread-safe control + status surface between the engine (UI thread)
/// and the source (rodio's audio thread).
pub struct Controls {
    input_rate: u32,
    position_frames: AtomicU64,
    speed_bits: AtomicU32,
    seek_frames: AtomicI64,
}

impl Controls {
    pub fn speed(&self) -> f32 {
        f32::from_bits(self.speed_bits.load(RELAXED))
    }

    pub fn set_speed(&self, speed: f32) {
        self.speed_bits.store(speed.clamp(0.25, 4.0).to_bits(), RELAXED);
    }

    pub fn position(&self) -> Duration {
        Duration::from_secs_f64(self.position_frames.load(RELAXED) as f64 / self.input_rate as f64)
    }

    pub fn request_seek(&self, pos: Duration) {
        let frames = (pos.as_secs_f64() * self.input_rate as f64).max(0.0) as i64;
        self.seek_frames.store(frames, RELAXED);
        // Reflect the target immediately so the UI updates even while paused
        // (the source only applies the seek on its next pull). The source
        // re-affirms this same value once it actually seeks.
        self.position_frames.store(frames as u64, RELAXED);
    }

    fn take_seek(&self) -> Option<u64> {
        let v = self.seek_frames.swap(NO_SEEK, RELAXED);
        (v >= 0).then_some(v as u64)
    }
}

/// An ordered set of files played back to back, with global seeking.
struct MultiFeed {
    files: Vec<PathBuf>,
    starts: Vec<Duration>,
    total: Duration,
    idx: usize,
    decoder: Option<FileDecoder>,
    channels: u16,
    sample_rate: u32,
}

fn open_decoder(path: &std::path::Path) -> Result<FileDecoder> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    rodio::Decoder::try_from(file).with_context(|| format!("decoding {}", path.display()))
}

impl MultiFeed {
    fn open(files: Vec<(PathBuf, Duration)>) -> Result<Self> {
        ensure!(!files.is_empty(), "no files to play");
        let mut paths = Vec::with_capacity(files.len());
        let mut starts = Vec::with_capacity(files.len());
        let mut acc = Duration::ZERO;
        for (path, dur) in files {
            starts.push(acc);
            acc += dur;
            paths.push(path);
        }
        let first = open_decoder(&paths[0])?;
        let channels = first.channels().get();
        let sample_rate = first.sample_rate().get();
        Ok(Self {
            files: paths,
            starts,
            total: acc,
            idx: 0,
            decoder: Some(first),
            channels,
            sample_rate,
        })
    }

    fn next_sample(&mut self) -> Option<f32> {
        loop {
            if self.decoder.is_none() {
                if self.idx >= self.files.len() {
                    return None;
                }
                // Skip a file we can't open rather than ending the book.
                self.decoder = open_decoder(&self.files[self.idx]).ok();
                if self.decoder.is_none() {
                    self.idx += 1;
                    continue;
                }
            }
            match self.decoder.as_mut().unwrap().next() {
                Some(s) => return Some(s),
                None => {
                    self.idx += 1;
                    self.decoder = None;
                }
            }
        }
    }

    fn seek_global(&mut self, target: Duration) {
        let i = self.starts.iter().rposition(|&s| s <= target).unwrap_or(0);
        let within = target.saturating_sub(self.starts[i]);
        self.idx = i;
        self.decoder = open_decoder(&self.files[i]).ok();
        if let Some(d) = self.decoder.as_mut() {
            let _ = d.try_seek(within);
        }
    }
}

pub struct StretchSource {
    feed: MultiFeed,
    controls: Arc<Controls>,
    processor: StreamProcessor,
    input_rate: u32,
    channels: u16,
    inbuf: Vec<f32>,
    out: Vec<f32>,
    out_pos: usize,
    last_ratio: f64,
    finished: bool,
}

impl StretchSource {
    /// Open an ordered list of `(file, duration)` as one playable book, plus a
    /// shared control handle. A single-file book passes a one-element list.
    pub fn open_files(files: Vec<(PathBuf, Duration)>) -> Result<(Self, Arc<Controls>)> {
        let feed = MultiFeed::open(files)?;
        let input_rate = feed.sample_rate;
        let channels = feed.channels;

        let controls = Arc::new(Controls {
            input_rate,
            position_frames: AtomicU64::new(0),
            speed_bits: AtomicU32::new(1.0f32.to_bits()),
            seek_frames: AtomicI64::new(NO_SEEK),
        });
        let processor = new_processor(1.0, input_rate, channels);

        let source = Self {
            feed,
            controls: controls.clone(),
            processor,
            input_rate,
            channels,
            inbuf: Vec::new(),
            out: Vec::new(),
            out_pos: 0,
            last_ratio: 1.0,
            finished: false,
        };
        Ok((source, controls))
    }

    fn ratio(&self) -> f64 {
        (1.0 / self.controls.speed() as f64).clamp(0.25, 4.0)
    }

    fn refill(&mut self) {
        self.out.clear();
        self.out_pos = 0;

        if let Some(frames) = self.controls.take_seek() {
            let pos = Duration::from_secs_f64(frames as f64 / self.input_rate as f64);
            self.feed.seek_global(pos);
            self.processor = new_processor(self.ratio(), self.input_rate, self.channels);
            self.last_ratio = self.ratio();
            self.controls.position_frames.store(frames, RELAXED);
        }

        let ratio = self.ratio();
        if (ratio - self.last_ratio).abs() > 1e-6 {
            let _ = self.processor.set_stretch_ratio(ratio);
            self.last_ratio = ratio;
        }

        let frame = self.channels as usize;
        const CHUNK_FRAMES: usize = 2048;
        loop {
            self.inbuf.clear();
            for _ in 0..CHUNK_FRAMES * frame {
                match self.feed.next_sample() {
                    Some(s) => self.inbuf.push(s),
                    None => break,
                }
            }

            if self.inbuf.is_empty() {
                let _ = self.processor.flush_into(&mut self.out);
                self.finished = true;
                return;
            }

            let frames_read = (self.inbuf.len() / frame.max(1)) as u64;
            self.controls.position_frames.fetch_add(frames_read, RELAXED);

            self.out.reserve(self.inbuf.len() * 2 + 64);
            let _ = self.processor.process_into(&self.inbuf, &mut self.out);
            if !self.out.is_empty() {
                return;
            }
        }
    }
}

impl Iterator for StretchSource {
    type Item = f32;

    #[inline]
    fn next(&mut self) -> Option<f32> {
        if self.out_pos >= self.out.len() {
            if self.finished {
                return None;
            }
            self.refill();
            if self.out.is_empty() {
                return None;
            }
        }
        let s = self.out[self.out_pos];
        self.out_pos += 1;
        Some(s)
    }
}

impl Source for StretchSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        NonZeroU16::new(self.channels.max(1)).unwrap()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        NonZeroU32::new(self.input_rate.max(1)).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        Some(self.feed.total)
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.controls.request_seek(pos);
        Ok(())
    }
}

fn new_processor(ratio: f64, sample_rate: u32, channels: u16) -> StreamProcessor {
    StreamProcessor::new(
        StretchParams::new(ratio)
            .with_sample_rate(sample_rate)
            .with_channels(channels as u32),
    )
}
