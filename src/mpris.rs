//! MPRIS (org.mpris.MediaPlayer2) D-Bus integration: media keys, now-playing
//! metadata, and play/pause state for GNOME Shell / KDE Plasma / playerctl.
//!
//! `mpris-server`'s `Player` is `!Send` and async, so it lives on its own thread
//! running an `async-io` event loop. Desktop → app control flows over an mpsc
//! channel (drained each UI frame); app → desktop updates flow over an async
//! channel. If there's no session bus (e.g. headless), [`Mpris::spawn`] simply
//! yields a no-op handle and the app runs normally.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use async_channel::{Sender as AsyncSender, TrySendError};
use mpris_server::{Metadata, PlaybackStatus, Player, Time};

/// A control request coming from the desktop environment.
#[derive(Debug, Clone, Copy)]
pub enum MprisCommand {
    PlayPause,
    Play,
    Pause,
    Stop,
    Next,
    Prev,
    /// Seek by a relative number of seconds (may be negative).
    SeekBy(i64),
    /// Seek to an absolute number of seconds.
    SeekTo(i64),
    Raise,
}

/// Now-playing info pushed to the desktop.
#[derive(Debug, Clone)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub length: Duration,
    pub position: Duration,
    pub art_url: Option<String>,
}

/// A status update pushed from the app to the MPRIS server.
#[derive(Debug, Clone)]
pub enum MprisUpdate {
    NowPlaying(NowPlaying),
    Status { playing: bool },
    Position(Duration),
}

/// Handle held by the app. Cheap no-op when MPRIS is unavailable.
pub struct Mpris {
    cmd_rx: Option<Receiver<MprisCommand>>,
    update_tx: Option<AsyncSender<MprisUpdate>>,
}

impl Mpris {
    /// Spawn the MPRIS server thread. Always returns a handle; if the bus is
    /// unreachable the handle is inert.
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = std::sync::mpsc::channel::<MprisCommand>();
        let (update_tx, update_rx) = async_channel::bounded::<MprisUpdate>(64);

        let spawned = std::thread::Builder::new()
            .name("sap-mpris".into())
            .spawn(move || {
                if let Err(e) = async_io::block_on(run(cmd_tx, update_rx)) {
                    eprintln!("some-audiobook-player: MPRIS unavailable: {e}");
                }
            })
            .is_ok();

        if spawned {
            Self {
                cmd_rx: Some(cmd_rx),
                update_tx: Some(update_tx),
            }
        } else {
            Self {
                cmd_rx: None,
                update_tx: None,
            }
        }
    }

    /// Drain the next pending desktop command, if any.
    pub fn poll_command(&self) -> Option<MprisCommand> {
        self.cmd_rx.as_ref()?.try_recv().ok()
    }

    /// Push a status update to the desktop (best effort; dropped if full).
    pub fn push(&self, update: MprisUpdate) {
        if let Some(tx) = &self.update_tx {
            match tx.try_send(update) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Closed(_)) => {}
            }
        }
    }
}

async fn run(
    cmd_tx: Sender<MprisCommand>,
    update_rx: async_channel::Receiver<MprisUpdate>,
) -> mpris_server::zbus::Result<()> {
    let player = Player::builder("org.some_audiobook_player.player")
        .identity("Some Audiobook Player")
        .can_play(true)
        .can_pause(true)
        .can_go_next(true)
        .can_go_previous(true)
        .can_seek(true)
        .can_control(true)
        .build()
        .await?;
    // Associate with the installed .desktop file so shells show our icon/name.
    let _ = player.set_desktop_entry("some-audiobook-player").await;

    let send = |tx: &Sender<MprisCommand>, c: MprisCommand| {
        let _ = tx.send(c);
    };
    macro_rules! on {
        ($connect:ident, $cmd:expr) => {{
            let tx = cmd_tx.clone();
            player.$connect(move |_| send(&tx, $cmd));
        }};
    }
    on!(connect_play_pause, MprisCommand::PlayPause);
    on!(connect_play, MprisCommand::Play);
    on!(connect_pause, MprisCommand::Pause);
    on!(connect_stop, MprisCommand::Stop);
    on!(connect_next, MprisCommand::Next);
    on!(connect_previous, MprisCommand::Prev);
    on!(connect_raise, MprisCommand::Raise);
    {
        let tx = cmd_tx.clone();
        player.connect_seek(move |_, offset| send(&tx, MprisCommand::SeekBy(offset.as_secs())));
    }
    {
        let tx = cmd_tx.clone();
        player.connect_set_position(move |_, _id, pos| {
            send(&tx, MprisCommand::SeekTo(pos.as_secs()))
        });
    }

    let updates = async {
        while let Ok(update) = update_rx.recv().await {
            match update {
                MprisUpdate::NowPlaying(np) => {
                    let mut b = Metadata::builder()
                        .title(np.title)
                        .artist([np.artist])
                        .album(np.album)
                        .length(Time::from_micros(np.length.as_micros() as i64));
                    if let Some(url) = np.art_url {
                        b = b.art_url(url);
                    }
                    let _ = player.set_metadata(b.build()).await;
                    let _ = player
                        .seeked(Time::from_micros(np.position.as_micros() as i64))
                        .await;
                }
                MprisUpdate::Status { playing } => {
                    let status = if playing {
                        PlaybackStatus::Playing
                    } else {
                        PlaybackStatus::Paused
                    };
                    let _ = player.set_playback_status(status).await;
                }
                MprisUpdate::Position(p) => {
                    let _ = player.seeked(Time::from_micros(p.as_micros() as i64)).await;
                }
            }
        }
    };

    // Run the D-Bus event loop and the update pump together, forever.
    futures_lite::future::zip(player.run(), updates).await;
    Ok(())
}
