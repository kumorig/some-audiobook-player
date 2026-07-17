//! Phase 3 verification: scan a folder, import into SQLite, print the report,
//! then prove resume-position persistence.
//!
//! Usage: cargo run --example scan -- "<folder>"

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use some_audiobook_player::library::db::{Database, Sort};
use some_audiobook_player::library::scan;

fn main() -> Result<()> {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("usage: cargo run --example scan -- <folder>")?,
    );

    // Target a persistent DB when LIBRARY_DB is set (also a handy bulk-import
    // CLI); otherwise a throwaway file so we can also test persistence.
    let persistent = std::env::var_os("LIBRARY_DB").is_some();
    let db_path = std::env::var_os("LIBRARY_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("audiobook-scan-test.sqlite"));
    if !persistent {
        std::fs::remove_file(&db_path).ok();
    }
    let db = Database::open(&db_path)?;
    db.add_storage_location(&root)?;

    println!("Scanning {} ...\n", root.display());
    let report = scan::scan_and_import(&db, &root);
    println!("Import: {}", report.summary());
    for (path, reason) in &report.failed {
        println!("  FAILED  {}\n          -> {reason}", path.display());
    }

    let books = db.list_books(Sort::Title)?;
    println!("\nLibrary ({} books):", books.len());
    for b in &books {
        println!(
            "  [{}] {:<45}  {:>9}  {} ch  by {}",
            b.id,
            truncate(&b.title, 45),
            fmt_hms(b.duration.as_secs()),
            b.chapter_count,
            b.author.as_deref().unwrap_or("?"),
        );
    }

    // Persistence check: set a resume position on the first book, reopen the DB.
    if let Some(first) = books.first().filter(|_| !persistent) {
        db.update_position(first.id, Duration::from_secs(1234), false)?;
        drop(db);
        let db2 = Database::open(&db_path)?;
        let reloaded = db2.get_book(first.id)?.context("book vanished")?;
        println!(
            "\nPersistence: book {} resume position = {} ({:.0}% done) after reopen",
            reloaded.id,
            fmt_hms(reloaded.position.as_secs()),
            reloaded.progress_fraction() * 100.0,
        );
    }

    if !persistent {
        std::fs::remove_file(&db_path).ok();
    } else {
        println!("\nSaved to {}", db_path.display());
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

fn fmt_hms(s: u64) -> String {
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}
