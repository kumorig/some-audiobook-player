# Some Audiobook Player

A pure-Rust audiobook player, built as a native replacement for
[Cozy](https://github.com/geigi/cozy).

## Why

Cozy (GTK/Python/GStreamer) reports **"some files could not be imported"** on
large `.m4b` audiobooks. This player decodes everything in pure Rust —
[symphonia](https://github.com/pdeljanov/Symphonia) for AAC/MP4 (via
[rodio](https://github.com/RustAudio/rodio)), with [mp4ameta] for chapters and
[lofty] for other formats — so there is no GStreamer dependency and full control
over the fragile import step. Import **tells you which files failed and why**,
instead of a vague blanket error.

[mp4ameta]: https://github.com/Saecki/rust-mp4ameta
[lofty]: https://github.com/Serial-ATA/lofty-rs

## Features

- **Formats**: `.m4b`/`.m4a`/`.mp4` (AAC & ALAC, with embedded chapters), plus
  `.mp3`, `.flac`, `.ogg`, `.wav`. Folders of per-chapter files are grouped and
  played as a single book.
- **Dolphin-style library**: Icons / Compact / Details layouts (icon toggles),
  **Ctrl+scroll to zoom**, sort, and search.
- **Browse by category**: Books, Authors, Readers (narrator, from the composer
  tag) — the latter two open a sliding sidebar to filter the grid.
- **Playback**: chapter list & navigation, ±30 s skip, draggable seek bar, and
  resume-where-you-left-off (rewound a few seconds).
- **Mini-player**: while browsing the library a compact transport stays visible;
  click it to jump to the full player without interrupting playback.
- **My History**: rate and note any book; entries store a copy of the cover and
  **survive the files being removed**. A month-by-month timeline lets you look
  back, with editable notes and listened-dates. Finished books are added
  automatically; you can also add/remove entries by hand.
- **Sleep timer** (10/15/30/45/60 min or end-of-chapter), with an optional
  **Shutdown** that powers the machine off when the timer fires — after a
  10-second countdown you can cancel.
- **Bookmarks**, and unobtrusive **corner toasts** for status messages.
- **Desktop integration**: full **MPRIS** support — media keys, GNOME/KDE
  now-playing card (title, author, cover), and lock-screen controls.

## Build & run

```sh
cargo run --release                 # opens the library window
cargo run --release -- book.m4b     # opens straight into a book
```

The library database lives at
`~/.local/share/some-audiobook-player/library.sqlite`.

### Developer tools (examples)

```sh
cargo run --example probe -- book.m4b     # decode/chapters/seek diagnostics
cargo run --example play  -- book.m4b     # terminal playback REPL
LIBRARY_DB=~/.local/share/some-audiobook-player/library.sqlite \
  cargo run --example scan -- ~/Audiobooks  # bulk-import a folder (no GUI)
```

## Install (Linux)

```sh
cargo build --release
install -Dm755 target/release/some-audiobook-player ~/.local/bin/some-audiobook-player
install -Dm644 packaging/some-audiobook-player.desktop \
  ~/.local/share/applications/some-audiobook-player.desktop
install -Dm644 packaging/some-audiobook-player.svg \
  ~/.local/share/icons/hicolor/scalable/apps/some-audiobook-player.svg
update-desktop-database ~/.local/share/applications 2>/dev/null || true
```

## Architecture

```
src/
  main.rs            eframe entry point
  app.rs             egui UI: library (Books/Authors/Readers/History) <-> player
  toast.rs           corner toast notifications
  icons.rs           vendored Phosphor icon font
  mpris.rs           MPRIS D-Bus server (own thread, async-io event loop)
  audio/
    mod.rs           PlaybackEngine: rodio Player for play/pause/volume
    source.rs        StretchSource: symphonia decode, publishes book position
                     via shared Controls
  library/
    mod.rs           domain types (Book, Chapter, Bookmark, Cover, HistoryEntry)
    meta.rs          metadata: mp4ameta (chapters) / lofty (other formats)
    scan.rs          recursive import with per-file failure reasons
    db.rs            SQLite (rusqlite): books, tracks, bookmarks, history, …
```

## Known limitations / future work

- HE-AAC / SBR `.m4b` files rely on symphonia's limited SBR support.
- The MPRIS `Position` property updates via `Seeked` signals rather than
  continuous polling.
- The **Shutdown** option uses `systemctl poweroff`, which relies on the active
  local session being permitted to power off without authentication (the default
  on most systemd desktops).

## Contributing

Contributions are welcome, including AI-assisted ones — please just review and
test your changes first. See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Dual-licensed under either of

- MIT license ([LICENSE-MIT](LICENSE-MIT))
- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))

at your option. Unless you explicitly state otherwise, any contribution you
submit for inclusion shall be dual-licensed as above, without any additional
terms or conditions.
