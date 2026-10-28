//! App: window + state + animator + renderer + tray + timers.
//! Owns the HWND (via GWLP_USERDATA) and drives the pill lifecycle:
//! Zen (idle) -> hover -> Compact -> click/hover (with session) ->
//! Expanded (music card) -> bubble -> Lyrics (700x180) -> bubble back
//! to Expanded; click outside controls / leave -> Compact (with
//! session) or Zen (silent). Timers: 0.25s tick (clock + read of the
//! GSMTC cache written by the background poller thread + MediaTracker
//! wall-clock estimate + synchronous repaint for realtime
//! lyrics/progress), 0.25s alert placeholder (Phase 4), 16ms
//! (animation, only while animating). The UI thread NEVER calls
//! media::poll() or a control directly (Not Responding fix 2026-10-27:
//! blocking WinRT `.get()` on the UI thread froze the message loop).


use std::sync::{Arc, Mutex};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{HBRUSH, InvalidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TRACKMOUSEEVENT, TME_LEAVE, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::animator;
use crate::drag;
use crate::lan;
use crate::lyrics;
use crate::media;
use crate::pomodoro;
use crate::renderer;
use crate::state;
use crate::todo;
use crate::tray;
use crate::water;
use crate::window;

const TIMER_TICK: usize = 1; // 1s: clock + (Phase 1+) media poll
const TIMER_ALERT: usize = 2; // 0.25s: alert placeholder (Phase 4)
const TIMER_ANIM: usize = 3; // 16ms: size animation (only while animating)
const TIMER_CLOSE: usize = 4; // 150ms: debounced mouse-leave close (hover-glitch fix)
// Tray tile right-click menu ids (Phase 3b). Share submenu ids start
// at MENU_TILE_SHARE_BASE, max 100 peers (never collides with the
// two fixed ids).
const MENU_TILE_OPEN: usize = 2001;
const MENU_TILE_REMOVE: usize = 2002;
const MENU_TILE_SHARE_BASE: usize = 2100;
// Win32 WM_MOUSELEAVE (0x02A3), sent by TrackMouseEvent when the cursor
// leaves the window. Defined locally because windows-rs 0.58 does not export
// it from KeyboardAndMouse nor WindowsAndMessaging.
const WM_MOUSELEAVE: u32 = 0x02A3;
// GWL_EXSTYLE index + WS_EX_NOACTIVATE bit, for temporarily taking
// keyboard focus while editing a todo (pill is normally NOACTIVATE).
const GWL_EXSTYLE_IDX: WINDOW_LONG_PTR_INDEX = WINDOW_LONG_PTR_INDEX(-20);
const NOACTIVATE_BIT: isize = 0x0800_0000;

pub struct App {
    hwnd: HWND,
    renderer: renderer::Renderer,
    pill: state::PillState,
    anim: Option<animator::SizeAnimation>,
    tracking_mouse: bool,
    clock_text: String, // HH:MM, updated on 1s tick
    media: Option<media::MediaInfo>, // Current GSMTC session (Phase 1), None = silent.
    media_tracker: media::MediaTracker, // Wall-clock position estimate (realtime fix).
    media_cache: Arc<Mutex<media::PollCache>>, // Written by the poller thread (with its own message loop), read by the tick.
    poll_status: String, // Last poll status, shown on the no-music card for diagnosis.
    suppress_leave: bool, // True after a shrink: cursor may end up outside the smaller window without moving; that leave is not real (see set_state).
    media_none_streak: u32, // Consecutive ticks with no GSMTC session (transient-None hysteresis, hover-flicker fix).
    lyrics: Option<lyrics::LyricsCache>, // Fetched lyrics for current song (Phase 1b), None = not fetched yet.
    pomodoro: pomodoro::Pomodoro, // In-memory pomodoro timer (Phase 2).
    todo: todo::TodoList, // Tasks from todo.txt (Phase 2).
    todo_editing: bool, // True while the user types a task in the pill.
    todo_input: String, // Text being typed (committed on Enter / click-outside).
    shelf: crate::shelf::Shelf, // Tray file references (Phase 3a).
    tray_page: usize,
    lan: Option<Arc<lan::LanShare>>, // LAN share handle (Phase 3b), set by run() after window creation.
    dragging_out: bool, // True while a tray drag-out (DoDragDrop) is in flight: suppresses card close.
    drag_arm: Option<(usize, i32, i32)>, // Armed tray drag: (shelf index, down x_px, down y_px).
    swallow_next_click: bool, // Swallow the first LBUTTONUP after a drag-out (else the tile would open right after drop). // Current tray grid page (0-based).
    water: water::Water, // Daily intake (Phase 2).
    last_section: state::IslandState, // Last section card (music/pomodoro/todo/water); reopen shows this, not always music.
}

fn current_time_text() -> String {
    // Local clock via chrono (avoids Win32 GetLocalTime module uncertainty
    // in windows-rs 0.58; chrono handles Windows local time internally).
    chrono::Local::now().format("%H:%M").to_string()
}

impl App {
    fn new(hwnd: HWND, media_cache: Arc<Mutex<media::PollCache>>) -> Result<Box<Self>> {
        let renderer = renderer::Renderer::new()?;
        let mut app = Box::new(Self {
            hwnd,
            renderer,
            pill: state::PillState::new(),
            anim: None,
            tracking_mouse: false,
            clock_text: current_time_text(),
            media: None,
            media_tracker: media::MediaTracker::new(),
            media_cache,
            poll_status: "waiting for first poll...".to_string(),
            suppress_leave: false,
            media_none_streak: 0,
            lyrics: None,
            pomodoro: pomodoro::Pomodoro::new(),
            todo: todo::TodoList::load(),
            todo_editing: false,
            todo_input: String::new(),
            shelf: crate::shelf::Shelf::load(),
            tray_page: 0,
        lan: None,
        dragging_out: false,
        drag_arm: None,
        swallow_next_click: false,
            water: water::Water::load(),
            last_section: state::IslandState::Expanded,
        });
        // Initial size + region + placement (Zen, top-center, no animation).
        app.apply_size();
        Ok(app)
    }

    /// Apply current pill size to the window (resize + reposition top-center + region).
    /// Attach the LAN share handle (called by run() after window creation).
    fn set_lan(&mut self, share: Arc<lan::LanShare>) {
        self.lan = Some(share);
    }

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

    /// End todo editing without committing (used when leaving the
    /// Todo card by any path other than the Todo click handler).
    fn end_todo_editing(&mut self, commit: bool) {
        if !self.todo_editing {
            return;
        }
        if commit {
            let text = std::mem::take(&mut self.todo_input);
            self.todo.add(&text);
        } else {
            self.todo_input.clear();
        }
        self.todo_editing = false;
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE_IDX);
            let _ = SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE_IDX, ex | NOACTIVATE_BIT);
            let _ = InvalidateRect(self.hwnd, None, BOOL(0));
        }
    }

    /// Start todo editing: temporarily clear NOACTIVATE so the pill can
    /// take keyboard focus, then activate + focus the window. Only
    /// during editing does the pill steal focus; all other states stay
    /// NOACTIVATE as before.
    fn begin_todo_editing(&mut self) {
        if self.todo_editing {
            return;
        }
        self.todo_editing = true;
        self.todo_input.clear();
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE_IDX);
            let _ = SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE_IDX, ex & !NOACTIVATE_BIT);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = InvalidateRect(self.hwnd, None, BOOL(0));
        }
    }

    /// Keyboard input while editing a todo. WM_CHAR (printable chars)
    /// appends; control chars (<32, incl. Return/Backspace echoes) are
    /// ignored here because WM_KEYDOWN handles them.
    fn on_todo_char(&mut self, code: u16) {
        if !self.todo_editing {
            return;
        }
        if code < 32 {
            return;
        }
        if let Some(ch) = char::from_u32(code as u32) {
            if self.todo_input.chars().count() < 200 {
                self.todo_input.push(ch);
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                }
            }
        }
    }

    /// WM_KEYDOWN while editing: Backspace deletes, Return commits and
    /// STAYS editing (rapid entry of several tasks), Esc cancels.
    fn on_todo_keydown(&mut self, vk: u32) {
        if !self.todo_editing {
            return;
        }
        match vk {
            0x08 => {
                self.todo_input.pop();
            }
            0x0D => {
                let text = std::mem::take(&mut self.todo_input);
                self.todo.add(&text);
            }
            0x1B => {
                self.end_todo_editing(false);
                return;
            }
            _ => return,
        }
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, BOOL(0));
        }
    }

    fn set_state(&mut self, new_state: state::IslandState) {
        // Leaving the Todo card discards uncommitted typed text and
        // restores NOACTIVATE (the Todo click handler commits on
        // click-outside before reaching set_state, so this path is
        // only for leaves via hover/tick/other sections).
        if new_state != state::IslandState::Todo && self.todo_editing {
            self.end_todo_editing(false);
        }
        // Remember the last section (Mac: reopen shows last section, not
        // always music). Lyrics is a panel, not a section, so it never
        // becomes last_section (reopen after lyrics shows music).
        if new_state == state::IslandState::Expanded
            || new_state == state::IslandState::Pomodoro
            || new_state == state::IslandState::Todo
            || new_state == state::IslandState::Water
            || new_state == state::IslandState::Tray
        {
            self.last_section = new_state;
        }
        if new_state == self.pill.state && self.anim.is_none() {
            return;
        }
        let (from_w, from_h) = (self.pill.width, self.pill.height);
        self.pill.state = new_state;
        // Reload todo on entering the Todo card (external edits via
        // Notepad appear on re-entry; no tick polling for the file).
        if new_state == state::IslandState::Todo {
            self.todo = todo::TodoList::load();
        }
        let (to_w, to_h) = new_state.logical_size();
        // Shrink-leave suppression (freeze/dot bug 2026-10-27): returning
        // to Music shrinks 640/700 wide to 520, top-center anchored, so
        // the right edge moves left by 60-90px and a cursor sitting on
        // the old right-edge dot (x 612) ends up OUTSIDE the new window
        // without the user moving the mouse at all. The WM_MOUSELEAVE
        // + tick close that follow are artefacts of the shrink, not a
        // real leave: they interrupt the morph and leave the card
        // frozen at the previous size. Suppress the first leave after
        // any shrink; the next mouse move (a real re-enter) clears it,
        // so a later genuine leave still closes normally.
        if to_w < from_w - 0.01 || to_h < from_h - 0.01 {
            self.suppress_leave = true;
        }
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
                // 0.25s tick: read the poller-thread cache + update
                // clock + repaint NOW. NEVER media::poll() here:
                // ca373ed put poll on the UI thread (every 1s) and the
                // app went "Not Responding" immediately after run —
                // RequestAsync().get() blocks the message loop in this
                // environment. Poll lives on its own STA thread with
                // its own message loop (media.rs spawn_poller); the
                // tick only reads the cache, so a stuck poll freezes
                // the cache, never the window.
                let cached = self
                    .media_cache
                    .lock()
                    .ok()
                    .map(|guard| (
                        guard.info.clone(),
                        guard.status.clone(),
                        guard.phase.clone(),
                    ))
                    .unwrap_or((None, "cache locked".to_string(), String::new()));
                self.poll_status = if cached.2.is_empty() {
                    cached.1
                } else {
                    format!("{} [{}]", cached.1, cached.2)
                };
                let cached = cached.0;
                // Transient-None hysteresis: a single failed/empty GSMTC
                // poll must not close the card (that close + hover-reopen
                // is one open-close flicker source). Only after 4
                // consecutive empty ticks (1s) is the session treated
                // as really ended; until then the previous media stays.
                match cached {
                    Some(info) => {
                        self.media_none_streak = 0;
                        self.media = self.media_tracker.update(Some(info));
                    }
                    None => {
                        self.media_none_streak += 1;
                        if self.media_none_streak >= 4 {
                            self.media = self.media_tracker.update(None);
                        }
                    }
                }
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
                    // While the user hovers the pill, never close it
                    // from the tick: the card stays until a real leave
                    // (debounced below) closes it. Without this guard,
                    // tick-close + hover-reopen alternate every 250ms.
                    // Also suppressed after a shrink (see set_state):
                    // the cursor may be outside only because the window
                    // shrank away from it, not because the user left.
                    if !self.suppress_leave && !self.dragging_out && !self.cursor_inside_window() {
                        self.set_state(state::IslandState::Zen);
                    }
                }
                // Pomodoro: if running and time is up, stop (00:00, Start).
                // (Wall-clock: remaining is computed from ends_at, so this
                // check on tick is just for stopping promptly, not counting.)
                self.pomodoro.check_finished(chrono::Local::now());
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
            TIMER_CLOSE => {
                // Debounced close fires: verify once more, because the
                // cursor may have re-entered during the 150ms window.
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_CLOSE);
                }
                // A drag-out in flight is not a leave: never close from
                // the debounced timer while dragging.
                if self.dragging_out {
                    return;
                }
                if self.todo_editing {
                    return;
                }
                if self.cursor_inside_window() {
                    self.arm_mouse_tracking();
                    return;
                }
                self.apply_mouse_leave();
            }
            _ => {}
        }
    }

    /// (Re-)arm WM_MOUSELEAVE tracking. Called on every mouse move
    /// and after a spurious leave (see on_mouse_leave): without
    /// re-arming, a later real exit would never be reported and the
    /// card would stay open forever.
    fn arm_mouse_tracking(&mut self) {
        if self.tracking_mouse {
            return;
        }
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

    /// True when the cursor is currently over the window RECT
    /// (not the region: tucked sides / rounded corners put the cursor
    /// outside the region while still visually over the pill).
    fn cursor_inside_window(&self) -> bool {
        let mut pt = POINT::default();
        let mut rect = RECT::default();
        unsafe {
            GetCursorPos(&mut pt).is_ok()
                && GetWindowRect(self.hwnd, &mut rect).is_ok()
                && pt.x >= rect.left
                && pt.x < rect.right
                && pt.y >= rect.top
                && pt.y < rect.bottom
        }
    }

    /// The actual leave action (session -> Compact, silent -> Zen),
    /// used by the debounced close timer, not directly by WM_MOUSELEAVE.
    fn apply_mouse_leave(&mut self) {
        if self.media.is_some() {
            self.set_state(state::IslandState::Compact);
        } else {
            self.set_state(state::IslandState::Zen);
        }
    }

    fn on_mouse_move(&mut self) {
        // A real re-enter: any shrink-suppression is over, future
        // leaves are genuine again.
        self.suppress_leave = false;
        // A re-enter cancels a pending debounced close.
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_CLOSE);
        }
        self.arm_mouse_tracking();
        // Hover: Zen -> Compact (idle); Compact + session -> last section
        // (Aura Mac: hover opens the card when a session exists, and
        // reopen shows the last section, not always music).
        // If already in a card (Expanded/Pomodoro/Lyrics), hovering keeps it.
        if self.pill.state == state::IslandState::Zen {
            self.set_state(state::IslandState::Compact);
        } else if self.pill.state == state::IslandState::Compact && self.media.is_some() {
            let target = self.last_section;
            self.set_state(target);
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking_mouse = false;
        // While a drag-out is in flight the cursor leaving the window
        // is part of the drag, not a real leave: ignore it (and do NOT
        // start the debounced close) so the card stays open.
        if self.dragging_out {
            return;
        }
        // While typing a todo, cursor exit must NOT close the card
        // (the user may move the mouse away mid-typing; Esc or a click
        // outside the input field ends editing, not mouse-leave).
        if self.todo_editing {
            return;
        }
        // Shrink-suppressed leave (see set_state): ignore it outright
        // (no close timer, no close); the flag is consumed here, and a
        // later leave after a mouse-move re-enter will be real.
        if self.suppress_leave {
            self.suppress_leave = false;
            return;
        }
        // Debounced close (hover-glitch fix 2026-10-27, round 2):
        // the user still sees Zen ("Aura") <-> Compact (clock +
        // waveform) alternating on hover. A single WM_MOUSELEAVE is
        // not trusted on its own: region leaves, resize leaves, and
        // tick closes can all fire while the cursor is effectively
        // still over the pill. So: if the cursor is inside the rect,
        // ignore the leave outright; otherwise do NOT close yet —
        // start a 150ms one-shot timer and only close if the cursor
        // is still outside when it fires. Any mouse move in between
        // cancels the timer, so a rapid leave/re-enter can never
        // flip the state. A real leave is delayed by just 150ms,
        // which is imperceptible next to the 350ms animation.
        if self.cursor_inside_window() {
            self.arm_mouse_tracking();
            return;
        }
        unsafe {
            let _ = SetTimer(self.hwnd, TIMER_CLOSE, 150, None);
        }
    }

    /// WM_LBUTTONDOWN on a tray tile arms a drag-out (Phase 3b). The
    /// drag itself starts from WM_MOUSEMOVE once the cursor passes the
    /// system drag threshold; a plain click (down + up, no drag) still
    /// opens the file via on_click, unchanged.
    fn on_mouse_down(&mut self, x_px: i32, y_px: i32) {
        self.drag_arm = None;
        if self.pill.state != state::IslandState::Tray {
            return;
        }
        let scale = window::dpi_scale(self.hwnd);
        let x = x_px as f32 / scale;
        let y = y_px as f32 / scale;
        let (gx, gy, tw, th, px, py) = (
            renderer::TRAY_TILE_GEOM.0,
            renderer::TRAY_TILE_GEOM.1,
            renderer::TRAY_TILE_GEOM.2,
            renderer::TRAY_TILE_GEOM.3,
            renderer::TRAY_TILE_GEOM.4,
            renderer::TRAY_TILE_GEOM.5,
        );
        let page_count = self.shelf.page_count(renderer::TRAY_PAGE_SIZE);
        if self.tray_page >= page_count {
            self.tray_page = page_count.saturating_sub(1);
        }
        let start = self.tray_page * renderer::TRAY_PAGE_SIZE;
        let shown = self.shelf.files.len().saturating_sub(start).min(renderer::TRAY_PAGE_SIZE);
        for i in 0..shown {
            let col = (i % 4) as f32;
            let row = (i / 4) as f32;
            let left = gx + col * px;
            let top = gy + row * py;
            if x >= left && x <= left + tw && y >= top && y <= top + th {
                let idx = start + i;
                // Only arm when the file still exists (stale tiles do
                // nothing on left-click, so they must not drag either).
                if std::path::Path::new(&self.shelf.files[idx]).exists() {
                    self.drag_arm = Some((idx, x_px, y_px));
                }
                return;
            }
        }
    }

    /// WM_MOUSEMOVE drag leg (Phase 3b): called after the hover leg.
    /// If a tray drag is armed and the cursor passed the system drag
    /// threshold, run the blocking OLE drag; afterwards the next
    /// LBUTTONUP is swallowed so the tile does not open after drop.
    fn on_mouse_move_drag(&mut self, x_px: i32, y_px: i32) {
        let Some((idx, down_x, down_y)) = self.drag_arm else {
            return;
        };
        if self.pill.state != state::IslandState::Tray {
            self.drag_arm = None;
            return;
        }
        let (threshold_x, threshold_y) =
            unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
        if (x_px - down_x).abs() < threshold_x && (y_px - down_y).abs() < threshold_y {
            return;
        }
        // Threshold passed: start the drag (blocking until drop/cancel).
        self.drag_arm = None;
        let path = match self.shelf.files.get(idx) {
            Some(p) => p.clone(),
            None => return,
        };
        if !std::path::Path::new(&path).exists() {
            return;
        }
        self.dragging_out = true;
        let _dropped = drag::do_drag_file(&path);
        self.dragging_out = false;
        self.swallow_next_click = true;
        unsafe {
            let _ = InvalidateRect(self.hwnd, None, BOOL(0));
        }
    }

    fn on_click(&mut self, x_px: i32, y_px: i32) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_CLOSE);
        }
        // First click after a drag-out is swallowed: without this, the
        // button-up that ends the drag would open the tile just dropped.
        if self.swallow_next_click {
            self.swallow_next_click = false;
            return;
        }
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
                // Controls run on a throwaway thread (never wait
                // here): the next poller tick (<=250ms) refreshes the
                // icon/progress. Waiting here used to freeze the window
                // into "Not Responding" when the media app was slow.
                if in_rect(renderer::MUSIC_PREV_RECT) {
                    media::skip_previous_async();
                } else if in_rect(renderer::MUSIC_PLAY_RECT) {
                    if playing {
                        media::pause_async();
                    } else {
                        media::play_async();
                    }
                } else if in_rect(renderer::MUSIC_NEXT_RECT) {
                    media::skip_next_async();
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
                } else if in_rect(renderer::MUSIC_DOT_POMODORO_RECT) {
                    self.set_state(state::IslandState::Pomodoro);
                    return;
                } else if in_rect(renderer::MUSIC_DOT_TODO_RECT) {
                    self.set_state(state::IslandState::Todo);
                    return;
                } else if in_rect(renderer::MUSIC_DOT_WATER_RECT) {
                    self.set_state(state::IslandState::Water);
                    return;
                } else if in_rect(renderer::MUSIC_DOT_TRAY_RECT) {
                    self.set_state(state::IslandState::Tray);
                    return;
                } else if in_rect(renderer::MUSIC_DOT_MUSIC_RECT) {
                    // Current section dot: no-op (stay open, do not close).
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
            // No session: dots still navigate (pomodoro needs no session);
            // any other click closes the no-music card to Compact.
            {
                let scale = window::dpi_scale(self.hwnd);
                let x = x_px as f32 / scale;
                let y = y_px as f32 / scale;
                let in_rect =
                    |r: (f32, f32, f32, f32)| x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3;
                if in_rect(renderer::MUSIC_DOT_POMODORO_RECT) {
                    self.set_state(state::IslandState::Pomodoro);
                    return;
                }
                if in_rect(renderer::MUSIC_DOT_TODO_RECT) {
                    self.set_state(state::IslandState::Todo);
                    return;
                }
                if in_rect(renderer::MUSIC_DOT_WATER_RECT) {
                    self.set_state(state::IslandState::Water);
                    return;
                }
                if in_rect(renderer::MUSIC_DOT_TRAY_RECT) {
                    self.set_state(state::IslandState::Tray);
                    return;
                }
                if in_rect(renderer::MUSIC_DOT_MUSIC_RECT) {
                    return;
                }
            }
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
        // Pomodoro card: mode pills select mode, Start/Pause toggles,
        // dots switch section, any other click closes to Compact.
        if self.pill.state == state::IslandState::Pomodoro {
            let scale = window::dpi_scale(self.hwnd);
            let x = x_px as f32 / scale;
            let y = y_px as f32 / scale;
            let in_rect =
                |r: (f32, f32, f32, f32)| x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3;
            if in_rect(renderer::POMODORO_FOCUS_RECT) {
                self.pomodoro.select_mode(pomodoro::PomodoroMode::Focus);
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                }
                return;
            }
            if in_rect(renderer::POMODORO_BREAK_RECT) {
                self.pomodoro.select_mode(pomodoro::PomodoroMode::Break);
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                }
                return;
            }
            if in_rect(renderer::POMODORO_START_RECT) {
                self.pomodoro.toggle_start_pause(chrono::Local::now());
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                }
                return;
            }
            if in_rect(renderer::POMODORO_DOT_MUSIC_RECT) {
                self.set_state(state::IslandState::Expanded);
                return;
            }
            if in_rect(renderer::POMODORO_DOT_TODO_RECT) {
                self.set_state(state::IslandState::Todo);
                return;
            }
            if in_rect(renderer::POMODORO_DOT_WATER_RECT) {
                self.set_state(state::IslandState::Water);
                return;
            }
            if in_rect(renderer::POMODORO_DOT_TRAY_RECT) {
                self.set_state(state::IslandState::Tray);
                return;
            }
            if in_rect(renderer::POMODORO_DOT_POMODORO_RECT) {
                // Current section dot: no-op (stay open, do not close).
                return;
            }
            self.set_state(state::IslandState::Compact);
            return;
        }
        // Todo card: click the input field to type in the pill,
        // click a row toggles done, dots switch section, any other
        // click closes to Compact. While editing, the FIRST click
        // outside the input field commits the typed task (if any)
        // and stays on the card; the next click then acts normally.
        if self.pill.state == state::IslandState::Todo {
            let scale = window::dpi_scale(self.hwnd);
            let x = x_px as f32 / scale;
            let y = y_px as f32 / scale;
            let in_rect =
                |r: (f32, f32, f32, f32)| x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3;
            if in_rect(renderer::TODO_INPUT_RECT) {
                self.begin_todo_editing();
                return;
            }
            if self.todo_editing {
                self.end_todo_editing(true);
                return;
            }
            if in_rect(renderer::TODO_DOT_MUSIC_RECT) {
                self.set_state(state::IslandState::Expanded);
                return;
            }
            if in_rect(renderer::TODO_DOT_POMODORO_RECT) {
                self.set_state(state::IslandState::Pomodoro);
                return;
            }
            if in_rect(renderer::TODO_DOT_TRAY_RECT) {
                self.set_state(state::IslandState::Tray);
                return;
            }
            if in_rect(renderer::TODO_DOT_TODO_RECT) {
                return;
            }
            if in_rect(renderer::TODO_DOT_WATER_RECT) {
                self.set_state(state::IslandState::Water);
                return;
            }
            // Rows: TODO_ROW_GEOM (x0, y0, x1, pitch), height 24, max 6.
            // (Row rect check, not just row index, so gap clicks close.)
            let (rx0, ry0, rx1, pitch) = (
                renderer::TODO_ROW_GEOM.0,
                renderer::TODO_ROW_GEOM.1,
                renderer::TODO_ROW_GEOM.2,
                renderer::TODO_ROW_GEOM.3,
            );
            if x >= rx0 && x <= rx1 && y >= ry0 {
                let idx = ((y - ry0) / pitch) as usize;
                let row_bottom = ry0 + (idx as f32) * pitch + 24.0;
                if y <= row_bottom
                    && idx < self.todo.tasks.len()
                    && idx < renderer::TODO_MAX_ROWS
                {
                    self.todo.toggle(idx);
                    unsafe {
                        let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                    }
                    return;
                }
            }
            self.set_state(state::IslandState::Compact);
            return;
        }
        // Tray shelf card (Phase 3a): Clear clears references only
        // (never the real files), arrows page, dots switch, a tile
        // click opens the file/folder (existing only; stale tiles do
        // nothing on left-click, right-click opens the tile menu),
        // any other click closes to Compact.
        if self.pill.state == state::IslandState::Tray {
            let scale = window::dpi_scale(self.hwnd);
            let x = x_px as f32 / scale;
            let y = y_px as f32 / scale;
            let in_rect =
                |r: (f32, f32, f32, f32)| x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3;
            if in_rect(renderer::TRAY_CLEAR_RECT) {
                self.shelf.clear();
                self.tray_page = 0;
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                }
                return;
            }
            let page_count = self.shelf.page_count(renderer::TRAY_PAGE_SIZE);
            if self.tray_page >= page_count {
                self.tray_page = page_count.saturating_sub(1);
            }
            if page_count > 1 {
                if in_rect(renderer::TRAY_PREV_RECT) && self.tray_page > 0 {
                    self.tray_page -= 1;
                    unsafe {
                        let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                    }
                    return;
                }
                if in_rect(renderer::TRAY_NEXT_RECT)
                    && self.tray_page + 1 < page_count
                {
                    self.tray_page += 1;
                    unsafe {
                        let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                    }
                    return;
                }
            }
            if in_rect(renderer::TRAY_DOT_MUSIC_RECT) {
                self.set_state(state::IslandState::Expanded);
                return;
            }
            if in_rect(renderer::TRAY_DOT_POMODORO_RECT) {
                self.set_state(state::IslandState::Pomodoro);
                return;
            }
            if in_rect(renderer::TRAY_DOT_TODO_RECT) {
                self.set_state(state::IslandState::Todo);
                return;
            }
            if in_rect(renderer::TRAY_DOT_WATER_RECT) {
                self.set_state(state::IslandState::Water);
                return;
            }
            if in_rect(renderer::TRAY_DOT_TRAY_RECT) {
                return;
            }
            // Tiles: 4x2 grid from TRAY_TILE_GEOM (x0, y0, w, h, px, py).
            let (gx, gy, tw, th, px, py) = (
                renderer::TRAY_TILE_GEOM.0,
                renderer::TRAY_TILE_GEOM.1,
                renderer::TRAY_TILE_GEOM.2,
                renderer::TRAY_TILE_GEOM.3,
                renderer::TRAY_TILE_GEOM.4,
                renderer::TRAY_TILE_GEOM.5,
            );
            let start = self.tray_page * renderer::TRAY_PAGE_SIZE;
            let shown = self.shelf.files.len().saturating_sub(start).min(renderer::TRAY_PAGE_SIZE);
            for i in 0..shown {
                let col = (i % 4) as f32;
                let row = (i / 4) as f32;
                let left = gx + col * px;
                let top = gy + row * py;
                if x >= left && x <= left + tw && y >= top && y <= top + th {
                    let path = self.shelf.files[start + i].clone();
                    if std::path::Path::new(&path).exists() {
                        // Open with the associated app (explorer) — std
                        // only, to avoid ShellExecuteW signature risk.
                        let _ = std::process::Command::new("explorer.exe")
                            .arg(&path)
                            .spawn();
                    }
                    return;
                }
            }
            self.set_state(state::IslandState::Compact);
            return;
        }
        // Water card: +250 adds a glass, Reset clears today, dots switch,
        // any other click closes to Compact.
        if self.pill.state == state::IslandState::Water {
            let scale = window::dpi_scale(self.hwnd);
            let x = x_px as f32 / scale;
            let y = y_px as f32 / scale;
            let in_rect =
                |r: (f32, f32, f32, f32)| x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3;
            if in_rect(renderer::WATER_ADD_RECT) {
                self.water.add_glass();
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                }
                return;
            }
            if in_rect(renderer::WATER_RESET_RECT) {
                self.water.reset();
                unsafe {
                    let _ = InvalidateRect(self.hwnd, None, BOOL(0));
                }
                return;
            }
            if in_rect(renderer::WATER_DOT_MUSIC_RECT) {
                self.set_state(state::IslandState::Expanded);
                return;
            }
            if in_rect(renderer::WATER_DOT_POMODORO_RECT) {
                self.set_state(state::IslandState::Pomodoro);
                return;
            }
            if in_rect(renderer::WATER_DOT_TODO_RECT) {
                self.set_state(state::IslandState::Todo);
                return;
            }
            if in_rect(renderer::WATER_DOT_TRAY_RECT) {
                self.set_state(state::IslandState::Tray);
                return;
            }
            if in_rect(renderer::WATER_DOT_WATER_RECT) {
                return;
            }
            self.set_state(state::IslandState::Compact);
            return;
        }
        // Not in a card: click opens the last section (music by default).
        {
            let target = self.last_section;
            self.set_state(target);
        }
    }

    /// Right-click on a tray tile opens the tile menu (Phase 3b:
    /// Open / Share / Remove). Elsewhere, right-click does nothing.
    /// Tray tile right-click menu (Phase 3b): Open / Share (peer
    /// submenu, or one disabled "No peers nearby") / Remove. Remove
    /// deletes only the tray reference, never the real file (Phase 3a
    /// rule, unchanged). Share runs on a worker thread so the 60s
    /// approval wait never freezes the pill; only errors pop up.
    fn show_tile_menu(&mut self, idx: usize) {
        let path = match self.shelf.files.get(idx) {
            Some(p) => p.clone(),
            None => return,
        };
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            let Ok(share_menu) = CreatePopupMenu() else {
                let _ = DestroyMenu(menu);
                return;
            };
            let _ = AppendMenuW(menu, MF_STRING, MENU_TILE_OPEN, w!("Open"));
            // Share submenu: live peers, sorted by name; empty list
            // shows one disabled item instead of an empty submenu.
            let peers = self.lan.as_ref().map(|l| l.peers()).unwrap_or_default();
            let shown_peers: Vec<lan::LanPeer> = peers.into_iter().take(100).collect();
            if shown_peers.is_empty() {
                let _ = AppendMenuW(
                    share_menu,
                    MF_STRING | MF_GRAYED,
                    0,
                    w!("No peers nearby"),
                );
            } else {
                for (i, peer) in shown_peers.iter().enumerate() {
                    let mut wide: Vec<u16> = peer.name.encode_utf16().collect();
                    wide.push(0);
                    let _ = AppendMenuW(
                        share_menu,
                        MF_STRING,
                        MENU_TILE_SHARE_BASE + i,
                        PCWSTR::from_raw(wide.as_ptr()),
                    );
                }
            }
            let _ = AppendMenuW(menu, MF_POPUP, share_menu.0 as usize, w!("Share"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
            let _ = AppendMenuW(menu, MF_STRING, MENU_TILE_REMOVE, w!("Remove"));
            let _ = SetForegroundWindow(self.hwnd);
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let id = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_RETURNCMD,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(share_menu);
            let _ = DestroyMenu(menu);
            let picked = id.0 as usize;
            if picked == MENU_TILE_OPEN {
                if std::path::Path::new(&path).exists() {
                    let _ = std::process::Command::new("explorer.exe")
                        .arg(&path)
                        .spawn();
                }
            } else if picked == MENU_TILE_REMOVE {
                self.shelf.remove(idx);
                let new_count = self.shelf.page_count(renderer::TRAY_PAGE_SIZE);
                if self.tray_page >= new_count {
                    self.tray_page = new_count.saturating_sub(1);
                }
                let _ = InvalidateRect(self.hwnd, None, BOOL(0));
            } else if picked >= MENU_TILE_SHARE_BASE
                && picked < MENU_TILE_SHARE_BASE + shown_peers.len()
            {
                let peer = shown_peers[picked - MENU_TILE_SHARE_BASE].clone();
                match &self.lan {
                    Some(share) => {
                        let share = share.clone();
                        std::thread::spawn(move || {
                            if let Err(msg) =
                                share.send_file(&peer, std::path::Path::new(&path))
                            {
                                lan::show_error(&msg);
                            }
                        });
                    }
                    None => {
                        lan::show_error("LAN share is not running.");
                    }
                }
            }
            // picked == 0 (dismissed) or a disabled item: nothing.
        }
    }

    fn on_right_click(&mut self, x_px: i32, y_px: i32) {
        if self.pill.state != state::IslandState::Tray {
            return;
        }
        let scale = window::dpi_scale(self.hwnd);
        let x = x_px as f32 / scale;
        let y = y_px as f32 / scale;
        let (gx, gy, tw, th, px, py) = (
            renderer::TRAY_TILE_GEOM.0,
            renderer::TRAY_TILE_GEOM.1,
            renderer::TRAY_TILE_GEOM.2,
            renderer::TRAY_TILE_GEOM.3,
            renderer::TRAY_TILE_GEOM.4,
            renderer::TRAY_TILE_GEOM.5,
        );
        let page_count = self.shelf.page_count(renderer::TRAY_PAGE_SIZE);
        if self.tray_page >= page_count {
            self.tray_page = page_count.saturating_sub(1);
        }
        let start = self.tray_page * renderer::TRAY_PAGE_SIZE;
        let shown = self.shelf.files.len().saturating_sub(start).min(renderer::TRAY_PAGE_SIZE);
        for i in 0..shown {
            let col = (i % 4) as f32;
            let row = (i / 4) as f32;
            let left = gx + col * px;
            let top = gy + row * py;
            if x >= left && x <= left + tw && y >= top && y <= top + th {
                self.show_tile_menu(start + i);
                return;
            }
        }
    }

    /// WM_DROPFILES handler (Phase 3a drag-in). The window accepts
    /// drops via DragAcceptFiles (classic, not full OLE IDropTarget:
    /// no hover highlight, but the drop itself works from Explorer).
    /// Every dropped path is added as a reference (dedupe, must
    /// exist), then the Tray card opens on the last page.
    fn on_drop_files(&mut self, hdrop_raw: isize) {
        unsafe {
            let hdrop = HDROP(hdrop_raw as *mut core::ffi::c_void);
            let count = DragQueryFileW(hdrop, u32::MAX, None);
            let mut added = false;
            for i in 0..count {
                let len = DragQueryFileW(hdrop, i, None);
                if len == 0 {
                    continue;
                }
                let mut buf = vec![0u16; (len + 1) as usize];
                let got = DragQueryFileW(hdrop, i, Some(&mut buf));
                if got == 0 {
                    continue;
                }
                let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                let path = String::from_utf16_lossy(&buf[..end]);
                if self.shelf.add(&path) {
                    added = true;
                }
            }
            DragFinish(hdrop);
            if added {
                self.tray_page = self
                    .shelf
                    .page_count(renderer::TRAY_PAGE_SIZE)
                    .saturating_sub(1);
                self.set_state(state::IslandState::Tray);
                let _ = InvalidateRect(self.hwnd, None, BOOL(0));
            }
        }
    }

    fn on_paint(&mut self) {
        // Renderer draws current state (clock for Compact, music/no-music
        // card for Expanded, lyrics panel for Lyrics, pomodoro card for
        // Pomodoro, todo card for Todo, water card for Water, "Aura"
        // label for Zen). ValidateRect inside draw prevents a paint loop.
        self.renderer.draw(
            self.hwnd,
            self.pill.state,
            &self.clock_text,
            self.media.as_ref(),
            self.lyrics.as_ref(),
            &self.pomodoro,
            &self.todo,
            &self.water,
            self.todo_editing,
            &self.todo_input,
            &self.shelf,
            self.tray_page,
            self.pill.width,
            self.pill.height,
            &self.poll_status,
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
            let x = ((lparam.0 as u32) & 0xFFFF) as u16 as i16 as i32;
            let y = (((lparam.0 as u32) >> 16) & 0xFFFF) as u16 as i16 as i32;
            app.on_mouse_move_drag(x, y);
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            app.on_mouse_leave();
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = ((lparam.0 as u32) & 0xFFFF) as u16 as i16 as i32;
            let y = (((lparam.0 as u32) >> 16) & 0xFFFF) as u16 as i16 as i32;
            app.on_mouse_down(x, y);
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
        WM_RBUTTONUP => {
            // Right-click: tray tile menu (Phase 3b); elsewhere no-op.
            let x = ((lparam.0 as u32) & 0xFFFF) as u16 as i16 as i32;
            let y = (((lparam.0 as u32) >> 16) & 0xFFFF) as u16 as i16 as i32;
            app.on_right_click(x, y);
            LRESULT(0)
        }
        0x0233 => {
            // WM_DROPFILES: file(s) dropped from Explorer onto the pill.
            app.on_drop_files(lparam.0);
            LRESULT(0)
        }
        WM_TIMER => {
            app.on_timer(wparam.0);
            LRESULT(0)
        }
        0x0102 => {
            // WM_CHAR: printable key typed while editing a todo.
            let code = (wparam.0 & 0xFFFF) as u16;
            app.on_todo_char(code);
            LRESULT(0)
        }
        0x0100 => {
            // WM_KEYDOWN: Backspace / Return / Esc while editing.
            app.on_todo_keydown(wparam.0 as u32);
            LRESULT(0)
        }
        0x0008 => {
            // WM_KILLFOCUS: focus left the pill (user clicked another
            // app) — commit typed text if any, then restore NOACTIVATE.
            if app.todo_editing {
                app.end_todo_editing(true);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
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
        // OLE for drag-out (Phase 3b): DoDragDrop needs STA on this
        // thread. Failure is non-fatal here (drag-out just will not
        // work); everything else keeps running.
        let _ = OleInitialize(None);
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
        // Accept file drops from Explorer (Phase 3a, WM_DROPFILES).
        let _ = DragAcceptFiles(hwnd, BOOL(1));
        // Background GSMTC poller (STA thread with its own message
        // loop — see media.rs; a thread with no message loop never
        // returns from RequestAsync, and polling on the UI thread
        // freezes the window immediately). Started before App so the
        // cache exists from the first tick; UI only reads it.
        let media_cache = Arc::new(Mutex::new(media::PollCache::waiting()));
        media::spawn_poller(media_cache.clone());
        // App (owns renderer + state). Store in USERDATA for WndProc.
        let mut app = App::new(hwnd, media_cache)?;
        // LAN share (Phase 3b): start after the window exists, before
        // the message loop; the handle lives in App for the tile menu.
        app.set_lan(lan::LanShare::start());
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
        OleUninitialize();
        Ok(())
    }
}
