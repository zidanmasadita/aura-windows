//! App: window + state + animator + renderer + tray + timers.
//! Owns the HWND (via GWLP_USERDATA) and drives the pill lifecycle:
//! Zen (idle) -> hover -> Compact -> click/hover (with session) ->
//! Expanded (music card) -> bubble -> Lyrics (700x180) -> bubble back
//! to Expanded; click outside controls / leave -> Compact (with
//! session) or Zen (silent). Timers: 0.25s tick (clock + GSMTC poll +
//! synchronous repaint for realtime lyrics/progress), 0.25s alert
//! placeholder (Phase 4), 16ms (animation, only while animating).

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{HBRUSH, InvalidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TRACKMOUSEEVENT, TME_LEAVE, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::animator;
use crate::lyrics;
use crate::media;
use crate::renderer;
use crate::state;
use crate::tray;
use crate::window;

const TIMER_TICK: usize = 1; // 1s: clock + (Phase 1+) media poll
const TIMER_ALERT: usize = 2; // 0.25s: alert placeholder (Phase 4)
const TIMER_ANIM: usize = 3; // 16ms: size animation (only while animating)
// Win32 WM_MOUSELEAVE (0x02A3), sent by TrackMouseEvent when the cursor
// leaves the window. Defined locally because windows-rs 0.58 does not export
// it from KeyboardAndMouse nor WindowsAndMessaging.
const WM_MOUSELEAVE: u32 = 0x02A3;

pub struct App {
    hwnd: HWND,
    renderer: renderer::Renderer,
    pill: state::PillState,
    anim: Option<animator::SizeAnimation>,
    tracking_mouse: bool,
    clock_text: String, // HH:MM, updated on 1s tick
    media: Option<media::MediaInfo>, // Current GSMTC session (Phase 1), None = silent.
    lyrics: Option<lyrics::LyricsCache>, // Fetched lyrics for current song (Phase 1b), None = not fetched yet.
}

fn current_time_text() -> String {
    // Local clock via chrono (avoids Win32 GetLocalTime module uncertainty
    // in windows-rs 0.58; chrono handles Windows local time internally).
    chrono::Local::now().format("%H:%M").to_string()
}

impl App {
    fn new(hwnd: HWND) -> Result<Box<Self>> {
        let renderer = renderer::Renderer::new()?;
        let mut app = Box::new(Self {
            hwnd,
            renderer,
            pill: state::PillState::new(),
            anim: None,
            tracking_mouse: false,
            clock_text: current_time_text(),
            media: None,
            lyrics: None,
        });
        // Initial size + region + placement (Zen, top-center, no animation).
        app.apply_size();
        Ok(app)
    }

    /// Apply current pill size to the window (resize + reposition top-center + region).
    fn apply_size(&mut self) {
        let scale = window::dpi_scale(self.hwnd);
        let w_px = (self.pill.width * scale).round() as i32;
        let h_px = (self.pill.height * scale).round() as i32;
        window::place_top_center(self.hwnd, w_px, h_px);
        // Region radius: fully rounded for short pills (height/2), 28pt*scale for cards.
        let radius = if self.pill.height <= 33.0 {
            h_px / 2
        } else {
            (28.0 * scale).round() as i32
        };
        window::set_pill_region(self.hwnd, w_px, h_px, radius);
        // Resize render target to new physical size (D2D scales DIPs via DPI).
        self.renderer.resize(w_px, h_px);
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, BOOL(0));
        }
    }

    fn set_state(&mut self, new_state: state::IslandState) {
        if new_state == self.pill.state && self.anim.is_none() {
            return;
        }
        let (from_w, from_h) = (self.pill.width, self.pill.height);
        self.pill.state = new_state;
        let (to_w, to_h) = new_state.logical_size();
        // If size is already the target, no animation needed.
        if (from_w - to_w).abs() < 0.01 && (from_h - to_h).abs() < 0.01 {
            self.pill.width = to_w;
            self.pill.height = to_h;
            self.anim = None;
            self.apply_size();
            return;
        }
        self.anim = Some(animator::SizeAnimation::new(from_w, from_h, to_w, to_h));
        unsafe {
            let _ = SetTimer(self.hwnd, TIMER_ANIM, 16, None);
        }
    }

    fn on_anim_tick(&mut self) {
        if let Some(anim) = &self.anim {
            if let Some((w, h)) = anim.tick() {
                self.pill.width = w;
                self.pill.height = h;
                self.apply_size();
                return;
            }
        }
        // Finished (or no animation): snap to target + stop timer.
        let (tw, th) = self.pill.target_size();
        self.pill.width = tw;
        self.pill.height = th;
        self.anim = None;
        self.apply_size();
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_ANIM);
        }
    }

    fn on_timer(&mut self, id: usize) {
        match id {
            TIMER_TICK => {
                // 0.25s tick: poll GSMTC + update clock + repaint NOW.
                // (Was 1s + async InvalidateRect; user report 2026-10-27:
                // open lyrics panel did not advance until pill reopened,
                // so paint synchronously here instead of queuing WM_PAINT.)
                self.media = media::poll();
                // Lyrics cache follows song: new song (or session end) clears
                // cached lyrics; if the lyrics card is open for the old song,
                // fall back to the music card for the new song (fetching new
                // lyrics happens lazily on bubble click, never on tick, so a
                // song change never freezes the UI on a network fetch).
                let media_key = self
                    .media
                    .as_ref()
                    .map(|m| lyrics::key_for(&m.title, &m.artist));
                let lyrics_stale = match (&self.lyrics, &media_key) {
                    (Some(cache), Some(key)) => &cache.key != key,
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                if lyrics_stale {
                    self.lyrics = None;
                    if self.pill.state == state::IslandState::Lyrics {
                        self.set_state(state::IslandState::Expanded);
                    }
                }
                // Idle-state follows session: session -> Compact, none -> Zen
                // (Expanded/Lyrics also close to Zen when the session ends).
                // (Bool first, so set_state below holds no borrow of media.)
                let has_media = self.media.is_some();
                if has_media {
                    if self.pill.state == state::IslandState::Zen {
                        self.set_state(state::IslandState::Compact);
                    }
                } else if self.pill.state == state::IslandState::Compact
                    || self.pill.state == state::IslandState::Expanded
                    || self.pill.state == state::IslandState::Lyrics
                {
                    self.set_state(state::IslandState::Zen);
                }
                self.clock_text = current_time_text();
                // Synchronous repaint (not just InvalidateRect): draws the
                // current position immediately, so lyrics/progress advance
                // in realtime while the panel stays open.
                self.on_paint();
            }
            TIMER_ALERT => {
                // 0.25s alert tick: placeholder for Phase 4 (no-op in Phase 0).
            }
            TIMER_ANIM => self.on_anim_tick(),
            _ => {}
        }
    }

    fn on_mouse_move(&mut self) {
        if !self.tracking_mouse {
            unsafe {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: self.hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut tme);
            }
            self.tracking_mouse = true;
        }
        // Hover: Zen -> Compact (idle); Compact + session -> Expanded
        // (Aura Mac: hover opens the card when a session exists).
        // If already Expanded, hovering keeps it Expanded.
        if self.pill.state == state::IslandState::Zen {
            self.set_state(state::IslandState::Compact);
        } else if self.pill.state == state::IslandState::Compact && self.media.is_some() {
            self.set_state(state::IslandState::Expanded);
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking_mouse = false;
        // Cursor exit: session -> Compact, silent -> Zen (Aura Mac rule).
        if self.media.is_some() {
            self.set_state(state::IslandState::Compact);
        } else {
            self.set_state(state::IslandState::Zen);
        }
    }

    fn on_click(&mut self, x_px: i32, y_px: i32) {
        // Expanded music card: control buttons act (stay Expanded, refresh
        // media immediately so the play/pause icon updates without waiting
        // for the 1s tick); bubble opens lyrics; any other click closes.
        if self.pill.state == state::IslandState::Expanded {
            if self.media.is_some() {
                let scale = window::dpi_scale(self.hwnd);
                let x = x_px as f32 / scale;
                let y = y_px as f32 / scale;
                let in_rect =
                    |r: (f32, f32, f32, f32)| x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3;
                let playing = self.media.as_ref().map(|m| m.playing).unwrap_or(false);
                if in_rect(renderer::MUSIC_PREV_RECT) {
                    let _ = media::skip_previous();
                    self.media = media::poll();
                } else if in_rect(renderer::MUSIC_PLAY_RECT) {
                    if playing {
                        let _ = media::pause();
                    } else {
                        let _ = media::play();
                    }
                    self.media = media::poll();
                } else if in_rect(renderer::MUSIC_NEXT_RECT) {
                    let _ = media::skip_next();
                    self.media = media::poll();
                } else if in_rect(renderer::MUSIC_LYRICS_BUBBLE_RECT) {
                    // Fetch lyrics lazily on first entry (blocking; may
                    // freeze briefly on slow networks — async is a follow-up).
                    if self.lyrics.is_none() {
                        let song = self.media.as_ref().map(|m| {
                            (m.title.clone(), m.artist.clone(), m.album.clone())
                        });
                        if let Some((title, artist, album)) = song {
                            let cache = lyrics::fetch(&title, &artist, &album);
                            self.lyrics = Some(cache);
                        }
                    }
                    self.set_state(state::IslandState::Lyrics);
                    return;
                } else {
                    self.set_state(state::IslandState::Compact);
                    return;
                }
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                }
                return;
            }
            // No session: any click closes the no-music card to Compact.
            self.set_state(state::IslandState::Compact);
            return;
        }
        // Lyrics panel: bubble back to music, nudge buttons shift sync
        // (synced lyrics only), any other click closes to Compact.
        if self.pill.state == state::IslandState::Lyrics {
            if self.media.is_some() {
                let scale = window::dpi_scale(self.hwnd);
                let x = x_px as f32 / scale;
                let y = y_px as f32 / scale;
                let in_rect =
                    |r: (f32, f32, f32, f32)| x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3;
                let synced = self.lyrics.as_ref().map(|c| c.synced).unwrap_or(false);
                if in_rect(renderer::LYRICS_MUSIC_BUBBLE_RECT) {
                    self.set_state(state::IslandState::Expanded);
                    return;
                }
                if synced && in_rect(renderer::LYRICS_NUDGE_MINUS_RECT) {
                    let mut saved: Option<(String, i64)> = None;
                    if let Some(cache) = &mut self.lyrics {
                        cache.nudge_ms -= 500;
                        saved = Some((cache.key.clone(), cache.nudge_ms));
                    }
                    if let Some((key, nudge)) = saved {
                        lyrics::save_nudge(&key, nudge);
                    }
                    unsafe {
                        let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                    }
                    return;
                }
                if synced && in_rect(renderer::LYRICS_NUDGE_PLUS_RECT) {
                    let mut saved: Option<(String, i64)> = None;
                    if let Some(cache) = &mut self.lyrics {
                        cache.nudge_ms += 500;
                        saved = Some((cache.key.clone(), cache.nudge_ms));
                    }
                    if let Some((key, nudge)) = saved {
                        lyrics::save_nudge(&key, nudge);
                    }
                    unsafe {
                        let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                    }
                    return;
                }
                self.set_state(state::IslandState::Compact);
                return;
            }
            // No session: any click closes the lyrics panel to Compact.
            self.set_state(state::IslandState::Compact);
            return;
        }
        // Not Expanded/Lyrics: click expands (music card if session, no-music card if not).
        self.set_state(state::IslandState::Expanded);
    }

    fn on_paint(&mut self) {
        // Renderer draws current state (clock for Compact, music/no-music
        // card for Expanded, lyrics panel for Lyrics, "Aura" label for
        // Zen). ValidateRect inside draw prevents a paint loop.
        self.renderer.draw(
            self.hwnd,
            self.pill.state,
            &self.clock_text,
            self.media.as_ref(),
            self.lyrics.as_ref(),
        );
    }

    fn on_tray_callback(&mut self, event: u32) {
        if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
            let id = tray::show_menu(self.hwnd);
            if id == tray::MENU_QUIT {
                unsafe {
                    PostQuitMessage(0);
                }
            } else if id == tray::MENU_STATUS {
                tray::show_status_placeholder(self.hwnd);
            }
        }
    }
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Retrieve App (may be null for early messages during CreateWindowExW).
    let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if app_ptr.is_null() {
        // No App yet (or already destroyed): default handling.
        // WM_DESTROY with null App still needs PostQuitMessage (DefWindowProc
        // does NOT post quit).
        if msg == WM_DESTROY {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let app = &mut *app_ptr;
    match msg {
        WM_PAINT => {
            app.on_paint();
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            app.on_mouse_move();
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            app.on_mouse_leave();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            // Client-area click position (physical pixels) for music-card
            // control hit-testing (prev/play/next vs close).
            let x = ((lparam.0 as u32) & 0xFFFF) as u16 as i16 as i32;
            let y = (((lparam.0 as u32) >> 16) & 0xFFFF) as u16 as i16 as i32;
            app.on_click(x, y);
            LRESULT(0)
        }
        WM_TIMER => {
            app.on_timer(wparam.0);
            LRESULT(0)
        }
        WM_DESTROY => {
            // Remove tray icon, free App, quit.
            tray::remove_icon(hwnd);
            // Free the Box (reconstruct + drop). After this, app_ptr is dangling;
            // do not touch `app` after this point (we return immediately).
            let _ = Box::from_raw(app_ptr);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => {
            if msg == tray::TRAY_CALLBACK_MSG {
                let event = (lparam.0 & 0xFFFF) as u32;
                app.on_tray_callback(event);
                return LRESULT(0);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }
}

pub fn run() -> Result<()> {
    unsafe {
        // Single instance (Nusa pattern). If another Aura is running, exit quietly.
        // CreateMutexW returns a valid handle even when already-exists; the
        // distinction is GetLastError == 183 (ERROR_ALREADY_EXISTS).
        let mutex = CreateMutexW(None, BOOL(1), w!("Aura.SingleInstance"))?;
        if GetLastError().0 == 183 {
            // Another instance owns the mutex. Exit (do not release; the
            // existing instance keeps it).
            return Ok(());
        }
        // DPI awareness (PerMonitorV2) before window creation.
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let hmodule = GetModuleHandleW(None)?;
        let hinstance: HINSTANCE = hmodule.into();
        // Window class.
        let class_name = w!("AuraIslandWindow");
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()), // No background brush (D2D paints everything).
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: HICON(std::ptr::null_mut()),
        };
        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            return Err(windows::core::Error::from_win32());
        }
        // Create window (initial size = Zen placeholder; App::new resizes immediately).
        // ExStyle: NOACTIVATE | TOOLWINDOW | TOPMOST (Nusa recipe, minus
        // NOREDIRECTIONBITMAP for Phase 0 D2D-HWND renderer — see window.rs note).
        let hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name,
            w!("Aura"),
            WS_POPUP,
            0,
            0,
            140,
            32,
            None,
            None,
            hinstance,
            None,
        )?;
        // App (owns renderer + state). Store in USERDATA for WndProc.
        let app = App::new(hwnd)?;
        let app_ptr = Box::into_raw(app);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize);
        // Tray icon (must be after window creation; uses hwnd).
        let _ = tray::add_icon(hwnd);
        // Timers: 0.25s tick (clock + media, realtime) + 0.25s alert placeholder.
        let _ = SetTimer(hwnd, TIMER_TICK, 250, None);
        let _ = SetTimer(hwnd, TIMER_ALERT, 250, None);
        // Show (no activation, so it never steals focus).
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        // Message loop.
        let mut msg = MSG::default();
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if ret.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
        // Cleanup: release mutex (OS would reclaim on exit anyway, but be tidy).
        let _ = ReleaseMutex(mutex);
        Ok(())
    }
}
