//! GSMTC media sensor + controls (Phase 1, music card).
//! Uses the public Windows GSMTC API (GlobalSystemMediaTransportControls
//! SessionManager) — no Chrome extension, no Accessibility prompt, unlike
//! Mac (which needed JXA because macOS 15.4+ blocks third-party
//! MediaRemote reads). Covers Spotify, Chrome/Edge YouTube Music, etc.
//!
//! Polling every 0.25s: RequestAsync + GetCurrentSession +
//! TryGetMediaPropertiesAsync + GetPlaybackInfo + GetTimelineProperties,
//! all via blocking `.get()` on the WinRT async operations.
//!
//! BUG 2026-10-27 (dugaan dari kode, bukan dari log Windows): poll dan
//! kontrol itu TADINYA jalan di UI thread (tick 250ms + klik tombol).
//! Kalau satu panggilan WinRT `.get()` itu nunggu lama (mis. app media
//! sibuk saat play), message loop ikut berhenti dan Windows menandai
//! window "Not Responding". Karena itu sejak fix ini poll + kontrol
//! TIDAK PERNAH jalan di UI thread lagi: poll jalan di satu thread
//! latar yang menulis cache, kontrol jalan di thread sekali-pakai.
//! UI thread (tick) hanya membaca cache — jadi kalaupun satu poll
//! nyangkut, pill tetap responsif (progress/judul boleh beku, tapi
//! window tidak Not Responding; itu sekaligus pemisah diagnostik:
//! kalau setelah fix ini UI responsif tapi judul/progress beku,
//! berarti poll GSMTC-nya yang nyangkut di app media itu).
//!
//! Position: GSMTC Position alone is NOT realtime for some apps (they
//! set it on track change/seek only, not during playback), so App feeds
//! every poll through MediaTracker (below), which advances the position
//! by wall-clock elapsed time while Playing.
//!
//! Artwork thumbnail decoding (WIC) is NOT in this increment: the music
//! card draws a placeholder cover box until basic title/artist/progress/
//! controls are verified working. Thumbnail arrives after that.
//!
//! If `cargo build` reports a signature mismatch (notably `.get()` on
//! IAsyncOperation, HSTRING conversion, or TimeSpan field access), paste
//! it — Eve cannot compile Rust in this VM, so the first build on the
//! user's Windows PC is where WinRT signatures get corrected.

use std::sync::{Arc, Mutex};

use windows::core::Result;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};

/// Init COM (STA) untuk thread pemanggil. Best-effort: kalau sudah
/// di-init thread itu, hasilnya diabaikan (bukan error).
///
/// STA, bukan MTA, dengan sengaja (dugaan dari timeline 2026-10-27:
/// musik terdeteksi saat poll jalan di UI thread, lalu TIDAK
/// terdeteksi lagi setelah poll pindah ke thread latar MTA di
/// b19b92a — RequestAsync GSMTC tampaknya gagal / nyangkut di
/// apartment MTA, sementara UI thread STA + message loop adalah
/// kombinasi yang terbukti jalan. .get() di thread STA memompa
/// pesan COM selagi menunggu, sama seperti di UI thread dulu).
fn init_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
    }
}

/// Start the background poller: one thread, every 250ms, poll GSMTC
/// (blocking `.get()` is fine HERE, off the UI thread) and write the
/// result into `cache`. The UI thread only ever reads `cache`, never
/// calls poll() itself. A stuck poll only freezes the cache, never
/// the window. Runs until the process exits (no shutdown handshake:
/// the thread holds only the cache Arc, never the HWND).
pub fn spawn_poller(cache: Arc<Mutex<Option<MediaInfo>>>) {
    std::thread::spawn(move || {
        init_com();
        loop {
            let info = poll();
            if let Ok(mut guard) = cache.lock() {
                *guard = info;
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    });
}

/// Run a control action on a throwaway thread (never the UI thread).
/// The caller does not wait: the next poller cycle (<=250ms) refreshes
/// the card icon/progress, so the play/pause icon flips within a tick.
fn spawn_control(action: fn() -> bool) {
    std::thread::spawn(move || {
        init_com();
        let _ = action();
    });
}

pub fn play_async() {
    spawn_control(play);
}

pub fn pause_async() {
    spawn_control(pause);
}

pub fn skip_next_async() {
    spawn_control(skip_next);
}

pub fn skip_previous_async() {
    spawn_control(skip_previous);
}

/// Current media state for the music card + compact/zen decisions.
#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub position_secs: f64,
    pub duration_secs: f64,
}

/// Poll the current GSMTC session. Returns None when there is no session
/// (silent) or when any step fails (treated as silent, never a crash).
pub fn poll() -> Option<MediaInfo> {
    poll_inner().ok()
}

fn poll_inner() -> Result<MediaInfo> {
    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
    let session = manager.GetCurrentSession()?;
    let props = session.TryGetMediaPropertiesAsync()?.get()?;
    let title = props.Title()?.to_string();
    let artist = props.Artist()?.to_string();
    let album = props.AlbumTitle()?.to_string();
    let info = session.GetPlaybackInfo()?;
    let playing =
        info.PlaybackStatus()? == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing;
    let timeline = session.GetTimelineProperties()?;
    let position_secs = timeline.Position()?.Duration as f64 / 10_000_000.0;
    let duration_secs = timeline.EndTime()?.Duration as f64 / 10_000_000.0;
    Ok(MediaInfo {
        title,
        artist,
        album,
        playing,
        position_secs: position_secs.max(0.0),
        duration_secs: duration_secs.max(0.0),
    })
}

/// Run a control action against the current session. Returns false when
/// there is no session or the action fails (caller ignores the result).
fn with_session<F>(f: F) -> bool
where
    F: FnOnce(GlobalSystemMediaTransportControlsSession) -> Result<bool>,
{
    let run = || -> Result<bool> {
        let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()?;
        let session = manager.GetCurrentSession()?;
        f(session)
    };
    run().unwrap_or(false)
}

pub fn play() -> bool {
    with_session(|s| Ok(s.TryPlayAsync()?.get()?))
}

pub fn pause() -> bool {
    with_session(|s| Ok(s.TryPauseAsync()?.get()?))
}

pub fn skip_next() -> bool {
    with_session(|s| Ok(s.TrySkipNextAsync()?.get()?))
}

pub fn skip_previous() -> bool {
    with_session(|s| Ok(s.TrySkipPreviousAsync()?.get()?))
}

/// Wall-clock position estimate (realtime fix, attempt 2).
///
/// Some GSMTC apps do not update TimelineProperties.Position during
/// normal playback (they set it on track change/seek only), so polling
/// Position alone looks frozen even with a fast tick + synchronous
/// paint (both already in place since 89fbe58, and still not realtime
/// per user report 2026-10-27). This tracker advances the position by
/// real elapsed time while Playing, and re-syncs to GSMTC only when
/// GSMTC itself jumps (seek, or an app catch-up after lag).
///
/// Rules (per poll, same song):
/// - Playing: estimated += elapsed (Instant, monotonic, no drift).
/// - Paused: estimated frozen.
/// - If GSMTC position jumped by >2s since last poll (seek, or app
///   finally reporting a new position after lag), adopt GSMTC (seek
///   target wins; catch-up lands on the estimate anyway, so no jump).
/// - New song (title|artist changed) or session gap: re-anchor to GSMTC.
/// - Clamp to [0, duration] (when duration is known).
///
/// PlaybackStatus (playing/paused) and title/artist are updated
/// promptly by apps even when Position is not, so the estimate stays
/// correct. If an app DOES update Position every poll, the estimate
/// tracks it within timer jitter (<0.5s) and only re-syncs on seeks.
pub struct MediaTracker {
    initialized: bool,
    last_key: String,
    last_raw_secs: f64,
    estimated_secs: f64,
    last_update: std::time::Instant,
    playing: bool,
}

impl MediaTracker {
    pub fn new() -> Self {
        Self {
            initialized: false,
            last_key: String::new(),
            last_raw_secs: 0.0,
            estimated_secs: 0.0,
            last_update: std::time::Instant::now(),
            playing: false,
        }
    }

    /// Feed a raw poll result, get back the same info with an estimated
    /// (realtime) position. None passes through (and resets, so the
    /// next session re-anchors instead of guessing across the gap).
    pub fn update(&mut self, raw: Option<MediaInfo>) -> Option<MediaInfo> {
        let mut raw = match raw {
            Some(m) => m,
            None => {
                self.initialized = false;
                return None;
            }
        };
        let key = format!("{}|{}", raw.title, raw.artist);
        let now = std::time::Instant::now();
        if !self.initialized || key != self.last_key {
            // New song (or first poll): anchor to GSMTC.
            self.estimated_secs = raw.position_secs;
            self.last_raw_secs = raw.position_secs;
            self.last_key = key;
            self.initialized = true;
        } else {
            // Same song: advance by elapsed while previously playing.
            if self.playing {
                let elapsed = now.duration_since(self.last_update).as_secs_f64();
                self.estimated_secs += elapsed;
            }
            // Re-sync if GSMTC jumped (seek, or app catch-up after lag).
            if (raw.position_secs - self.last_raw_secs).abs() > 2.0 {
                self.estimated_secs = raw.position_secs;
            }
            self.last_raw_secs = raw.position_secs;
            // Clamp to [0, duration] when duration is known.
            if raw.duration_secs > 0.0 && self.estimated_secs > raw.duration_secs {
                self.estimated_secs = raw.duration_secs;
            }
            if self.estimated_secs < 0.0 {
                self.estimated_secs = 0.0;
            }
        }
        self.last_update = now;
        self.playing = raw.playing;
        raw.position_secs = self.estimated_secs;
        Some(raw)
    }
}
