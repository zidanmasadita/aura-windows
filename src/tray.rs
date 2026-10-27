//! System tray icon (Shell_NotifyIconW) + popup menu.
//! Mirrors Aura Mac's menu-bar "Aura" item (Status Deteksi, Quit).
//! Phase 0: menu has "Status Deteksi (Fase 4)" (placeholder MessageBox)
//! and "Quit". Callback message = WM_APP + 1.

use windows::core::w;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, NIF_ICON, NIF_MESSAGE, NIF_TIP};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, LoadIconW, MessageBoxW,
    SetForegroundWindow, TrackPopupMenu, IDI_APPLICATION, MB_ICONINFORMATION, MB_OK,
    MF_STRING, POINT, TPM_RETURNCMD, TPM_RIGHTBUTTON,
};

pub const TRAY_CALLBACK_MSG: u32 = 0x8001; // WM_APP + 1
pub const TRAY_ID: u32 = 1;
pub const MENU_STATUS: usize = 1001;
pub const MENU_QUIT: usize = 1002;

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
/// Caller handles MENU_STATUS / MENU_QUIT.
pub fn show_menu(hwnd: HWND) -> usize {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return 0;
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_STATUS, w!("Status Deteksi (Fase 4)"));
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
        let _ = SetForegroundWindow(hwnd);
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let id = TrackPopupMenu(menu, TPM_RIGHTBUTTON | TPM_RETURNCMD, pt.x, pt.y, 0, hwnd, None);
        // TrackPopupMenu does not destroy the menu; free it to avoid a leak.
        let _ = DestroyMenu(menu);
        id as usize
    }
}

pub fn show_status_placeholder(hwnd: HWND) {
    unsafe {
        let _ = MessageBoxW(
            Some(hwnd),
            w!("Aura Fase 0\n\nStatus Deteksi menyusul di Fase 4.\nGSMTC, Bluetooth, dan diagnostik belum aktif."),
            w!("Aura"),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}
