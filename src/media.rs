//! GSMTC media sensor + controls (Phase 1, music card).
//! Uses the public Windows GSMTC API (GlobalSystemMediaTransportControls
//! SessionManager) — no Chrome extension, no Accessibility prompt, unlike
//! Mac (which needed JXA because macOS 15.4+ blocks third-party
//! MediaRemote reads). Covers Spotify, Chrome/Edge YouTube Music, etc.
//!
//! Polling only in this increment (1s tick from app.rs): RequestAsync +
//! GetCurrentSession + TryGetMediaPropertiesAsync + GetPlaybackInfo +
//! GetTimelineProperties, all via blocking `.get()` on the WinRT async
//! operations. GSMTC change events (CurrentSessionChanged, etc.) for
//! instant updates are a follow-up if 1s polling feels laggy.
//!
//! Artwork thumbnail decoding (WIC) is NOT in this increment: the music
//! card draws a placeholder cover box until basic title/artist/progress/
//! controls are verified working. Thumbnail arrives after that.
//!
//! If `cargo build` reports a signature mismatch (notably `.get()` on
//! IAsyncOperation, HSTRING conversion, or TimeSpan field access), paste
//! it — Eve cannot compile Rust in this VM, so the first build on the
//! user's Windows PC is where WinRT signatures get corrected.

use windows::core::Result;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession, GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus,
};

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
