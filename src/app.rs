//! egui front-end (Phase 3): a Library grid and a Player view over a SQLite
//! library, wired to the playback engine.
//!
//! Targets the redesigned egui 0.35 API where `eframe::App` provides a root
//! `Ui`, panels mount *inside* a `Ui`, and the context is `ui.ctx()`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;

use some_audiobook_player::audio::PlaybackEngine;
use some_audiobook_player::library::db::{Database, Sort};
use some_audiobook_player::library::{Book, BookMeta, Bookmark, Chapter, Cover, HistoryEntry, scan};

use crate::icons;
use crate::mpris::{Mpris, MprisCommand, MprisUpdate, NowPlaying};
use crate::toast::Toasts;

/// On resume, step back a few seconds so you re-hear context (like Cozy).
const RESUME_REWIND: Duration = Duration::from_secs(8);
/// Grace period before the sleep-timer shutdown actually powers off.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(10);
/// How often to persist the playback position while playing.
const SAVE_EVERY: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Library,
    Player,
}

/// Sleep-timer mode.
#[derive(Clone, Copy, PartialEq)]
enum Sleep {
    Off,
    /// Pause at this instant.
    Until(Instant),
    /// Pause once we advance past this chapter index.
    EndOfChapter(usize),
}

/// Library layout, à la Dolphin: cover grid, compact list, or details list.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LibView {
    Icons,
    Compact,
    Details,
}

/// Default library item size (grid card width); Ctrl+scroll zooms around it.
const ZOOM_DEFAULT: f32 = 150.0;
const ZOOM_MIN: f32 = 90.0;
const ZOOM_MAX: f32 = 320.0;

impl LibView {
    fn label(self) -> &'static str {
        match self {
            LibView::Icons => "Icons",
            LibView::Compact => "Compact",
            LibView::Details => "Details",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            LibView::Icons => icons::SQUARES_FOUR,
            LibView::Compact => icons::ROWS,
            LibView::Details => icons::LIST_BULLETS,
        }
    }
    fn as_key(self) -> &'static str {
        match self {
            LibView::Icons => "icons",
            LibView::Compact => "compact",
            LibView::Details => "details",
        }
    }
    fn from_key(s: &str) -> Self {
        match s {
            "compact" => LibView::Compact,
            "details" => LibView::Details,
            // "grid" is the pre-rename key for the icon view.
            _ => LibView::Icons,
        }
    }
}

/// Library browsing category: all books, grouped by author or reader, or the
/// "My History" timeline.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Category {
    Books,
    Authors,
    Readers,
    History,
}

impl Category {
    fn label(self) -> &'static str {
        match self {
            Category::Books => "Books",
            Category::Authors => "Authors",
            Category::Readers => "Readers",
            Category::History => "My History",
        }
    }
    fn icon(self) -> &'static str {
        match self {
            Category::Books => icons::BOOKS,
            Category::Authors => icons::USERS,
            Category::Readers => icons::MICROPHONE,
            Category::History => icons::CLOCK_COUNTER_CLOCKWISE,
        }
    }
    fn as_key(self) -> &'static str {
        match self {
            Category::Books => "books",
            Category::Authors => "authors",
            Category::Readers => "readers",
            Category::History => "history",
        }
    }
    fn from_key(s: &str) -> Self {
        match s {
            "authors" => Category::Authors,
            "readers" => Category::Readers,
            "history" => Category::History,
            _ => Category::Books,
        }
    }
}

/// Deferred UI actions, applied after the panels are drawn to keep borrows sane.
enum Action {
    AddFolder(PathBuf),
    OpenBook(i64),
    ShowPlayer,
    Rescan,
    Back,
    SetSort(Sort),
    AddBookmark,
    JumpTo(Duration),
    DeleteBookmark(i64),
    SetSleep(Sleep),
    RateBook(u8),
    SetNote(String),
    AddToHistory,
    AddBookToHistory(i64),
    RemoveHistory(i64),
    RateHistory(i64, u8),
    SetHistoryNote(i64, String),
    SetHistoryDate(i64, i64),
}

/// The way a library item was interacted with.
enum Hit {
    /// Left-clicked — open the book in the player.
    Open,
    /// Chosen from the right-click menu — add to My History.
    AddToHistory,
}

pub struct PlayerApp {
    db: Database,
    engine: Option<PlaybackEngine>,
    view: View,

    // Library state.
    books: Vec<Book>,
    sort: Sort,
    search: String,
    category: Category,
    selected_author: Option<String>,
    selected_reader: Option<String>,
    lib_view: LibView,
    /// Item size for the library grid/rows (Ctrl+scroll to zoom).
    lib_zoom: f32,
    covers: HashMap<i64, Option<egui::TextureHandle>>,
    failures: Vec<(PathBuf, String)>,

    // My History.
    history: Vec<HistoryEntry>,
    history_covers: HashMap<i64, Option<egui::TextureHandle>>,
    /// Per-entry text buffers for inline note/date editing in the timeline.
    hist_note_edit: HashMap<i64, String>,
    hist_date_edit: HashMap<i64, String>,

    // Player state.
    current_book_id: Option<i64>,
    player_cover: Option<egui::TextureHandle>,
    bookmarks: Vec<Bookmark>,
    /// History entry for the loaded book (rating/note shown in the player).
    current_history: Option<HistoryEntry>,
    note_edit: String,
    /// Set once we've auto-added the loaded book to history on finishing it.
    history_pushed: bool,
    sleep: Sleep,
    /// When true, the sleep timer powers the machine off instead of just pausing.
    shutdown_on_sleep: bool,
    /// Set when a shutdown is pending; the user can cancel until this instant.
    shutdown_at: Option<Instant>,
    /// Last observed playback position, used to spot manual seeks/track changes
    /// so an "end of chapter" sleep timer isn't tripped by them.
    last_pos: Option<Duration>,
    scrub: Option<f64>,
    last_save: Instant,

    // Desktop (MPRIS) integration.
    mpris: Mpris,
    last_mpris_playing: Option<bool>,
    last_pos_push: Instant,

    status: String,
    /// Transient corner notifications (rescan results, "added to history", …).
    toasts: Toasts,
    pending_open: Option<PathBuf>,
}

impl PlayerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        crate::icons::install(&cc.egui_ctx);
        let (db, status) = match open_library_db() {
            Ok(db) => (db, "Ready.".to_owned()),
            Err(e) => (
                Database::open_in_memory().expect("in-memory sqlite"),
                format!("Using a temporary library (couldn't open on disk: {e})"),
            ),
        };
        let sort = Sort::RecentlyPlayed;
        let books = db.list_books(sort).unwrap_or_default();
        let lib_view = db
            .get_setting("lib_view")
            .map(|s| LibView::from_key(&s))
            .unwrap_or(LibView::Icons);
        let lib_zoom = db
            .get_setting("lib_zoom")
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(ZOOM_DEFAULT)
            .clamp(ZOOM_MIN, ZOOM_MAX);
        let category = db
            .get_setting("category")
            .map(|s| Category::from_key(&s))
            .unwrap_or(Category::Books);
        let history = db.list_history().unwrap_or_default();
        Self {
            db,
            engine: None,
            view: View::Library,
            books,
            sort,
            search: String::new(),
            category,
            selected_author: None,
            selected_reader: None,
            lib_view,
            lib_zoom,
            covers: HashMap::new(),
            failures: Vec::new(),
            history,
            history_covers: HashMap::new(),
            hist_note_edit: HashMap::new(),
            hist_date_edit: HashMap::new(),
            current_book_id: None,
            player_cover: None,
            bookmarks: Vec::new(),
            current_history: None,
            note_edit: String::new(),
            history_pushed: false,
            sleep: Sleep::Off,
            shutdown_on_sleep: false,
            shutdown_at: None,
            last_pos: None,
            scrub: None,
            last_save: Instant::now(),
            mpris: Mpris::spawn(),
            last_mpris_playing: None,
            last_pos_push: Instant::now(),
            status,
            toasts: Toasts::default(),
            pending_open: initial,
        }
    }

    /// Process desktop media-key commands and push status/position to MPRIS.
    fn sync_mpris(&mut self, ctx: &egui::Context) {
        while let Some(cmd) = self.mpris.poll_command() {
            match (self.engine.as_ref(), cmd) {
                (_, MprisCommand::Raise) => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                (Some(engine), MprisCommand::PlayPause) => engine.toggle(),
                (Some(engine), MprisCommand::Play) => engine.play(),
                (Some(engine), MprisCommand::Pause | MprisCommand::Stop) => engine.pause(),
                (Some(engine), MprisCommand::Next) => {
                    let _ = engine.next_chapter();
                }
                (Some(engine), MprisCommand::Prev) => {
                    let _ = engine.prev_chapter();
                }
                (Some(engine), MprisCommand::SeekBy(secs)) => {
                    let _ = engine.seek_relative(secs);
                }
                (Some(engine), MprisCommand::SeekTo(secs)) => {
                    let _ = engine.seek(Duration::from_secs(secs.max(0) as u64));
                }
                (None, _) => {}
            }
        }

        let playing = self
            .engine
            .as_ref()
            .is_some_and(|e| e.current().is_some() && !e.is_paused());
        if self.last_mpris_playing != Some(playing) {
            self.mpris.push(MprisUpdate::Status { playing });
            self.last_mpris_playing = Some(playing);
        }
        if playing && self.last_pos_push.elapsed() >= Duration::from_secs(1) {
            if let Some(engine) = self.engine.as_ref() {
                self.mpris.push(MprisUpdate::Position(engine.position()));
            }
            self.last_pos_push = Instant::now();
        }
    }

    /// Publish the current book's now-playing metadata to the desktop.
    fn push_now_playing(&self) {
        if let Some(book) = self.engine.as_ref().and_then(|e| e.current()) {
            let m = &book.meta;
            self.mpris.push(MprisUpdate::NowPlaying(NowPlaying {
                title: m.display_title().to_string(),
                artist: m.author.clone().unwrap_or_default(),
                album: m.album.clone().unwrap_or_default(),
                length: m.duration.unwrap_or_default(),
                position: self.engine.as_ref().map(|e| e.position()).unwrap_or_default(),
                art_url: m.cover.as_ref().and_then(write_cover_tmp),
            }));
        }
    }

    fn refresh_books(&mut self) {
        self.books = self.db.list_books(self.sort).unwrap_or_default();
    }

    fn ensure_engine(&mut self) -> Result<&mut PlaybackEngine, String> {
        if self.engine.is_none() {
            match PlaybackEngine::new() {
                Ok(e) => self.engine = Some(e),
                Err(e) => return Err(format!("Audio error: {e}")),
            }
        }
        Ok(self.engine.as_mut().unwrap())
    }

    /// Import a folder tree and refresh the library, surfacing any failures.
    fn add_folder(&mut self, path: &Path) {
        let _ = self.db.add_storage_location(path);
        let report = scan::scan_and_import(&self.db, path);
        self.toasts.success(report.summary());
        self.failures = report.failed;
        self.refresh_books();
    }

    /// Re-scan every registered storage location.
    fn rescan_all(&mut self) {
        let roots = self.db.storage_locations().unwrap_or_default();
        let mut imported = 0;
        self.failures.clear();
        for root in &roots {
            let report = scan::scan_and_import(&self.db, root);
            imported += report.imported;
            self.failures.extend(report.failed);
        }
        self.toasts.info(format!(
            "Rescanned {} location(s): {imported} new, {} failed",
            roots.len(),
            self.failures.len()
        ));
        self.refresh_books();
    }

    /// Import a single file (ad-hoc "Open file…") then play it.
    fn open_file(&mut self, ctx: &egui::Context, path: &Path) {
        match scan::import_file(&self.db, path) {
            Ok(id) => {
                self.refresh_books();
                self.open_book(ctx, id);
            }
            Err(e) => self.status = format!("Could not open: {e}"),
        }
    }

    /// Load a library book into the engine, resuming where we left off.
    fn open_book(&mut self, ctx: &egui::Context, id: i64) {
        // Already the loaded book — just show the player and keep playing.
        // (Reloading would clear + pause the live stream, stopping playback.)
        if self.current_book_id == Some(id)
            && self.engine.as_ref().and_then(|e| e.current()).is_some()
        {
            self.view = View::Player;
            self.status = String::new();
            return;
        }

        let book = match self.db.get_book(id) {
            Ok(Some(b)) => b,
            Ok(None) => {
                self.status = "That book is no longer in the library.".to_owned();
                return;
            }
            Err(e) => {
                self.status = format!("Database error: {e}");
                return;
            }
        };

        if let Err(e) = self.ensure_engine() {
            self.status = e;
            return;
        }

        // Build the load request (tracks are read before borrowing the engine).
        let loaded = if book.is_multi {
            let tracks = self.db.tracks(id).unwrap_or_default();
            if tracks.is_empty() {
                self.status = "This book has no tracks.".to_owned();
                return;
            }
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
            self.engine.as_mut().unwrap().load(files, book.path.clone(), meta)
        } else {
            self.engine.as_mut().unwrap().load_path(&book.path)
        };
        if let Err(e) = loaded {
            self.status = format!("Could not open {}: {e}", book.path.display());
            return;
        }
        let engine = self.engine.as_mut().unwrap();

        // Resume, rewound a little for context.
        let resume = book.position.saturating_sub(RESUME_REWIND);
        if resume > Duration::ZERO {
            let _ = engine.seek(resume);
        }
        engine.set_speed(1.0);

        self.player_cover = engine
            .current()
            .and_then(|b| b.meta.cover.as_ref())
            .and_then(|c| load_cover(ctx, c));
        self.current_book_id = Some(id);
        self.last_pos = None;
        self.bookmarks = self.db.bookmarks(id).unwrap_or_default();
        self.load_current_history(&book);
        self.history_pushed = false;
        self.sleep = Sleep::Off;
        self.last_save = Instant::now();
        self.last_mpris_playing = None;
        self.push_now_playing();
        self.view = View::Player;
        self.status = String::new();
    }

    /// Load the My-History entry (rating/note) for the loaded book, if any.
    fn load_current_history(&mut self, book: &Book) {
        self.current_history = self
            .db
            .history_for_key(&book.path.to_string_lossy())
            .unwrap_or_default();
        self.note_edit = self
            .current_history
            .as_ref()
            .map(|h| h.note.clone())
            .unwrap_or_default();
    }

    /// Reload the history list + drop cached history covers (after edits).
    fn refresh_history(&mut self) {
        self.history = self.db.list_history().unwrap_or_default();
        self.history_covers.clear();
    }

    /// Ensure the loaded book has a history entry, then return its id.
    fn ensure_history_entry(&mut self, manual: bool) -> Option<i64> {
        let id = self.current_book_id?;
        let book = self.db.get_book(id).ok().flatten()?;
        if self.current_history.is_none() {
            let _ = self.db.upsert_history_from_book(&book, false, manual);
            self.load_current_history(&book);
            self.refresh_history();
        }
        self.current_history.as_ref().map(|h| h.id)
    }

    /// Persist the current playback position.
    fn save_progress(&mut self) {
        if let (Some(engine), Some(id)) = (self.engine.as_ref(), self.current_book_id) {
            let pos = engine.position();
            let total = engine
                .current()
                .and_then(|b| b.meta.duration)
                .unwrap_or_default();
            let finished = total > Duration::ZERO && pos + Duration::from_secs(30) >= total;
            let _ = self.db.update_position(id, pos, finished);
            // Auto-add finished books to My History (once), copying the cover so
            // the entry survives the files later being removed.
            if finished && !self.history_pushed {
                if let Ok(Some(book)) = self.db.get_book(id) {
                    let _ = self.db.upsert_history_from_book(&book, true, false);
                }
                self.history_pushed = true;
            }
            self.last_save = Instant::now();
        }
    }

    /// Pause playback if the sleep timer has elapsed.
    fn enforce_sleep(&mut self) {
        let fire = match self.sleep {
            Sleep::Off => false,
            Sleep::Until(t) => Instant::now() >= t,
            Sleep::EndOfChapter(ch) => self
                .engine
                .as_ref()
                .and_then(|e| e.current_chapter())
                .map(|c| c > ch)
                .unwrap_or(false),
        };
        if fire {
            if let Some(engine) = self.engine.as_ref() {
                engine.pause();
            }
            self.save_progress();
            self.sleep = Sleep::Off;
            if self.shutdown_on_sleep {
                // Start a grace period the user can cancel before we power off.
                self.shutdown_at = Some(Instant::now() + SHUTDOWN_GRACE);
            }
        }
    }

    /// Ask systemd-logind to power the machine off. On a normal desktop the
    /// active local user is allowed to do this without a password; if it's
    /// refused we surface why rather than failing silently.
    fn shutdown_now(&mut self) {
        let result = std::process::Command::new("systemctl")
            .arg("poweroff")
            .status();
        match result {
            Ok(s) if s.success() => {}
            Ok(s) => self
                .toasts
                .error(format!("Shutdown was refused (systemctl exited {s}).")),
            Err(e) => self
                .toasts
                .error(format!("Couldn't run shutdown: {e}")),
        }
    }

    /// While a shutdown is pending, show a centred countdown with a Cancel
    /// button; power off once the grace period elapses.
    fn shutdown_overlay(&mut self, ctx: &egui::Context) {
        let Some(at) = self.shutdown_at else {
            return;
        };
        let remaining = at.saturating_duration_since(Instant::now());
        if remaining == Duration::ZERO {
            self.shutdown_at = None;
            self.shutdown_now();
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(100));
        let mut cancel = false;
        egui::Area::new(egui::Id::new("shutdown_overlay"))
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .stroke(egui::Stroke::new(1.5, egui::Color32::from_rgb(0xDA, 0x54, 0x4F)))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::same(16))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.heading(format!(
                                "Shutting down in {}…",
                                remaining.as_secs() + 1
                            ));
                            ui.add_space(8.0);
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("  Cancel shutdown  ").size(16.0),
                                ))
                                .clicked()
                            {
                                cancel = true;
                            }
                        });
                    });
            });
        if cancel {
            self.shutdown_at = None;
            self.toasts.info("Shutdown cancelled.");
        }
    }

    fn add_bookmark(&mut self) {
        if let (Some(engine), Some(id)) = (self.engine.as_ref(), self.current_book_id) {
            let pos = engine.position();
            let label = engine
                .current_chapter()
                .and_then(|i| engine.current().unwrap().meta.chapters.get(i))
                .map(|c| c.title.clone())
                .unwrap_or_default();
            let _ = self.db.add_bookmark(id, pos, &label);
            self.bookmarks = self.db.bookmarks(id).unwrap_or_default();
        }
    }

    /// Ensure a cover texture exists for every listed book (decoded lazily).
    fn prepare_covers(&mut self, ctx: &egui::Context) {
        let missing: Vec<i64> = self
            .books
            .iter()
            .filter(|b| !self.covers.contains_key(&b.id))
            .map(|b| b.id)
            .collect();
        for id in missing {
            let tex = self
                .books
                .iter()
                .find(|b| b.id == id)
                .and_then(|b| b.cover.as_ref())
                .and_then(|c| load_cover(ctx, c));
            self.covers.insert(id, tex);
        }
    }
}

impl eframe::App for PlayerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Open a book passed on the command line (once).
        if let Some(path) = self.pending_open.take() {
            let ctx = ui.ctx().clone();
            self.open_file(&ctx, &path);
        }

        // Desktop media keys / now-playing.
        let ctx = ui.ctx().clone();
        self.sync_mpris(&ctx);

        // Watch for manual seeks / track changes (a discontinuous position jump)
        // so an "end of chapter" sleep timer follows the new chapter instead of
        // firing. Natural playback advances smoothly and never jumps.
        if let Some(pos) = self
            .engine
            .as_ref()
            .filter(|e| e.current().is_some())
            .map(|e| e.position())
        {
            if let Some(prev) = self.last_pos {
                let jumped = pos < prev || pos > prev + Duration::from_secs(2);
                if jumped {
                    if let Sleep::EndOfChapter(_) = self.sleep {
                        if let Some(cur) = self.engine.as_ref().and_then(|e| e.current_chapter()) {
                            self.sleep = Sleep::EndOfChapter(cur);
                        }
                    }
                }
            }
            self.last_pos = Some(pos);
        } else {
            self.last_pos = None;
        }

        // Periodically persist progress and keep the seek bar live while playing.
        // This runs regardless of the current view so the library's mini-player
        // stays live while browsing.
        let playing = self
            .engine
            .as_ref()
            .is_some_and(|e| e.current().is_some() && !e.is_paused());
        if playing {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
            if self.last_save.elapsed() >= SAVE_EVERY {
                self.save_progress();
            }
            self.enforce_sleep();
        }

        match self.view {
            View::Library => self.library_ui(ui),
            View::Player => self.player_ui(ui),
        }

        // A pending sleep-timer shutdown shows a cancelable countdown.
        self.shutdown_overlay(&ctx);

        // Corner toasts float above whichever view is showing.
        self.toasts.show(&ctx);
    }
}

impl PlayerApp {
    fn library_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        self.prepare_covers(&ctx);
        if self.category == Category::History {
            self.prepare_history_covers(&ctx);
        }

        let mut action: Option<Action> = None;

        egui::Panel::top("lib_top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .button(format!("{}  Add", icons::PLUS))
                    .on_hover_text("Add a folder of audiobooks")
                    .clicked()
                {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        action = Some(Action::AddFolder(dir));
                    }
                }
                if ui
                    .button(icons::ARROWS_CLOCKWISE)
                    .on_hover_text("Rescan the library")
                    .clicked()
                {
                    action = Some(Action::Rescan);
                }
                ui.separator();

                // Category switcher (Books / Authors / Readers / My History).
                for cat in [
                    Category::Books,
                    Category::Authors,
                    Category::Readers,
                    Category::History,
                ] {
                    let label = format!("{}  {}", cat.icon(), cat.label());
                    if ui.selectable_label(self.category == cat, label).clicked() && self.category != cat {
                        self.category = cat;
                        self.selected_author = None;
                        self.selected_reader = None;
                        self.db.set_setting("category", cat.as_key());
                        if cat == Category::History {
                            self.history = self.db.list_history().unwrap_or_default();
                            self.history_covers.clear();
                        }
                    }
                }

                // Search, layout and sort don't apply to the History timeline.
                if self.category != Category::History {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.search).desired_width(160.0));
                        ui.label(icons::MAGNIFYING_GLASS);
                        ui.separator();
                        // View-mode icon toggles (Icons / Compact / Details).
                        for v in [LibView::Details, LibView::Compact, LibView::Icons] {
                            if ui
                                .selectable_label(self.lib_view == v, v.icon())
                                .on_hover_text(v.label())
                                .clicked()
                            {
                                self.lib_view = v;
                                self.db.set_setting("lib_view", v.as_key());
                            }
                        }
                        ui.separator();
                        let mut sort = self.sort;
                        egui::ComboBox::from_id_salt("sort")
                            .selected_text(sort_label(sort))
                            .show_ui(ui, |ui| {
                                for s in [
                                    Sort::RecentlyPlayed,
                                    Sort::RecentlyAdded,
                                    Sort::Title,
                                    Sort::Author,
                                ] {
                                    ui.selectable_value(&mut sort, s, sort_label(s));
                                }
                            });
                        if sort != self.sort {
                            action = Some(Action::SetSort(sort));
                        }
                    });
                }
            });
            ui.add_space(4.0);
        });

        // Mini-player: visible whenever a book is loaded, so playback controls
        // (and a shortcut back to the player) stay available while browsing.
        if self.engine.as_ref().and_then(|e| e.current()).is_some() {
            egui::Panel::bottom("lib_miniplayer").show(ui, |ui| {
                self.mini_player(ui, &mut action);
            });
        }

        egui::Panel::bottom("lib_bottom").show(ui, |ui| {
            if !self.failures.is_empty() {
                ui.add_space(2.0);
                egui::CollapsingHeader::new(format!(
                    "{} file(s) could not be imported — why",
                    self.failures.len()
                ))
                .show(ui, |ui| {
                    for (path, reason) in &self.failures {
                        ui.label(format!("• {}", path.display()));
                        ui.label(egui::RichText::new(format!("    {reason}")).weak());
                    }
                });
                ui.add_space(2.0);
            }
        });

        // Sliding sidebar with the author/reader list (animates in and out).
        let mut sidebar_open = matches!(self.category, Category::Authors | Category::Readers);
        egui::Panel::left("lib_sidebar")
            .exact_size(210.0)
            .show_collapsible(ui, &mut sidebar_open, |ui| {
                self.sidebar_ui(ui);
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if self.category == Category::History {
                self.history_ui(ui, &mut action);
                return;
            }

            if self.books.is_empty() {
                ui.centered_and_justified(|ui| {
                    ui.label("Your library is empty. Use “Add” to import a folder of audiobooks.");
                });
                return;
            }

            // Ctrl+scroll zooms the item size (Dolphin-style).
            let (ctrl, dy) = ui.input(|i| (i.modifiers.ctrl, i.smooth_scroll_delta.y));
            if ctrl && dy != 0.0 {
                self.lib_zoom = (self.lib_zoom * (1.0 + dy * 0.0015)).clamp(ZOOM_MIN, ZOOM_MAX);
                self.db.set_setting("lib_zoom", &self.lib_zoom.to_string());
            }

            let needle = self.search.trim().to_lowercase();
            let by_author = self.selected_author.clone();
            let by_reader = self.selected_reader.clone();
            let visible = |b: &&Book| {
                (needle.is_empty() || book_matches(b, &needle))
                    && by_author
                        .as_ref()
                        .map(|a| &book_group_name(b, false) == a)
                        .unwrap_or(true)
                    && by_reader
                        .as_ref()
                        .map(|r| &book_group_name(b, true) == r)
                        .unwrap_or(true)
            };
            let view = self.lib_view;
            let zoom = self.lib_zoom;
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| match view {
                    LibView::Icons => {
                        ui.horizontal_wrapped(|ui| {
                            for book in self.books.iter().filter(visible) {
                                let tex = self.covers.get(&book.id).and_then(|t| t.as_ref());
                                if let Some(a) = hit_action(book_card(ui, book, tex, zoom), book.id) {
                                    action = Some(a);
                                }
                            }
                        });
                    }
                    LibView::Compact => {
                        ui.horizontal_wrapped(|ui| {
                            for book in self.books.iter().filter(visible) {
                                let tex = self.covers.get(&book.id).and_then(|t| t.as_ref());
                                if let Some(a) = hit_action(compact_item(ui, book, tex, zoom), book.id) {
                                    action = Some(a);
                                }
                            }
                        });
                    }
                    LibView::Details => {
                        for book in self.books.iter().filter(visible) {
                            let tex = self.covers.get(&book.id).and_then(|t| t.as_ref());
                            let h = details_row(ui, book, tex, (zoom * 0.32).clamp(32.0, 96.0));
                            if let Some(a) = hit_action(h, book.id) {
                                action = Some(a);
                            }
                        }
                    }
                });
        });

        if let Some(action) = action {
            self.apply(&ctx, action);
        }
    }

    /// The author/reader list shown in the sliding sidebar. Selecting a name
    /// filters the central grid; the currently selected name toggles off.
    fn sidebar_ui(&mut self, ui: &mut egui::Ui) {
        let reader = self.category == Category::Readers;
        let heading = if reader { "Readers" } else { "Authors" };
        ui.add_space(6.0);
        ui.heading(heading);
        ui.separator();

        let entries = group_counts(&self.books, reader);
        let total = self.books.len();
        let selected = if reader {
            &mut self.selected_reader
        } else {
            &mut self.selected_author
        };
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if ui.selectable_label(selected.is_none(), format!("All ({total})")).clicked() {
                    *selected = None;
                }
                for (name, count) in &entries {
                    let on = selected.as_deref() == Some(name.as_str());
                    if ui.selectable_label(on, format!("{name}  ({count})")).clicked() {
                        *selected = if on { None } else { Some(name.clone()) };
                    }
                }
            });
    }

    /// Compact playback bar shown on the library view while a book is loaded.
    fn mini_player(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        ui.add_space(4.0);
        let (title, author) = self
            .engine
            .as_ref()
            .and_then(|e| e.current())
            .map(|b| {
                (
                    b.meta.display_title().to_owned(),
                    b.meta.author.clone().unwrap_or_default(),
                )
            })
            .unwrap_or_default();

        let resp = ui
            .horizontal(|ui| {
                if let Some(tex) = &self.player_cover {
                    ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(40.0, 40.0)));
                    ui.add_space(6.0);
                }
                ui.vertical(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(&title).strong()).truncate());
                    if !author.is_empty() {
                        ui.add(egui::Label::new(egui::RichText::new(&author).weak().small()).truncate());
                    }
                });
            })
            .response;
        let open = ui.interact(resp.rect, egui::Id::new("mini_open"), egui::Sense::click());
        if open.hovered() {
            ui.output_mut(|o| o.cursor_icon = egui::CursorIcon::PointingHand);
        }
        if open.clicked() {
            *action = Some(Action::ShowPlayer);
        }

        self.render_transport(ui);
        ui.add_space(2.0);
    }

    /// The transport strip (seek bar + buttons), shared by the player and the
    /// library mini-player.
    fn render_transport(&mut self, ui: &mut egui::Ui) {
        let Some(engine) = self.engine.as_ref() else {
            return;
        };
        let duration = engine
            .current()
            .and_then(|b| b.meta.duration)
            .unwrap_or_default()
            .as_secs_f64();
        let pos = engine.position().as_secs_f64();
        let mut scrub = self.scrub;
        transport(ui, engine, pos, duration, &mut scrub);
        self.scrub = scrub;
    }

    /// Ensure a texture exists for every history entry's saved cover.
    fn prepare_history_covers(&mut self, ctx: &egui::Context) {
        let missing: Vec<i64> = self
            .history
            .iter()
            .filter(|h| !self.history_covers.contains_key(&h.id))
            .map(|h| h.id)
            .collect();
        for id in missing {
            let tex = self
                .history
                .iter()
                .find(|h| h.id == id)
                .and_then(|h| h.cover.as_ref())
                .and_then(|c| load_cover(ctx, c));
            self.history_covers.insert(id, tex);
        }
    }

    /// The "My History" timeline: entries grouped by the month they were last
    /// listened, each with an editable rating + note and a remove button.
    fn history_ui(&mut self, ui: &mut egui::Ui, action: &mut Option<Action>) {
        if self.history.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label("No history yet. Finish a book, or rate one from the player.");
            });
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut last_month: Option<String> = None;
                // `self.history` is already ordered newest-first by last_played.
                for entry in &self.history {
                    let month = fmt_month(entry.last_played);
                    if last_month.as_ref() != Some(&month) {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(&month).heading());
                        ui.separator();
                        last_month = Some(month);
                    }
                    ui.horizontal_top(|ui| {
                        if let Some(Some(tex)) = self.history_covers.get(&entry.id) {
                            ui.add(egui::Image::new(tex).fit_to_exact_size(egui::vec2(56.0, 56.0)));
                        } else {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(56.0, 56.0),
                                egui::Sense::hover(),
                            );
                            ui.painter().text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                icons::FILE_AUDIO,
                                egui::FontId::proportional(28.0),
                                ui.visuals().weak_text_color(),
                            );
                        }
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(&entry.title).strong());
                            if let Some(a) = &entry.author {
                                ui.label(egui::RichText::new(a).weak());
                            }
                            ui.horizontal(|ui| {
                                if let Some(r) = stars_widget(ui, entry.rating) {
                                    *action = Some(Action::RateHistory(entry.id, r));
                                }
                                ui.separator();
                                ui.label(egui::RichText::new("Listened"));
                                let date_buf = self
                                    .hist_date_edit
                                    .entry(entry.id)
                                    .or_insert_with(|| fmt_date(entry.last_played).unwrap_or_default());
                                let dresp = ui.add(
                                    egui::TextEdit::singleline(date_buf)
                                        .desired_width(96.0)
                                        .hint_text("YYYY-MM-DD"),
                                );
                                if dresp.lost_focus() {
                                    match parse_date(date_buf.as_str()) {
                                        Some(epoch) => {
                                            *action = Some(Action::SetHistoryDate(entry.id, epoch));
                                        }
                                        None => {
                                            *date_buf =
                                                fmt_date(entry.last_played).unwrap_or_default();
                                        }
                                    }
                                }
                                if entry.finished {
                                    ui.label(egui::RichText::new("· finished").weak());
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button(icons::TRASH).on_hover_text("Remove from history").clicked() {
                                            *action = Some(Action::RemoveHistory(entry.id));
                                        }
                                    },
                                );
                            });
                            // Editable note (persists on losing focus).
                            let note_buf = self
                                .hist_note_edit
                                .entry(entry.id)
                                .or_insert_with(|| entry.note.clone());
                            let nresp = ui.add(
                                egui::TextEdit::multiline(note_buf)
                                    .hint_text("A note about this audiobook…")
                                    .desired_rows(2)
                                    .desired_width(f32::INFINITY),
                            );
                            if nresp.lost_focus() {
                                *action = Some(Action::SetHistoryNote(entry.id, note_buf.clone()));
                            }
                        });
                    });
                    ui.add_space(6.0);
                }
            });
    }

    fn player_ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let mut action: Option<Action> = None;

        egui::Panel::top("play_top").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui.button(format!("{}  Library", icons::ARROW_LEFT)).clicked() {
                    action = Some(Action::Back);
                }
                ui.separator();

                // Compact sleep control: one dropdown + live remaining time.
                ui.label(icons::MOON);
                let label = match self.sleep {
                    Sleep::Off => "Off".to_owned(),
                    Sleep::Until(t) => {
                        let rem = t.saturating_duration_since(Instant::now()).as_secs();
                        format!("{} {}:{:02}", icons::CLOCK, rem / 60, rem % 60)
                    }
                    Sleep::EndOfChapter(_) => "End of chapter".to_owned(),
                };
                egui::ComboBox::from_id_salt("sleep")
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        if ui.selectable_label(false, "Off").clicked() {
                            action = Some(Action::SetSleep(Sleep::Off));
                        }
                        for mins in [10u64, 15, 30, 45, 60] {
                            if ui.selectable_label(false, format!("{mins} min")).clicked() {
                                let until = Instant::now() + Duration::from_secs(mins * 60);
                                action = Some(Action::SetSleep(Sleep::Until(until)));
                            }
                        }
                        if ui.selectable_label(false, "End of chapter").clicked() {
                            if let Some(ch) = self.engine.as_ref().and_then(|e| e.current_chapter())
                            {
                                action = Some(Action::SetSleep(Sleep::EndOfChapter(ch)));
                            }
                        }
                    });

                ui.checkbox(&mut self.shutdown_on_sleep, "Shutdown")
                    .on_hover_text("Power off the computer when the sleep timer fires");

                ui.separator();
                ui.label(&self.status);
            });
            ui.add_space(4.0);
        });

        egui::Panel::bottom("play_bottom").show(ui, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .button(icons::BOOKMARK)
                    .on_hover_text("Bookmark current position")
                    .clicked()
                {
                    action = Some(Action::AddBookmark);
                }
                if !self.bookmarks.is_empty() {
                    egui::CollapsingHeader::new(format!("{} bookmark(s)", self.bookmarks.len()))
                        .id_salt("bookmarks")
                        .show(ui, |ui| {
                            for bm in &self.bookmarks {
                                ui.horizontal(|ui| {
                                    if ui
                                        .button(format!(
                                            "{}  {}",
                                            icons::ARROW_U_DOWN_LEFT,
                                            fmt_hms(bm.position.as_secs())
                                        ))
                                        .clicked()
                                    {
                                        action = Some(Action::JumpTo(bm.position));
                                    }
                                    if !bm.label.is_empty() {
                                        ui.label(&bm.label);
                                    }
                                    if ui.small_button(icons::TRASH).clicked() {
                                        action = Some(Action::DeleteBookmark(bm.id));
                                    }
                                });
                            }
                        });
                }
            });
            ui.separator();
            self.render_transport(ui);
            ui.add_space(2.0);
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let Some(engine) = self.engine.as_ref() else {
                ui.centered_and_justified(|ui| ui.label("No book loaded."));
                return;
            };
            let Some(book) = engine.current() else {
                ui.centered_and_justified(|ui| ui.label("No book loaded."));
                return;
            };

            let duration = book.meta.duration.unwrap_or_default().as_secs_f64();
            let title = book.meta.display_title().to_owned();
            let author = book.meta.author.clone().unwrap_or_default();
            let chapters = book.meta.chapters.clone();
            let cur_chapter = engine.current_chapter();

            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                if let Some(tex) = &self.player_cover {
                    ui.add(egui::Image::new(tex).max_width(200.0));
                    ui.add_space(12.0);
                }
                ui.vertical(|ui| {
                    ui.heading(&title);
                    if !author.is_empty() {
                        ui.label(egui::RichText::new(&author).weak());
                    }
                    if let Some(reader) = book.meta.reader.as_deref().filter(|s| !s.is_empty()) {
                        ui.label(egui::RichText::new(format!("{}  {reader}", icons::MICROPHONE)).weak().small());
                    }
                    ui.add_space(4.0);
                    ui.label(format!(
                        "{} chapters · {}",
                        chapters.len(),
                        fmt_hms(duration as u64)
                    ));

                    // My History: rate + note. Editing creates the entry, which
                    // persists even after the files are later removed.
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new("My rating").weak());
                        let rating = self.current_history.as_ref().and_then(|h| h.rating);
                        if let Some(r) = stars_widget(ui, rating) {
                            action = Some(Action::RateBook(r));
                        }
                        if self.current_history.is_none()
                            && ui
                                .button(icons::PLUS_CIRCLE)
                                .on_hover_text("Add to My History")
                                .clicked()
                        {
                            action = Some(Action::AddToHistory);
                        }
                    });
                    let note = ui.add(
                        egui::TextEdit::multiline(&mut self.note_edit)
                            .hint_text("A note about this audiobook…")
                            .desired_rows(2)
                            .desired_width(280.0),
                    );
                    if note.lost_focus() {
                        action = Some(Action::SetNote(self.note_edit.clone()));
                    }
                });
            });

            ui.add_space(10.0);
            ui.separator();

            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for ch in &chapters {
                        let selected = cur_chapter == Some(ch.index);
                        let label = format!(
                            "{:>2}.  {:>9}   {}",
                            ch.index + 1,
                            fmt_hms(ch.start.as_secs()),
                            ch.title
                        );
                        if ui.selectable_label(selected, label).clicked() {
                            let _ = engine.seek_chapter(ch.index);
                        }
                    }
                });
        });

        if let Some(action) = action {
            self.apply(&ctx, action);
        }
    }

    fn apply(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::AddFolder(p) => self.add_folder(&p),
            Action::OpenBook(id) => self.open_book(ctx, id),
            Action::ShowPlayer => {
                if self.engine.as_ref().and_then(|e| e.current()).is_some() {
                    self.view = View::Player;
                }
            }
            Action::Rescan => self.rescan_all(),
            Action::SetSort(s) => {
                self.sort = s;
                self.refresh_books();
            }
            Action::Back => {
                self.save_progress();
                self.view = View::Library;
                self.refresh_books();
            }
            Action::AddBookmark => self.add_bookmark(),
            Action::JumpTo(pos) => {
                if let Some(engine) = self.engine.as_ref() {
                    let _ = engine.seek(pos);
                }
            }
            Action::DeleteBookmark(bid) => {
                let _ = self.db.remove_bookmark(bid);
                if let Some(id) = self.current_book_id {
                    self.bookmarks = self.db.bookmarks(id).unwrap_or_default();
                }
            }
            Action::SetSleep(s) => {
                self.sleep = s;
                self.status = match s {
                    Sleep::Off => "Sleep timer off.".to_owned(),
                    Sleep::Until(t) => format!(
                        "Sleep in {} min.",
                        t.saturating_duration_since(Instant::now()).as_secs() / 60 + 1
                    ),
                    Sleep::EndOfChapter(_) => "Sleep at end of chapter.".to_owned(),
                };
            }
            Action::RateBook(r) => {
                if let Some(id) = self.ensure_history_entry(false) {
                    let _ = self.db.set_history_rating(id, Some(r));
                    self.reload_current_history();
                }
            }
            Action::SetNote(note) => {
                if let Some(id) = self.ensure_history_entry(false) {
                    let _ = self.db.set_history_note(id, &note);
                    self.reload_current_history();
                }
            }
            Action::AddToHistory => {
                self.ensure_history_entry(true);
            }
            Action::AddBookToHistory(id) => {
                if let Ok(Some(book)) = self.db.get_book(id) {
                    let _ = self.db.upsert_history_from_book(&book, book.finished, true);
                    // Keep the player's view of history in sync if it's loaded.
                    if self.current_book_id == Some(id) {
                        self.load_current_history(&book);
                    }
                    self.refresh_history();
                    self.toasts.success(format!("Added “{}” to My History.", book.title));
                }
            }
            Action::RemoveHistory(id) => {
                let _ = self.db.remove_history(id);
                // If it was the loaded book's entry, forget it here too.
                if self.current_history.as_ref().map(|h| h.id) == Some(id) {
                    self.current_history = None;
                    self.note_edit.clear();
                }
                self.refresh_history();
            }
            Action::RateHistory(id, r) => {
                let _ = self.db.set_history_rating(id, Some(r));
                if self.current_history.as_ref().map(|h| h.id) == Some(id) {
                    self.reload_current_history();
                }
                self.refresh_history();
            }
            Action::SetHistoryNote(id, note) => {
                let _ = self.db.set_history_note(id, &note);
                self.hist_note_edit.remove(&id);
                self.refresh_history();
            }
            Action::SetHistoryDate(id, epoch) => {
                let _ = self.db.set_history_date(id, epoch);
                self.hist_date_edit.remove(&id);
                self.refresh_history();
            }
        }
    }

    /// Re-read the loaded book's history entry (after a rating/note edit),
    /// keeping the note editor in sync, and refresh the history list.
    fn reload_current_history(&mut self) {
        if let Some(id) = self.current_book_id {
            if let Ok(Some(book)) = self.db.get_book(id) {
                let keep = self.note_edit.clone();
                self.load_current_history(&book);
                // Don't clobber what the user is typing.
                self.note_edit = keep;
            }
        }
        self.refresh_history();
    }
}

/// Map a library item's interaction to the deferred action it triggers.
fn hit_action(hit: Option<Hit>, book_id: i64) -> Option<Action> {
    match hit {
        Some(Hit::Open) => Some(Action::OpenBook(book_id)),
        Some(Hit::AddToHistory) => Some(Action::AddBookToHistory(book_id)),
        None => None,
    }
}

/// Attach the shared "Add to My History" right-click menu to a library item.
fn history_context_menu(resp: &egui::Response, hit: &mut Option<Hit>) {
    resp.context_menu(|ui| {
        if ui
            .button(format!("{}  Add to My History", icons::PLUS_CIRCLE))
            .clicked()
        {
            *hit = Some(Hit::AddToHistory);
            ui.close();
        }
    });
}

/// A cover-grid card of the given width. Returns how it was interacted with.
fn book_card(ui: &mut egui::Ui, book: &Book, tex: Option<&egui::TextureHandle>, w: f32) -> Option<Hit> {
    let mut hit = None;
    ui.allocate_ui(egui::vec2(w, w + 64.0), |ui| {
        ui.vertical(|ui| {
            ui.set_width(w);
            let resp = match tex {
                Some(t) => ui.add(
                    egui::Button::image(egui::Image::new(t).fit_to_exact_size(egui::vec2(w, w)))
                        .frame(false),
                ),
                None => ui.add_sized(
                    egui::vec2(w, w),
                    egui::Button::new(egui::RichText::new(icons::FILE_AUDIO).size(w * 0.32)),
                ),
            };
            if resp.clicked() {
                hit = Some(Hit::Open);
            }
            history_context_menu(&resp, &mut hit);
            title_label(ui, book.id, &book.title);
            if let Some(author) = &book.author {
                ui.label(egui::RichText::new(truncate(author, 40)).weak().small());
            }
            let frac = book.progress_fraction();
            if frac > 0.0 {
                ui.add(egui::ProgressBar::new(frac).desired_width(w));
            }
        });
    });
    hit
}

/// A details-list row: small cover, title/author, duration + progress. Returns
/// how it was interacted with.
fn details_row(ui: &mut egui::Ui, book: &Book, tex: Option<&egui::TextureHandle>, cover: f32) -> Option<Hit> {
    let mut hit = None;
    let resp = ui
        .horizontal(|ui| {
            match tex {
                Some(t) => {
                    ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(cover, cover)));
                }
                None => {
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(cover, cover), egui::Sense::hover());
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        icons::FILE_AUDIO,
                        egui::FontId::proportional(cover * 0.5),
                        ui.visuals().weak_text_color(),
                    );
                }
            }
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(&book.title).strong());
                if let Some(author) = &book.author {
                    ui.label(egui::RichText::new(author).weak());
                }
                let frac = book.progress_fraction();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(fmt_hms(book.duration.as_secs())).small().weak());
                    if frac > 0.0 {
                        ui.add(egui::ProgressBar::new(frac).desired_width(120.0).desired_height(6.0));
                    }
                });
            });
        })
        .response;

    // Make the whole row clickable, with a translucent hover tint that keeps
    // the text readable (painted over the content).
    let row = ui.interact(resp.rect, egui::Id::new(("row", book.id)), egui::Sense::click());
    if row.hovered() {
        let c = ui.visuals().selection.bg_fill;
        let tint = egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 32);
        ui.painter().rect_filled(resp.rect, 4.0, tint);
    }
    if row.clicked() {
        hit = Some(Hit::Open);
    }
    history_context_menu(&row, &mut hit);
    ui.add_space(4.0);
    hit
}

/// A strong title label that, when truncated, shows the full title on hover in
/// place (seamlessly aligned over the truncated text).
fn title_label(ui: &mut egui::Ui, id: i64, full: &str) {
    let text = egui::RichText::new(full).strong();
    let resp = ui.add(egui::Label::new(text.clone()).truncate());
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let full_w = ui
        .painter()
        .layout_no_wrap(full.to_owned(), font_id, egui::Color32::WHITE)
        .size()
        .x;
    if resp.hovered() && full_w > resp.rect.width() + 0.5 {
        egui::Area::new(egui::Id::new(("title_ov", id)))
            .order(egui::Order::Tooltip)
            .fixed_pos(resp.rect.left_top())
            .constrain(true)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::ZERO)
                    .show(ui, |ui| ui.label(text));
            });
    }
}

/// A compact list item (Dolphin "Compact"): a small icon/cover on the left and
/// the title on the right, sized so items flow into columns. Returns how it was
/// interacted with.
fn compact_item(ui: &mut egui::Ui, book: &Book, tex: Option<&egui::TextureHandle>, zoom: f32) -> Option<Hit> {
    let icon = (zoom * 0.16).clamp(18.0, 44.0);
    let item_w = (zoom * 1.6).clamp(180.0, 460.0);
    let row_h = icon + 6.0;
    let mut hit = None;
    // A fixed-size, vertically-centred region so every item is the same height
    // and the icons line up across a wrapped row.
    let resp = ui
        .allocate_ui_with_layout(
            egui::vec2(item_w, row_h),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                match tex {
                    Some(t) => {
                        ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(icon, icon)));
                    }
                    None => {
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(icon, icon), egui::Sense::hover());
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            icons::FILE_AUDIO,
                            egui::FontId::proportional(icon * 0.55),
                            ui.visuals().weak_text_color(),
                        );
                    }
                }
                ui.add_space(6.0);
                ui.add(egui::Label::new(egui::RichText::new(&book.title).strong()).truncate());
            },
        )
        .response;

    let row = ui.interact(resp.rect, egui::Id::new(("compact", book.id)), egui::Sense::click());
    if row.hovered() {
        let c = ui.visuals().selection.bg_fill;
        let tint = egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), 32);
        ui.painter().rect_filled(resp.rect, 4.0, tint);
    }
    if row.clicked() {
        hit = Some(Hit::Open);
    }
    history_context_menu(&row, &mut hit);
    hit
}

/// A row of five clickable stars. Returns the newly picked rating (1..=5) when
/// the user clicks a star, else `None`.
fn stars_widget(ui: &mut egui::Ui, rating: Option<u8>) -> Option<u8> {
    let filled = rating.unwrap_or(0);
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for n in 1..=5u8 {
            let color = if n <= filled {
                egui::Color32::from_rgb(0xF5, 0xB5, 0x0A)
            } else {
                ui.visuals().weak_text_color()
            };
            let star = egui::RichText::new(icons::STAR).color(color).size(16.0);
            if ui
                .add(egui::Button::new(star).frame(false))
                .on_hover_text(format!("{n} / 5"))
                .clicked()
            {
                // Clicking the only lit star clears the rating.
                picked = Some(if rating == Some(n) { 0 } else { n });
            }
        }
    });
    picked
}

/// The author (or reader) display name for a book, "Unknown" if unset.
fn book_group_name(book: &Book, reader: bool) -> String {
    let raw = if reader {
        book.reader.as_deref()
    } else {
        book.author.as_deref()
    };
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("Unknown")
        .to_string()
}

/// Distinct author/reader names with their book counts, sorted by name.
fn group_counts(books: &[Book], reader: bool) -> Vec<(String, usize)> {
    let mut map: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for b in books {
        *map.entry(book_group_name(b, reader)).or_default() += 1;
    }
    map.into_iter().collect()
}

fn book_matches(book: &Book, needle: &str) -> bool {
    book.title.to_lowercase().contains(needle)
        || book
            .author
            .as_deref()
            .map(|a| a.to_lowercase().contains(needle))
            .unwrap_or(false)
        || book
            .reader
            .as_deref()
            .map(|r| r.to_lowercase().contains(needle))
            .unwrap_or(false)
}

/// Bottom transport: full-width seek bar + playback buttons.
fn transport(
    ui: &mut egui::Ui,
    engine: &PlaybackEngine,
    pos: f64,
    duration: f64,
    scrub: &mut Option<f64>,
) {
    let shown = scrub.unwrap_or(pos);
    let max = duration.max(1.0);

    ui.horizontal(|ui| {
        ui.label(fmt_hms(shown as u64));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(format!("-{}", fmt_hms((max - shown).max(0.0) as u64)));
        });
    });

    if let Some(target) = seek_bar(ui, shown, max, scrub) {
        let _ = engine.seek(Duration::from_secs_f64(target));
    }

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let btn = |ui: &mut egui::Ui, icon: &str, tip: &str| -> bool {
            ui.add(
                egui::Button::new(egui::RichText::new(icon).size(18.0))
                    .min_size(egui::vec2(48.0, 32.0)),
            )
            .on_hover_text(tip)
            .clicked()
        };
        if btn(ui, icons::SKIP_BACK, "Previous chapter") {
            let _ = engine.prev_chapter();
        }
        if btn(ui, icons::ARROW_ARC_LEFT, "Back 30s") {
            let _ = engine.seek_relative(-30);
        }
        let play_icon = if engine.is_paused() { icons::PLAY } else { icons::PAUSE };
        if btn(ui, play_icon, "Play / pause") {
            engine.toggle();
        }
        if btn(ui, icons::ARROW_ARC_RIGHT, "Forward 30s") {
            let _ = engine.seek_relative(30);
        }
        if btn(ui, icons::SKIP_FORWARD, "Next chapter") {
            let _ = engine.next_chapter();
        }
    });
}

/// A full-width progress bar the user can click or drag to seek. Returns the
/// seek target (seconds) once the interaction completes.
fn seek_bar(ui: &mut egui::Ui, pos: f64, duration: f64, scrub: &mut Option<f64>) -> Option<f64> {
    let height = 16.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click_and_drag(),
    );

    let frac = (pos / duration).clamp(0.0, 1.0) as f32;
    let radius = height / 2.0;
    let visuals = ui.style().visuals.clone();
    let painter = ui.painter();

    painter.rect_filled(rect, radius, visuals.extreme_bg_color);
    if frac > 0.0 {
        let mut filled = rect;
        filled.set_width(rect.width() * frac);
        painter.rect_filled(filled, radius, visuals.selection.bg_fill);
    }
    let handle_x = rect.left() + rect.width() * frac;
    painter.circle_filled(
        egui::pos2(handle_x, rect.center().y),
        radius,
        visuals.selection.stroke.color,
    );

    if resp.dragged() || resp.is_pointer_button_down_on() {
        if let Some(p) = resp.interact_pointer_pos() {
            let f = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
            *scrub = Some(f * duration);
        }
    }
    if resp.drag_stopped() || resp.clicked() {
        return Some(scrub.take().unwrap_or(pos));
    }
    None
}

fn open_library_db() -> anyhow::Result<Database> {
    let dirs = directories::ProjectDirs::from("", "", "some-audiobook-player")
        .ok_or_else(|| anyhow::anyhow!("could not determine a data directory"))?;
    Database::open(&dirs.data_dir().join("library.sqlite"))
}

/// Write cover bytes to a temp file and return a `file://` URL for MPRIS art.
fn write_cover_tmp(cover: &Cover) -> Option<String> {
    let ext = if cover.is_png { "png" } else { "jpg" };
    let path = std::env::temp_dir().join(format!("some-audiobook-player-cover.{ext}"));
    std::fs::write(&path, &cover.data).ok()?;
    Some(format!("file://{}", path.display()))
}

fn load_cover(ctx: &egui::Context, cover: &Cover) -> Option<egui::TextureHandle> {
    let img = image::load_from_memory(&cover.data).ok()?;
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    let color = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
    Some(ctx.load_texture("cover", color, egui::TextureOptions::LINEAR))
}

fn sort_label(sort: Sort) -> &'static str {
    match sort {
        Sort::Title => "Title",
        Sort::Author => "Author",
        Sort::RecentlyAdded => "Recently added",
        Sort::RecentlyPlayed => "Recently played",
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>())
    }
}

fn fmt_hms(s: u64) -> String {
    format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

const MONTHS: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August",
    "September", "October", "November", "December",
];

/// Convert Unix epoch seconds to a civil `(year, month 1..=12, day 1..=31)`
/// (UTC), via Howard Hinnant's days-from-civil inverse. No external deps.
fn civil_from_epoch(epoch: i64) -> (i64, u32, u32) {
    let days = epoch.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A "Month YYYY" heading for the history timeline (or "Unknown date").
fn fmt_month(epoch: Option<i64>) -> String {
    match epoch {
        Some(e) => {
            let (y, m, _) = civil_from_epoch(e);
            format!("{} {}", MONTHS[(m as usize - 1).min(11)], y)
        }
        None => "Unknown date".to_owned(),
    }
}

/// A short "YYYY-MM-DD" date for a history entry, if known.
fn fmt_date(epoch: Option<i64>) -> Option<String> {
    let (y, m, d) = civil_from_epoch(epoch?);
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's days-from-civil).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + (d as i64 - 1);
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse a "YYYY-MM-DD" date into Unix epoch seconds (UTC midnight).
fn parse_date(s: &str) -> Option<i64> {
    let mut it = s.trim().split('-');
    let y: i64 = it.next()?.trim().parse().ok()?;
    let m: u32 = it.next()?.trim().parse().ok()?;
    let d: u32 = it.next()?.trim().parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400)
}
