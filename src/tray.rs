//! System tray icon (`Shell_NotifyIconW`), popup menu, "Start with Windows"
//! registry integration (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`),
//! procedural in-memory WAV chime (`PlaySoundW` with `SND_MEMORY | SND_ASYNC`),
//! and system power/battery & Caps Lock sensors (ported from `nusa.exe`).

use std::sync::OnceLock;

use windows::core::{w, GUID, PCWSTR};
use windows::Win32::Foundation::{HANDLE, HWND, POINT};
use windows::Win32::Media::Audio::{
    PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT,
};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, RegisterPowerSettingNotification, HPOWERNOTIFY, SYSTEM_POWER_STATUS,
};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, MessageBoxW,
    SetForegroundWindow, TrackPopupMenu, DEVICE_NOTIFY_WINDOW_HANDLE, IDI_APPLICATION,
    MB_ICONINFORMATION, MB_OK, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED, TPM_RETURNCMD,
    TPM_RIGHTBUTTON,
};

pub const TRAY_CALLBACK_MSG: u32 = 0x8001; // WM_APP + 1
pub const TRAY_ID: u32 = 1;
pub const MENU_STATUS: usize = 1001;
pub const MENU_QUIT: usize = 1002;
pub const MENU_AUTORUN: usize = 1003;

/// `GUID_ACDC_POWER_SOURCE` (`{5D3E9A59-E9D5-4B00-A6BD-FF34FF516548}`) from `nusa.exe` (`0x1400236C0`).
const GUID_ACDC_POWER_SOURCE: GUID = GUID::from_u128(0x5d3e9a59_e9d5_4b00_a6bd_ff34ff516548);

pub fn add_icon(hwnd: HWND) -> bool {
    unsafe {
        let Ok(icon) = LoadIconW(None, IDI_APPLICATION) else {
            return false;
        };
        let mut tip = [0u16; 128];
        for (i, c) in "Aura".encode_utf16().enumerate() {
            if i < 127 {
                tip[i] = c;
            }
        }
        let data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_CALLBACK_MSG,
            hIcon: icon,
            szTip: tip,
            ..Default::default()
        };
        Shell_NotifyIconW(NIM_ADD, &data).0 != 0
    }
}

pub fn remove_icon(hwnd: HWND) {
    unsafe {
        let data = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            ..Default::default()
        };
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// Show the tray popup menu at the cursor. Returns the selected id (0 = dismissed).
pub fn show_menu(hwnd: HWND) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_STATUS, w!("Status Deteksi"));
        let autorun_flags = if is_autorun_enabled() {
            MF_STRING | MF_CHECKED
        } else {
            MF_STRING | MF_UNCHECKED
        };
        let _ = AppendMenuW(menu, autorun_flags, MENU_AUTORUN, w!("Start with Windows"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
        let _ = SetForegroundWindow(hwnd);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let id = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            0,
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
        id.0 as usize
    }
}

/// Check if "Aura" is registered under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`
/// (`nusa.exe` `0x140023740`).
pub fn is_autorun_enabled() -> bool {
    unsafe {
        let mut size: u32 = 0;
        let status = RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            w!("Aura"),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut size),
        );
        status.is_ok() && size > 2
    }
}

/// Toggle "Start with Windows" in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
pub fn toggle_autorun() -> bool {
    unsafe {
        if is_autorun_enabled() {
            let _ = RegDeleteKeyValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                w!("Aura"),
            );
            false
        } else {
            let Ok(exe) = std::env::current_exe() else {
                return false;
            };
            let quoted = format!("\"{}\"", exe.display());
            let mut wide: Vec<u16> = quoted.encode_utf16().collect();
            wide.push(0);
            let bytes = std::slice::from_raw_parts(
                wide.as_ptr() as *const u8,
                wide.len() * std::mem::size_of::<u16>(),
            );
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
                w!("Aura"),
                REG_SZ.0,
                Some(bytes.as_ptr() as *const core::ffi::c_void),
                bytes.len() as u32,
            );
            true
        }
    }
}

/// Live diagnostic dialog showing GSMTC, Battery, Caps Lock, LAN Share, and Autorun status.
pub fn show_status_dialog(
    hwnd: HWND,
    poll_status: &str,
    media: Option<&crate::media::MediaInfo>,
    peer_count: usize,
) {
    let media_line = match media {
        Some(m) => format!(
            "Aktif ({})\n  Lagu: {} — {}\n  Status: {} ({:.0}s / {:.0}s)\n  Cover Art: {}",
            if m.source_app.is_empty() { "GSMTC" } else { &m.source_app },
            m.title,
            m.artist,
            if m.playing { "Playing" } else { "Paused" },
            m.position_secs,
            m.duration_secs,
            if m.cover_bgra.is_some() { "Decoded (WIC BGRA)" } else { "None" }
        ),
        None => "Tidak ada sesi aktif".to_string(),
    };
    let pwr = read_power_status();
    let bat_line = match pwr.percent {
        Some(pct) => format!(
            "{}% ({})",
            pct,
            if pwr.ac_online {
                if pwr.charging { "Charging / AC" } else { "Plugged in (AC)" }
            } else {
                "On Battery"
            }
        ),
        None => "Desktop PC (AC Power)".to_string(),
    };
    let caps = if is_caps_lock_on() { "ON" } else { "OFF" };
    let autorun = if is_autorun_enabled() { "Aktif" } else { "Nonaktif" };

    let body = format!(
        "Aura Status Deteksi (Nusa MTA Engine)\n\n\
         • GSMTC Engine: {}\n\
         • Sesi Media: {}\n\
         • Daya / Baterai: {}\n\
         • Caps Lock: {}\n\
         • LAN Share Peers: {} perangkat\n\
         • Start with Windows: {}",
        poll_status, media_line, bat_line, caps, peer_count, autorun
    );
    let wide: Vec<u16> = body.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            hwnd,
            PCWSTR::from_raw(wide.as_ptr()),
            w!("Aura — Status Deteksi"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

// ---------------------------------------------------------------------------
// Power / Battery & Caps Lock sensors (Nusa `0x140021AA9`, `0x1400197DC`)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PowerState {
    pub ac_online: bool,
    pub charging: bool,
    pub percent: Option<u8>,
}

pub fn read_power_status() -> PowerState {
    unsafe {
        let mut sps = SYSTEM_POWER_STATUS::default();
        if GetSystemPowerStatus(&mut sps).is_ok() {
            let ac_online = sps.ACLineStatus == 1;
            let charging = (sps.BatteryFlag & 8) != 0;
            let has_battery = (sps.BatteryFlag & 128) == 0 && sps.BatteryLifePercent <= 100;
            let percent = if has_battery {
                Some(sps.BatteryLifePercent)
            } else {
                None
            };
            PowerState {
                ac_online,
                charging,
                percent,
            }
        } else {
            PowerState {
                ac_online: true,
                charging: false,
                percent: None,
            }
        }
    }
}

/// Register the window for `WM_POWERBROADCAST` (`GUID_ACDC_POWER_SOURCE`) notifications
/// (`nusa.exe` `0x1400236C0`).
pub fn register_power_notify(hwnd: HWND) -> Option<HPOWERNOTIFY> {
    unsafe {
        RegisterPowerSettingNotification(
            HANDLE(hwnd.0),
            &GUID_ACDC_POWER_SOURCE,
            DEVICE_NOTIFY_WINDOW_HANDLE,
        )
        .ok()
    }
}

/// True when Caps Lock (`VK_CAPITAL = 0x14`) toggle bit is active.
pub fn is_caps_lock_on() -> bool {
    unsafe { (GetKeyState(0x14) & 1) != 0 }
}

// ---------------------------------------------------------------------------
// Procedural In-Memory WAV Sound Synthesizer (Nusa `0x14003CF27`, `0x14004017E`)
// ---------------------------------------------------------------------------

static CHIME_WAV: OnceLock<Vec<u8>> = OnceLock::new();
static POP_WAV: OnceLock<Vec<u8>> = OnceLock::new();

/// Build a 16-bit mono 44.1 kHz RIFF/WAVE buffer in memory (`0x46464952` "RIFF",
/// `0x45564157` "WAVE", `0x20746D66` "fmt ", `0x61746164` "data").
fn build_wav_pcm16(samples: &[i16]) -> Vec<u8> {
    let sample_rate: u32 = 44_100;
    let data_bytes = (samples.len() * 2) as u32;
    let riff_size = 36 + data_bytes;
    let mut buf = Vec::with_capacity(44 + data_bytes as usize);
    buf.extend_from_slice(b"RIFF");
    buf.extend_from_slice(&riff_size.to_le_bytes());
    buf.extend_from_slice(b"WAVE");
    buf.extend_from_slice(b"fmt ");
    buf.extend_from_slice(&16u32.to_le_bytes()); // Subchunk1Size (PCM)
    buf.extend_from_slice(&1u16.to_le_bytes()); // AudioFormat = 1 (PCM)
    buf.extend_from_slice(&1u16.to_le_bytes()); // NumChannels = 1 (mono)
    buf.extend_from_slice(&sample_rate.to_le_bytes());
    buf.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // ByteRate
    buf.extend_from_slice(&2u16.to_le_bytes()); // BlockAlign
    buf.extend_from_slice(&16u16.to_le_bytes()); // BitsPerSample = 16
    buf.extend_from_slice(b"data");
    buf.extend_from_slice(&data_bytes.to_le_bytes());
    for &s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    buf
}

/// Synthesize a two-tone harmonic bell chime (A5 880Hz -> E6 1318.5Hz) for Pomodoro completion.
fn generate_chime_wav() -> Vec<u8> {
    let sr = 44_100.0f32;
    let total_samples = (sr * 0.55) as usize;
    let mut samples = Vec::with_capacity(total_samples);
    for i in 0..total_samples {
        let t = i as f32 / sr;
        let (freq, local_t) = if t < 0.16 {
            (880.0f32, t)
        } else {
            (1318.5f32, t - 0.16)
        };
        let env = (-local_t * 9.5).exp() * (local_t / 0.005).min(1.0);
        let fundamental = (2.0 * std::f32::consts::PI * freq * local_t).sin();
        let overtone = 0.35 * (2.0 * std::f32::consts::PI * freq * 2.0 * local_t).sin();
        let val = ((fundamental + overtone) * env * 9000.0).clamp(-32000.0, 32000.0) as i16;
        samples.push(val);
    }
    build_wav_pcm16(&samples)
}

/// Synthesize a subtle soft pop/tick sound for transient pill notifications.
fn generate_pop_wav() -> Vec<u8> {
    let sr = 44_100.0f32;
    let total_samples = (sr * 0.06) as usize;
    let mut samples = Vec::with_capacity(total_samples);
    for i in 0..total_samples {
        let t = i as f32 / sr;
        let env = (-t * 55.0).exp() * (t / 0.002).min(1.0);
        let freq = 660.0 + 220.0 * (-t * 40.0).exp();
        let val = ((2.0 * std::f32::consts::PI * freq * t).sin() * env * 4500.0)
            .clamp(-32000.0, 32000.0) as i16;
        samples.push(val);
    }
    build_wav_pcm16(&samples)
}

/// Play the Pomodoro completion bell chime asynchronously from memory (`nusa.exe` `0x14003CF27`).
pub fn play_chime() {
    let wav = CHIME_WAV.get_or_init(generate_chime_wav);
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(wav.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}

/// Play a soft notification pop asynchronously from memory (`nusa.exe` `0x14004017E`).
pub fn play_alert_pop() {
    let wav = POP_WAV.get_or_init(generate_pop_wav);
    unsafe {
        let _ = PlaySoundW(
            PCWSTR(wav.as_ptr() as *const u16),
            None,
            SND_MEMORY | SND_ASYNC | SND_NODEFAULT,
        );
    }
}
