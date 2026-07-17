//! Phase 1 verification REPL: drive the playback engine from the terminal.
//!
//! Usage: cargo run --example play -- "<file.m4b>"
//!
//! Commands (enter after each):
//!   <enter> / p   play or pause          f   forward 30s      r   back 30s
//!   n             next chapter           b   previous chapter
//!   c <n>         jump to chapter n      s <secs>   seek to absolute seconds
//!   + / -         speed up / down 0.1x   i   print position + chapter
//!   q             quit

use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use some_audiobook_player::audio::PlaybackEngine;
use some_audiobook_player::library::db::{Database, Sort};
use some_audiobook_player::library::{BookMeta, Chapter, scan};

fn main() -> Result<()> {
    let path = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("usage: cargo run --example play -- <file.m4b>")?,
    );

    let mut engine = PlaybackEngine::new()?;
    if path.is_dir() {
        // Multi-file book: import into an in-memory library and load its tracks.
        let db = Database::open_in_memory()?;
        scan::scan_and_import(&db, &path);
        let book = db
            .list_books(Sort::Title)?
            .into_iter()
            .next()
            .context("no book found in folder")?;
        let tracks = db.tracks(book.id)?;
        let files = tracks.iter().map(|t| (t.path.clone(), t.duration)).collect();
        let chapters = tracks
            .iter()
            .map(|t| Chapter {
                index: t.idx,
                title: t.title.clone(),
                start: t.start,
            })
            .collect();
        let meta = BookMeta {
            title: Some(book.title.clone()),
            author: book.author.clone(),
            reader: book.reader.clone(),
            album: None,
            duration: Some(book.duration),
            chapters,
            cover: book.cover.clone(),
        };
        engine.load(files, book.path.clone(), meta)?;
    } else {
        engine.load_path(&path)?;
    }

    let book = engine.current().unwrap();
    println!("Loaded: {}", book.meta.display_title());
    if let Some(a) = &book.meta.author {
        println!("Author: {a}");
    }
    println!("Chapters ({}):", book.meta.chapters.len());
    for ch in &book.meta.chapters {
        println!("  {:>2}  {:>8}  {}", ch.index, fmt(ch.start), ch.title);
    }
    println!("\nType a command (h for help, q to quit). Starting paused.");
    engine.play();
    println!("Playing.");

    let stdin = io::stdin();
    let mut speed = 1.0f32;
    for line in stdin.lock().lines() {
        let line = line?;
        let mut parts = line.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        match cmd {
            "" | "p" => {
                engine.toggle();
                println!("{}", if engine.is_paused() { "Paused" } else { "Playing" });
            }
            "f" => rep(engine.seek_relative(30)),
            "r" => rep(engine.seek_relative(-30)),
            "n" => rep(engine.next_chapter()),
            "b" => rep(engine.prev_chapter()),
            "c" => match parts.next().and_then(|n| n.parse::<usize>().ok()) {
                Some(n) => rep(engine.seek_chapter(n)),
                None => println!("usage: c <chapter-index>"),
            },
            "s" => match parts.next().and_then(|n| n.parse::<u64>().ok()) {
                Some(secs) => rep(engine.seek(Duration::from_secs(secs))),
                None => println!("usage: s <seconds>"),
            },
            "+" => {
                speed = (speed + 0.1).min(3.0);
                engine.set_speed(speed);
                println!("speed {speed:.1}x");
            }
            "-" => {
                speed = (speed - 0.1).max(0.5);
                engine.set_speed(speed);
                println!("speed {speed:.1}x");
            }
            "i" => print_status(&engine),
            "h" => println!(
                "commands: <enter>/p pause  f fwd30  r back30  n next  b prev  c <n>  s <secs>  +/- speed  i info  q quit"
            ),
            "q" => break,
            other => println!("unknown command: {other:?} (h for help)"),
        }
        io::stdout().flush().ok();
    }
    Ok(())
}

fn print_status(engine: &PlaybackEngine) {
    let pos = engine.position();
    let ch = engine
        .current_chapter()
        .and_then(|i| engine.current().unwrap().meta.chapters.get(i));
    match ch {
        Some(c) => println!("pos {}  |  chapter {} \"{}\"", fmt(pos), c.index, c.title),
        None => println!("pos {}", fmt(pos)),
    }
}

fn rep(r: Result<()>) {
    match r {
        Ok(()) => {}
        Err(e) => println!("error: {e}"),
    }
}

/// Format a duration as H:MM:SS.
fn fmt(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}
