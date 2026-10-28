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
//! Fix 2026-10-28 (deteksi intermittent, lebih sering tidak kedetect):
//! - poll_status kini mengutamakan sesi PLAYING dari daftar semua sesi
//!   dibanding GetCurrentSession mentah: kode lama mengembalikan sesi
//!   current apa adanya bila ada judulnya, walau itu sesi paused yang
//!   sudah basi sementara aplikasi lain sedang memutar lagu.
//! - Poller kini loop thread + satu worker per poll + watchdog: satu
//!   .get() yang nyangkut dulu membekukan cache di None selamanya;
//!   kini worker tanpa cache write >10 detik ditinggalkan (written_seq
//!   guard) dan diganti, dengan backoff 3s -> 30s.
//! - Kontrol jalan di thread STA sekali-pakai DENGAN queue+loop
//!   sendiri, pelajaran yang sama dengan poller (thread tanpa queue
//!   tidak pernah kembali dari RequestAsync di sini).
//! Penyebab pasti di mesin user tidak bisa dibuktikan dari VM ini
//! (tidak ada Rust/Windows di sini); fix ini menutup tiga jalur
//! gagal yang nyata ada di kode + status line menunjukkan kasus
//! stuck bila terjadi ("poll stuck, worker restarted").
//! Lanjutan 2026-10-28 (laporan user: status stuck muncul):
//! - Ambang watchdog 3s -> 10s dan tulisan worker basi tidak lagi
//!   dibuang mentah-mentah (written_seq): poll lambat tapi selesai
//!   kini tetap mendarat; hanya worker yang benar-benar menggantung
//!   yang diganti. Ambang 3s sebelumnya bisa salah menuduh poll
//!   lambat sebagai stuck, lalu membuang hasilnya sendiri.
//! - Urutan poll diubah: current-yang-Playing dulu (fast path),
//!   baru pindai daftar untuk Playing — satu sesi macet di daftar
//!   tidak lagi memblokir sesi sehat yang sedang diputar.
//! RE Nusa 2026-10-28 (status stuck MASIH muncul setelah fix 10 detik):
//! - Poller diganti ke gaya Nusa: SATU STA loop thread dengan timer
//!   (RE: Nusa memakai satu message-loop thread tulisan tangan +
//!   timer, tanpa worker-thread polling di mana pun di biner).
//!   Worker-thread Aura adalah outlier — dan karena stuck bertahan
//!   melewati ambang 10 detik, worker terbukti tidak pernah selesai
//!   sama sekali, cocok dengan kegagalan tanpa-pump yang sudah
//!   didokumentasikan tim (b19b92a/9f1f911).
//! - Poll kini INLINE di timer handler thread yang dipump (satu-satunya
//!   bentuk yang terbukti mengembalikan hasil di riwayat Aura).
//!   Watchdog thread terpisah (tanpa COM, hanya Atomics) melaporkan
//!   freeze dan menelurkan incarnation baru bila tidak ada cache
//!   write >10 detik; incarnation lama yang belakangan lolos tetap
//!   boleh menulis sekali (data segar yang tersedia), lalu keluar
//!   di tick berikutnya — gate lama terbukti membuang poll lambat
//!   selamanya sehingga 'stuck' tampil permanen (fix malam 28 Okt).
//! - Batas kejujuran: RE membuktikan arsitektur timer + single-thread
//!   Nusa, BUKAN pola panggil GSMTC persisnya (poll vs event WinRT
//!   tidak bisa dipulihkan dari biner stripped — hanya string
//!   runtime-class GSMTC yang ada). Yang diikuti adalah arsitektur,
//!   bukan klaim salinan panggilan GSMTC Nusa.
//! If `cargo build` reports a signature mismatch (notably `.get()` on
//! IAsyncOperation, HSTRING conversion, or TimeSpan field access), paste
//! it — Eve cannot compile Rust in this VM, so the first build on the
//! user's Windows PC is where WinRT signatures get corrected.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::Result;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, PeekMessageW, PostThreadMessageW, SetTimer, TranslateMessage,
    MSG, PM_NOREMOVE, WM_TIMER,
};
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

/// Start the background poller: one STA thread with its OWN
/// message loop, polling GSMTC every 250ms on a thread timer and
/// writing the result into `cache`. The UI thread only ever reads
/// the cache, never calls poll() itself.
///
/// Why a message loop, and why this is the third attempt:
/// - b19b92a poller (MTA, sleep loop): cache stayed None.
/// - 9f1f911 poller (STA, sleep loop): cache stayed None.
/// - ca373ed poll on the UI thread (1s): app went "Not Responding"
///   immediately after run.
/// The common thread of the two poller failures: a thread with NO
/// message queue/loop never returns from RequestAsync().get() — the
/// WinRT async completion cannot be delivered to it, so .get()
/// waits forever and the cache never updates. The UI thread has a
/// message loop (which is why Phase 1 polling worked there), but
/// blocking it with .get() now freezes the window outright. So the
/// poller thread gets its own loop: PeekMessage creates the queue,
/// SetTimer(None, ...) posts WM_TIMER to that thread every 250ms,
/// and GetMessage dispatches it; poll() runs inside that loop, where
/// .get() can pump the completion messages. A stuck poll only
/// freezes this thread's cache, never the main window.
/// Shared state for the Nusa-style poller (see spawn_poller).
/// Only one poller incarnation polls at a time BY DESIGN, but an
/// incarnation frozen inside .get() cannot stop itself: a separate
/// watchdog thread watches last_write and spawns a fresh incarnation.
/// Generation marks the active incarnation; a superseded one exits
/// at its next timer tick instead of polling forever. Writes are
/// NOT generation-gated (see incarnation handler): a late write is
/// still the freshest available data.
struct PollerShared {
    cache: Arc<Mutex<PollCache>>,
    generation: AtomicU64,
    tick_counter: AtomicU64,
    health: Mutex<PollerHealth>,
}

struct PollerHealth {
    last_write: Instant,
    backoff_secs: u64,
    next_respawn_at: Instant,
}

/// Start the poller, Nusa-style (RE 2026-10-28, static-findings.md):
/// Nusa runs everything on ONE hand-written message-loop thread with
/// timers (timer 1 = main tick; animation on a re-armed timer; no
/// worker-thread polling anywhere in the binary). Aura's worker
/// threads were the outlier — and after the 10s-threshold fix the
/// "poll stuck" status STILL persisted, i.e. workers never complete
/// at all on this machine. That matches the team's own documented
/// failure (b19b92a/9f1f911): a thread whose queue is created but
/// never pumped never returns from RequestAsync().get().
/// So: the poll runs INLINE in a pumped STA loop thread's timer
/// handler (the configuration that demonstrably returned results in
/// Aura's own history), and a watchdog thread (no COM, only Atomics)
/// reports a freeze and spawns a fresh incarnation when no cache
/// write lands for >10s. A superseded incarnation that later
/// unblocks writes once if it completes, then exits at its next
/// timer tick (see handler).
/// Honest limit: the RE proved Nusa's timer-driven single-thread
/// architecture, not its exact GSMTC call pattern (poll vs WinRT
/// events is not recoverable from the stripped binary — only the
/// runtime-class string is). This follows the architecture, not a
/// claimed copy of Nusa's GSMTC calls.
pub fn spawn_poller(cache: Arc<Mutex<PollCache>>) {
    let shared = Arc::new(PollerShared {
        cache,
        generation: AtomicU64::new(0),
        tick_counter: AtomicU64::new(0),
        health: Mutex::new(PollerHealth {
            last_write: Instant::now(),
            backoff_secs: 3,
            next_respawn_at: Instant::now(),
        }),
    });
    spawn_poller_incarnation(shared.clone());
    // Watchdog: sleep loop, no COM, never touches GSMTC.
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(250));
        let now = Instant::now();
        let mut respawn = false;
        if let Ok(mut health) = shared.health.lock() {
            if now.duration_since(health.last_write) > Duration::from_secs(10)
                && now >= health.next_respawn_at
            {
                shared.generation.fetch_add(1, Ordering::SeqCst);
                health.next_respawn_at = now + Duration::from_secs(health.backoff_secs);
                health.backoff_secs = (health.backoff_secs * 3).min(30);
                respawn = true;
            }
        }
        if respawn {
            if let Ok(mut guard) = shared.cache.lock() {
                guard.status = "poll stuck, loop restarted".to_string();
            }
            spawn_poller_incarnation(shared.clone());
        }
    });
}

/// One poller incarnation: STA thread, own queue, 250ms timer, poll
/// INLINE in the timer handler (pumped thread — the only shape that
/// ever returned results here). Exits when its message loop ends
/// (practically never; superseded incarnations are abandoned, not
/// killed — a frozen one just leaks; a superseded one that later
/// unblocks writes once, then exits at its next timer tick).
fn spawn_poller_incarnation(shared: Arc<PollerShared>) {
    std::thread::spawn(move || {
        init_com();
        let gen = shared.generation.load(Ordering::SeqCst);
        unsafe {
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            let _ = SetTimer(None, 1, 250, None);
            loop {
                let ret = GetMessageW(&mut msg, None, 0, 0);
                if ret.0 <= 0 {
                    break;
                }
                if msg.message == WM_TIMER {
                    // Superseded incarnation: exit instead of polling
                    // forever alongside its replacement (bounds leaked
                    // threads; the replacement already polls).
                    if shared.generation.load(Ordering::SeqCst) != gen {
                        break;
                    }
                    let tick = shared.tick_counter.fetch_add(1, Ordering::SeqCst) + 1;
                    let (info, status) = poll_status();
                    // Accept EVERY completed poll (fix 2026-10-28 malam):
                    // the old generation gate discarded slow polls
                    // forever — a poll slower than the watchdog fuse
                    // was replaced, then its result rejected, then the
                    // replacement was replaced too, so no write ever
                    // landed and 'poll stuck' showed permanently even
                    // though polls eventually completed. A late write
                    // is still the freshest available data; the next
                    // poll (250ms later) corrects any staleness.
                    if let Ok(mut guard) = shared.cache.lock() {
                        *guard = PollCache {
                            info,
                            status: format!("{} | #{}", status, tick),
                        };
                    }
                    if let Ok(mut health) = shared.health.lock() {
                        health.last_write = Instant::now();
                        health.backoff_secs = 3;
                    }
                } else {
                    let _ = TranslateMessage(&msg);
                    let _ = DispatchMessageW(&msg);
                }
            }
        }
    });
}

/// Message a control thread posts to itself (see spawn_control).
const CONTROL_MSG: u32 = 0x8002; // WM_APP + 2 (tray callback uses 0x8001)

/// Run a control action on a throwaway STA thread WITH its own message
/// queue + loop. Same lesson as the poller: a bare thread (init_com +
/// direct .get(), no queue) never returns from RequestAsync on this
/// machine's COM setup (b19b92a/9f1f911). The thread posts itself
/// CONTROL_MSG, runs the action while its loop owns the queue, then
/// exits. Fire-and-forget, like before: the next poll refreshes the
/// card icon/progress within a tick.
fn spawn_control(action: fn() -> bool) {
    std::thread::spawn(move || {
        init_com();
        unsafe {
            let mut msg = MSG::default();
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
            let tid = GetCurrentThreadId();
            let _ = PostThreadMessageW(tid, CONTROL_MSG, WPARAM(action as usize), LPARAM(0));
            loop {
                let ret = GetMessageW(&mut msg, None, 0, 0);
                if ret.0 <= 0 {
                    break;
                }
                if msg.message == CONTROL_MSG {
                    let f: fn() -> bool = std::mem::transmute(msg.wParam.0);
                    let _ = f();
                    break;
                }
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
            }
        }
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

/// Poll cache: what the last poll found + a short status string
/// shown on the no-music card, so a failed detection can be diagnosed
/// from a screenshot instead of guessing (waiting / no session /
/// request failed / sessions: N, none usable / ...).
#[derive(Clone)]
pub struct PollCache {
    pub info: Option<MediaInfo>,
    pub status: String,
}

impl PollCache {
    pub fn waiting() -> Self {
        Self {
            info: None,
            status: "waiting for first poll...".to_string(),
        }
    }
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

/// Poll with a status string. Selection order (fix 2026-10-28,
/// revisi malam yang sama setelah laporan 'poll stuck'):
/// 1. Current session bila PLAYING (fast path lama): kembali
///    langsung tanpa memindai daftar, supaya satu sesi macet di
///    daftar tidak memblokir sesi sehat yang sedang diputar.
/// 2. PLAYING session dari daftar penuh (memperbaiki bug lama di
///    mana current paused yang basi mengalahkan sesi Playing lain
///    — satu penyebab langsung 'musik tidak kedetect' saat lagu jalan).
/// 3. Current paused bertitel, lalu 4. paused pertama dari daftar.
/// Per-session property errors are still skipped (transient during
/// track changes), and the UI's 1s None-hysteresis still covers short
/// gaps. Status strings stay short for the no-music card line.
pub fn poll_status() -> (Option<MediaInfo>, String) {
    let manager = match GlobalSystemMediaTransportControlsSessionManager::RequestAsync() {
        Ok(op) => match op.get() {
            Ok(m) => m,
            Err(e) => {
                return (
                    None,
                    format!("request failed {:#010x}", e.code().0 as u32),
                )
            }
        },
        Err(e) => {
            return (
                None,
                format!("request failed {:#010x}", e.code().0 as u32),
            )
        }
    };
    // 1. Current session dulu (fast path lama): bila Playing,
    //    langsung kembali tanpa memindai daftar.
    let mut current_fallback: Option<MediaInfo> = None;
    if let Ok(session) = manager.GetCurrentSession() {
        if let Ok(info) = session_info(&session) {
            if !info.title.is_empty() {
                if info.playing {
                    return (Some(info), "ok".to_string());
                }
                current_fallback = Some(info);
            }
        }
    }
    // 2. Daftar penuh: cari yang Playing.
    let mut paused_fallback: Option<MediaInfo> = None;
    let mut session_count: u32 = 0;
    if let Ok(sessions) = manager.GetSessions() {
        let size = sessions.Size().unwrap_or(0);
        session_count = size;
        for i in 0..size {
            let Ok(session) = sessions.GetAt(i) else {
                continue;
            };
            let Ok(info) = session_info(&session) else {
                continue;
            };
            if info.title.is_empty() {
                continue;
            }
            if info.playing {
                return (Some(info), "ok (playing)".to_string());
            }
            if paused_fallback.is_none() {
                paused_fallback = Some(info);
            }
        }
    }
    // 3. Paused fallbacks: current first, then list.
    if let Some(info) = current_fallback {
        return (Some(info), "ok (current, paused)".to_string());
    }
    if let Some(info) = paused_fallback {
        return (Some(info), "ok (session list, paused)".to_string());
    }
    if session_count == 0 {
        (None, "no session".to_string())
    } else {
        (None, format!("sessions: {}, none usable", session_count))
    }
}

/// Kept for callers that only need the info (controls do not use it).
pub fn poll() -> Option<MediaInfo> {
    poll_status().0
}

/// One session -> MediaInfo (properties + playback + timeline).
fn session_info(session: &GlobalSystemMediaTransportControlsSession) -> Result<MediaInfo> {
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
