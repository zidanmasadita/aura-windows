//! App: window + state + animator + renderer + tray + timers + event handlers.
//! Matches `aura-island` (`AppDelegate.swift` & `IslandView.swift`) 1:1 while preserving
//! Windows-native GSMTC media events, async LRCLIB lyrics worker, LAN file share, and
//! system power/Caps Lock sensors.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{w, GUID, Result, PCWSTR, HSTRING};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{HBRUSH, ScreenToClient, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_INPROC_SERVER};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP, IFileOpenDialog, SIGDN_FILESYSPATH};
use windows::Win32::UI::WindowsAndMessaging::*;

#[link(name = "winmm")]
extern "system" {
    fn timeBeginPeriod(uPeriod: u32) -> u32;
    fn timeEndPeriod(uPeriod: u32) -> u32;
}

#[link(name = "psapi")]
extern "system" {
    fn EmptyWorkingSet(hProcess: HANDLE) -> BOOL;
}

pub fn trim_working_set() {
    unsafe {
        use windows::Win32::System::Threading::GetCurrentProcess;
        let _ = EmptyWorkingSet(GetCurrentProcess());
    }
}

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
const TIMER_HOVER_EXPAND: usize = 5; // hover wait-bulge auto-expand timer (delay from settings)

#[derive(Clone, Copy, Debug, PartialEq)]
enum EyeExpression {
    Normal,
    BlinkBoth,
    WinkLeft,
    WinkRight,
    Inquisitive,
}

const MENU_TILE_OPEN: usize = 2001;
const MENU_TILE_REMOVE: usize = 2002;
const MENU_TILE_SHARE_BASE: usize = 2100;
#[allow(dead_code)]
const MENU_RB_TRAY_BASE: usize = 2200;
const MENU_RB_MODEL_QUALITY: usize = 2301;
const MENU_RB_MODEL_BALANCED: usize = 2302;
const MENU_RB_MODEL_FAST: usize = 2303;

const MENU_CONVERT_FORMAT_AVIF: usize = 2401;
const MENU_CONVERT_FORMAT_WEBP: usize = 2402;
const MENU_CONVERT_FORMAT_PNG: usize = 2403;
#[allow(dead_code)]
const MENU_CONVERT_TRAY_BASE: usize = 2500;

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

/// An extra pill canvas on another monitor (mirrors the main pill).
struct CanvasWin {
    hwnd: HWND,
    base_x: i32,
    base_y: i32,
}

pub struct App {
    hwnd: HWND,
    hinstance: HINSTANCE,
    canvas_class: PCWSTR,
    extra_canvases: Vec<CanvasWin>,
    main_base: (i32, i32),
    pill_y_offset: f32,
    last_offset_applied: f32,
    hide_fs: bool,
    dodge_since: Option<Instant>,
    dodging: bool,
    pill_covered: bool,
    cover_poll_at: Instant,
    volume_hover_since: Option<Instant>,
    pill_hovered: bool,
    preview_close_at: Option<Instant>,
    manual_hold: bool,
    was_hide_fs: bool,
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
    convert: crate::convert::ConvertState,
    convert_pending: Arc<Mutex<Option<crate::convert::ConvertResult>>>,
    convert_drag_arm: Option<(i32, i32)>,
    eye_openness: [f32; 2],
    eye_openness_target: [f32; 2],
    eye_slant: [f32; 2],
    eye_slant_target: [f32; 2],
    next_eye_expr_at: Instant,
    eye_expr_until: Instant,
    current_eye_expr: EyeExpression,
    master_volume: f32,
    last_non_zero_volume: f32,
    volume_bar_hover: bool,
    volume_bar_zoom: f32,
    volume_bar_zoom_target: f32,
    volume_dragging: bool,
    music_button_zooms: [f32; 4],
    music_button_zoom_targets: [f32; 4],
    music_bar_hover: bool,
    music_seek_pos: Option<f64>,
    music_progress_dragging: bool,
    water: water::Water,
    last_water_reminder: Instant,
    last_section: IslandState,
    last_scroll_switch: Instant,
    power_state: tray::PowerState,
    low_bat_warned: bool,
    crit_bat_warned: bool,
    caps_lock: bool,
    active_alert: Option<(PillAlert, Instant)>,
    hover_bulging: bool,
    hover_expand_timer_active: bool,
    card_expanded: bool,
    todo_scroll: f32,
    todo_scroll_target: f32,
    todo_editing_idx: Option<usize>,
    tray_scroll: f32,
    tray_scroll_target: f32,
    transition_active: bool,
    transition_start: Instant,
    transition_from: IslandState,
    transition_to: IslandState,
    transition_dir: f32,
    dot_spring: animator::FluidDotIndicator,
    wheel_accum: f32,
    hovered_dot: Option<usize>,
    music_bar_zoom: f32,
    music_bar_zoom_target: f32,
    tray_genie_start: Option<Instant>,
    trim_counter: u32,
    mouse_pt: Option<(f32, f32)>,
    tray_btn_hovers: [f32; 4],
    tray_btn_hover_targets: [f32; 4],
    rb_btn_hovers: [f32; 2],
    rb_btn_hover_targets: [f32; 2],
    cv_btn_hovers: [f32; 3],
    cv_btn_hover_targets: [f32; 3],
    pub settings: crate::settings::AuraSettings,
    pub pdf_md_drag: Option<(usize, f32, f32)>,
}

impl App {
    fn new(
        hwnd: HWND,
        hinstance: HINSTANCE,
        canvas_class: PCWSTR,
        media_cache: Arc<Mutex<media::PollCache>>,
        dropped_files: Arc<Mutex<Vec<String>>>,
    ) -> Result<Box<Self>> {
        let renderer = renderer::Renderer::new()?;
        let power_state = tray::read_power_status();
        let caps_lock = tray::is_caps_lock_on();
        let now = Instant::now();
        let pill = state::PillState::new();
        let spring = animator::Spring2D::new(pill.width, pill.height);
        let vol = 0.5f32;
        let todo = todo::TodoList::load();
        let shelf = crate::shelf::Shelf::load();
        let removebg = removebg::RemoveBgState::new();
        let convert = crate::convert::ConvertState::new();
        let water = water::Water::load();
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
            pomodoro: pomodoro::Pomodoro::new(),
            todo,
            todo_editing: false,
            todo_input: String::new(),
            shelf,
            tray_page: 0,
            lan: None,
            dragging_out: false,
            drag_hovering: false,
            dropped_files,
            drag_arm: None,
            swallow_next_click: false,
            removebg,
            removebg_pending: Arc::new(Mutex::new(None)),
            removebg_drag_arm: None,
            convert,
            convert_pending: Arc::new(Mutex::new(None)),
            convert_drag_arm: None,
            eye_openness: [1.0, 1.0],
            eye_openness_target: [1.0, 1.0],
            eye_slant: [0.0, 0.0],
            eye_slant_target: [0.0, 0.0],
            next_eye_expr_at: now + Duration::from_millis(2500),
            eye_expr_until: now,
            current_eye_expr: EyeExpression::Normal,
            master_volume: vol,
            last_non_zero_volume: if vol > 0.05 { vol } else { 0.5 },
            volume_bar_hover: false,
            volume_bar_zoom: 0.0,
            volume_bar_zoom_target: 0.0,
            volume_dragging: false,
            music_button_zooms: [1.0, 1.0, 1.0, 1.0],
            music_button_zoom_targets: [1.0, 1.0, 1.0, 1.0],
            music_bar_hover: false,
            music_seek_pos: None,
            music_progress_dragging: false,
            water,
            last_water_reminder: now,
            last_section: IslandState::Expanded,
            last_scroll_switch: now,
            power_state,
            low_bat_warned: false,
            crit_bat_warned: false,
            caps_lock,
            active_alert: None,
            hover_bulging: false,
            hover_expand_timer_active: false,
            card_expanded: false,
            todo_scroll: 0.0,
            todo_scroll_target: 0.0,
            todo_editing_idx: None,
            tray_scroll: 0.0,
            tray_scroll_target: 0.0,
            transition_active: false,
            transition_start: now,
            transition_from: IslandState::Compact,
            transition_to: IslandState::Expanded,
            transition_dir: 1.0,
            dot_spring: animator::FluidDotIndicator::new(0),
            wheel_accum: 0.0,
            hovered_dot: None,
            music_bar_zoom: 0.0,
            music_bar_zoom_target: 0.0,
            tray_genie_start: None,
            trim_counter: 0,
            mouse_pt: None,
            tray_btn_hovers: [0.0; 4],
            tray_btn_hover_targets: [0.0; 4],
            rb_btn_hovers: [0.0; 2],
            rb_btn_hover_targets: [0.0; 2],
            cv_btn_hovers: [0.0; 3],
            cv_btn_hover_targets: [0.0; 3],
            settings: crate::settings::AuraSettings::load(),
            pdf_md_drag: None,
            hinstance,
            canvas_class,
            extra_canvases: Vec::new(),
            main_base: (0, 0),
            pill_y_offset: 0.0,
            last_offset_applied: f32::NAN,
            hide_fs: false,
            dodge_since: None,
            dodging: false,
            pill_covered: false,
            cover_poll_at: now,
            volume_hover_since: None,
            pill_hovered: false,
            preview_close_at: None,
            manual_hold: false,
            was_hide_fs: false,
        });
        app.water.set_goal(app.settings.water_daily_goal as i64);
        app.pomodoro.focus_mins = app.settings.pomodoro_focus_mins;
        app.removebg.model = [
            removebg::RemoveBgModel::Fast,
            removebg::RemoveBgModel::Balanced,
            removebg::RemoveBgModel::Quality,
        ][app.settings.removebg_model_idx.min(2)];
        app.convert.photo_format = [
            crate::convert::ConvertFormat::Avif,
            crate::convert::ConvertFormat::Webp,
            crate::convert::ConvertFormat::Png,
        ][app.settings.convert_format_idx.min(2)];
        let (bx, by, _, _) = window::place_canvas_on_monitor(hwnd, app.settings.pill_monitor);
        app.main_base = (bx, by);
        app.rebuild_canvases();
        app.on_paint();
        trim_working_set();
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

    /// Compute the exact target logical size `(width, height)` for `state`.
    /// Compact/Zen are minimalist (146x26 / 100x26), with smooth hover-bulge (1.08x).
    /// All expanded sections share the exact unified size (520x180), and can expand via button.
    fn target_size_for(&self, state: IslandState) -> (f32, f32) {
        let (w, h) = self.target_size_for_inner(state);
        let scale = self.settings.notch_scale.clamp(0.8, 1.5);
        (w * scale, h * scale)
    }

    fn target_size_for_inner(&self, state: IslandState) -> (f32, f32) {
        if !state.is_expanded() {
            if self.current_alert().is_some() {
                return (320.0, 26.0);
            }
            if state == IslandState::Zen {
                let (w, h) = (104.0, 28.0);
                if self.hover_bulging {
                    return (w * 1.08, h * 1.08);
                }
                return (w, h);
            }
            // Compact state:
            if self.pomodoro.is_active(chrono::Local::now()) {
                let (w, h) = (160.0, 30.0);
                if self.hover_bulging {
                    return (w * 1.08, h * 1.08);
                }
                return (w, h);
            }
            if self.lyrics_mode && self.media.is_some() {
                let (w, h) = (320.0, 30.0);
                if self.hover_bulging {
                    return (w * 1.05, h * 1.05);
                }
                return (w, h);
            }
            let (w, h) = (180.0f32, 30.0f32);
            if self.hover_bulging {
                return ((w * 1.08).round(), (h * 1.08).round());
            }
            return (w, h);
        }
        let (w, h) = state.logical_size();
        (w, h)
    }

    fn animate_to_size(&mut self, to_w: f32, to_h: f32) {
        if self.spring.set_target(to_w, to_h, animator::SpringPreset::Morph) {
            self.ensure_anim_timer();
        }
        self.on_paint();
    }

    fn anim_interval_ms(&self) -> u32 {
        if self.settings.high_refresh_120fps {
            8
        } else {
            16
        }
    }

    fn ensure_anim_timer(&mut self) {
        if !self.anim_timer_running {
            self.anim_timer_running = true;
            let ms = self.anim_interval_ms();
            unsafe {
                let _ = SetTimer(self.hwnd, TIMER_ANIM, ms, None);
            }
        }
    }

    fn refresh_anim_timer(&mut self) {
        if self.anim_timer_running {
            let ms = self.anim_interval_ms();
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_ANIM);
                let _ = SetTimer(self.hwnd, TIMER_ANIM, ms, None);
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
            if let Some(idx) = self.todo_editing_idx.take() {
                if let Some(task) = self.todo.tasks.get_mut(idx) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        task.title = trimmed.to_string();
                        self.todo.save();
                    }
                }
            } else {
                self.todo.add(&text);
            }
        } else {
            self.todo_input.clear();
            self.todo_editing_idx = None;
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
        self.todo_editing_idx = None;
        self.todo_input.clear();
        unsafe {
            let ex = GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE_IDX);
            let _ = SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE_IDX, ex & !NOACTIVATE_BIT);
            let _ = SetForegroundWindow(self.hwnd);
        }
        self.on_paint();
    }

    fn begin_todo_edit_idx(&mut self, idx: usize) {
        if idx >= self.todo.tasks.len() {
            return;
        }
        self.todo_editing = true;
        self.todo_editing_idx = Some(idx);
        self.todo_input = self.todo.tasks[idx].title.clone();
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
                self.end_todo_editing(true);
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

        if old_state == IslandState::Lyrics || new_state == IslandState::Lyrics || new_state == IslandState::Expanded {
            self.renderer.cancel_lyric_roll();
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
        self.update_dot_indicator_target();
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

    fn update_dot_indicator_target(&mut self) {
        let active = self.pill.state.section_index();
        self.dot_spring.set_target_slot(active);
    }

    pub fn is_animating(&mut self) -> bool {
        self.spring.active
            || self.transition_active
            || self.tray_bounce.is_active()
            || self.renderer.is_lyric_rolling()
            || (self.todo_scroll - self.todo_scroll_target).abs() > 0.5
            || (self.tray_scroll - self.tray_scroll_target).abs() > 0.5
            || self.music_button_zooms.iter().zip(&self.music_button_zoom_targets).any(|(a, b)| (a - b).abs() > 0.005)
            || (self.music_bar_zoom - self.music_bar_zoom_target).abs() > 0.005
            || (self.volume_bar_zoom - self.volume_bar_zoom_target).abs() > 0.005
            || (self.eye_openness[0] - self.eye_openness_target[0]).abs() > 0.01
            || (self.eye_openness[1] - self.eye_openness_target[1]).abs() > 0.01
            || (self.eye_slant[0] - self.eye_slant_target[0]).abs() > 0.01
            || (self.eye_slant[1] - self.eye_slant_target[1]).abs() > 0.01
            || self.tray_genie_start.is_some()
            || self.dot_spring.active
            || (self.pill_y_offset - self.pill_offset_target()).abs() > 0.5
    }

    /// Target vertical offset (logical px, negative = up) for peek/hide behaviors.
    fn pill_offset_target(&self) -> f32 {
        let h = self.pill.height.max(1.0);
        let idle = !self.pill.state.is_expanded() && self.current_alert().is_none();
        // Peek: the pill is drawn at the top of the canvas (y=0..h), so slide
        // the window up by (h-5), leaving a 5px tip visible. Never hide fully.
        let peek = 5.0 - h;
        // Once opened (not idle), never hide until the user closes it.
        // If the user manually clicked to open, hold it visible (no auto-hide).
        // Hovering the pill (even the peek tip) reveals it so hover-open works.
        if !idle || self.manual_hold || self.pill_hovered {
            return 0.0;
        }
        let hide = self.hide_fs
            || (self.settings.hide_when_covered && self.pill_covered)
            || self.dodging;
        if hide || self.settings.minimal_notch {
            peek
        } else {
            0.0
        }
    }

    /// Smoothly move every canvas toward the offset target.
    fn apply_pill_offset(&mut self) {
        let target = self.pill_offset_target();
        let diff = target - self.pill_y_offset;
        if diff.abs() < 0.5 {
            self.pill_y_offset = target;
        } else {
            self.pill_y_offset += diff * 0.22;
        }
        if (self.pill_y_offset - self.last_offset_applied).abs() < 0.01 {
            return;
        }
        self.last_offset_applied = self.pill_y_offset;
        let off = self.pill_y_offset;
        unsafe {
            let apply_one = |hwnd: HWND, bx: i32, by: i32| {
                let dpi = window::dpi_scale(hwnd);
                let y = (by as f32 + off * dpi).round() as i32;
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    bx,
                    y,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            };
            apply_one(self.hwnd, self.main_base.0, self.main_base.1);
            for c in &self.extra_canvases {
                apply_one(c.hwnd, c.base_x, c.base_y);
            }
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
        let mut zoom_anim = false;
        for i in 0..4 {
            let diff = self.music_button_zoom_targets[i] - self.music_button_zooms[i];
            if diff.abs() > 0.005 {
                self.music_button_zooms[i] += diff * 0.35;
                zoom_anim = true;
            } else {
                self.music_button_zooms[i] = self.music_button_zoom_targets[i];
            }
        }
        let bdiff = self.music_bar_zoom_target - self.music_bar_zoom;
        if bdiff.abs() > 0.005 {
            self.music_bar_zoom += bdiff * 0.30;
            zoom_anim = true;
        } else {
            self.music_bar_zoom = self.music_bar_zoom_target;
        }
        let vdiff = self.volume_bar_zoom_target - self.volume_bar_zoom;
        if vdiff.abs() > 0.005 {
            self.volume_bar_zoom += vdiff * 0.30;
            zoom_anim = true;
        } else {
            self.volume_bar_zoom = self.volume_bar_zoom_target;
        }
        // Volume slider appears only after a short hover delay (it used to pop up instantly).
        if self.volume_bar_hover && self.volume_bar_zoom_target < 1.0 {
            if let Some(since) = self.volume_hover_since {
                if since.elapsed().as_millis() >= 350 {
                    self.volume_bar_zoom_target = 1.0;
                }
            }
            zoom_anim = true; // keep ticking through the delay
        }
        // Peek / hide behaviors (minimal notch, dodge, covered, fullscreen).
        self.apply_pill_offset();
        if let Some(start) = self.tray_genie_start {
            let elapsed = start.elapsed().as_secs_f32();
            if elapsed >= 0.35 {
                self.shelf.clear();
                self.tray_genie_start = None;
            }
            zoom_anim = true;
        }
        let trans_active = if self.transition_active {
            let elapsed = self.transition_start.elapsed().as_secs_f32();
            if elapsed >= 0.20 {
                self.transition_active = false;
                false
            } else {
                true
            }
        } else {
            false
        };
        let mut scroll_anim = false;
        let todo_diff = self.todo_scroll_target - self.todo_scroll;
        if todo_diff.abs() > 0.5 {
            self.todo_scroll += todo_diff * 0.25;
            scroll_anim = true;
        } else {
            self.todo_scroll = self.todo_scroll_target;
        }
        let tray_diff = self.tray_scroll_target - self.tray_scroll;
        if tray_diff.abs() > 0.5 {
            self.tray_scroll += tray_diff * 0.25;
            scroll_anim = true;
        } else {
            self.tray_scroll = self.tray_scroll_target;
        }

        let (_, _, dot_anim) = self.dot_spring.step();

        let mut btn_hover_anim = false;
        for i in 0..4 {
            let diff = self.tray_btn_hover_targets[i] - self.tray_btn_hovers[i];
            if diff.abs() > 0.005 {
                self.tray_btn_hovers[i] += diff * 0.28;
                btn_hover_anim = true;
            } else {
                self.tray_btn_hovers[i] = self.tray_btn_hover_targets[i];
            }
        }
        for i in 0..2 {
            let diff = self.rb_btn_hover_targets[i] - self.rb_btn_hovers[i];
            if diff.abs() > 0.005 {
                self.rb_btn_hovers[i] += diff * 0.28;
                btn_hover_anim = true;
            } else {
                self.rb_btn_hovers[i] = self.rb_btn_hover_targets[i];
            }
        }
        for i in 0..3 {
            let diff = self.cv_btn_hover_targets[i] - self.cv_btn_hovers[i];
            if diff.abs() > 0.005 {
                self.cv_btn_hovers[i] += diff * 0.28;
                btn_hover_anim = true;
            } else {
                self.cv_btn_hovers[i] = self.cv_btn_hover_targets[i];
            }
        }

        let mut eye_anim = false;
        if self.pill.state == IslandState::Zen {
            for i in 0..2 {
                let odiff = self.eye_openness_target[i] - self.eye_openness[i];
                if odiff.abs() > 0.005 {
                    self.eye_openness[i] += odiff * 0.35;
                    eye_anim = true;
                } else {
                    self.eye_openness[i] = self.eye_openness_target[i];
                }
                let sdiff = self.eye_slant_target[i] - self.eye_slant[i];
                if sdiff.abs() > 0.005 {
                    self.eye_slant[i] += sdiff * 0.30;
                    eye_anim = true;
                } else {
                    self.eye_slant[i] = self.eye_slant_target[i];
                }
            }
        }

        if was_active || bounce_active || rolling || zoom_anim || trans_active || scroll_anim || dot_anim || btn_hover_anim || eye_anim {
            self.on_paint();
        }
        if !still_active && !self.tray_bounce.is_active() && !self.renderer.is_lyric_rolling() && !zoom_anim && !trans_active && !scroll_anim && !dot_anim && !btn_hover_anim && !eye_anim {
            self.stop_anim_timer();
        }
    }

    /// Update eye cursor tracking & expressions (`AppDelegate.eyeTimer` + `IslandView.drawEyes`).
    fn update_eyes(&mut self) {
        let now = Instant::now();
        if self.pill.state == IslandState::Zen {
            if self.current_eye_expr == EyeExpression::Normal {
                if now >= self.next_eye_expr_at {
                    let r = (now.elapsed().as_millis() % 100) as u32;
                    if r < 45 {
                        self.current_eye_expr = EyeExpression::BlinkBoth;
                        self.eye_openness_target = [0.06, 0.06];
                        self.eye_slant_target = [0.0, 0.0];
                        self.eye_expr_until = now + Duration::from_millis(160);
                    } else if r < 65 {
                        self.current_eye_expr = EyeExpression::WinkLeft;
                        self.eye_openness_target = [0.06, 1.0];
                        self.eye_slant_target = [0.0, 0.0];
                        self.eye_expr_until = now + Duration::from_millis(400);
                    } else if r < 85 {
                        self.current_eye_expr = EyeExpression::WinkRight;
                        self.eye_openness_target = [1.0, 0.06];
                        self.eye_slant_target = [0.0, 0.0];
                        self.eye_expr_until = now + Duration::from_millis(400);
                    } else {
                        self.current_eye_expr = EyeExpression::Inquisitive;
                        self.eye_openness_target = [1.0, 0.88];
                        self.eye_slant_target = [-1.6, 1.6];
                        self.eye_expr_until = now + Duration::from_millis(1200);
                    }
                    self.ensure_anim_timer();
                }
            } else if now >= self.eye_expr_until {
                self.current_eye_expr = EyeExpression::Normal;
                self.eye_openness_target = [1.0, 1.0];
                self.eye_slant_target = [0.0, 0.0];
                let next_ms = 2200 + ((now.elapsed().as_millis() % 2800) as u64);
                self.next_eye_expr_at = now + Duration::from_millis(next_ms);
                self.ensure_anim_timer();
            }
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
        if !self.settings.lyrics_sync {
            return;
        }
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
            m.duration_secs,
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
            if self.settings.pill_alerts_enabled && self.settings.alert_capslock {
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
        }

        // 2. Power / Battery status
        let pwr = tray::read_power_status();
        let prev = self.power_state;
        self.power_state = pwr;
        let pct_f = pwr.percent.unwrap_or(100) as f32 / 100.0;
        let battery_alerts = self.settings.pill_alerts_enabled && self.settings.alert_battery;

        if battery_alerts && pwr.ac_online != prev.ac_online {
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
        } else if battery_alerts {
            if let Some(pct) = pwr.percent {
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
                // Fullscreen hide is now a smooth slide-up (see pill_offset_target).
                self.hide_fs =
                    self.settings.hide_in_fullscreen && window::foreground_is_fullscreen();

                let idle =
                    !self.pill.state.is_expanded() && self.current_alert().is_none();

                // Dodge: hide when the cursor stays inside the radius around the pill.
                // Measured against the pill's home position so it cannot oscillate.
                if self.settings.dodge_enabled && idle && !self.pill_hovered {
                    let mut pt = POINT::default();
                    let mut dodge_now = false;
                    if unsafe { GetCursorPos(&mut pt).is_ok() } {
                        let dpi = window::dpi_scale(self.hwnd);
                        let w = self.pill.width * dpi;
                        let h = self.pill.height * dpi;
                        let r = self.settings.dodge_radius;
                        // Check the pill rect on every canvas (multi-monitor).
                        let mut bases: Vec<(i32, i32)> = vec![self.main_base];
                        for c in &self.extra_canvases {
                            bases.push((c.base_x, c.base_y));
                        }
                        let mut min_dist = f32::MAX;
                        for (bx, by) in bases {
                            let cx = bx as f32 + (window::CANVAS_W * dpi - w) / 2.0;
                            let cy = by as f32 + (window::CANVAS_H * dpi - h) / 2.0;
                            let dx = (pt.x as f32 - (cx + w / 2.0)).abs() - w / 2.0;
                            let dy = (pt.y as f32 - (cy + h / 2.0)).abs() - h / 2.0;
                            let dist = dx.max(0.0).hypot(dy.max(0.0));
                            if dist < min_dist {
                                min_dist = dist;
                            }
                        }
                        if min_dist < r {
                            if self.dodge_since.is_none() {
                                self.dodge_since = Some(Instant::now());
                            }
                            if self.dodge_since.unwrap().elapsed().as_millis() as u64
                                >= self.settings.dodge_delay_ms
                            {
                                dodge_now = true;
                            }
                        } else if min_dist > r + 30.0 {
                            self.dodge_since = None;
                        }
                    }
                    self.dodging = dodge_now;
                } else {
                    self.dodging = false;
                    self.dodge_since = None;
                }

                // Covered poll (every 500ms): hide when a window is behind the pill.
                let now = Instant::now();
                if now.duration_since(self.cover_poll_at).as_millis() >= 500 {
                    self.cover_poll_at = now;
                    if self.settings.hide_when_covered {
                        let dpi = window::dpi_scale(self.hwnd);
                        let w = (self.pill.width * dpi).round() as i32;
                        let h = (self.pill.height * dpi).round() as i32;
                        let cw = (window::CANVAS_W * dpi).round() as i32;
                        let ch = (window::CANVAS_H * dpi).round() as i32;
                        let pill_rect = RECT {
                            left: self.main_base.0 + (cw - w) / 2,
                            top: self.main_base.1 + (ch - h) / 2,
                            right: self.main_base.0 + (cw + w) / 2,
                            bottom: self.main_base.1 + (ch + h) / 2,
                        };
                        let mut exclude = vec![self.hwnd];
                        for c in &self.extra_canvases {
                            exclude.push(c.hwnd);
                        }
                        if let Some(sh) = crate::settings_window::settings_hwnd() {
                            exclude.push(sh);
                        }
                        self.pill_covered =
                            window::is_rect_covered_by_window(pill_rect, &exclude);
                    } else {
                        self.pill_covered = false;
                    }
                }
                // If dodge / covered / fullscreen changed the slide target while
                // the animation timer was stopped, restart it so the slide is smooth.
                if (self.pill_y_offset - self.pill_offset_target()).abs() > 0.5 {
                    self.ensure_anim_timer();
                }
                // Release the manual hold only on fullscreen EXIT transition
                // (was fullscreen, now not). Don't clear when hide_fs is simply
                // false (e.g. hide_when_covered without fullscreen).
                if self.was_hide_fs && !self.hide_fs && self.manual_hold {
                    self.manual_hold = false;
                }
                self.was_hide_fs = self.hide_fs;

                self.update_eyes();
                self.refresh_media_from_cache(false);
                if self.pomodoro.check_finished(chrono::Local::now()) {
                    if self.settings.sound_chimes {
                        tray::play_chime();
                    }
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
                self.trim_counter = self.trim_counter.wrapping_add(1);
                if self.trim_counter % 20 == 0 {
                    trim_working_set();
                }
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
            TIMER_HOVER_EXPAND => {
                unsafe {
                    let _ = KillTimer(self.hwnd, TIMER_HOVER_EXPAND);
                }
                self.hover_expand_timer_active = false;
                self.hover_bulging = false;
                if !self.pill.state.is_expanded() && self.cursor_inside_window() {
                    let target = self.last_section;
                    self.set_state(target);
                }
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

    fn cursor_inside_pill(&self, x_px: i32, y_px: i32) -> bool {
        let (target_w, target_h) = self.target_size_for(self.pill.state);
        let max_w = self.pill.width.max(target_w);
        let max_h = self.pill.height.max(target_h);
        let scale = window::dpi_scale(self.hwnd);
        let canvas_x = (x_px as f32) / scale;
        let canvas_y = (y_px as f32) / scale;
        let left = (window::CANVAS_W - max_w) * 0.5;
        let right = (window::CANVAS_W + max_w) * 0.5;
        canvas_x >= left - 6.0 && canvas_x <= right + 6.0 && canvas_y >= 0.0 && canvas_y <= max_h + 6.0
    }

    fn cursor_inside_window(&self) -> bool {
        let mut pt = POINT::default();
        let mut rect = RECT::default();
        if unsafe { !GetCursorPos(&mut pt).is_ok() || !GetWindowRect(self.hwnd, &mut rect).is_ok() } {
            return false;
        }
        let scale = window::dpi_scale(self.hwnd);
        let (target_w, target_h) = self.target_size_for(self.pill.state);
        let active_w = if self.pill.state.is_expanded() {
            self.pill.width.max(target_w)
        } else {
            self.pill.width.max(160.0)
        };
        let active_h = if self.pill.state.is_expanded() {
            self.pill.height.max(target_h)
        } else {
            self.pill.height.max(30.0)
        };
        let offset_x_px = (((window::CANVAS_W - active_w) * 0.5) * scale).round() as i32;
        let pill_w_px = (active_w * scale).round() as i32;
        let pill_h_px = (active_h * scale).round() as i32;
        let pill_left = rect.left + offset_x_px - 6;
        let pill_right = rect.left + offset_x_px + pill_w_px + 6;
        let pill_top = rect.top;
        let pill_bottom = rect.top + pill_h_px + 6;
        pt.x >= pill_left && pt.x <= pill_right && pt.y >= pill_top && pt.y <= pill_bottom
    }

    fn apply_mouse_leave(&mut self) {
        if self.cursor_inside_window() {
            self.arm_mouse_tracking();
            return;
        }
        self.suppress_hover_open_after_click_close = false;
        if self.hover_expand_timer_active {
            self.hover_expand_timer_active = false;
            self.hover_bulging = false;
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_HOVER_EXPAND);
            }
        }
        self.card_expanded = false;
        self.mouse_pt = None;
        self.pill_hovered = false;
        self.music_bar_hover = false;
        self.music_bar_zoom_target = 0.0;
        self.music_button_zoom_targets = [1.0, 1.0, 1.0, 1.0];
        // If the user manually opened the pill, keep it open (don't auto-close).
        // They close it explicitly via click.
        if self.manual_hold {
            return;
        }
        let target = self.closed_target_state();
        self.set_state(target);
        trim_working_set();
    }

    fn on_mouse_move(&mut self, x_px: i32, y_px: i32) {
        self.suppress_leave = false;
        self.pill_hovered = true;
        self.arm_mouse_tracking();

        let (pill_x, pill_y) = self.client_px_to_pill_pt(x_px, y_px);
        self.mouse_pt = Some((pill_x, pill_y));
        let old_hovered_dot = self.hovered_dot;
        self.hovered_dot = if self.pill.state.is_expanded() {
            renderer::dot_index_at(pill_x, pill_y, self.pill.width, self.pill.height)
        } else {
            None
        };
        if old_hovered_dot != self.hovered_dot || self.pdf_md_drag.is_some() {
            self.on_paint();
        }

        let inside = self.cursor_inside_pill(x_px, y_px);
        if !inside {
            if !self.pill.state.is_expanded() {
                // If cursor moved away while wait-bulging, cancel timer and smoothly deflate back:
                if self.hover_expand_timer_active {
                    self.hover_expand_timer_active = false;
                    self.hover_bulging = false;
                    unsafe {
                        let _ = KillTimer(self.hwnd, TIMER_HOVER_EXPAND);
                    }
                    let (to_w, to_h) = self.target_size_for(self.pill.state);
                    self.spring.set_target(to_w, to_h, animator::SpringPreset::Collapse);
                    self.ensure_anim_timer();
                }
                return;
            } else if !self.cursor_inside_window() {
                // Moved outside the expanded card area: schedule close timer
                unsafe {
                    let _ = SetTimer(self.hwnd, TIMER_CLOSE, 150, None);
                }
                return;
            }
        } else {
            // Truly inside the pill: cancel any close timer immediately
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_CLOSE);
            }
        }

        if self.pill.state == IslandState::Zen {
            let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
            let mid_x = self.pill.width * 0.5;
            let mid_y = self.pill.height * 0.5;
            self.eye_look = (((x - mid_x) / 45.0).clamp(-1.0, 1.0), ((y - mid_y) / 25.0).clamp(-1.0, 1.0));
            if self.media.is_none() && !self.pomodoro.is_active(chrono::Local::now()) {
                self.on_paint();
            }
        }
        if self.volume_dragging {
            let (x, _) = self.client_px_to_pill_pt(x_px, y_px);
            let v_left = renderer::MUSIC_VOLUME_BAR_RECT.0;
            let v_w = renderer::MUSIC_VOLUME_BAR_RECT.2 - v_left;
            let level = ((x - v_left) / v_w).clamp(0.0, 1.0);
            self.master_volume = level;
            if level > 0.02 {
                self.last_non_zero_volume = level;
            }
            crate::media::set_master_volume_async(level);
            self.on_paint();
            return;
        }
        if self.music_progress_dragging {
            let (x, _) = self.client_px_to_pill_pt(x_px, y_px);
            let lyrics_t = ((self.pill.width - 540.0) / (700.0 - 540.0)).clamp(0.0, 1.0);
            let bar_w = 340.0 + (164.0 - 340.0) * lyrics_t;
            let bar_left = 98.0f32;
            let bar_right = bar_left + bar_w;
            if let Some(ref m) = self.media {
                if m.duration_secs > 0.0 {
                    let t = ((x - bar_left) / (bar_right - bar_left)).clamp(0.0, 1.0) as f64;
                    self.music_seek_pos = Some(t * m.duration_secs);
                    self.on_paint();
                }
            }
            return;
        }
        if self.pill.state == IslandState::Expanded || self.pill.state == IslandState::Lyrics {
            let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
            let lyrics_t = ((self.pill.width - 540.0) / (700.0 - 540.0)).clamp(0.0, 1.0);
            let bar_w = 324.0 + (164.0 - 324.0) * lyrics_t;
            let bar_hover = x >= 90.0 && x <= (98.0 + bar_w + 4.0) && y >= 98.0 && y <= 122.0;
            if bar_hover != self.music_bar_hover {
                self.music_bar_hover = bar_hover;
                self.music_bar_zoom_target = if bar_hover { 1.0 } else { 0.0 };
                self.ensure_anim_timer();
                self.on_paint();
            }

            let vol_hover = self.settings.master_volume_control
                && x >= renderer::MUSIC_VOLUME_BAR_RECT.0 - 4.0
                && x <= renderer::MUSIC_SPEAKER_RECT.2 + 4.0
                && y >= renderer::MUSIC_VOLUME_BAR_RECT.1 - 4.0
                && y <= renderer::MUSIC_VOLUME_BAR_RECT.3 + 4.0;
            if vol_hover != self.volume_bar_hover {
                self.volume_bar_hover = vol_hover;
                if vol_hover {
                    // Wait a beat before showing the slider (no more instant popup).
                    self.volume_hover_since = Some(Instant::now());
                } else {
                    self.volume_hover_since = None;
                    self.volume_bar_zoom_target = 0.0;
                }
                self.ensure_anim_timer();
                self.on_paint();
            }

            let c0 = renderer::MUSIC_CONTROL_CENTERS[0]
                + (renderer::LYRICS_CONTROL_CENTERS[0] - renderer::MUSIC_CONTROL_CENTERS[0]) * lyrics_t;
            let c1 = renderer::MUSIC_CONTROL_CENTERS[1]
                + (renderer::LYRICS_CONTROL_CENTERS[1] - renderer::MUSIC_CONTROL_CENTERS[1]) * lyrics_t;
            let c2 = renderer::MUSIC_CONTROL_CENTERS[2]
                + (renderer::LYRICS_CONTROL_CENTERS[2] - renderer::MUSIC_CONTROL_CENTERS[2]) * lyrics_t;
            let button_centers = [
                (84.0f32, 140.0f32),
                (c0, 140.0f32),
                (c1, 140.0f32),
                (c2, 140.0f32),
            ];
            let mut changed = false;
            for (i, &(bx, by)) in button_centers.iter().enumerate() {
                let dist_sq = (x - bx) * (x - bx) + (y - by) * (y - by);
                let target = if dist_sq <= 18.0 * 18.0 { 1.2 } else { 1.0 };
                if (self.music_button_zoom_targets[i] - target).abs() > 0.01 {
                    self.music_button_zoom_targets[i] = target;
                    changed = true;
                }
            }
            if changed {
                self.ensure_anim_timer();
            }
        }
        if self.pill.state == IslandState::Tray {
            let rects = renderer::compute_tray_button_rects(&self.tray_btn_hovers);
            let mut changed = false;
            for i in 0..4 {
                let inside = pill_x >= rects[i].0 && pill_x <= rects[i].2 && pill_y >= rects[i].1 && pill_y <= rects[i].3;
                let target = if inside { 1.0 } else { 0.0 };
                if (self.tray_btn_hover_targets[i] - target).abs() > 0.01 {
                    self.tray_btn_hover_targets[i] = target;
                    changed = true;
                }
            }
            if changed {
                self.ensure_anim_timer();
            }
        } else {
            for i in 0..4 { self.tray_btn_hover_targets[i] = 0.0; }
        }

        if self.pill.state == IslandState::RemoveBg {
            let rects = renderer::compute_rb_button_rects(&self.rb_btn_hovers, self.removebg.model.label().len());
            let mut changed = false;
            for i in 0..2 {
                let inside = pill_x >= rects[i].0 && pill_x <= rects[i].2 && pill_y >= rects[i].1 && pill_y <= rects[i].3;
                let target = if inside { 1.0 } else { 0.0 };
                if (self.rb_btn_hover_targets[i] - target).abs() > 0.01 {
                    self.rb_btn_hover_targets[i] = target;
                    changed = true;
                }
            }
            if changed {
                self.ensure_anim_timer();
            }
        } else {
            for i in 0..2 { self.rb_btn_hover_targets[i] = 0.0; }
        }

        if self.pill.state == IslandState::Convert && self.convert.view != crate::convert::ConvertView::AppGrid {
            let has_format_btn = self.convert.active_tool == crate::convert::ConvertTool::Photo;
            let rects = renderer::compute_cv_button_rects(&self.cv_btn_hovers, self.convert.photo_format.display_name().len(), has_format_btn);
            let mut changed = false;
            for i in 0..3 {
                let inside = pill_x >= rects[i].0 && pill_x <= rects[i].2 && pill_y >= rects[i].1 && pill_y <= rects[i].3;
                let target = if inside { 1.0 } else { 0.0 };
                if (self.cv_btn_hover_targets[i] - target).abs() > 0.01 {
                    self.cv_btn_hover_targets[i] = target;
                    changed = true;
                }
            }
            if changed {
                self.ensure_anim_timer();
            }
        } else {
            for i in 0..3 { self.cv_btn_hover_targets[i] = 0.0; }
        }

        if self.pill.state == IslandState::RemoveBg || self.pill.state == IslandState::Convert {
            self.on_paint();
        }
        if self.suppress_hover_open_after_click_close {
            return;
        }

        if !self.pill.state.is_expanded() {
            // Nusa-style hover wait-bulge: slightly swell the pill (1.08x) for the
            // configured delay before expanding.
            if !self.hover_expand_timer_active {
                self.hover_expand_timer_active = true;
                self.hover_bulging = true;
                let delay = self.settings.hover_open_delay_ms.max(50) as u32;
                unsafe {
                    let _ = SetTimer(self.hwnd, TIMER_HOVER_EXPAND, delay, None);
                }
                let (to_w, to_h) = self.target_size_for(self.pill.state);
                self.spring.set_target(to_w, to_h, animator::SpringPreset::Bulge);
                self.ensure_anim_timer();
            }
        }
    }

    fn on_mouse_leave(&mut self) {
        self.tracking_mouse = false;
        self.pill_hovered = false;
        self.hovered_dot = None;
        self.mouse_pt = None;
        if self.hover_expand_timer_active {
            self.hover_expand_timer_active = false;
            self.hover_bulging = false;
            unsafe {
                let _ = KillTimer(self.hwnd, TIMER_HOVER_EXPAND);
            }
            let (to_w, to_h) = self.target_size_for(self.pill.state);
            self.spring.set_target(to_w, to_h, animator::SpringPreset::Collapse);
            self.ensure_anim_timer();
        }
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

    fn is_section_enabled(&self, state: IslandState) -> bool {
        match state {
            IslandState::Expanded | IslandState::Lyrics => self.settings.music_enabled,
            IslandState::Pomodoro => self.settings.pomodoro_enabled,
            IslandState::Todo => self.settings.todo_enabled,
            IslandState::Water => self.settings.water_enabled,
            IslandState::Tray => self.settings.tray_enabled,
            IslandState::RemoveBg => self.settings.removebg_enabled,
            IslandState::Convert => self.settings.convert_enabled,
            IslandState::Zen | IslandState::Compact | IslandState::Settings => true,
        }
    }

    fn switch_section_with_transition(&mut self, target: IslandState, dir: f32) {
        if target == self.pill.state {
            return;
        }
        self.transition_from = self.pill.state;
        self.transition_to = target;
        self.transition_dir = dir;
        self.transition_start = Instant::now();
        self.transition_active = true;
        self.set_state(target);
        self.ensure_anim_timer();
    }

    /// Vertical mouse-wheel scrolling switches between the expanded sections with smooth slide transition,
    /// or scrolls the Todo list and Tray carousel when hovering over them.
    fn on_mouse_wheel(&mut self, wheel_delta: i16) {
        if !self.pill.state.is_expanded() || wheel_delta == 0 {
            return;
        }

        let mut pt = POINT::default();
        unsafe {
            let _ = GetCursorPos(&mut pt);
            let _ = ScreenToClient(self.hwnd, &mut pt);
        }
        let (x, y) = self.client_px_to_pill_pt(pt.x, pt.y);

        // 1. Scroll Todo list when hovering over tasks column:
        if self.pill.state == IslandState::Todo && x >= 150.0 {
            let max_scroll = (self.todo.tasks.len() as f32 * renderer::TODO_ROW_PITCH - 110.0).max(0.0);
            if max_scroll > 0.0 {
                let step = 34.0f32;
                if wheel_delta > 0 && self.todo_scroll_target > 0.5 {
                    self.todo_scroll_target = (self.todo_scroll_target - step).max(0.0);
                    self.ensure_anim_timer();
                    return;
                } else if wheel_delta < 0 && self.todo_scroll_target < max_scroll - 0.5 {
                    self.todo_scroll_target = (self.todo_scroll_target + step).min(max_scroll);
                    self.ensure_anim_timer();
                    return;
                }
            }
            // If the list cannot scroll further or is already at boundary, fall through to section switching!
        }

        // 2. Scroll Tray horizontal carousel when hovering over shelf:
        if self.pill.state == IslandState::Tray && y >= 40.0 {
            let total_w = self.shelf.files.len() as f32 * renderer::TRAY_TILE_PITCH;
            let max_scroll = (total_w - renderer::TRAY_CAROUSEL_W).max(0.0);
            if max_scroll > 0.0 {
                let step = 56.0f32;
                if wheel_delta > 0 && self.tray_scroll_target > 0.5 {
                    self.tray_scroll_target = (self.tray_scroll_target - step).max(0.0);
                    self.ensure_anim_timer();
                    return;
                } else if wheel_delta < 0 && self.tray_scroll_target < max_scroll - 0.5 {
                    self.tray_scroll_target = (self.tray_scroll_target + step).min(max_scroll);
                    self.ensure_anim_timer();
                    return;
                }
            }
            // If the carousel cannot scroll further or is already at boundary, fall through to section switching!
        }

        let now = Instant::now();
        if now.duration_since(self.last_scroll_switch) > Duration::from_millis(300) {
            self.wheel_accum = 0.0;
        }
        self.wheel_accum += wheel_delta as f32;
        let notch_threshold = 90.0f32;
        if self.wheel_accum.abs() >= notch_threshold {
            let steps = (self.wheel_accum / notch_threshold) as isize;
            self.wheel_accum -= steps as f32 * notch_threshold;
            self.last_scroll_switch = now;

            let count = crate::state::SECTION_COUNT as isize;
            let base_idx = if self.transition_active {
                self.transition_to.section_index() as isize
            } else {
                self.pill.state.section_index() as isize
            };

            // wheel_delta > 0 (scroll up) -> previous section (-steps)
            // wheel_delta < 0 (scroll down) -> next section (+steps)
            let delta_sec = -steps;
            let step_dir = if delta_sec > 0 { 1 } else { -1 };
            let mut cand_idx = base_idx;
            for _ in 0..count {
                cand_idx = (cand_idx + step_dir).rem_euclid(count);
                let target = IslandState::from_section_index(cand_idx as usize);
                if self.is_section_enabled(target) {
                    let dir = if step_dir > 0 { 1.0 } else { -1.0 };
                    self.switch_section_with_transition(target, dir);
                    break;
                }
            }
        }
    }

    fn on_mouse_down(&mut self, x_px: i32, y_px: i32) {
        self.drag_arm = None;
        self.removebg_drag_arm = None;
        self.convert_drag_arm = None;
        if self.pill.state == IslandState::Convert {
            let tool = self.convert.active_tool;
            if tool == crate::convert::ConvertTool::PdfToMd {
                let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
                let st = self.convert.tool_state(tool);
                for i in 0..st.queue.len().min(3) {
                    let card_x = renderer::PDF_MD_CARD_START_X + (i as f32) * renderer::PDF_MD_CARD_PITCH;
                    if x >= card_x && x <= card_x + renderer::PDF_MD_CARD_W && y >= renderer::PDF_MD_CARD_Y && y <= renderer::PDF_MD_CARD_Y + renderer::PDF_MD_CARD_H {
                        self.convert.set_queue_index(tool, i);
                        self.pdf_md_drag = Some((i, x, y));
                        self.on_paint();
                        return;
                    }
                }
            }
            let st = self.convert.tool_state(tool);
            if st.result_bytes.is_some() {
                let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
                let r = renderer::CONVERT_LIST_VIEW_RECT;
                if x >= r.0 && x <= r.2 && y >= r.1 && y <= r.3 {
                    self.convert_drag_arm = Some((x_px, y_px));
                }
            }
            return;
        }
        if self.pill.state == IslandState::Expanded || self.pill.state == IslandState::Lyrics {
            let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
            let lyrics_t = ((self.pill.width - 540.0) / (700.0 - 540.0)).clamp(0.0, 1.0);
            let bar_w = 340.0 + (164.0 - 340.0) * lyrics_t;
            let bar_left = 98.0f32;
            let bar_right = bar_left + bar_w;
            if x >= bar_left - 4.0 && x <= bar_right + 4.0 && y >= 102.0 && y <= 118.0 {
                self.music_progress_dragging = true;
                if let Some(ref m) = self.media {
                    if m.duration_secs > 0.0 {
                        let t = ((x - bar_left) / (bar_right - bar_left)).clamp(0.0, 1.0) as f64;
                        self.music_seek_pos = Some(t * m.duration_secs);
                        self.on_paint();
                    }
                }
                return;
            }

            // Volume speaker click (mute toggle):
            if self.settings.master_volume_control
                && x >= renderer::MUSIC_SPEAKER_RECT.0 - 4.0 && x <= renderer::MUSIC_SPEAKER_RECT.2 + 4.0
                && y >= renderer::MUSIC_SPEAKER_RECT.1 - 4.0 && y <= renderer::MUSIC_SPEAKER_RECT.3 + 4.0 {
                if self.master_volume > 0.02 {
                    self.last_non_zero_volume = self.master_volume;
                    self.master_volume = 0.0;
                } else {
                    self.master_volume = self.last_non_zero_volume.max(0.2);
                }
                crate::media::set_master_volume_async(self.master_volume);
                self.on_paint();
                return;
            }
            // Volume bar click & drag:
            if self.settings.master_volume_control
                && self.volume_bar_zoom > 0.15
                && x >= renderer::MUSIC_VOLUME_BAR_RECT.0 - 4.0
                && x <= renderer::MUSIC_VOLUME_BAR_RECT.2 + 4.0
                && y >= renderer::MUSIC_VOLUME_BAR_RECT.1 - 4.0
                && y <= renderer::MUSIC_VOLUME_BAR_RECT.3 + 4.0
            {
                self.volume_dragging = true;
                let v_left = renderer::MUSIC_VOLUME_BAR_RECT.0;
                let v_w = renderer::MUSIC_VOLUME_BAR_RECT.2 - v_left;
                let level = ((x - v_left) / v_w).clamp(0.0, 1.0);
                self.master_volume = level;
                if level > 0.02 {
                    self.last_non_zero_volume = level;
                }
                crate::media::set_master_volume_async(level);
                self.on_paint();
                return;
            }
        }
        if self.pill.state == IslandState::RemoveBg {
            // Arm drag-out from the result panel (mirror of tray tile arming).
            if self.removebg.result_bytes.is_some() {
                let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
                let r = renderer::RB_RIGHT_PREVIEW_RECT;
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
        if x < renderer::TRAY_CAROUSEL_X || x > renderer::TRAY_CAROUSEL_X + renderer::TRAY_CAROUSEL_W || y < renderer::TRAY_CAROUSEL_Y || y > renderer::TRAY_CAROUSEL_Y + renderer::TRAY_CAROUSEL_H {
            return;
        }
        for (i, path) in self.shelf.files.iter().enumerate() {
            let left = renderer::TRAY_CAROUSEL_X + (i as f32) * renderer::TRAY_TILE_PITCH - self.tray_scroll;
            let top = renderer::TRAY_CAROUSEL_Y;
            let tw = renderer::TRAY_TILE_W;
            let th = renderer::TRAY_TILE_H;
            if left + tw < renderer::TRAY_CAROUSEL_X || left > renderer::TRAY_CAROUSEL_X + renderer::TRAY_CAROUSEL_W {
                continue;
            }
            // Don't arm file drag when clicking the delete "×" button at top-right:
            if x >= left + tw - 24.0 && x <= left + tw && y >= top && y <= top + 24.0 {
                return;
            }
            if x >= left && x <= left + tw && y >= top && y <= top + th {
                if !self.shelf.select_mode && std::path::Path::new(path).exists() {
                    self.drag_arm = Some((i, x_px, y_px));
                }
                return;
            }
        }
    }

    fn on_mouse_move_drag(&mut self, x_px: i32, y_px: i32) {
        if let Some((down_x, down_y)) = self.convert_drag_arm {
            if self.pill.state != IslandState::Convert {
                self.convert_drag_arm = None;
                return;
            }
            let (threshold_x, threshold_y) =
                unsafe { (GetSystemMetrics(SM_CXDRAG), GetSystemMetrics(SM_CYDRAG)) };
            if (x_px - down_x).abs() < threshold_x && (y_px - down_y).abs() < threshold_y {
                return;
            }
            self.convert_drag_arm = None;
            let tool = self.convert.active_tool;
            if let Some(path) = self.convert.ensure_saved(tool) {
                self.dragging_out = true;
                let _dropped = drag::do_drag_file(&path);
                self.dragging_out = false;
                self.swallow_next_click = true;
                self.on_paint();
            }
            return;
        }
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
        if self.shelf.select_mode {
            self.drag_arm = None;
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
        self.manual_hold = false;
        let target = self.closed_target_state();
        self.set_state(target);
        trim_working_set();
    }

    fn on_click(&mut self, x_px: i32, y_px: i32) {
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_CLOSE);
        }
        if self.volume_dragging {
            self.volume_dragging = false;
            self.on_paint();
            return;
        }
        if self.music_progress_dragging {
            self.music_progress_dragging = false;
            if let Some(pos) = self.music_seek_pos.take() {
                crate::media::seek_async(pos);
            }
            self.on_paint();
            return;
        }
        if self.swallow_next_click {
            self.swallow_next_click = false;
            return;
        }

        // Closed pill click -> expand to `last_section` (or `Pomodoro` if active timer pill clicked):
        if !self.pill.state.is_expanded() {
            self.suppress_hover_open_after_click_close = false;
            // Manual open: hold the pill visible, don't auto-hide/peek afterwards.
            self.manual_hold = true;
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

        if let Some((card_idx, _sx, _sy)) = self.pdf_md_drag.take() {
            let in_portal = x >= renderer::PDF_MD_PORTAL_RECT.0 - 40.0
                && x <= renderer::PDF_MD_PORTAL_RECT.2 + 20.0
                && y >= renderer::PDF_MD_PORTAL_RECT.1 - 20.0
                && y <= renderer::PDF_MD_PORTAL_RECT.3 + 20.0;
            if in_portal || x >= 340.0 {
                self.convert.set_queue_index(crate::convert::ConvertTool::PdfToMd, card_idx);
                self.start_convert_run(crate::convert::ConvertTool::PdfToMd);
            }
            self.on_paint();
            return;
        }

        // 1. Bottom interactive section dots:
        if let Some(dot_idx) = renderer::dot_index_at(x, y, self.pill.width, self.pill.height) {
            let target = IslandState::from_section_index(dot_idx);
            if target != self.pill.state
                && !(target == IslandState::Expanded && self.pill.state == IslandState::Lyrics)
            {
                let cur_idx = self.pill.state.section_index();
                let dir = if dot_idx > cur_idx { 1.0 } else { -1.0 };
                self.switch_section_with_transition(target, dir);
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
                    let x_520 = canvas_x - (window::CANVAS_W - 540.0) * 0.5;
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

                    if self.media.is_some() && (in_bubble_at(x) || repeat_bubble || morphing_bubble) {
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

                    if self.media.is_some() {
                        let lyrics_t = ((self.pill.width - 540.0) / (700.0 - 540.0)).clamp(0.0, 1.0);
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
                }
                if !self.spring.active
                    && !in_rect(renderer::MUSIC_SPEAKER_RECT)
                    && !in_rect(renderer::MUSIC_VOLUME_BAR_RECT)
                {
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
                if in_rect(renderer::TODO_ADD_PILL_RECT) && !self.todo_editing {
                    self.begin_todo_editing();
                    return;
                }
                if self.todo_editing {
                    if !in_rect(renderer::TODO_INPUT_RECT) {
                        self.end_todo_editing(true);
                    }
                    return;
                }
                for (row_i, _) in self.todo.tasks.iter().enumerate() {
                    let row_top = renderer::TODO_ROW_Y0 + (row_i as f32) * renderer::TODO_ROW_PITCH - self.todo_scroll;
                    let row_bot = row_top + renderer::TODO_ROW_H;
                    if row_bot < 40.0 || row_top > 160.0 {
                        continue;
                    }
                    if y >= row_top && y <= row_bot {
                        if x >= renderer::TODO_ROW_DELETE_LEFT && x <= renderer::TODO_ROW_DELETE_RIGHT {
                            self.todo.remove(row_i);
                            self.on_paint();
                            return;
                        }
                        if x >= renderer::TODO_ROW_EDIT_LEFT && x <= renderer::TODO_ROW_EDIT_RIGHT {
                            self.begin_todo_edit_idx(row_i);
                            return;
                        }
                        if x >= renderer::TODO_ROW_X && x <= renderer::TODO_ROW_TOGGLE_RIGHT {
                            let was_done = self.todo.tasks.get(row_i).map(|t| t.done).unwrap_or(true);
                            self.todo.toggle(row_i);
                            let now_done = self.todo.tasks.get(row_i).map(|t| t.done).unwrap_or(false);
                            if !was_done && now_done && self.settings.sound_on_complete {
                                tray::play_chime();
                            }
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
                let rects = renderer::compute_tray_button_rects(&self.tray_btn_hovers);
                if !self.shelf.files.is_empty() && in_rect(rects[0]) {
                    if self.tray_genie_start.is_none() {
                        self.tray_genie_start = Some(Instant::now());
                        self.ensure_anim_timer();
                    }
                    return;
                }
                if !self.shelf.files.is_empty() && in_rect(rects[1]) {
                    self.shelf.select_mode = !self.shelf.select_mode;
                    if !self.shelf.select_mode {
                        self.shelf.clear_selection();
                    }
                    self.on_paint();
                    return;
                }
                if !self.shelf.files.is_empty() && in_rect(rects[2]) {
                    let files = self.shelf.get_selected_or_all();
                    self.removebg.add_to_queue(&files);
                    self.set_state(IslandState::RemoveBg);
                    self.on_paint();
                    return;
                }
                if !self.shelf.files.is_empty() && in_rect(rects[3]) {
                    let files = self.shelf.get_selected_or_all();
                    self.convert.add_to_queue(crate::convert::ConvertTool::Photo, &files);
                    self.convert.add_to_queue(crate::convert::ConvertTool::MergePdf, &files);
                    self.convert.add_to_queue(crate::convert::ConvertTool::PdfToMd, &files);
                    self.set_state(IslandState::Convert);
                    self.on_paint();
                    return;
                }
                for (i, _) in self.shelf.files.iter().enumerate() {
                    let left = renderer::TRAY_CAROUSEL_X + (i as f32) * renderer::TRAY_TILE_PITCH - self.tray_scroll;
                    let top = renderer::TRAY_CAROUSEL_Y;
                    let tw = renderer::TRAY_TILE_W;
                    let th = renderer::TRAY_TILE_H;
                    if left + tw < renderer::TRAY_CAROUSEL_X || left > renderer::TRAY_CAROUSEL_X + renderer::TRAY_CAROUSEL_W {
                        continue;
                    }
                    if self.shelf.select_mode {
                        if x >= left && x <= left + tw && y >= top && y <= top + th {
                            self.shelf.toggle_select(i);
                            self.on_paint();
                            return;
                        }
                    } else {
                        // Top-right delete button inside tile:
                        if x >= left + tw - 24.0 && x <= left + tw && y >= top && y <= top + 24.0 {
                            self.shelf.remove(i);
                            self.on_paint();
                            return;
                        }
                        if x >= left && x <= left + tw && y >= top && y <= top + th {
                            self.show_tile_menu(i);
                            return;
                        }
                    }
                }
                if self.shelf.files.is_empty() && in_rect((28.0, 44.0, 492.0, 150.0)) {
                    if let Some(path) = Self::pick_image_file(self.hwnd) {
                        self.shelf.add(&path);
                        self.on_paint();
                    }
                    return;
                }
                self.close_by_click();
            }
            IslandState::Convert => {
                if self.convert.view == crate::convert::ConvertView::AppGrid {
                    for (i, &t) in [
                        crate::convert::ConvertTool::Photo,
                        crate::convert::ConvertTool::MergePdf,
                        crate::convert::ConvertTool::PdfToMd,
                    ].iter().enumerate() {
                        let card_x = renderer::CONVERT_APP_START_X + (i as f32) * renderer::CONVERT_APP_PITCH;
                        let card_y = renderer::CONVERT_APP_Y;
                        let card_w = renderer::CONVERT_APP_CARD_W;
                        let card_h = renderer::CONVERT_APP_CARD_H;
                        if x >= card_x && x <= card_x + card_w && y >= card_y && y <= card_y + card_h {
                            self.convert.active_tool = t;
                            self.convert.view = crate::convert::ConvertView::ToolDetail;
                            let target_size = self.target_size_for(IslandState::Convert);
                            self.animate_to_size(target_size.0, target_size.1);
                            return;
                        }
                    }
                    self.close_by_click();
                    return;
                }

                // In ToolDetail mode:
                let tool = self.convert.active_tool;
                let has_format_btn = tool == crate::convert::ConvertTool::Photo;
                let cv_rects = renderer::compute_cv_button_rects(&self.cv_btn_hovers, self.convert.photo_format.display_name().len(), has_format_btn);

                // Back button:
                if in_rect(cv_rects[0]) {
                    self.convert.view = crate::convert::ConvertView::AppGrid;
                    let target_size = self.target_size_for(IslandState::Convert);
                    self.animate_to_size(target_size.0, target_size.1);
                    return;
                }

                // Queue Prev/Next buttons:
                let st = self.convert.tool_state(tool);
                if st.queue.len() > 1 && in_rect(renderer::CONVERT_QUEUE_PREV_RECT) {
                    self.convert.prev_in_queue(tool);
                    self.on_paint();
                    return;
                }
                if st.queue.len() > 1 && in_rect(renderer::CONVERT_QUEUE_NEXT_RECT) {
                    self.convert.next_in_queue(tool);
                    self.on_paint();
                    return;
                }

                // Format picker for Photo tool:
                if tool == crate::convert::ConvertTool::Photo && in_rect(cv_rects[1]) {
                    self.show_convert_format_menu();
                    return;
                }

                let st = self.convert.tool_state(tool);

                // Save result button:
                if st.result_bytes.is_some() && in_rect(renderer::CONVERT_SAVE_BTN_RECT) {
                    if let Some(path) = self.convert.ensure_saved(tool) {
                        let name = std::path::Path::new(&path)
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or(path.clone());
                        let cur = self.convert.tool_state_mut(tool);
                        cur.status_text = format!("Saved: {}", name);
                        cur.status_is_error = false;
                    }
                    self.on_paint();
                    return;
                }

                // Reset button:
                if (st.input_preview.is_some() || !st.input_name.is_empty() || st.result_bytes.is_some() || !st.queue.is_empty() || !self.convert.merge_files.is_empty()) && in_rect(cv_rects[2]) {
                    let cur = self.convert.tool_state_mut(tool);
                    cur.queue.clear();
                    cur.queue_idx = 0;
                    cur.input_path = None;
                    cur.input_name.clear();
                    cur.input_preview = None;
                    cur.result_bytes = None;
                    cur.result_preview = None;
                    cur.result_file_name = None;
                    cur.saved_path = None;
                    cur.uploading = false;
                    cur.status_text = tool.default_status().to_string();
                    cur.status_is_error = false;
                    self.convert.merge_files.clear();
                    let target_size = self.target_size_for(IslandState::Convert);
                    self.animate_to_size(target_size.0, target_size.1);
                    return;
                }

                // Manual Process button:
                if !st.uploading && st.result_bytes.is_none() && in_rect(renderer::CONVERT_PROCESS_BTN_RECT) {
                    self.start_convert_run(tool);
                    return;
                }

                // Drop zone / list view / character select clicks:
                if tool == crate::convert::ConvertTool::MergePdf {
                    if in_rect(renderer::MERGE_PDF_ADD_BTN) {
                        let paths = Self::pick_pdf_files(self.hwnd, true);
                        for p in &paths {
                            self.convert.add_merge_file(p);
                        }
                        self.on_paint();
                        return;
                    }

                    if in_rect(renderer::MERGE_PDF_RUN_BTN) {
                        if st.result_bytes.is_some() {
                            if let Some(path) = self.convert.ensure_saved(tool) {
                                let name = std::path::Path::new(&path)
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or(path.clone());
                                let cur = self.convert.tool_state_mut(tool);
                                cur.status_text = format!("Saved: {}", name);
                                cur.status_is_error = false;
                            }
                        } else if self.convert.merge_files.len() >= 2 {
                            self.start_convert_merge();
                        }
                        self.on_paint();
                        return;
                    }

                    // Row delete × and reorder ☰ clicks:
                    let visible_count = self.convert.merge_files.len().min(3);
                    for i in 0..visible_count {
                        let ry = 58.0 + (i as f32) * 27.0;
                        let rx = 54.0f32;
                        let rw = 432.0f32;
                        let del_x = rx + rw - 36.0;
                        if x >= del_x - 6.0 && x <= del_x + 14.0 && y >= ry && y <= ry + 24.0 {
                            self.convert.remove_merge_file(i);
                            self.on_paint();
                            return;
                        }
                        let bar_x = rx + rw - 14.0;
                        if x >= bar_x - 10.0 && x <= bar_x + 10.0 && y >= ry && y <= ry + 24.0 {
                            let len = self.convert.merge_files.len();
                            if len > 1 {
                                self.convert.swap_merge_files(i, (i + 1) % len);
                                self.on_paint();
                                return;
                            }
                        }
                    }

                    if self.convert.merge_files.is_empty() && in_rect((54.0, 58.0, 486.0, 154.0)) {
                        let paths = Self::pick_pdf_files(self.hwnd, true);
                        for p in &paths {
                            self.convert.add_merge_file(p);
                        }
                        self.on_paint();
                        return;
                    }
                } else if tool == crate::convert::ConvertTool::PdfToMd {
                    if in_rect(renderer::PDF_MD_PORTAL_RECT) {
                        if st.result_bytes.is_some() {
                            if let Some(path) = self.convert.ensure_saved(tool) {
                                let name = std::path::Path::new(&path)
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or(path.clone());
                                let cur = self.convert.tool_state_mut(tool);
                                cur.status_text = format!("Saved: {}", name);
                                cur.status_is_error = false;
                            }
                        } else if !st.queue.is_empty() {
                            self.start_convert_run(tool);
                        }
                        self.on_paint();
                        return;
                    }

                    if st.queue.is_empty() && in_rect((54.0, 58.0, 132.0, 156.0)) {
                        let paths = Self::pick_pdf_files(self.hwnd, true);
                        self.convert.add_to_queue(tool, &paths);
                        self.on_paint();
                        return;
                    }

                    for i in 0..st.queue.len().min(3) {
                        let card_x = renderer::PDF_MD_CARD_START_X + (i as f32) * renderer::PDF_MD_CARD_PITCH;
                        if x >= card_x && x <= card_x + renderer::PDF_MD_CARD_W && y >= renderer::PDF_MD_CARD_Y && y <= renderer::PDF_MD_CARD_Y + renderer::PDF_MD_CARD_H {
                            self.convert.set_queue_index(tool, i);
                            self.on_paint();
                            return;
                        }
                    }

                    if st.queue.len() < 3 {
                        let add_i = st.queue.len();
                        let card_x = renderer::PDF_MD_CARD_START_X + (add_i as f32) * renderer::PDF_MD_CARD_PITCH;
                        if x >= card_x && x <= card_x + renderer::PDF_MD_CARD_W && y >= renderer::PDF_MD_CARD_Y && y <= renderer::PDF_MD_CARD_Y + renderer::PDF_MD_CARD_H {
                            let paths = Self::pick_pdf_files(self.hwnd, true);
                            self.convert.add_to_queue(tool, &paths);
                            self.on_paint();
                            return;
                        }
                    }
                } else {
                    let dz_rect = if self.convert.has_active_content() {
                        renderer::CONVERT_DROP_ZONE_RECT
                    } else {
                        (54.0, 58.0, 486.0, 154.0)
                    };
                    if in_rect(dz_rect) {
                        if let Some(path) = Self::pick_image_file(self.hwnd) {
                            self.convert_set_input_only(tool, &path);
                        }
                        return;
                    }
                }

                self.close_by_click();
            }
            IslandState::RemoveBg => {
                if self.removebg.queue.len() > 1 && in_rect(renderer::RB_QUEUE_PREV_RECT) {
                    self.removebg.prev_in_queue();
                    self.on_paint();
                    return;
                }
                if self.removebg.queue.len() > 1 && in_rect(renderer::RB_QUEUE_NEXT_RECT) {
                    self.removebg.next_in_queue();
                    self.on_paint();
                    return;
                }
                let rb_rects = renderer::compute_rb_button_rects(&self.rb_btn_hovers, self.removebg.model.label().len());
                if self.removebg.input_path.is_some() && in_rect(rb_rects[0]) {
                    self.show_removebg_model_menu();
                    return;
                }
                if self.removebg.result_bytes.is_some() && in_rect(renderer::RB_SAVE_BTN_RECT) {
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
                if (self.removebg.input_path.is_some() || self.removebg.result_bytes.is_some() || !self.removebg.queue.is_empty()) && in_rect(rb_rects[1]) {
                    self.removebg.queue.clear();
                    self.removebg.queue_idx = 0;
                    self.removebg.input_path = None;
                    self.removebg.input_name.clear();
                    self.removebg.input_size = 0;
                    self.removebg.input_preview = None;
                    self.removebg.result_bytes = None;
                    self.removebg.result_preview = None;
                    self.removebg.saved_path = None;
                    self.removebg.notice.clear();
                    self.removebg.error = None;
                    self.removebg.uploading = false;
                    let target_size = self.target_size_for(IslandState::RemoveBg);
                    self.animate_to_size(target_size.0, target_size.1);
                    return;
                }
                // Manual process button:
                if self.removebg.input_path.is_some() && self.removebg.result_bytes.is_none() && !self.removebg.uploading && in_rect(renderer::RB_PROCESS_BTN_RECT) {
                    self.removebg_start_run();
                    return;
                }
                // Drop zone / Left preview pick:
                let (has_input, has_result) = (self.removebg.input_path.is_some(), self.removebg.result_bytes.is_some());
                let pick_rect = if !has_input && !has_result {
                    renderer::RB_DROP_ZONE_RECT
                } else {
                    renderer::RB_LEFT_PREVIEW_RECT
                };
                if in_rect(pick_rect) {
                    if let Some(path) = Self::pick_image_file(self.hwnd) {
                        if removebg::is_image_path(&path) {
                            self.removebg_set_input_only(&path);
                        } else {
                            self.removebg.error = Some("That file is not an image.".to_string());
                            self.on_paint();
                        }
                    }
                    return;
                }
                self.close_by_click();
            }
            IslandState::Settings => {
                if in_rect(renderer::SETTINGS_CLOSE_BTN) {
                    self.set_state(IslandState::Expanded);
                    self.on_paint();
                    return;
                }
                // Section chips (7 items):
                for i in 0..7 {
                    let col = i % 2;
                    let row = i / 2;
                    let cx = 54.0 + (col as f32) * 110.0;
                    let cy = 70.0 + (row as f32) * 25.0;
                    let cw = 102.0;
                    let ch = 21.0;
                    if x >= cx && x <= cx + cw && y >= cy && y <= cy + ch {
                        match i {
                            0 => self.settings.music_enabled = !self.settings.music_enabled,
                            1 => self.settings.pomodoro_enabled = !self.settings.pomodoro_enabled,
                            2 => self.settings.todo_enabled = !self.settings.todo_enabled,
                            3 => self.settings.water_enabled = !self.settings.water_enabled,
                            4 => self.settings.tray_enabled = !self.settings.tray_enabled,
                            5 => self.settings.removebg_enabled = !self.settings.removebg_enabled,
                            6 => self.settings.convert_enabled = !self.settings.convert_enabled,
                            _ => {}
                        }
                        self.settings.save();
                        self.on_paint();
                        return;
                    }
                }
                // Preference switches (4 items):
                let rx = 290.0f32;
                for i in 0..4 {
                    let py = 72.0 + (i as f32) * 24.0;
                    let sw_x = rx + 164.0;
                    let sw_y = py;
                    let sw_w = 28.0;
                    let sw_h = 16.0;
                    if x >= rx && x <= sw_x + sw_w + 10.0 && y >= sw_y && y <= sw_y + sw_h {
                        match i {
                            0 => {
                                self.settings.start_with_windows = !self.settings.start_with_windows;
                                crate::tray::set_autorun(self.settings.start_with_windows);
                            }
                            1 => self.settings.sound_chimes = !self.settings.sound_chimes,
                            2 => self.settings.high_refresh_120fps = !self.settings.high_refresh_120fps,
                            3 => self.settings.eye_expressions = !self.settings.eye_expressions,
                            _ => {}
                        }
                        self.settings.save();
                        self.on_paint();
                        return;
                    }
                }
                self.close_by_click();
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

    /// Set Remove BG input only (user must manually press action button to start run).
    fn removebg_set_input_only(&mut self, path: &str) {
        self.removebg.set_input(path);
        let target_size = self.target_size_for(IslandState::RemoveBg);
        if (self.pill.width - target_size.0).abs() > 0.5 || (self.pill.height - target_size.1).abs() > 0.5 {
            self.animate_to_size(target_size.0, target_size.1);
        } else {
            self.on_paint();
        }
    }

    fn removebg_start_run(&mut self) {
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
                    let target_size = self.target_size_for(IslandState::RemoveBg);
                    self.animate_to_size(target_size.0, target_size.1);
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
                    // Changing the model updates state with stored input.
                    if let Some(path) = self.removebg.input_path.clone() {
                        self.removebg_set_input_only(&path);
                    } else {
                        self.on_paint();
                    }
                }
            }
        }
    }

    /// Popup menu listing tray images for Remove BG input (Mac `From Tray` pill).
    #[allow(dead_code)]
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
                self.removebg_set_input_only(&path);
            }
        }
    }

    /// Single-file picker helper (sets input only; manual button starts processing).
    fn convert_set_input_only(&mut self, tool: crate::convert::ConvertTool, path: &str) {
        self.convert.set_input(tool, path);
        self.convert.view = crate::convert::ConvertView::ToolDetail;
        self.convert.active_tool = tool;
        let target_size = self.target_size_for(IslandState::Convert);
        if (self.pill.width - target_size.0).abs() > 0.5 || (self.pill.height - target_size.1).abs() > 0.5 {
            self.animate_to_size(target_size.0, target_size.1);
        } else {
            self.on_paint();
        }
    }

    fn start_convert_run(&mut self, tool: crate::convert::ConvertTool) {
        if tool == crate::convert::ConvertTool::MergePdf {
            self.start_convert_merge();
            return;
        }
        let st = self.convert.tool_state(tool);
        let Some(ref path) = st.input_path.clone() else { return };
        let Ok(data) = std::fs::read(path) else { return };
        let fname = st.input_name.clone();
        let fmt = self.convert.photo_format;
        self.convert.tool_state_mut(tool).run_gen += 1;
        let gen = self.convert.tool_state(tool).run_gen;
        self.convert.tool_state_mut(tool).uploading = true;
        self.convert.tool_state_mut(tool).status_text = "Processing...".to_string();
        self.convert.tool_state_mut(tool).status_is_error = false;
        crate::convert::spawn_convert_worker(
            crate::convert::WorkerArgs {
                tool,
                gen,
                file_data: data,
                file_name: fname,
                format: fmt,
                merge_files: None,
            },
            self.hwnd,
            self.convert_pending.clone(),
        );
        self.on_paint();
    }

    fn start_convert_merge(&mut self) {
        let tool = crate::convert::ConvertTool::MergePdf;
        if self.convert.merge_files.len() < 2 {
            let st = self.convert.tool_state_mut(tool);
            st.status_text = "Add at least 2 PDFs to merge.".to_string();
            st.status_is_error = false;
            self.on_paint();
            return;
        }
        let st = self.convert.tool_state_mut(tool);
        st.run_gen += 1;
        let gen = st.run_gen;
        st.uploading = true;
        st.status_text = "Merging...…".to_string();
        st.status_is_error = false;
        let files = self.convert.merge_files.clone();
        crate::convert::spawn_convert_worker(
            crate::convert::WorkerArgs {
                tool,
                gen,
                file_data: Vec::new(),
                file_name: String::new(),
                format: self.convert.photo_format,
                merge_files: Some(files),
            },
            self.hwnd,
            self.convert_pending.clone(),
        );
        self.on_paint();
    }

    fn on_convert_ready(&mut self) {
        let result_opt = {
            let mut guard = self.convert_pending.lock().unwrap();
            guard.take()
        };
        let Some(res) = result_opt else { return };
        let tool = res.tool;
        let photo_format = self.convert.photo_format;
        let st = self.convert.tool_state_mut(tool);
        if st.run_gen != res.gen {
            return;
        }
        st.uploading = false;
        match res.result {
            Ok(bytes) => {
                st.status_is_error = false;
                st.status_text = "Done — drag or save the result.".to_string();
                let fname = crate::convert::ConvertState::result_file_name(tool, &st.input_name, photo_format);
                st.result_file_name = Some(fname);
                if tool == crate::convert::ConvertTool::Photo {
                    st.result_preview = crate::removebg::decode_preview_fit(&bytes, 360, 140);
                }
                st.result_bytes = Some(bytes);
                let target_size = self.target_size_for(IslandState::Convert);
                self.animate_to_size(target_size.0, target_size.1);
            }
            Err(err) => {
                st.status_is_error = true;
                st.status_text = err;
            }
        }
        self.on_paint();
    }

    fn show_convert_format_menu(&mut self) {
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
            let _ = AppendMenuW(menu, flag(self.convert.photo_format == crate::convert::ConvertFormat::Avif), MENU_CONVERT_FORMAT_AVIF, w!("AVIF"));
            let _ = AppendMenuW(menu, flag(self.convert.photo_format == crate::convert::ConvertFormat::Webp), MENU_CONVERT_FORMAT_WEBP, w!("WebP"));
            let _ = AppendMenuW(menu, flag(self.convert.photo_format == crate::convert::ConvertFormat::Png), MENU_CONVERT_FORMAT_PNG, w!("PNG"));

            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_LEFTALIGN | TPM_TOPALIGN,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            self.suppress_leave = false;
            let new_fmt = match cmd.0 as usize {
                MENU_CONVERT_FORMAT_AVIF => Some(crate::convert::ConvertFormat::Avif),
                MENU_CONVERT_FORMAT_WEBP => Some(crate::convert::ConvertFormat::Webp),
                MENU_CONVERT_FORMAT_PNG => Some(crate::convert::ConvertFormat::Png),
                _ => None,
            };
            if let Some(fmt) = new_fmt {
                self.convert.photo_format = fmt;
                let st = self.convert.tool_state(crate::convert::ConvertTool::Photo);
                if let Some(p) = st.input_path.clone() {
                    self.convert_set_input_only(crate::convert::ConvertTool::Photo, &p);
                } else {
                    self.on_paint();
                }
            }
        }
    }

    #[allow(dead_code)]
    fn show_convert_tray_menu(&mut self) {
        self.suppress_leave = true;
        let tool = self.convert.active_tool;
        let matching: Vec<String> = self.shelf.files.iter().filter(|p| tool.accepts(p)).cloned().collect();
        if matching.is_empty() {
            self.suppress_leave = false;
            return;
        }
        unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                self.suppress_leave = false;
                return;
            };
            for (i, p) in matching.iter().enumerate() {
                let name = std::path::Path::new(p)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or(p.clone());
                let hstr = HSTRING::from(&name);
                let _ = AppendMenuW(menu, MF_STRING, MENU_CONVERT_TRAY_BASE + i, windows::core::PCWSTR(hstr.as_ptr()));
            }
            let mut pt = POINT::default();
            let _ = GetCursorPos(&mut pt);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_NONOTIFY | TPM_LEFTALIGN | TPM_TOPALIGN,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            self.suppress_leave = false;
            let idx = cmd.0 as usize;
            if idx >= MENU_CONVERT_TRAY_BASE && idx < MENU_CONVERT_TRAY_BASE + matching.len() {
                let chosen = &matching[idx - MENU_CONVERT_TRAY_BASE];
                if tool == crate::convert::ConvertTool::MergePdf {
                    let _ = self.convert.add_merge_file(chosen);
                    self.on_paint();
                } else {
                    self.convert_set_input_only(tool, chosen);
                }
            }
        }
    }

    fn pick_image_file(hwnd: HWND) -> Option<String> {
        unsafe {
            // CLSID_FileOpenDialog (ShObjIdl_core.h; not exported by the
            // windows 0.58 Shell bindings, so the documented value is used).
            let clsid = GUID::from_values(
                0xDC1C5A9C,
                0xE88A,
                0x4DDE,
                [0xA5, 0xA1, 0x60, 0xF8, 0x2A, 0x20, 0xAE, 0xF7],
            );
            let dialog: IFileOpenDialog =
                CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER).ok()?;
            dialog.Show(hwnd).ok()?;
            let item = dialog.GetResult().ok()?;
            let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let s = name.to_string().ok()?;
            CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));
            Some(s)
        }
    }

    fn pick_pdf_files(hwnd: HWND, multiselect: bool) -> Vec<String> {
        unsafe {
            use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
            use windows::Win32::UI::Shell::{FOS_ALLOWMULTISELECT, FOS_FORCEFILESYSTEM};

            let clsid = GUID::from_values(
                0xDC1C5A9C,
                0xE88A,
                0x4DDE,
                [0xA5, 0xA1, 0x60, 0xF8, 0x2A, 0x20, 0xAE, 0xF7],
            );
            let Ok(dialog) = CoCreateInstance::<_, IFileOpenDialog>(&clsid, None, CLSCTX_INPROC_SERVER) else {
                return Vec::new();
            };
            if multiselect {
                if let Ok(opts) = dialog.GetOptions() {
                    let _ = dialog.SetOptions(opts | FOS_ALLOWMULTISELECT | FOS_FORCEFILESYSTEM);
                }
            }
            let filter = [
                COMDLG_FILTERSPEC {
                    pszName: windows::core::w!("PDF Documents (*.pdf)"),
                    pszSpec: windows::core::w!("*.pdf"),
                },
                COMDLG_FILTERSPEC {
                    pszName: windows::core::w!("All Files (*.*)"),
                    pszSpec: windows::core::w!("*.*"),
                },
            ];
            let _ = dialog.SetFileTypes(&filter);
            if dialog.Show(hwnd).is_err() {
                return Vec::new();
            }
            let mut out = Vec::new();
            if multiselect {
                if let Ok(items) = dialog.GetResults() {
                    if let Ok(count) = items.GetCount() {
                        for i in 0..count {
                            if let Ok(item) = items.GetItemAt(i) {
                                if let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                                    if let Ok(s) = name.to_string() {
                                        out.push(s);
                                    }
                                    CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));
                                }
                            }
                        }
                    }
                }
            } else if let Ok(item) = dialog.GetResult() {
                if let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH) {
                    if let Ok(s) = name.to_string() {
                        out.push(s);
                    }
                    CoTaskMemFree(Some(name.0 as *const core::ffi::c_void));
                }
            }
            out
        }
    }

    fn on_right_click(&mut self, x_px: i32, y_px: i32) {
        if self.pill.state != IslandState::Tray {
            return;
        }
        let (x, y) = self.client_px_to_pill_pt(x_px, y_px);
        if x < renderer::TRAY_CAROUSEL_X || x > renderer::TRAY_CAROUSEL_X + renderer::TRAY_CAROUSEL_W || y < renderer::TRAY_CAROUSEL_Y || y > renderer::TRAY_CAROUSEL_Y + renderer::TRAY_CAROUSEL_H {
            return;
        }
        for (i, _) in self.shelf.files.iter().enumerate() {
            let left = renderer::TRAY_CAROUSEL_X + (i as f32) * renderer::TRAY_TILE_PITCH - self.tray_scroll;
            let top = renderer::TRAY_CAROUSEL_Y;
            let tw = renderer::TRAY_TILE_W;
            let th = renderer::TRAY_TILE_H;
            if left + tw < renderer::TRAY_CAROUSEL_X || left > renderer::TRAY_CAROUSEL_X + renderer::TRAY_CAROUSEL_W {
                continue;
            }
            if x >= left && x <= left + tw && y >= top && y <= top + th {
                self.show_tile_menu(i);
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
        if self.pill.state == IslandState::Convert || self.pill.state == IslandState::RemoveBg {
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
        if self.pill.state == IslandState::Convert {
            let tool = self.convert.active_tool;
            if tool == crate::convert::ConvertTool::MergePdf {
                let mut added = false;
                for path in &paths {
                    if self.convert.add_merge_file(path) {
                        added = true;
                    }
                }
                if added {
                    self.on_paint();
                }
                return;
            } else {
                let mut used = false;
                for path in &paths {
                    if !used && tool.accepts(path) {
                        self.convert_set_input_only(tool, path);
                        used = true;
                    } else {
                        let _ = self.shelf.add(path);
                    }
                }
                if used {
                    self.on_paint();
                    return;
                }
            }
        }
        if self.pill.state == IslandState::RemoveBg {
            // Open tool section handles its own drop (Mac rule): first image
            // becomes Remove BG input, the rest fall through to Tray.
            let mut used_as_input = false;
            for path in &paths {
                if !used_as_input && removebg::is_image_path(path) {
                    self.removebg_set_input_only(path);
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
            let total_w = self.shelf.files.len() as f32 * renderer::TRAY_TILE_PITCH;
            self.tray_scroll_target = (total_w - renderer::TRAY_CAROUSEL_W).max(0.0);
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
            if self.pill.state == IslandState::Convert {
                let tool = self.convert.active_tool;
                if tool == crate::convert::ConvertTool::MergePdf {
                    let mut added = false;
                    for path in &paths {
                        if self.convert.add_merge_file(path) {
                            added = true;
                        }
                    }
                    if added {
                        self.on_paint();
                    }
                    return;
                } else {
                    let mut used = false;
                    for path in &paths {
                        if !used && tool.accepts(path) {
                            self.convert_set_input_only(tool, path);
                            used = true;
                        }
                    }
                    if used {
                        return;
                    }
                }
            }
            if self.pill.state == IslandState::RemoveBg {
                // Same routing as the OLE drop path (Mac rule).
                let mut used_as_input = false;
                for path in &paths {
                    if !used_as_input && removebg::is_image_path(path) {
                        self.removebg_set_input_only(path);
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
                let total_w = self.shelf.files.len() as f32 * renderer::TRAY_TILE_PITCH;
                self.tray_scroll_target = (total_w - renderer::TRAY_CAROUSEL_W).max(0.0);
                self.set_state(IslandState::Tray);
                self.tray_bounce.trigger(1.0, Some(1.12));
                self.ensure_anim_timer();
                self.on_paint();
            }
        }
    }

    /// (Re)build the extra pill canvases for "show on all monitors".
    /// The main canvas always stays on `pill_monitor`.
    fn rebuild_canvases(&mut self) {
        unsafe {
            for c in self.extra_canvases.drain(..) {
                let _ = DestroyWindow(c.hwnd);
            }
            if self.settings.pill_show_all_monitors {
                let mons = window::list_monitors();
                for (i, m) in mons.iter().enumerate() {
                    if i as i32 == self.settings.pill_monitor {
                        continue;
                    }
                    let Ok(hwnd) =
                        window::create_canvas_window(self.hinstance, self.canvas_class)
                    else {
                        continue;
                    };
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, (self as *mut App) as isize);
                    let dpi = window::dpi_scale(hwnd);
                    let (x, y) = window::canvas_xy_for_work(&m.work, dpi);
                    let _ = SetWindowPos(
                        hwnd,
                        None,
                        x,
                        y,
                        0,
                        0,
                        SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    // Enable file drag-and-drop on extra monitors too.
                    let _ = DragAcceptFiles(hwnd, BOOL(1));
                    drag::register_drop_target(hwnd, self.dropped_files.clone());
                    self.extra_canvases.push(CanvasWin { hwnd, base_x: x, base_y: y });
                }
            }
            // main canvas may have moved to another monitor
            let (bx, by, _, _) =
                window::place_canvas_on_monitor(self.hwnd, self.settings.pill_monitor);
            self.main_base = (bx, by);
            self.last_offset_applied = f32::NAN; // force re-apply
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
                eyes_closed: false,
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
                eye_openness: self.eye_openness,
                eye_slant: self.eye_slant,
                convert: &self.convert,
                music_button_zooms: self.music_button_zooms,
                music_bar_hover: self.music_bar_hover,
                music_seek_pos: self.music_seek_pos,
                master_volume: self.master_volume,
                volume_bar_hover: self.volume_bar_hover,
                volume_bar_zoom: self.volume_bar_zoom,
                card_expanded: self.card_expanded,
                todo_scroll: self.todo_scroll,
                todo_editing_idx: self.todo_editing_idx,
                tray_scroll: self.tray_scroll,
                transition_active: self.transition_active,
                transition_progress: if self.transition_active {
                    (self.transition_start.elapsed().as_secs_f32() / 0.20).clamp(0.0, 1.0)
                } else {
                    1.0
                },
                transition_dir: self.transition_dir,
                transition_from: self.transition_from,
                transition_to: self.transition_to,
                dot_indicator_bounds: self.dot_spring.bounds(),
                hovered_dot: self.hovered_dot,
                music_bar_zoom: self.music_bar_zoom,
                tray_genie_progress: self.tray_genie_start.map(|s| (s.elapsed().as_secs_f32() / 0.35).clamp(0.0, 1.0)),
                mouse_pt: self.mouse_pt,
                header_active: (self.pill.state.has_top_header()
                    || (self.spring.active && self.last_section.has_top_header()))
                    && self.pill.state != IslandState::Zen
                    && self.last_section != IslandState::Zen,
                tray_btn_hovers: self.tray_btn_hovers,
                rb_btn_hovers: self.rb_btn_hovers,
                cv_btn_hovers: self.cv_btn_hovers,
                settings: &self.settings,
                pdf_md_drag: self.pdf_md_drag,
            };
            if self.renderer.draw(self.hwnd, &ctx) {
                // Mirror the same frame onto every extra canvas.
                let alpha =
                    (self.settings.pill_opacity.clamp(0.4, 1.0) * 255.0).round() as u8;
                for c in &self.extra_canvases {
                    self.renderer.present_to(c.hwnd, alpha);
                }
            }
        }
        if self.renderer.is_lyric_rolling() || self.tray_bounce.is_active() || self.transition_active || self.dot_spring.active {
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
            } else if id == tray::MENU_SETTINGS {
                crate::settings_window::open(self.hwnd);
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
            // Validate the hwnd that actually received the paint message.
            // Extra canvases (show-on-all-monitors) share this wndproc;
            // on_paint only validates the main hwnd, so without this their
            // paint region is never cleared -> infinite WM_PAINT storm.
            unsafe {
                let _ = ValidateRect(hwnd, None);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let x = ((lparam.0 as u32) & 0xFFFF) as u16 as i16 as i32;
            let y = (((lparam.0 as u32) >> 16) & 0xFFFF) as u16 as i16 as i32;
            app.on_mouse_move(x, y);
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
        crate::convert::WM_CONVERT_READY => {
            app.on_convert_ready();
            LRESULT(0)
        }
        crate::settings_window::WM_SETTINGS_PREVIEW => {
            // Only open if not already expanded; redundant set_state causes flicker.
            if !app.pill.state.is_expanded() {
                let target = app.last_section;
                app.set_state(target);
            }
            app.ensure_anim_timer();
            app.on_paint();
            // Cancel any pending auto-close while dragging.
            app.preview_close_at = None;
            LRESULT(0)
        }
        crate::settings_window::WM_SETTINGS_DRAG_END => {
            // Pill stays open after slider adjustment (user closes manually).
            LRESULT(0)
        }
        crate::settings_window::WM_SETTINGS_UPDATED => {
            app.settings = crate::settings::AuraSettings::load();
            crate::tray::set_autorun(app.settings.start_with_windows);
            app.water.set_goal(app.settings.water_daily_goal as i64);
            app.pomodoro.focus_mins = app.settings.pomodoro_focus_mins;
            app.removebg.model = [
                removebg::RemoveBgModel::Fast,
                removebg::RemoveBgModel::Balanced,
                removebg::RemoveBgModel::Quality,
            ][app.settings.removebg_model_idx.min(2)];
            app.convert.photo_format = [
                crate::convert::ConvertFormat::Avif,
                crate::convert::ConvertFormat::Webp,
                crate::convert::ConvertFormat::Png,
            ][app.settings.convert_format_idx.min(2)];
            app.rebuild_canvases();
            app.refresh_anim_timer();
            let state = app.pill.state;
            let (tw, th) = app.target_size_for(state);
            // Direct 1:1 size tracking for slider drags. The pill opening
            // itself animates smoothly via set_state -> spring.
            // Sync the spring so it doesn't fight the new size.
            app.spring.snap_to(tw, th);
            app.pill.width = tw;
            app.pill.height = th;
            app.on_paint();
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
            // Only the main canvas owns the App (Box::into_raw). Extra canvases
            // (show-on-all-monitors) just borrow it -- freeing here would be a
            // use-after-free (STATUS_ACCESS_VIOLATION).
            if hwnd == app.hwnd {
                tray::remove_icon(hwnd);
                let _ = Box::from_raw(app_ptr);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                PostQuitMessage(0);
            }
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
        SetLastError(WIN32_ERROR(0));
        let mutex = CreateMutexW(None, BOOL(1), w!("Aura.SingleInstance"))?;
        let err = GetLastError().0;
        if err == 183 {
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
            hIcon: tray::load_aura_icon(64).unwrap_or_else(|| LoadIconW(None, IDI_APPLICATION).unwrap_or_default()),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: class_name,
            hIconSm: tray::load_aura_icon(32).unwrap_or_default(),
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

        let mut app = App::new(hwnd, hinstance, class_name, media_cache, dropped_files)?;
        app.set_lan(lan::LanShare::start());
        let app_ptr = Box::into_raw(app);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app_ptr as isize);

        let _ = tray::add_icon(hwnd);
        let _ = SetTimer(hwnd, TIMER_TICK, 100, None);
        let _ = SetTimer(hwnd, TIMER_ALERT, 250, None);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);

        timeBeginPeriod(1);

        let mut msg = MSG::default();
        let target_frame = Duration::from_micros(8333); // 120 FPS frame quantum (~8.33ms)
        let mut last_frame = Instant::now();

        loop {
            let is_anim = if app_ptr.is_null() {
                false
            } else {
                (*app_ptr).is_animating()
            };

            if is_anim {
                let has_msg = PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE);
                if has_msg.as_bool() {
                    if msg.message == WM_QUIT {
                        break;
                    }
                    let _ = TranslateMessage(&msg);
                    let _ = DispatchMessageW(&msg);
                } else {
                    let elapsed = last_frame.elapsed();
                    if elapsed >= target_frame {
                        (*app_ptr).on_anim_tick();
                        last_frame = Instant::now();
                    } else {
                        let remain = target_frame.saturating_sub(elapsed);
                        if remain >= Duration::from_millis(1) {
                            std::thread::sleep(Duration::from_millis(1));
                        } else {
                            std::hint::spin_loop();
                        }
                    }
                }
            } else {
                let ret = GetMessageW(&mut msg, None, 0, 0);
                if ret.0 <= 0 {
                    break;
                }
                let _ = TranslateMessage(&msg);
                let _ = DispatchMessageW(&msg);
                last_frame = Instant::now();
            }
        }

        timeEndPeriod(1);
        let _ = ReleaseMutex(mutex);
        OleUninitialize();
        Ok(())
    }
}
