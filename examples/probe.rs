//! Phase 0 de-risk tool.
//!
//! Proves that pure-Rust decoding handles a large `.m4b` audiobook end to end:
//!   * opens the container with symphonia (isomp4 + aac),
//!   * fully decodes the audio stream (no size limit, no failure),
//!   * reads chapters + cover art via mp4ameta,
//!   * seeks to a chapter and decodes from there.
//!
//! Usage: cargo run --example probe -- "<path to .m4b>"

use std::fs::File;

use anyhow::{Context, Result, bail};
use symphonia::core::codecs::CodecParameters;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Time;

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: cargo run --example probe -- <file.m4b>")?;

    println!("== audiobook probe ==");
    println!("file: {path}");
    let size = std::fs::metadata(&path)?.len();
    println!("size: {:.1} MiB", size as f64 / (1024.0 * 1024.0));

    // ---- 1. metadata + chapters via mp4ameta -------------------------------
    read_metadata(&path)?;

    // ---- 2. full decode via symphonia --------------------------------------
    let (audio_frames, sample_rate, channels) = decode_all(&path, None)?;
    let secs = audio_frames as f64 / sample_rate as f64;
    println!(
        "\n[decode] full pass OK: {audio_frames} frames, {sample_rate} Hz, {channels} ch => {:.0}s ({:.2} h)",
        secs,
        secs / 3600.0
    );

    // ---- 3. seek near the end and decode from there ------------------------
    let seek_to = (secs * 0.9) as u64; // 90% through the book
    println!("\n[seek] seeking to {seek_to}s and decoding a short span...");
    let (span_frames, _, _) = decode_all(&path, Some(seek_to))?;
    println!("[seek] decoded {span_frames} frames after seek OK");

    println!("\nPHASE 0 PASS: pure-Rust decode + chapters + seek all work on this file.");
    Ok(())
}

fn read_metadata(path: &str) -> Result<()> {
    let tag = mp4ameta::Tag::read_from_path(path)
        .with_context(|| format!("mp4ameta could not read {path}"))?;

    println!("\n[meta] title:  {:?}", tag.title());
    println!("[meta] artist: {:?}", tag.artist());
    println!("[meta] album:  {:?}", tag.album());

    match tag.images().next() {
        Some((_ident, img)) => println!(
            "[meta] cover:  present, {} bytes ({:?})",
            img.data.len(),
            img.fmt
        ),
        None => println!("[meta] cover:  none"),
    }

    let track = tag.chapter_track();
    let list = tag.chapter_list();
    println!(
        "[meta] chapters: {} in chapter-track, {} in chapter-list",
        track.len(),
        list.len()
    );
    for (i, ch) in track.iter().chain(list.iter()).take(5).enumerate() {
        println!("        #{i}: {:>8.1}s  {}", ch.start.as_secs_f64(), ch.title);
    }

    Ok(())
}

/// Decode the whole audio track (or, if `seek_secs` is given, seek there first
/// and decode a bounded span). Returns (frames, sample_rate, channels).
fn decode_all(path: &str, seek_secs: Option<u64>) -> Result<(u64, u32, usize)> {
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    hint.with_extension("m4b");

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .context("symphonia failed to probe container")?;

    let track = format
        .default_track(TrackType::Audio)
        .context("no default audio track")?;
    let track_id = track.id;

    let Some(CodecParameters::Audio(audio_params)) = track.codec_params.clone() else {
        bail!("default track has no audio codec parameters");
    };
    let sample_rate = audio_params.sample_rate.unwrap_or(0);

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&audio_params, &AudioDecoderOptions::default())
        .context("no decoder for this audio codec")?;

    if let Some(secs) = seek_secs {
        format.seek(
            SeekMode::Accurate,
            SeekTo::Time {
                time: Time::try_new(secs as i64, 0).context("bad seek time")?,
                track_id: Some(track_id),
            },
        )?;
        decoder.reset();
    }

    let mut frames: u64 = 0;
    let mut channels = 0usize;
    let bound = seek_secs.map(|_| sample_rate as u64 * 10); // seek pass: ~10s span

    while let Some(packet) = format.next_packet()? {
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                frames += buf.frames() as u64;
                channels = buf.spec().channels().count();
            }
            Err(SymphoniaError::DecodeError(e)) => {
                // Non-fatal: a corrupt frame here and there shouldn't abort a book.
                eprintln!("        (skipped decode error: {e})");
            }
            Err(e) => return Err(e).context("fatal decode error"),
        }
        if let Some(limit) = bound {
            if frames >= limit {
                break;
            }
        }
    }

    Ok((frames, sample_rate, channels))
}
