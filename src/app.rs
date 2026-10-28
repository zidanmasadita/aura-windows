//! App: window + state + animator + renderer + tray + timers + event handlers.
//! Matches `aura-island` (`AppDelegate.swift` & `IslandView.swift`) 1:1 while preserving
//! Windows-native GSMTC media events, async LRCLIB lyrics worker, LAN file share, and
//! system power/Caps Lock sensors.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, Result, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{CLSID_FileOpenDialog, DragAcceptFiles, DragFinish, DragQueryFileW, HDROP, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::animator;
use crate::drag;
use crate::lan;
use crate::lyrics;
use crate::media;
use crate::pomodoro;
use crate::removebg;
use crate::renderer::{self, AlertIcon, PillAlert, RenderContext};
use crate::state::{self, IslandState};
use crate::todo;
use crate::tray;
use crate::water;
use crate::window;

const TIMER_TICK: usize = 1; // 100ms: eye tracking, waveform, media tracker, Pomodoro/Water checks
const TIMER_ALERT: usize = 2; // 250ms: Caps Lock + power/battery alert check
const TIMER_ANIM: usize = 3; // 8ms: Nusa 2D spring animation + smooth lyric roll
const TIMER_CLOSE: usize = 4; // 150ms: debounced mouse-leave close

const MENU_TILE_OPEN: usize = 2001;
const MENU_TILE_REMOVE: usize = 2002;
const MENU_TILE_SHARE_BASE: usize = 2100;
const MENU_RB_TRAY_BASE: usize = 2200;
const MENU_RB_MODEL_QUALITY: usize = 2301;
const MENU_RB_MODEL_BALANCED: usize = 2302;
const MENU_RB_MODEL_FAST: usize = 2303;

const MENU_WATER_GOAL_1500: usize = 3001;
const MENU_WATER_GOAL_2000: usize = 3002;
const MENU_WATER_GOAL_2500: usize = 3003;
const MENU_WATER_GOAL_3000: usize = 3004;
const MENU_WATER_GLASS_200: usize = 3011;
const MENU_WATER_GLASS_250: usize = 3012;
const MENU_WATER_GLASS_350: usize = 3013;
const MENU_WATER_INT_30: usize = 3021;
const MENU_WATER_INT_60: usize = 3022;
const MENU_WATER_INT_90: usize = 3023;
const MENU_WATER_RESET: usize = 3030;

const WM_MOUSELEAVE_MSG: u32 = 0x02A3;
const WM_MOUSEWHEEL_MSG: u32 = 0x020A;
const WM_POWERBROADCAST_MSG: u32 = 0x0218;
const GWL_EXSTYLE_IDX: WINDOW_LONG_PTR_INDEX = WINDOW_LONG_PTR_INDEX(-20);
const NOACTIVATE_BIT: isize = 0x0800_0000;

pub struct App {
    hwnd: HWND,
    renderer: renderer::Renderer,
    pill: state::PillState,
    spring: animator::Spring2D,
    anim_timer_running: bool,
    last_bubble_click: Option<(f32, f32, Instant)>,
    tray_bounce: animator::TrayBounceSpring,
    tray_drag_pt: Option<(f32, f32)>,
    tracking_mouse: bool,
    media: Option<media::MediaInfo>,
    media_tracker: media::MediaTracker,
    media_cache: Arc<Mutex<media::PollCache>>,
    poll_status: String,
    suppress_leave: bool,
    suppress_hover_open_after_click_close: bool,
    media_none_streak: u32,
    lyrics: Option<lyrics::LyricsCache>,
    lyrics_pending: Arc<Mutex<Option<lyrics::LyricsCache>>>,
    lyrics_fetching_key: Option<String>,
    lyrics_mode: bool,
    eye_look: (f32, f32),
    eyes_closed: bool,
    next_blink_at: Instant,
    blink_until: Instant,
    pomodoro: pomodoro::Pomodoro,
    todo: todo::TodoList,
    todo_editing: bool,
    todo_input: String,
    shelf: crate::shelf::Shelf,
    tray_page: usize,
    lan: Option<Arc<lan::LanShare>>,
    dragging_out: bool,
    drag_hovering: bool,
    dropped_files: Arc<Mutex<Vec<String>>>,
    drag_arm: Option<(usize, i32, i32)>,
    swallow_next_click: bool,
    removebg: removebg::RemoveBgState,
    removebg_pending: Arc<Mutex<Option<removebg::RemoveBgResult>>>,
    removebg_drag_arm: Option<(i32, i32)>,
    water: water::Water,
    last_water_reminder: Instant,
    last_section: IslandState,
    last_scroll_switch: Instant,
    power_state: tray::PowerState,
    low_bat_warned: bool,
    crit_bat_warned: bool,
    caps_lock: bool,
    active_alert: Option<(PillAlert, Instant)>,
}

impl App {
    fn new(
        hwnd: HWND,
        media_cache: Arc<Mutex<media::PollCache>>,
        dropped_files: Arc<Mutex<Vec<String>>>,
    ) -> Result<Box<Self>> {
        let renderer = renderer::Renderer::new()?;
        let power_state = tray::read_power_status();
        let caps_lock = tray::is_caps_lock_on();
        let now = Instant::now();
        let pill = state::PillState::new();
        let spring = animator::Spring2D::new(pill.width, pill.height);
        let mut app = Box::new(Self {
            hwnd,
            renderer,
            pill,
            spring,
            anim_timer_running: false,
            last_bubble_click: None,
            tray_bounce: animator::TrayBounceSpring::new(),
            tray_drag_pt: None,
            tracking_mouse: false,
            media: None,
            media_tracker: media::MediaTracker::new(),
            media_cache,
            poll_status: "waiting for GSMTC (MTA)...".to_string(),
            suppress_leave: false,
            suppress_hover_open_after_click_close: false,
            media_none_streak: 0,
            lyrics: None,
            lyrics_pending: Arc::new(Mutex::new(None)),
            lyrics_fetching_key: None,
            lyrics_mode: false,
            eye_look: (0.0, 0.0),
            eyes_closed: false,
            next_blink_at: now + Duration::from_millis(3200),
            blink_until: now,
            pomodoro: pomodoro::Pomodoro::new(),
            todo: todo::TodoList::load(),
            todo_editing: false,
            todo_input: String::new(),
            shelf: crate::shelf::Shelf::load(),
            tray_page: 0,
            lan: None,
            dragging_out: false,
            drag_hovering: false,
            dropped_files,
            drag_arm: None,
            swallow_next_click: false,
            removebg: removebg::RemoveBgState::new(),
            removebg_pending: Arc::new(Mutex::new(None)),
            removebg_drag_arm: None,
            water: water::Water::load(),
            last_water_reminder: now,
            last_section: IslandState::Expanded,
            last_scroll_switch: now,
            power_state,
            low_bat_warned: false,
            crit_bat_warned: false,
            caps_lock,
            active_alert: None,
        });
        window::place_canvas_top_center(hwnd);
        app.on_paint();
        Ok(app)
    }

    fn set_lan(&mut self, share: Arc<lan::LanShare>) {
        self.lan = Some(share);
    }

    fn current_alert(&self) -> Option<&PillAlert> {
        if let Some((ref alert, expires_at)) = self.active_alert {
            if Instant::now() < expires_at {
                return Some(alert);
            }
        }
        None
    }

    /// Compute the exact target logical size `(width, height)` for `state`, matching
    /// `IslandView.unbulgedSize` / `closedSize` in `aura-island`.
    fn target_size_for(&self, state: IslandState) -> (f32, f32) {
        if !state.is_expanded() {
            if self.current_alert().is_some() {
                return (360.0, 32.0);
            }
            if state == IslandState::Zen {
                return (140.0, 32.0);
            }
            // Compact state:
            if self.pomodoro.is_active(chrono::Local::now()) {
                return (160.0, 32.0);
            }
            if self.lyrics_mode && self.media.is_some() {
                return (360.0, 32.0);
            }
            return (180.0, 32.0);
        }
        state.logical_size()
    }

    fn ensure_anim_timer(&mut self) {
        if !self.anim_timer_running {
            self.anim_timer_running = true;
            unsafe {
                let _ = SetTimer(self.hwnd, TIMER_ANIM, 8, None);
            }
        }
    }

    fn stop_anim_timer(&mut self) {
        if self.anim_timer_running {
            self.anim_timer_running = false;
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_ANIM);
            }
        }
    }

    /// Smoothly animate pill size to match `target_size_for(self.pill.state)`.
    fn sync_size_to_current_state(&mut self) {
        let (to_w, to_h) = self.target_size_for(self.pill.state);
        if self.spring.set_target(to_w, to_h, animator::SpringPreset::Morph) {
            self.ensure_anim_timer();
        } else {
            self.pill.width = to_w;
            self.pill.height = to_h;
        }
    }

    fn trigger_alert(&mut self, alert: PillAlert, duration: Duration) {
        self.active_alert = Some((alert, Instant::now() + duration));
        if self.pill.state == IslandState::Zen {
            self.set_state(IslandState::Compact);
        } else if !self.pill.state.is_expanded() {
            self.sync_size_to_current_state();
            self.on_paint();
        } else {
            self.on_paint();
        }
    }

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
        }
        self.on_paint();
    }

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
        }
        self.on_paint();
    }

    fn on_todo_char(&mut self, code: u16) {
        if !self.todo_editing || code < 32 {
            return;
        }
        if let Some(ch) = char::from_u32(code as u32) {
            if self.todo_input.chars().count() < 200 {
                self.todo_input.push(ch);
                self.on_paint();
            }
        }
    }

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
                self.end_todo_editing(false);
                return;
            }
            0x1B => {
                self.end_todo_editing(false);
                return;
            }
            _ => return,
        }
        self.on_paint();
    }

    fn closed_target_state(&self) -> IslandState {
        if self.media.is_some()
            || self.pomodoro.is_active(chrono::Local::now())
            || self.current_alert().is_some()
        {
            IslandState::Compact
        } else {
            IslandState::Zen
        }
    }

    fn set_state(&mut self, mut new_state: IslandState) {
        let old_state = self.pill.state;
        if new_state != IslandState::Todo && self.todo_editing {
            self.end_todo_editing(false);
        }

        // Preserve `lyrics_mode` when closing from Lyrics/Expanded, and restore `Lyrics` when
        // expanding to the Music section if `lyrics_mode` was active (`AppDelegate.swift`).
        if !new_state.is_expanded() {
            self.tray_bounce.cancel();
            if old_state == IslandState::Lyrics {
                self.lyrics_mode = true;
            } else if old_state == IslandState::Expanded {
                self.lyrics_mode = false;
            }
        } else if new_state == IslandState::Expanded && self.lyrics_mode && self.media.is_some() {
            new_state = IslandState::Lyrics;
            self.request_lyrics_async();
        }

        if new_state.is_expanded() {
            self.last_section = if new_state == IslandState::Lyrics {
                IslandState::Expanded
            } else {
                new_state
            };
        }

        let (to_w, to_h) = self.target_size_for(new_state);
        if new_state == self.pill.state
            && !self.spring.active
            && (self.pill.width - to_w).abs() < 0.05
            && (self.pill.height - to_h).abs() < 0.05
        {
            return;
        }

        let (from_w, from_h) = (self.pill.width, self.pill.height);
        self.pill.state = new_state;
        if new_state == IslandState::Todo {
            self.todo = todo::TodoList::load();
        }
        if to_w < from_w - 0.01 || to_h < from_h - 0.01 {
            self.suppress_leave = true;
        }
        let preset = if !old_state.is_expanded() && new_state.is_expanded() {
            animator::SpringPreset::Expand
        } else if old_state.is_expanded() && !new_state.is_expanded() {
            animator::SpringPreset::Collapse
        } else {
            animator::SpringPreset::Morph
        };
        if self.spring.set_target(to_w, to_h, preset) {
            self.ensure_anim_timer();
            self.on_paint();
        } else {
            self.pill.width = to_w;
            self.pill.height = to_h;
            self.on_paint();
        }
    }

    fn on_anim_tick(&mut self) {
        let was_active = self.spring.active;
        let (w, h, still_active) = self.spring.step();
        if was_active {
            self.pill.width = w;
            self.pill.height = h;
        }
        let bounce_active = self.tray_bounce.is_active();
        let rolling = self.renderer.is_lyric_rolling();
        if was_active || bounce_active || rolling {
            self.on_paint();
        }
        if !still_active && !self.tray_bounce.is_active() && !self.renderer.is_lyric_rolling() {
            self.stop_anim_timer();
        }
    }

    /// Update eye cursor tracking & blinking (`AppDelegate.eyeTimer` + `IslandView.drawEyes`).
    fn update_eyes(&mut self) {
        let now = Instant::now();
        if now >= self.next_blink_at {
            self.eyes_closed = true;
            self.blink_until = now + Duration::from_millis(180);
            self.next_blink_at = now + Duration::from_millis(3400);
        } else if self.eyes_closed && now >= self.blink_until {
            self.eyes_closed = false;
        }

        let mut pt = POINT::default();
        let mut rect = RECT::default();
        if unsafe { GetCursorPos(&mut pt).is_ok() && GetWindowRect(self.hwnd, &mut rect).is_ok() } {
            let scale = window::dpi_scale(self.hwnd);
            let cx = (rect.left + rect.right) as f32 * 0.5;
            let cy = rect.top as f32 + (self.pill.height * scale) * 0.5;
            let tx = ((pt.x as f32 - cx) / 300.0).clamp(-1.0, 1.0);
            let ty = ((pt.y as f32 - cy) / 200.0).clamp(-1.0, 1.0);
            self.eye_look.0 += (tx - self.eye_look.0) * 0.28;
            self.eye_look.1 += (ty - self.eye_look.1) * 0.28;
        }
    }

    /// Start a non-blocking async lyrics fetch on an MTA worker thread.
    fn request_lyrics_async(&mut self) {
        let Some(m) = self.media.as_ref() else {
            return;
        };
        let key = lyrics::key_for(&m.title, &m.artist);
        if self.lyrics.as_ref().map(|c| &c.key == &key).unwrap_or(false) {
            return;
        }
        if self.lyrics_fetching_key.as_deref() == Some(key.as_str()) {
            return;
        }
        self.lyrics_fetching_key = Some(key);
        lyrics::fetch_async(
            self.hwnd,
            self.lyrics_pending.clone(),
            m.title.clone(),
            m.artist.clone(),
            m.album.clone(),
        );
    }

    /// Called when `WM_LYRICS_READY` (`0x800F`) arrives from the MTA lyrics worker.
    fn on_lyrics_ready(&mut self) {
        let fetched = self
            .lyrics_pending
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        self.lyrics_fetching_key = None;
        if let Some(cache) = fetched {
            let current_key = self
                .media
                .as_ref()
                .map(|m| lyrics::key_for(&m.title, &m.artist));
            if current_key.as_deref() == Some(cache.key.as_str()) {
                self.lyrics = Some(cache);
                self.on_paint();
            }
        }
    }

    /// Read the GSMTC cache and update media state + lyrics tracking.
    fn refresh_media_from_cache(&mut self, immediate_event: bool) {
        let cached = self
            .media_cache
            .lock()
            .ok()
            .map(|guard| (guard.info.clone(), guard.status.clone(), guard.phase.clone()))
            .unwrap_or((None, "cache locked".to_string(), String::new()));
        self.poll_status = if cached.2.is_empty() || cached.2 == "done" {
            cached.1
        } else {
            format!("{} [{}]", cached.1, cached.2)
        };
        match cached.0 {
            Some(info) => {
                self.media_none_streak = 0;
                self.media = self.media_tracker.update(Some(info));
            }
            None => {
                if !immediate_event {
                    self.media_none_streak += 1;
                }
                if self.media_none_streak >= 8 {
                    self.media = self.media_tracker.update(None);
                }
            }
        }

        // Check if track changed for lyrics:
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
            if (self.pill.state == IslandState::Lyrics || self.lyrics_mode) && self.media.is_some()
            {
                self.request_lyrics_async();
            } else if self.pill.state == IslandState::Lyrics && self.media.is_none() {
                self.set_state(IslandState::Expanded);
            }
        } else if self.lyrics.is_none()
            && (self.pill.state == IslandState::Lyrics || self.lyrics_mode)
            && self.media.is_some()
        {
            self.request_lyrics_async();
        }

        let has_media = self.media.is_some();
        let has_alert = self.current_alert().is_some();
        let pomo_active = self.pomodoro.is_active(chrono::Local::now());
        if has_media || pomo_active {
            if self.pill.state == IslandState::Zen {
                self.set_state(IslandState::Compact);
            } else if self.pill.state == IslandState::Compact {
                self.sync_size_to_current_state();
            }
        } else if !has_alert && self.pill.state == IslandState::Compact {
            if !self.suppress_leave
                && !self.dragging_out
                && !self.drag_hovering
                && !self.cursor_inside_window()
            {
                self.set_state(IslandState::Zen);
            }
        }
    }

    fn on_media_update(&mut self) {
        self.refresh_media_from_cache(true);
        self.on_paint();
    }

    /// Check power/battery transitions and Caps Lock toggle (`AppDelegate.swift` + `IslandView.PillAlert`).
    fn check_system_sensors(&mut self) {
        // 1. Caps Lock indicator
        let caps = tray::is_caps_lock_on();
        if caps != self.caps_lock {
            self.caps_lock = caps;
            let alert = PillAlert {
                icon: AlertIcon::CapsLock { on: caps },
                message: "Caps Lock".to_string(),
                detail_text: Some(if caps { "ON" } else { "OFF" }.to_string()),
                detail_color: Some(if caps {
                    (0.30, 0.85, 0.45)
                } else {
                    (0.62, 0.62, 0.66)
                }),
            };
            self.trigger_alert(alert, Duration::from_millis(1800));
        }

        // 2. Power / Battery status
        let pwr = tray::read_power_status();
        let prev = self.power_state;
        self.power_state = pwr;
        let pct_f = pwr.percent.unwrap_or(100) as f32 / 100.0;

        if pwr.ac_online != prev.ac_online {
            if pwr.ac_online {
                self.low_bat_warned = false;
                self.crit_bat_warned = false;
                tray::play_alert_pop();
                self.trigger_alert(
                    PillAlert {
                        icon: AlertIcon::Battery {
                            percent: pct_f,
                            charging: true,
                        },
                        message: "Charging".to_string(),
                        detail_text: pwr.percent.map(|p| format!("{}%", p)),
                        detail_color: Some((0.30, 0.85, 0.45)),
                    },
                    Duration::from_millis(2600),
                );
            } else {
                tray::play_alert_pop();
                self.trigger_alert(
                    PillAlert {
                        icon: AlertIcon::Battery {
                            percent: pct_f,
                            charging: false,
                        },
                        message: "On Battery".to_string(),
                        detail_text: pwr.percent.map(|p| format!("{}%", p)),
                        detail_color: Some((0.90, 0.90, 0.92)),
                    },
                    Duration::from_millis(2400),
                );
            }
        } else if let Some(pct) = pwr.percent {
            if pwr.ac_online {
                if pct == 100 && prev.percent.unwrap_or(100) < 100 {
                    tray::play_alert_pop();
                    self.trigger_alert(
                        PillAlert {
                            icon: AlertIcon::Battery {
                                percent: 1.0,
                                charging: true,
                            },
                            message: "Fully Charged".to_string(),
                            detail_text: Some("100%".to_string()),
                            detail_color: Some((0.30, 0.85, 0.45)),
                        },
                        Duration::from_millis(3000),
                    );
                }
            } else if pct <= 10 && !self.crit_bat_warned {
                self.crit_bat_warned = true;
                self.low_bat_warned = true;
                tray::play_chime();
                self.trigger_alert(
                    PillAlert {
                        icon: AlertIcon::Battery {
                            percent: pct_f,
                            charging: false,
                        },
                        message: "Low Battery".to_string(),
                        detail_text: Some(format!("{}%", pct)),
                        detail_color: Some((0.95, 0.30, 0.28)),
                    },
                    Duration::from_millis(4000),
                );
            } else if pct <= 20 && !self.low_bat_warned {
                self.low_bat_warned = true;
                tray::play_alert_pop();
                self.trigger_alert(
                    PillAlert {
                        icon: AlertIcon::Battery {
                            percent: pct_f,
                            charging: false,
                        },
                        message: "Low Battery".to_string(),
                        detail_text: Some(format!("{}%", pct)),
                        detail_color: Some((0.95, 0.30, 0.28)),
                    },
                    Duration::from_millis(3200),
                );
            }
        }

        // 3. Expire finished alert
        if let Some((_, expires_at)) = self.active_alert {
            if Instant::now() >= expires_at {
                self.active_alert = None;
                if !self.pill.state.is_expanded() {
                    let target = self.closed_target_state();
                    if target != self.pill.state && !self.cursor_inside_window() {
                        self.set_state(target);
                    } else {
                        self.sync_size_to_current_state();
                        self.on_paint();
                    }
                } else {
                    self.on_paint();
                }
            }
        }
    }

    fn on_timer(&mut self, id: usize) {
        match id {
            TIMER_TICK => {
                self.update_eyes();
                self.refresh_media_from_cache(false);
                if self.pomodoro.check_finished(chrono::Local::now()) {
                    tray::play_chime();
                    let done_msg = match self.pomodoro.mode {
                        pomodoro::PomodoroMode::Focus => "Focus Done",
                        pomodoro::PomodoroMode::Break => "Break Done",
                    };
                    self.trigger_alert(
                        PillAlert {
                            icon: AlertIcon::Stopwatch,
                            message: done_msg.to_string(),
                            detail_text: Some("00:00".to_string()),
                            detail_color: Some((1.0, 0.52, 0.20)),
                        },
                        Duration::from_millis(3500),
                    );
                }
                // Check Water reminder interval:
                if self.water.notifications_enabled && self.water.ml < self.water.goal_ml {
                    let interval_secs = (self.water.interval_minutes.max(1) as u64) * 60;
                    if self.last_water_reminder.elapsed() >= Duration::from_secs(interval_secs) {
                        self.last_water_reminder = Instant::now();
                        tray::play_alert_pop();
                        self.trigger_alert(
                            PillAlert {
                                icon: AlertIcon::Checkmark,
                                message: "Drink Water".to_string(),
                                detail_text: Some(format!("+{} ml", self.water.glass_ml)),
                                detail_color: Some((0.28, 0.62, 1.0)),
                            },
                            Duration::from_millis(3500),
                        );
                    }
                }
                self.on_paint();
            }
            TIMER_ALERT => {
                self.check_system_sensors();
            }
            TIMER_ANIM => self.on_anim_tick(),
            TIMER_CLOSE => {
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_CLOSE);
                }
                if self.dragging_out || self.drag_hovering || self.todo_editing {
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

    /// Convert window client pixel coordinates `(x_px, y_px)` into pill-local logical points `(x, y)`.
    fn client_px_to_pill_pt(&self, x_px: i32, y_px: i32) -> (f32, f32) {
        let scale = window::dpi_scale(self.hwnd);
        let canvas_x = (x_px as f32) / scale;
        let canvas_y = (y_px as f32) / scale;
        let offset_x = (window::CANVAS_W - self.pill.width) * 0.5;
        (canvas_x - offset_x, canvas_y)
    }

    fn cursor_inside_window(&self) -> bool {
        let mut pt = POINT::default();
        let mut rect = RECT::default();
        if unsafe { !GetCursorPos(&mut pt).is_ok() || !GetWindowRect(self.hwnd, &mut rect).is_ok() } {
            return false;
        }
        let scale = window::dpi_scale(self.hwnd);
        let offset_x_px = (((window::CANVAS_W - self.pill.width) * 0.5) * scale).round() as i32;
        let pill_w_px = (self.pill.width * scale).round() as i32;
        let pill_h_px = (self.pill.height * scale).round() as i32;
        let pill_left = rect.left + offset_x_px - 2;
        let pill_right = rect.left + offset_x_px + pill_w_px + 2;
        let pill_top = rect.top;
        let pill_bottom = rect.top + pill_h_px + 2;
        pt.x >= pill_left && pt.x <= pill_right && pt.y >= pill_top && pt.y <= pill_bottom
    }

    fn apply_mouse_leave(&mut self) {
        self.suppress_hover_open_after_click_close = false;
        let target = self.closed_target_state();
        self.set_state(target);
    }

    fn on_mouse_move(&mut self) {
        self.suppress_leave = false;
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_CLOSE);
        }
        self.arm_mouse_tracking();
        if self.suppress_hover_open_after_click_close {
            return;
        }
        if self.pill.state == IslandState::Zen {
            if self.media.is_some() || self.pomodoro.is_active(chrono::Local::now()) {
                let target = self.last_section;
                self.set_state(target);
            }
        } else if self.pill.state == IslandState::Compact
            && (self.media.is_some() || self.pomodoro.is_active(chrono::Local::now()))
        {
            let target = self.last_section;
            self.set_state(target);
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking_mouse = false;
        if self.dragging_out || self.drag_hovering || self.todo_editing {
            return;
        }
        if self.suppress_leave {
            self.suppress_leave = false;
            return;
        }
        if self.cursor_inside_window() {
            self.arm_mouse_tracking();
            return;
        }
        unsafe {
            let _ = SetTimer(self.hwnd, TIMER_CLOSE, 150, None);
        }
    }

    /// Vertical mouse-wheel scrolling switches between the 5 expanded sections (`IslandView.scrollWheel`).
    fn on_mouse_wheel(&mut self, wheel_delta: i16) {
        if !self.pill.state.is_expanded() || wheel_delta == 0 {
            return;
        }
        let now = Instant::now();
        if now.duration_since(self.last_scroll_switch) < Duration::from_millis(200) {
            return;
        }
        let cur_idx = self.pill.state.section_index() as isize;
        let next_idx = if wheel_delta < 0 {
            (cur_idx + 1).clamp(0, 5) as usize
        } else {
            (cur_idx - 1).clamp(0, 5) as usize
        };
        if next_idx != cur_idx as usize {
            self.last_scroll_switch = now;
            self.set_state(IslandState::from_section_index(next_idx));
        }
    }

    fn on_mouse_down(&mut self, x_px: i32, y_px: i32) {
        self.drag_arm = None;
        self.removebg_drag_arm = None;
        if self.pill.state == IslandState::RemoveBg {
            // Arm drag-out from the result panel (mirror of tray tile arming).
            if self.removebg.result_bytes.is_some() {
                let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
                let r = renderer::RB_RIGHT_RECT;
                if x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3 {
                    self.removebg_drag_arm = Some((x_px, y_px));
                }
            }
            return;
        }
        if self.pill.state != IslandState::Tray {
            return;
        }
        let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
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
        let shown = self
            .shelf
            .files
            .len()
            .saturating_sub(start)
            .min(renderer::TRAY_PAGE_SIZE);
        for i in 0..shown {
            let col = (i % 4) as f32;
            let row = (i / 4) as f32;
            let left = gx + col * px;
            let top = gy + row * py;
            // Don't arm file drag when pressing the top-right `×` delete button (`left + 112..left + 136, top..top + 24`):
            if x >= left + 112.0 && x <= left + tw && y >= top && y <= top + 24.0 {
                return;
            }
            if x >= left && x <= left + tw && y >= top && y <= top + th {
                let idx = start + i;
                if std::path::Path::new(&self.shelf.files[idx]).exists() {
                    self.drag_arm = Some((idx, x_px, y_px));
                }
                return;
            }
        }
    }

    fn on_mouse_move_drag(&mut self, x_px: i32, y_px: i32) {
        if let Some((down_x, down_y)) = self.removebg_drag_arm {
            if self.pill.state != IslandState::RemoveBg {
                self.removebg_drag_arm = None;
                return;
            }
            let (threshold_x, threshold_y) =
                unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
            if (x_px - down_x).abs() < threshold_x && (y_px - down_y).abs() < threshold_y {
                return;
            }
            self.removebg_drag_arm = None;
            if let Some(path) = self.removebg.ensure_saved() {
                self.dragging_out = true;
                let _dropped = drag::do_drag_file(&path);
                self.dragging_out = false;
                self.swallow_next_click = true;
                self.on_paint();
            }
            return;
        }
        let Some((idx, down_x, down_y)) = self.drag_arm else {
            return;
        };
        if self.pill.state != IslandState::Tray {
            self.drag_arm = None;
            return;
        }
        let (threshold_x, threshold_y) =
            unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
        if (x_px - down_x).abs() < threshold_x && (y_px - down_y).abs() < threshold_y {
            return;
        }
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
        self.on_paint();
    }

    fn close_by_click(&mut self) {
        self.suppress_hover_open_after_click_close = true;
        let target = self.closed_target_state();
        self.set_state(target);
    }

    fn on_click(&mut self, x_px: i32, y_px: i32) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_CLOSE);
        }
        if self.swallow_next_click {
            self.swallow_next_click = false;
            return;
        }

        // Closed pill click -> expand to `last_section` (or `Pomodoro` if active timer pill clicked):
        if !self.pill.state.is_expanded() {
            self.suppress_hover_open_after_click_close = false;
            if self.pomodoro.is_active(chrono::Local::now()) && self.media.is_none() {
                self.set_state(IslandState::Pomodoro);
            } else {
                let target = self.last_section;
                self.set_state(target);
            }
            return;
        }

        let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
        let in_rect = |r: (f32, f32, f32, f32)| x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3;

        // 1. Right-edge vertical section dots (`IslandView.dotIndex(at:)`):
        if let Some(dot_idx) = renderer::dot_index_at(x, y, self.pill.width, self.pill.height) {
            let target = IslandState::from_section_index(dot_idx);
            if target != self.pill.state
                && !(target == IslandState::Expanded && self.pill.state == IslandState::Lyrics)
            {
                self.set_state(target);
            }
            return;
        }

        // 2. Section-specific controls:
        match self.pill.state {
            IslandState::Expanded | IslandState::Lyrics => {
                if self.media.is_some() {
                    let scale = window::dpi_scale(self.hwnd);
                    let canvas_x = (x_px as f32) / scale;
                    let canvas_y = (y_px as f32) / scale;
                    let x_520 = canvas_x - (window::CANVAS_W - 520.0) * 0.5;
                    let x_700 = canvas_x - (window::CANVAS_W - 700.0) * 0.5;
                    let in_bubble_at = |bx: f32| {
                        bx >= renderer::MUSIC_BUBBLE_RECT.0 - 4.0
                            && bx <= renderer::MUSIC_BUBBLE_RECT.2 + 4.0
                            && y >= renderer::MUSIC_BUBBLE_RECT.1 - 4.0
                            && y <= renderer::MUSIC_BUBBLE_RECT.3 + 4.0
                    };
                    let repeat_bubble = self
                        .last_bubble_click
                        .map(|(lx, ly, lt)| {
                            lt.elapsed() < Duration::from_millis(750)
                                && (canvas_x - lx).abs() <= 32.0
                                && (canvas_y - ly).abs() <= 24.0
                        })
                        .unwrap_or(false);
                    let morphing_bubble = self.spring.active && (in_bubble_at(x_520) || in_bubble_at(x_700));

                    if in_bubble_at(x) || repeat_bubble || morphing_bubble {
                        self.last_bubble_click = Some((canvas_x, canvas_y, Instant::now()));
                        if self.pill.state == IslandState::Expanded {
                            self.lyrics_mode = true;
                            self.request_lyrics_async();
                            self.set_state(IslandState::Lyrics);
                        } else {
                            self.lyrics_mode = false;
                            self.set_state(IslandState::Expanded);
                        }
                        return;
                    }

                    let lyrics_t = ((self.pill.width - 520.0) / (700.0 - 520.0)).clamp(0.0, 1.0);
                    let centers = [
                        renderer::MUSIC_CONTROL_CENTERS[0]
                            + (renderer::LYRICS_CONTROL_CENTERS[0]
                                - renderer::MUSIC_CONTROL_CENTERS[0])
                                * lyrics_t,
                        renderer::MUSIC_CONTROL_CENTERS[1]
                            + (renderer::LYRICS_CONTROL_CENTERS[1]
                                - renderer::MUSIC_CONTROL_CENTERS[1])
                                * lyrics_t,
                        renderer::MUSIC_CONTROL_CENTERS[2]
                            + (renderer::LYRICS_CONTROL_CENTERS[2]
                                - renderer::MUSIC_CONTROL_CENTERS[2])
                                * lyrics_t,
                    ];
                    if let Some(ctrl) = renderer::control_hit_at(x, y, &centers) {
                        let playing = self.media.as_ref().map(|m| m.playing).unwrap_or(false);
                        match ctrl {
                            0 => media::skip_previous_async(),
                            1 => {
                                if playing {
                                    media::pause_async();
                                } else {
                                    media::play_async();
                                }
                            }
                            2 => media::skip_next_async(),
                            _ => {}
                        }
                        self.on_paint();
                        return;
                    }
                }
                if !self.spring.active {
                    self.close_by_click();
                }
            }
            IslandState::Pomodoro => {
                if in_rect(renderer::POMODORO_SHORT_BREAK_RECT) {
                    self.pomodoro.select_mode(pomodoro::PomodoroMode::Break);
                    self.on_paint();
                    return;
                }
                if in_rect(renderer::POMODORO_PLAY_RECT) {
                    self.pomodoro.toggle_start_pause(chrono::Local::now());
                    self.on_paint();
                    return;
                }
                if in_rect(renderer::POMODORO_FOCUS_RECT) {
                    self.pomodoro.select_mode(pomodoro::PomodoroMode::Focus);
                    self.on_paint();
                    return;
                }
                if in_rect(renderer::POMODORO_RESET_RECT) {
                    self.pomodoro.reset();
                    self.on_paint();
                    return;
                }
                self.close_by_click();
            }
            IslandState::Todo => {
                if in_rect(renderer::TODO_ADD_PILL_RECT)
                    || (self.todo_editing && in_rect(renderer::TODO_INPUT_RECT))
                {
                    self.begin_todo_editing();
                    return;
                }
                if self.todo_editing {
                    self.end_todo_editing(true);
                    return;
                }
                let visible = self.todo.visible_indices();
                for (row_i, &task_idx) in visible.iter().enumerate() {
                    let row_top = renderer::TODO_ROW_Y0 + (row_i as f32) * renderer::TODO_ROW_PITCH;
                    let row_bot = row_top + renderer::TODO_ROW_H;
                    if y >= row_top && y <= row_bot {
                        if x >= renderer::TODO_ROW_DELETE_LEFT
                            && x <= renderer::TODO_ROW_DELETE_RIGHT
                        {
                            self.todo.remove(task_idx);
                            self.on_paint();
                            return;
                        }
                        if x >= renderer::TODO_ROW_X && x <= renderer::TODO_ROW_TOGGLE_RIGHT {
                            self.todo.toggle(task_idx);
                            self.on_paint();
                            return;
                        }
                    }
                }
                self.close_by_click();
            }
            IslandState::Water => {
                if in_rect(renderer::WATER_TOGGLE_RECT) {
                    self.water.toggle_notifications();
                    self.last_water_reminder = Instant::now();
                    self.on_paint();
                    return;
                }
                if in_rect(renderer::WATER_DRINK_RECT) {
                    self.water.add_glass();
                    self.last_water_reminder = Instant::now();
                    self.on_paint();
                    return;
                }
                if in_rect(renderer::WATER_SETTINGS_RECT) {
                    self.show_water_settings_menu();
                    return;
                }
                self.close_by_click();
            }
            IslandState::Tray => {
                if !self.shelf.files.is_empty() && in_rect(renderer::TRAY_CLEAR_RECT) {
                    self.shelf.clear();
                    self.tray_page = 0;
                    self.on_paint();
                    return;
                }
                let page_count = self.shelf.page_count(renderer::TRAY_PAGE_SIZE);
                if self.tray_page >= page_count {
                    self.tray_page = page_count.saturating_sub(1);
                }
                if page_count > 1 {
                    if in_rect(renderer::TRAY_LEFT_ARROW_RECT) && self.tray_page > 0 {
                        self.tray_page -= 1;
                        self.on_paint();
                        return;
                    }
                    if in_rect(renderer::TRAY_RIGHT_ARROW_RECT) && self.tray_page + 1 < page_count {
                        self.tray_page += 1;
                        self.on_paint();
                        return;
                    }
                    // Bottom page dots (`y in 255..275`):
                    if y >= 255.0 && y <= 275.0 {
                        let dot_gap = 12.0f32;
                        let total_w = (page_count as f32 - 1.0) * dot_gap;
                        let start_x = self.pill.width * 0.5 - total_w * 0.5;
                        for p in 0..page_count {
                            let dx = start_x + (p as f32) * dot_gap;
                            if (x - dx).abs() <= 6.0 {
                                self.tray_page = p;
                                self.on_paint();
                                return;
                            }
                        }
                    }
                }
                let (gx, gy, tw, th, px, py) = (
                    renderer::TRAY_TILE_GEOM.0,
                    renderer::TRAY_TILE_GEOM.1,
                    renderer::TRAY_TILE_GEOM.2,
                    renderer::TRAY_TILE_GEOM.3,
                    renderer::TRAY_TILE_GEOM.4,
                    renderer::TRAY_TILE_GEOM.5,
                );
                let start = self.tray_page * renderer::TRAY_PAGE_SIZE;
                let shown = self
                    .shelf
                    .files
                    .len()
                    .saturating_sub(start)
                    .min(renderer::TRAY_PAGE_SIZE);
                for i in 0..shown {
                    let col = (i % 4) as f32;
                    let row = (i / 4) as f32;
                    let left = gx + col * px;
                    let top = gy + row * py;
                    // Top-right `×` delete button inside tile:
                    if x >= left + 112.0 && x <= left + tw && y >= top && y <= top + 24.0 {
                        self.shelf.remove(start + i);
                        let new_count = self.shelf.page_count(renderer::TRAY_PAGE_SIZE);
                        if self.tray_page >= new_count {
                            self.tray_page = new_count.saturating_sub(1);
                        }
                        self.on_paint();
                        return;
                    }
                    if x >= left && x <= left + tw && y >= top && y <= top + th {
                        self.show_tile_menu(start + i);
                        return;
                    }
                }
                self.close_by_click();
            }
            IslandState::RemoveBg => {
                if in_rect(renderer::RB_MODEL_RECT) {
                    self.show_removebg_model_menu();
                    return;
                }
                if in_rect(renderer::RB_FROM_TRAY_RECT)
                    && !removebg::tray_images(&self.shelf).is_empty()
                {
                    self.show_removebg_tray_menu();
                    return;
                }
                if in_rect(renderer::RB_SAVE_RECT) && self.removebg.result_bytes.is_some() {
                    if let Some(path) = self.removebg.ensure_saved() {
                        let name = std::path::Path::new(&path)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or(path.clone());
                        self.removebg.notice = format!("Saved: {}", name);
                    }
                    self.on_paint();
                    return;
                }
                if in_rect(renderer::RB_LEFT_RECT) {
                    if let Some(path) = Self::pick_image_file(self.hwnd) {
                        if removebg::is_image_path(&path) {
                            self.removebg_set_input_and_run(&path);
                        } else {
                            self.removebg.error =
                                Some("That file is not an image.".to_string());
                            self.on_paint();
                        }
                    }
                    return;
                }
            }
            IslandState::Zen | IslandState::Compact => {}
        }
    }

    /// Popup menu for Water settings (`AppDelegate.showWaterSettingsMenu()` in `aura-island`).
    fn show_water_settings_menu(&mut self) {
        self.suppress_leave = true;
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                self.suppress_leave = false;
                return;
            };
            let flag = |active: bool| -> MENU_ITEM_FLAGS {
                if active {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                }
            };
            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("Daily Goal"));
            let _ = AppendMenuW(
                menu,
                flag(self.water.goal_ml == 1500),
                MENU_WATER_GOAL_1500,
                w!("1.5 L"),
            );
            let _ = AppendMenuW(
                menu,
                flag(self.water.goal_ml == 2000),
                MENU_WATER_GOAL_2000,
                w!("2.0 L"),
            );
            let _ = AppendMenuW(
                menu,
                flag(self.water.goal_ml == 2500),
                MENU_WATER_GOAL_2500,
                w!("2.5 L"),
            );
            let _ = AppendMenuW(
                menu,
                flag(self.water.goal_ml == 3000),
                MENU_WATER_GOAL_3000,
                w!("3.0 L"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("Glass Size"));
            let _ = AppendMenuW(
                menu,
                flag(self.water.glass_ml == 200),
                MENU_WATER_GLASS_200,
                w!("200 ml"),
            );
            let _ = AppendMenuW(
                menu,
                flag(self.water.glass_ml == 250),
                MENU_WATER_GLASS_250,
                w!("250 ml"),
            );
            let _ = AppendMenuW(
                menu,
                flag(self.water.glass_ml == 350),
                MENU_WATER_GLASS_350,
                w!("350 ml"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

            let _ = AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, w!("Reminder Every"));
            let _ = AppendMenuW(
                menu,
                flag(self.water.interval_minutes == 30),
                MENU_WATER_INT_30,
                w!("30 min"),
            );
            let _ = AppendMenuW(
                menu,
                flag(self.water.interval_minutes == 60),
                MENU_WATER_INT_60,
                w!("60 min"),
            );
            let _ = AppendMenuW(
                menu,
                flag(self.water.interval_minutes == 90),
                MENU_WATER_INT_90,
                w!("90 min"),
            );
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

            let _ = AppendMenuW(menu, MF_STRING, MENU_WATER_RESET, w!("Reset Today's Intake"));

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
            let _ = DestroyMenu(menu);
            self.suppress_leave = false;

            match id.0 as usize {
                MENU_WATER_GOAL_1500 => self.water.set_goal(1500),
                MENU_WATER_GOAL_2000 => self.water.set_goal(2000),
                MENU_WATER_GOAL_2500 => self.water.set_goal(2500),
                MENU_WATER_GOAL_3000 => self.water.set_goal(3000),
                MENU_WATER_GLASS_200 => self.water.set_glass(200),
                MENU_WATER_GLASS_250 => self.water.set_glass(250),
                MENU_WATER_GLASS_350 => self.water.set_glass(350),
                MENU_WATER_INT_30 => self.water.set_interval(30),
                MENU_WATER_INT_60 => self.water.set_interval(60),
                MENU_WATER_INT_90 => self.water.set_interval(90),
                MENU_WATER_RESET => self.water.reset(),
                _ => {}
            }
        }
        self.on_paint();
    }

    fn show_tile_menu(&mut self, idx: usize) {
        let path = match self.shelf.files.get(idx) {
            Some(p) => p.clone(),
            None => return,
        };
        self.suppress_leave = true;
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                self.suppress_leave = false;
                return;
            };
            let Ok(share_menu) = CreatePopupMenu() else {
                let _ = DestroyMenu(menu);
                self.suppress_leave = false;
                return;
            };
            let _ = AppendMenuW(menu, MF_STRING, MENU_TILE_OPEN, w!("Open"));
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
            self.suppress_leave = false;
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
                self.on_paint();
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
        }
    }

    /// Set Remove BG input and start an upload run (shared by
    /// pick/drop/From Tray).
    fn removebg_set_input_and_run(&mut self, path: &str) {
        self.removebg.set_input(path);
        if let Some((gen, bytes, filename, model_param)) = self.removebg.start_run() {
            removebg::run_async(
                self.hwnd,
                self.removebg_pending.clone(),
                gen,
                bytes,
                filename,
                model_param,
            );
            self.on_paint();
        }
    }

    /// Called when `WM_REMOVEBG_READY` (`0x8010`) arrives from the upload
    /// worker. Stale results (superseded by a newer run) are dropped.
    fn on_removebg_ready(&mut self) {
        let fetched = self
            .removebg_pending
            .lock()
            .ok()
            .and_then(|mut g| g.take());
        if let Some(res) = fetched {
            if res.gen != self.removebg.run_gen {
                return;
            }
            self.removebg.uploading = false;
            match res.result {
                Ok(bytes) => {
                    self.removebg.result_preview =
                        removebg::decode_preview_fit(&bytes, 264, 144);
                    self.removebg.result_bytes = Some(bytes);
                    self.removebg.error = None;
                }
                Err(msg) => {
                    self.removebg.error = Some(msg);
                    self.removebg.result_bytes = None;
                    self.removebg.result_preview = None;
                }
            }
            self.on_paint();
        }
    }

    /// Popup menu for the Remove BG model pill (Mac `Model:` pill).
    fn show_removebg_model_menu(&mut self) {
        self.suppress_leave = true;
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                self.suppress_leave = false;
                return;
            };
            let items = [
                (MENU_RB_MODEL_QUALITY, removebg::RemoveBgModel::Quality),
                (MENU_RB_MODEL_BALANCED, removebg::RemoveBgModel::Balanced),
                (MENU_RB_MODEL_FAST, removebg::RemoveBgModel::Fast),
            ];
            for (id, model) in items {
                let flags = if model == self.removebg.model {
                    MF_STRING | MF_CHECKED
                } else {
                    MF_STRING
                };
                let mut wide: Vec<u16> = model.label().encode_utf16().collect();
                wide.push(0);
                let _ = AppendMenuW(menu, flags, id, PCWSTR::from_raw(wide.as_ptr()));
            }
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
            let _ = DestroyMenu(menu);
            self.suppress_leave = false;
            let picked = id.0 as usize;
            let new_model = match picked {
                MENU_RB_MODEL_QUALITY => Some(removebg::RemoveBgModel::Quality),
                MENU_RB_MODEL_BALANCED => Some(removebg::RemoveBgModel::Balanced),
                MENU_RB_MODEL_FAST => Some(removebg::RemoveBgModel::Fast),
                _ => None,
            };
            if let Some(model) = new_model {
                if model != self.removebg.model {
                    self.removebg.model = model;
                    // Mac: changing the model re-runs from the stored input.
                    if let Some(path) = self.removebg.input_path.clone() {
                        self.removebg_set_input_and_run(&path);
                    } else {
                        self.on_paint();
                    }
                }
            }
        }
    }

    /// Popup menu listing tray images for Remove BG input (Mac `From Tray` pill).
    fn show_removebg_tray_menu(&mut self) {
        let images = removebg::tray_images(&self.shelf);
        if images.is_empty() {
            return;
        }
        self.suppress_leave = true;
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                self.suppress_leave = false;
                return;
            };
            let shown: Vec<String> = images.into_iter().take(100).collect();
            for (i, path) in shown.iter().enumerate() {
                let name = std::path::Path::new(path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                let mut wide: Vec<u16> =
                    name.encode_utf16().collect();
                wide.push(0);
                let _ = AppendMenuW(
                    menu,
                    MF_STRING,
                    MENU_RB_TRAY_BASE + i,
                    PCWSTR::from_raw(wide.as_ptr()),
                );
            }
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
            let _ = DestroyMenu(menu);
            self.suppress_leave = false;
            let picked = id.0 as usize;
            if picked >= MENU_RB_TRAY_BASE && picked < MENU_RB_TRAY_BASE + shown.len() {
                let path = shown[picked - MENU_RB_TRAY_BASE].clone();
                self.removebg_set_input_and_run(&path);
            }
        }
    }

    /// Single-file picker (IFileOpenDialog; no new Cargo feature needed).
    fn pick_image_file(hwnd: HWND) -> Option<String> {
        unsafe {
            let dialog: IFileOpenDialog =
                CoCreateInstance(&CLSID_FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            dialog.Show(hwnd).ok()?;
            let item = dialog.GetResult().ok()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let s = name.to_string().ok()?;
            CoTaskMemFree(name.0 as *const core::ffi::c_void);
            Some(s)
        }
    }

    fn on_right_click(&mut self, x_px: i32, y_px: i32) {
        if self.pill.state != IslandState::Tray {
            return;
        }
        let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
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
        let shown = self
            .shelf
            .files
            .len()
            .saturating_sub(start)
            .min(renderer::TRAY_PAGE_SIZE);
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

    /// Compute cursor position in unscaled Tray base coordinates (`640x280`), matching
    /// `IslandView.basePoint(from:)` in `aura-island`.
    fn current_tray_drag_base_pt(&self) -> Option<(f32, f32)> {
        let mut pt = POINT::default();
        let mut rect = RECT::default();
        if unsafe { !GetCursorPos(&mut pt).is_ok() || !GetWindowRect(self.hwnd, &mut rect).is_ok() } {
            return None;
        }
        let dpi_s = window::dpi_scale(self.hwnd);
        let canvas_x = (pt.x - rect.left) as f32 / dpi_s;
        let canvas_y = (pt.y - rect.top) as f32 / dpi_s;
        let s = self.tray_bounce.scale().max(0.5);
        let lw = if self.pill.state == IslandState::Tray {
            self.pill.width
        } else {
            640.0
        };
        let tx = window::CANVAS_W * 0.5 - (lw * 0.5) * s;
        Some(((canvas_x - tx) / s, canvas_y / s))
    }

    /// OLE `IDropTarget::DragEnter` & `DragOver` handler (`nusa.exe` `0x1400108E1` / `aura-island` `draggingEntered` & `draggingUpdated`):
    /// Immediately expands the inactive pill (`Zen`/`Compact`) or switches any active section to `Tray`,
    /// updates the cursor-following radial light glow & `"+ Copy"` badge, and triggers the `1.03x` hover bounce!
    fn on_drag_enter_tray(&mut self) {
        if self.dragging_out {
            return;
        }
        if self.pill.state == IslandState::RemoveBg {
            // Open tool section handles its own drop (Mac rule): stay here
            // and highlight the card instead of switching to Tray.
            self.drag_hovering = true;
            self.on_paint();
            return;
        }
        self.suppress_hover_open_after_click_close = false;
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_CLOSE);
        }
        let was_hovering = self.drag_hovering;
        self.drag_hovering = true;
        if self.pill.state != IslandState::Tray {
            self.set_state(IslandState::Tray);
        }
        if !was_hovering {
            self.tray_bounce.trigger(1.03, None);
            self.ensure_anim_timer();
        }
        self.tray_drag_pt = self.current_tray_drag_base_pt();
        self.on_paint();
    }

    /// OLE `IDropTarget::DragLeave` handler (`nusa.exe` `0x140010C8A` / `aura-island` `draggingExited`):
    fn on_drag_leave_tray(&mut self) {
        self.drag_hovering = false;
        self.tray_drag_pt = None;
        if self.dragging_out || self.todo_editing {
            return;
        }
        self.tray_bounce.trigger(1.0, None);
        self.ensure_anim_timer();
        self.on_paint();
        unsafe {
            let _ = SetTimer(self.hwnd, TIMER_CLOSE, 250, None);
        }
    }

    /// OLE `IDropTarget::Drop` handler (`nusa.exe` `0x14000F088` / `aura-island` `performDragOperation`):
    /// Adds dropped files to the Tray and triggers the Mac `1.12x -> 1.0x` damped-cosine bubble pop bounce!
    fn on_drag_drop_tray(&mut self) {
        self.drag_hovering = false;
        self.tray_drag_pt = None;
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_CLOSE);
        }
        let paths = self
            .dropped_files
            .lock()
            .ok()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default();
        if self.pill.state == IslandState::RemoveBg {
            // Open tool section handles its own drop (Mac rule): first image
            // becomes Remove BG input, the rest fall through to Tray.
            let mut used_as_input = false;
            for path in &paths {
                if !used_as_input && removebg::is_image_path(path) {
                    self.removebg_set_input_and_run(path);
                    used_as_input = true;
                } else {
                    let _ = self.shelf.add(path);
                }
            }
            self.on_paint();
            return;
        }
        let mut added = false;
        for path in paths {
            if self.shelf.add(&path) {
                added = true;
            }
        }
        if added {
            self.tray_page = self
                .shelf
                .page_count(renderer::TRAY_PAGE_SIZE)
                .saturating_sub(1);
        }
        self.set_state(IslandState::Tray);
        self.tray_bounce.trigger(1.0, Some(1.12));
        self.ensure_anim_timer();
        self.on_paint();
    }

    fn on_drop_files(&mut self, hdrop_raw: isize) {
        unsafe {
            let hdrop = HDROP(hdrop_raw as *mut core::ffi::c_void);
            let count = DragQueryFileW(hdrop, u32::MAX, None);
            let mut paths: Vec<String> = Vec::new();
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
                paths.push(String::from_utf16_lossy(&buf[..end]));
            }
            DragFinish(hdrop);
            if self.pill.state == IslandState::RemoveBg {
                // Same routing as the OLE drop path (Mac rule).
                let mut used_as_input = false;
                for path in &paths {
                    if !used_as_input && removebg::is_image_path(path) {
                        self.removebg_set_input_and_run(path);
                        used_as_input = true;
                    } else {
                        let _ = self.shelf.add(path);
                    }
                }
                self.on_paint();
                return;
            }
            let mut added = false;
            for path in &paths {
                if self.shelf.add(path) {
                    added = true;
                }
            }
            if added {
                self.tray_page = self
                    .shelf
                    .page_count(renderer::TRAY_PAGE_SIZE)
                    .saturating_sub(1);
                self.set_state(IslandState::Tray);
                self.tray_bounce.trigger(1.0, Some(1.12));
                self.ensure_anim_timer();
                self.on_paint();
            }
        }
    }

    fn on_paint(&mut self) {
        let alert_clone = self.current_alert().cloned();
        let battery_percent = self.power_state.percent.map(|p| (p as f32) / 100.0);
        let battery_charging = self.power_state.ac_online;
        {
            let ctx = RenderContext {
                state: self.pill.state,
                lyrics_mode: self.lyrics_mode,
                eye_look: self.eye_look,
                eyes_closed: self.eyes_closed,
                battery_percent,
                battery_charging,
                media: self.media.as_ref(),
                lyrics: self.lyrics.as_ref(),
                pomodoro: &self.pomodoro,
                todo: &self.todo,
                water: &self.water,
                todo_editing: self.todo_editing,
                todo_input: &self.todo_input,
                shelf: &self.shelf,
                removebg: &self.removebg,
                removebg_drop_highlight: self.drag_hovering
                    && self.pill.state == IslandState::RemoveBg
                    && !self.dragging_out,
                tray_page: self.tray_page,
                tray_drop_highlight: self.drag_hovering && !self.dragging_out,
                tray_drag_pt: self.tray_drag_pt,
                tray_spring_scale: self.tray_bounce.scale(),
                cur_w: self.pill.width,
                cur_h: self.pill.height,
                alert: alert_clone.as_ref(),
            };
            self.renderer.draw(self.hwnd, &ctx);
        }
        if self.renderer.is_lyric_rolling() || self.tray_bounce.is_active() {
            self.ensure_anim_timer();
        }
    }

    fn on_tray_callback(&mut self, event: u32) {
        if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
            let id = tray::show_menu(self.hwnd);
            if id == tray::MENU_QUIT {
                unsafe {
                    PostQuitMessage(0);
                }
            } else if id == tray::MENU_STATUS {
                let peer_count = self.lan.as_ref().map(|l| l.peers().len()).unwrap_or(0);
                tray::show_status_dialog(
                    self.hwnd,
                    &self.poll_status,
                    self.media.as_ref(),
                    peer_count,
                );
            } else if id == tray::MENU_AUTORUN {
                let enabled = tray::toggle_autorun();
                let msg = if enabled {
                    "Start with Windows"
                } else {
                    "Autorun Disabled"
                };
                self.trigger_alert(
                    PillAlert {
                        icon: AlertIcon::Checkmark,
                        message: msg.to_string(),
                        detail_text: Some(if enabled { "ON" } else { "OFF" }.to_string()),
                        detail_color: Some(if enabled {
                            (0.30, 0.85, 0.45)
                        } else {
                            (0.62, 0.62, 0.66)
                        }),
                    },
                    Duration::from_millis(2200),
                );
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
    let app_ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
    if app_ptr.is_null() {
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
        WM_MOUSELEAVE_MSG => {
            app.on_mouse_leave();
            LRESULT(0)
        }
        WM_MOUSEWHEEL_MSG => {
            let delta = ((wparam.0 >> 16) & 0xFFFF) as u16 as i16;
            app.on_mouse_wheel(delta);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let x = ((lparam.0 as u32) & 0xFFFF) as u16 as i16 as i32;
            let y = (((lparam.0 as u32) >> 16) & 0xFFFF) as u16 as i16 as i32;
            app.on_mouse_down(x, y);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let x = ((lparam.0 as u32) & 0xFFFF) as u16 as i16 as i32;
            let y = (((lparam.0 as u32) >> 16) & 0xFFFF) as u16 as i16 as i32;
            app.on_click(x, y);
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            let x = ((lparam.0 as u32) & 0xFFFF) as u16 as i16 as i32;
            let y = (((lparam.0 as u32) >> 16) & 0xFFFF) as u16 as i16 as i32;
            app.on_right_click(x, y);
            LRESULT(0)
        }
        0x0233 => {
            app.on_drop_files(lparam.0);
            LRESULT(0)
        }
        drag::WM_DRAG_ENTER_TRAY => {
            app.on_drag_enter_tray();
            LRESULT(0)
        }
        drag::WM_DRAG_LEAVE_TRAY => {
            app.on_drag_leave_tray();
            LRESULT(0)
        }
        drag::WM_DRAG_DROP_TRAY => {
            app.on_drag_drop_tray();
            LRESULT(0)
        }
        WM_TIMER => {
            app.on_timer(wparam.0);
            LRESULT(0)
        }
        WM_POWERBROADCAST_MSG => {
            app.check_system_sensors();
            LRESULT(1)
        }
        media::WM_MEDIA_UPDATE => {
            app.on_media_update();
            LRESULT(0)
        }
        lyrics::WM_LYRICS_READY => {
            app.on_lyrics_ready();
            LRESULT(0)
        }
        removebg::WM_REMOVEBG_READY => {
            app.on_removebg_ready();
            LRESULT(0)
        }
        0x0102 => {
            let code = (wparam.0 & 0xFFFF) as u16;
            app.on_todo_char(code);
            LRESULT(0)
        }
        0x0100 => {
            app.on_todo_keydown(wparam.0 as u32);
            LRESULT(0)
        }
        0x0008 => {
            if app.todo_editing {
                app.end_todo_editing(true);
            }
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_DESTROY => {
            tray::remove_icon(hwnd);
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
        let mutex = CreateMutexW(None, BOOL(1), w!("Aura.SingleInstance"))?;
        if GetLastError().0 == 183 {
            return Ok(());
        }
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let _ = OleInitialize(None);
        let hmodule = GetModuleHandleW(None)?;
        let hinstance: HINSTANCE = hmodule.into();
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
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: HICON(std::ptr::null_mut()),
        };
        let atom = RegisterClassExW(&wc);
        if atom == 0 {
            return Err(windows::core::Error::from_win32());
        }
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            class_name,
            w!("Aura"),
            WS_POPUP,
            0,
            0,
            760,
            330,
            None,
            None,
            hinstance,
            None,
        )?;
        let _ = DragAcceptFiles(hwnd, BOOL(1));
        let dropped_files = Arc::new(Mutex::new(Vec::new()));
        drag::register_drop_target(hwnd, dropped_files.clone());
        let _power_notify = tray::register_power_notify(hwnd);

        // Start MTA + event-driven GSMTC coordinator.
        let media_cache = Arc::new(Mutex::new(media::PollCache::waiting()));
        media::spawn_poller(hwnd, media_cache.clone());

        let mut app = App::new(hwnd, media_cache, dropped_files)?;
        app.set_lan(lan::LanShare::start());
        let app_ptr = Box::into_raw(app);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize);

        let _ = tray::add_icon(hwnd);
        let _ = SetTimer(hwnd, TIMER_TICK, 100, None);
        let _ = SetTimer(hwnd, TIMER_ALERT, 250, None);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        let mut msg = MSG::default();
        loop {
            let ret = GetMessageW(&mut msg, None, 0, 0);
            if ret.0 <= 0 {
                break;
            }
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
        let _ = ReleaseMutex(mutex);
        OleUninitialize();
        Ok(())
    }
}
