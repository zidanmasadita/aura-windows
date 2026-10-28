//! GSMTC media sensor + controls + WIC cover art decoder.
//!
//! Exact 1:1 port of Nusa (`nusa.exe` static disassembly):
//! 1. **`RoInitialize(RO_INIT_MULTITHREADED)` + `.get()` (`0x14000AB44`, `0x140039D37`)**:
//!    Initializes MTA via `RoInitialize(RO_INIT_MULTITHREADED)` and calls
//!    `GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?.get()`
//!    ONCE at startup.
//! 2. **`GetCurrentSession()` Only — Never `GetSessions()` (`0x14002339C`, `0x14007A6CB`)**:
//!    Nusa ONLY queries `manager.GetCurrentSession()` (vtable `+0x30`) and NEVER
//!    iterates `manager.GetSessions()`. Iterating `GetSessions()` queries suspended/zombie
//!    browser tabs, which wedges Windows's `NPSMSvc` (`Now Playing Session Manager Service`)
//!    after a few minutes and causes subsequent `RequestAsync()` calls to time out
//!    (`0x800705b4`) until `NPSMSvc` or the PC is restarted.
//! 3. **Event-Driven Subscriptions (`0x14002339C`, `0x14002676A`)**:
//!    Subscribes `TypedEventHandler` on `manager.CurrentSessionChanged` (`+0x40`) and
//!    on the active session's `PlaybackInfoChanged` (`+0xD8`, flag 1),
//!    `TimelinePropertiesChanged` (`+0xC8`, flag 2), and `MediaPropertiesChanged`
//!    (`+0xE8`, flag 4).
//! 4. **One-Shot MTA Worker for `TryGetMediaPropertiesAsync()` + WIC Cover Art (`0x14000A5E8`)**:
//!    Only when the session changes or `MediaPropertiesChanged` fires, increments an
//!    atomic generation counter (`0x1400B5CB0`) and spawns a one-shot MTA thread to
//!    call `TryGetMediaPropertiesAsync()?.get()` and decode `Thumbnail()` via
//!    `DataReader` + WIC `IWICImagingFactory` (`0x1400033CF`).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::{Interface, Result};
use windows::Foundation::{EventRegistrationToken, TypedEventHandler};
use windows::Media::Control::{
    CurrentSessionChangedEventArgs, GlobalSystemMediaTransportControlsSession,
    GlobalSystemMediaTransportControlsSessionManager,
    GlobalSystemMediaTransportControlsSessionMediaProperties,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus, MediaPropertiesChangedEventArgs,
    PlaybackInfoChangedEventArgs, TimelinePropertiesChangedEventArgs,
};
use windows::Storage::Streams::{DataReader, IInputStream};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapFrameDecode,
    IWICBitmapSource, IWICFormatConverter, IWICImagingFactory, IWICStream,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeMedianCut,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Posted to the main HWND whenever GSMTC state updates (Nusa `0x8003`).
pub const WM_MEDIA_UPDATE: u32 = 0x8003;

/// Event bitmask flags (matching Nusa `0x14002676A` wParam conventions).
pub const FLAG_PLAYBACK: u32 = 1;
pub const FLAG_TIMELINE: u32 = 2;
pub const FLAG_PROPS: u32 = 4;
pub const FLAG_SESSION: u32 = 8;

/// Cover art decode target resolution (112x112 physical pixels = 2x for 56x56 DIP box).
pub const COVER_SIZE: u32 = 112;

/// 100ns ticks between 1601-01-01 (Win32 FILETIME / WinRT DateTime epoch) and 1970-01-01 (Unix epoch).
const FILETIME_UNIX_DIFF_TICKS: i64 = 116_444_736_000_000_000;

/// Initialize MTA WinRT apartment on the calling worker thread (`nusa.exe` `0x14000AB4D`:
/// `mov ecx, 1; call RoInitialize`).
pub fn init_mta() {
    unsafe {
        let _ = RoInitialize(RO_INIT_MULTITHREADED);
    }
}

/// Wake signal shared between WinRT event callbacks, control threads, and the coordinator.
struct EventSignal {
    flags: Mutex<u32>,
    cv: Condvar,
}

impl EventSignal {
    fn new() -> Self {
        Self {
            flags: Mutex::new(FLAG_SESSION | FLAG_PROPS | FLAG_PLAYBACK | FLAG_TIMELINE),
            cv: Condvar::new(),
        }
    }

    fn notify(&self, bits: u32) {
        if let Ok(mut guard) = self.flags.lock() {
            *guard |= bits;
            self.cv.notify_all();
        }
    }

    fn wait_timeout(&self, timeout: Duration) -> u32 {
        let Ok(mut guard) = self.flags.lock() else {
            std::thread::sleep(timeout);
            return FLAG_PLAYBACK | FLAG_TIMELINE;
        };
        if *guard == 0 {
            if let Ok((g, _)) = self.cv.wait_timeout(guard, timeout) {
                guard = g;
            } else {
                return FLAG_PLAYBACK | FLAG_TIMELINE;
            }
        }
        let bits = *guard;
        *guard = 0;
        bits
    }
}

static GLOBAL_SIGNAL: OnceLock<Arc<EventSignal>> = OnceLock::new();
static PROPS_GENERATION: AtomicU64 = AtomicU64::new(0);

fn signal_media_event(bits: u32) {
    if let Some(sig) = GLOBAL_SIGNAL.get() {
        sig.notify(bits);
    }
}

/// Cached metadata + cover art written by the one-shot MTA properties thread (`0x14000A5E8`).
#[derive(Clone, Default)]
struct CachedTrackProps {
    session_id: String,
    title: String,
    artist: String,
    album: String,
    cover_bgra: Option<Arc<Vec<u8>>>,
    cover_w: u32,
    cover_h: u32,
}

/// Active event registration tokens on the tracked `GlobalSystemMediaTransportControlsSession`
/// (`nusa.exe` `0x1400239B9`).
struct SessionSubscriptions {
    session: GlobalSystemMediaTransportControlsSession,
    session_id: String,
    playback_token: Option<EventRegistrationToken>,
    timeline_token: Option<EventRegistrationToken>,
    props_token: Option<EventRegistrationToken>,
}

impl Drop for SessionSubscriptions {
    fn drop(&mut self) {
        if let Some(tok) = self.playback_token.take() {
            let _ = self.session.RemovePlaybackInfoChanged(tok);
        }
        if let Some(tok) = self.timeline_token.take() {
            let _ = self.session.RemoveTimelinePropertiesChanged(tok);
        }
        if let Some(tok) = self.props_token.take() {
            let _ = self.session.RemoveMediaPropertiesChanged(tok);
        }
    }
}

/// Start the Nusa-style MTA + event-driven GSMTC coordinator (`0x14000AB44` + `0x14002339C`).
pub fn spawn_poller(hwnd: HWND, cache: Arc<Mutex<PollCache>>) {
    let signal = GLOBAL_SIGNAL
        .get_or_init(|| Arc::new(EventSignal::new()))
        .clone();
    let hwnd_raw = hwnd.0 as isize;
    let shared_props = Arc::new(Mutex::new(CachedTrackProps::default()));

    std::thread::spawn(move || {
        init_mta();
        let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);

        // Step 1: Request SessionManager ONCE via `.get()` on this MTA thread,
        // identical to Nusa's `0x14000AB44` -> `0x14007B933` -> `0x140039D37`.
        set_phase(&cache, "manager request (MTA)");
        let manager = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
            .and_then(|op| op.get())
        {
            Ok(mgr) => mgr,
            Err(e) => {
                update_cache(
                    &cache,
                    None,
                    format!("request failed {:#010x}", e.code().0 as u32),
                    "error",
                );
                return;
            }
        };

        // Subscribe to `CurrentSessionChanged` (`nusa.exe` `0x14002902F`, vtable `+0x40`).
        let sig_mgr = signal.clone();
        let mgr_handler = TypedEventHandler::<
            GlobalSystemMediaTransportControlsSessionManager,
            CurrentSessionChangedEventArgs,
        >::new(move |_, _| {
            sig_mgr.notify(FLAG_SESSION | FLAG_PROPS | FLAG_PLAYBACK | FLAG_TIMELINE);
            Ok(())
        });
        let _mgr_token = manager.CurrentSessionChanged(&mgr_handler).ok();

        let mut active_sub: Option<SessionSubscriptions> = None;
        let tick_counter = AtomicU64::new(0);

        loop {
            let flags = signal.wait_timeout(Duration::from_millis(1000));
            let tick = tick_counter.fetch_add(1, Ordering::Relaxed) + 1;

            // Step 2: Query `manager.GetCurrentSession()` ONLY (`nusa.exe` `0x1400233CF` -> `0x14007A6CB`).
            // Never call `GetSessions()` so we never touch suspended background browser tabs.
            let Ok(session) = manager.GetCurrentSession() else {
                drop(active_sub.take());
                if let Ok(mut gp) = shared_props.lock() {
                    *gp = CachedTrackProps::default();
                }
                update_cache(&cache, None, format!("no session | #{}", tick), "done");
                unsafe {
                    let _ = PostMessageW(hwnd, WM_MEDIA_UPDATE, WPARAM(flags as usize), LPARAM(0));
                }
                continue;
            };

            let session_id = session
                .SourceAppUserModelId()
                .map(|s| s.to_string())
                .unwrap_or_default();

            // Step 3: Re-bind session events if the current session changed (`nusa.exe` `0x1400234EB..0x140023650`).
            let session_changed = active_sub
                .as_ref()
                .map(|s| s.session_id != session_id)
                .unwrap_or(true);

            if session_changed {
                drop(active_sub.take());
                let sig_p = signal.clone();
                let hp = TypedEventHandler::<
                    GlobalSystemMediaTransportControlsSession,
                    PlaybackInfoChangedEventArgs,
                >::new(move |_, _| {
                    sig_p.notify(FLAG_PLAYBACK | FLAG_TIMELINE);
                    Ok(())
                });
                let playback_token = session.PlaybackInfoChanged(&hp).ok();

                let sig_t = signal.clone();
                let ht = TypedEventHandler::<
                    GlobalSystemMediaTransportControlsSession,
                    TimelinePropertiesChangedEventArgs,
                >::new(move |_, _| {
                    sig_t.notify(FLAG_TIMELINE);
                    Ok(())
                });
                let timeline_token = session.TimelinePropertiesChanged(&ht).ok();

                let sig_m = signal.clone();
                let hm = TypedEventHandler::<
                    GlobalSystemMediaTransportControlsSession,
                    MediaPropertiesChangedEventArgs,
                >::new(move |_, _| {
                    sig_m.notify(FLAG_PROPS | FLAG_TIMELINE);
                    Ok(())
                });
                let props_token = session.MediaPropertiesChanged(&hm).ok();

                active_sub = Some(SessionSubscriptions {
                    session: session.clone(),
                    session_id: session_id.clone(),
                    playback_token,
                    timeline_token,
                    props_token,
                });
            }

            // Step 4: Spawn one-shot MTA worker (`nusa.exe` `0x14000A5E8`) when session changed,
            // `MediaPropertiesChanged` fired (`FLAG_PROPS`), or cached title is still empty.
            let title_empty = shared_props
                .lock()
                .ok()
                .map(|g| g.title.is_empty() || g.session_id != session_id)
                .unwrap_or(true);

            if session_changed || (flags & (FLAG_PROPS | FLAG_SESSION)) != 0 || title_empty {
                let gen = PROPS_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
                let session_clone = session.clone();
                let sid_clone = session_id.clone();
                let props_slot = shared_props.clone();
                let sig_done = signal.clone();
                std::thread::spawn(move || {
                    init_mta();
                    if let Ok(op) = session_clone.TryGetMediaPropertiesAsync() {
                        if let Ok(props) = op.get() {
                            if PROPS_GENERATION.load(Ordering::SeqCst) != gen {
                                return;
                            }
                            let title = props.Title().map(|s| s.to_string()).unwrap_or_default();
                            let artist = props.Artist().map(|s| s.to_string()).unwrap_or_default();
                            let album =
                                props.AlbumTitle().map(|s| s.to_string()).unwrap_or_default();
                            let decoded = decode_session_thumbnail(&props);
                            if PROPS_GENERATION.load(Ordering::SeqCst) != gen {
                                return;
                            }
                            if let Ok(mut guard) = props_slot.lock() {
                                guard.session_id = sid_clone;
                                guard.title = title;
                                guard.artist = artist;
                                guard.album = album;
                                if let Some((bgra, w, h)) = decoded {
                                    guard.cover_bgra = Some(Arc::new(bgra));
                                    guard.cover_w = w;
                                    guard.cover_h = h;
                                } else {
                                    guard.cover_bgra = None;
                                    guard.cover_w = 0;
                                    guard.cover_h = 0;
                                }
                            }
                            sig_done.notify(FLAG_PLAYBACK | FLAG_TIMELINE);
                        }
                    }
                });
            }

            // Step 5: Synchronous playback + timeline read (`nusa.exe` `0x14002383D` + `0x1400270FF`).
            let playing = session
                .GetPlaybackInfo()
                .and_then(|info| info.PlaybackStatus())
                .map(|st| st == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing)
                .unwrap_or(false);

            let (position_secs, duration_secs) = read_realtime_timeline(&session, playing);

            let snapshot = shared_props.lock().ok().map(|g| g.clone()).unwrap_or_default();
            if snapshot.title.is_empty() && snapshot.artist.is_empty() {
                update_cache(
                    &cache,
                    None,
                    format!("session active, loading props... | #{}", tick),
                    "done",
                );
            } else {
                let info = MediaInfo {
                    title: snapshot.title,
                    artist: snapshot.artist,
                    album: snapshot.album,
                    source_app: session_id,
                    playing,
                    position_secs,
                    duration_secs,
                    cover_bgra: snapshot.cover_bgra,
                    cover_w: snapshot.cover_w,
                    cover_h: snapshot.cover_h,
                };
                update_cache(
                    &cache,
                    Some(info),
                    format!("ok ({}) | #{}", if playing { "playing" } else { "paused" }, tick),
                    "done",
                );
            }

            unsafe {
                let _ = PostMessageW(hwnd, WM_MEDIA_UPDATE, WPARAM(flags as usize), LPARAM(0));
            }
        }
    });
}

fn update_cache(cache: &Mutex<PollCache>, info: Option<MediaInfo>, status: String, phase: &str) {
    if let Ok(mut guard) = cache.lock() {
        guard.info = info;
        guard.status = status;
        guard.phase = phase.to_string();
    }
}

fn set_phase(cache: &Mutex<PollCache>, phase: &str) {
    if let Ok(mut guard) = cache.lock() {
        guard.phase = phase.to_string();
    }
}

/// Nusa's exact timeline calculation (`0x1400270FF..0x1400273A5`):
/// Uses `Position - StartTime`, `EndTime - StartTime`, and when `playing == true`,
/// adds `Now_FILETIME - LastUpdatedTime` so position is exact without drift.
fn read_realtime_timeline(
    session: &GlobalSystemMediaTransportControlsSession,
    playing: bool,
) -> (f64, f64) {
    let Ok(timeline) = session.GetTimelineProperties() else {
        return (0.0, 0.0);
    };
    let start_ticks = timeline.StartTime().map(|t| t.Duration).unwrap_or(0);
    let end_ticks = timeline.EndTime().map(|t| t.Duration).unwrap_or(0);
    let raw_pos_ticks = timeline.Position().map(|t| t.Duration).unwrap_or(0);
    let last_updated_ticks = timeline
        .LastUpdatedTime()
        .map(|dt| dt.UniversalTime)
        .unwrap_or(0);

    let dur_ticks = (end_ticks - start_ticks).max(0);
    let mut pos_ticks = (raw_pos_ticks - start_ticks).max(0);

    if playing && last_updated_ticks > 0 {
        let now_unix_ticks = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| (d.as_nanos() / 100) as i64)
            .unwrap_or(0);
        if now_unix_ticks > 0 {
            let now_filetime_ticks = now_unix_ticks + FILETIME_UNIX_DIFF_TICKS;
            let delta_ticks = now_filetime_ticks - last_updated_ticks;
            if delta_ticks > 0 && delta_ticks < 432_000_000_000 {
                pos_ticks += delta_ticks;
            }
        }
    }

    if dur_ticks > 0 && pos_ticks > dur_ticks {
        pos_ticks = dur_ticks;
    }

    let position_secs = (pos_ticks as f64 / 10_000_000.0).max(0.0);
    let duration_secs = (dur_ticks as f64 / 10_000_000.0).max(0.0);
    (position_secs, duration_secs)
}

/// Decode GSMTC `Thumbnail()` (`IRandomAccessStreamReference`) into `COVER_SIZE x COVER_SIZE`
/// premultiplied BGRA pixels using `DataReader` + WIC (`nusa.exe` `0x1400033CF` & `0x14000AA0A`).
fn decode_session_thumbnail(
    props: &GlobalSystemMediaTransportControlsSessionMediaProperties,
) -> Option<(Vec<u8>, u32, u32)> {
    let thumb_ref = props.Thumbnail().ok()?;
    let stream = thumb_ref.OpenReadAsync().ok()?.get().ok()?;
    let size = (stream.Size().ok()? as usize).min(8 * 1024 * 1024);
    if size == 0 {
        return None;
    }
    let input: IInputStream = stream.cast().ok()?;
    let reader = DataReader::CreateDataReader(&input).ok()?;
    let loaded = reader.LoadAsync(size as u32).ok()?.get().ok()? as usize;
    if loaded == 0 {
        return None;
    }
    let mut raw_bytes = vec![0u8; loaded];
    reader.ReadBytes(&mut raw_bytes).ok()?;
    let _ = reader.Close();

    decode_image_bytes_wic(&raw_bytes, COVER_SIZE, COVER_SIZE)
}

/// Decode arbitrary encoded image bytes (PNG/JPEG/BMP/WebP) into `target_w x target_h`
/// 32-bit premultiplied BGRA (`GUID_WICPixelFormat32bppPBGRA`) via WIC.
pub fn decode_image_bytes_wic(
    bytes: &[u8],
    target_w: u32,
    target_h: u32,
) -> Option<(Vec<u8>, u32, u32)> {
    unsafe {
        let factory: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let wic_stream: IWICStream = factory.CreateStream().ok()?;
        wic_stream.InitializeFromMemory(bytes).ok()?;
        let decoder = factory
            .CreateDecoderFromStream(&wic_stream, std::ptr::null(), WICDecodeMetadataCacheOnDemand)
            .ok()?;
        let frame: IWICBitmapFrameDecode = decoder.GetFrame(0).ok()?;

        let scaler = factory.CreateBitmapScaler().ok()?;
        let frame_src: IWICBitmapSource = frame.cast().ok()?;
        scaler
            .Initialize(
                &frame_src,
                target_w,
                target_h,
                WICBitmapInterpolationModeFant,
            )
            .ok()?;

        let converter: IWICFormatConverter = factory.CreateFormatConverter().ok()?;
        let scaler_src: IWICBitmapSource = scaler.cast().ok()?;
        converter
            .Initialize(
                &scaler_src,
                &GUID_WICPixelFormat32bppPBGRA,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )
            .ok()?;

        let stride = target_w * 4;
        let buf_size = (stride * target_h) as usize;
        let mut pixels = vec![0u8; buf_size];
        converter
            .CopyPixels(std::ptr::null(), stride, &mut pixels)
            .ok()?;
        Some((pixels, target_w, target_h))
    }
}

/// Run a media control command on a one-shot MTA worker thread (`nusa.exe` `0x14000ABBC`)
/// and immediately wake the GSMTC coordinator.
fn spawn_control(action: fn() -> bool) {
    std::thread::spawn(move || {
        init_mta();
        let _ = action();
        signal_media_event(FLAG_PLAYBACK | FLAG_TIMELINE | FLAG_PROPS);
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
    with_session(|s| s.TryPlayAsync()?.get())
}

pub fn pause() -> bool {
    with_session(|s| s.TryPauseAsync()?.get())
}

pub fn skip_next() -> bool {
    with_session(|s| s.TrySkipNextAsync()?.get())
}

pub fn skip_previous() -> bool {
    with_session(|s| s.TrySkipPreviousAsync()?.get())
}

#[derive(Clone)]
pub struct PollCache {
    pub info: Option<MediaInfo>,
    pub status: String,
    pub phase: String,
}

impl PollCache {
    pub fn waiting() -> Self {
        Self {
            info: None,
            status: "waiting for GSMTC (MTA)...".to_string(),
            phase: "starting".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub source_app: String,
    pub playing: bool,
    pub position_secs: f64,
    pub duration_secs: f64,
    pub cover_bgra: Option<Arc<Vec<u8>>>,
    pub cover_w: u32,
    pub cover_h: u32,
}

pub struct MediaTracker {
    initialized: bool,
    last_key: String,
    last_raw_secs: f64,
    estimated_secs: f64,
    last_update: Instant,
    playing: bool,
}

impl MediaTracker {
    pub fn new() -> Self {
        Self {
            initialized: false,
            last_key: String::new(),
            last_raw_secs: 0.0,
            estimated_secs: 0.0,
            last_update: Instant::now(),
            playing: false,
        }
    }

    pub fn update(&mut self, raw: Option<MediaInfo>) -> Option<MediaInfo> {
        let mut raw = match raw {
            Some(m) => m,
            None => {
                self.initialized = false;
                return None;
            }
        };
        let key = format!("{}|{}", raw.title, raw.artist);
        let now = Instant::now();
        if !self.initialized || key != self.last_key {
            self.estimated_secs = raw.position_secs;
            self.last_raw_secs = raw.position_secs;
            self.last_key = key;
            self.initialized = true;
        } else {
            if self.playing {
                let elapsed = now.duration_since(self.last_update).as_secs_f64();
                self.estimated_secs += elapsed;
            }
            if (raw.position_secs - self.estimated_secs).abs() > 1.5
                || (raw.position_secs - self.last_raw_secs).abs() > 1.5
            {
                self.estimated_secs = raw.position_secs;
            }
            self.last_raw_secs = raw.position_secs;
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
