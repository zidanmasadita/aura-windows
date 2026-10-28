//! OLE drag-and-drop for tray tiles:
//! 1. Drag-out (`do_drag_file`): drag a tile out of the pill into Explorer as a COPY.
//! 2. Drag-in (`register_drop_target`, `IDropTarget`): matches `aura-island` (`draggingEntered`)
//!    and `nusa.exe` (`0x140019BF4` `RegisterDragDrop`) — hovering a dragged file over the
//!    inactive/closed pill (`Zen` or `Compact`) immediately expands the island to `Tray`
//!    (`640x280`) before the user even releases the mouse button, and dropping adds the files
//!    to the shelf.

use std::mem::ManuallyDrop;
use std::sync::{Arc, Mutex};

use windows::core::{implement, HRESULT};
use windows::Win32::Foundation::{
    BOOL, E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY, GlobalFree, HGLOBAL, HWND, LPARAM, POINT,
    POINTL, WPARAM,
};
use windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IAdviseSink, IDataObject, IDataObject_Impl, IEnumFORMATETC,
    IEnumSTATDATA, STGMEDIUM, STGMEDIUM_0, TYMED_HGLOBAL,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    DoDragDrop, RegisterDragDrop, ReleaseStgMedium, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY,
    DROPEFFECT_NONE, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, DROPFILES, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, SendMessageW};

/// Sent synchronously to `hwnd` on `IDropTarget::DragEnter` (`wparam = 0`) and `DragOver` (`wparam = 1`)
/// when a `CF_HDROP` file drag enters/hovers over the pill so `App` immediately opens `IslandState::Tray`.
pub const WM_DRAG_ENTER_TRAY: u32 = 0x8020;
/// Posted to `hwnd` on `IDropTarget::DragLeave`.
pub const WM_DRAG_LEAVE_TRAY: u32 = 0x8021;
/// Sent to `hwnd` on `IDropTarget::Drop` after `dropped_files` has been populated.
pub const WM_DRAG_DROP_TRAY: u32 = 0x8022;

const DRAGDROP_S_DROP: HRESULT = HRESULT(0x00040100);
const DRAGDROP_S_CANCEL: HRESULT = HRESULT(0x00040101);
const DRAGDROP_S_USEDEFAULTCURSORS: HRESULT = HRESULT(0x00040102);
const DV_E_FORMATETC: HRESULT = HRESULT(0x80040064u32 as i32);
const DV_E_DVASPECT: HRESULT = HRESULT(0x80040066u32 as i32);
const DV_E_TYMED: HRESULT = HRESULT(0x80040069u32 as i32);
const MK_LBUTTON: u32 = 0x0001;

fn hdrop_bytes(path: &str) -> Vec<u8> {
    let mut wide: Vec<u16> = path.encode_utf16().collect();
    wide.push(0);
    wide.push(0);
    let header = DROPFILES {
        pFiles: std::mem::size_of::<DROPFILES>() as u32,
        pt: POINT { x: 0, y: 0 },
        fNC: BOOL(0),
        fWide: BOOL(1),
    };
    let mut bytes = Vec::with_capacity(std::mem::size_of::<DROPFILES>() + wide.len() * 2);
    unsafe {
        let src = &header as *const DROPFILES as *const u8;
        bytes.extend_from_slice(std::slice::from_raw_parts(
            src,
            std::mem::size_of::<DROPFILES>(),
        ));
    }
    for unit in wide {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn alloc_hdrop(path: &str) -> Option<HGLOBAL> {
    let bytes = hdrop_bytes(path);
    unsafe {
        let hglobal = match GlobalAlloc(GMEM_MOVEABLE, bytes.len()) {
            Ok(h) => h,
            Err(_) => return None,
        };
        if hglobal.is_invalid() {
            return None;
        }
        let ptr = GlobalLock(hglobal);
        if ptr.is_null() {
            let _ = GlobalFree(hglobal);
            return None;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        let _ = GlobalUnlock(hglobal);
        Some(hglobal)
    }
}

#[implement(IDataObject)]
struct FileDataObject {
    path: String,
}

impl IDataObject_Impl for FileDataObject_Impl {
    fn GetData(&self, pformatetc: *const FORMATETC) -> windows::core::Result<STGMEDIUM> {
        if pformatetc.is_null() {
            return Err(windows::core::Error::from(E_INVALIDARG));
        }
        let fmt = unsafe { &*pformatetc };
        if fmt.cfFormat != CF_HDROP.0 {
            return Err(windows::core::Error::from(DV_E_FORMATETC));
        }
        if fmt.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
            return Err(windows::core::Error::from(DV_E_TYMED));
        }
        if fmt.dwAspect != DVASPECT_CONTENT.0 {
            return Err(windows::core::Error::from(DV_E_DVASPECT));
        }
        let hglobal = match alloc_hdrop(&self.path) {
            Some(h) => h,
            None => return Err(windows::core::Error::from(E_OUTOFMEMORY)),
        };
        Ok(STGMEDIUM {
            tymed: TYMED_HGLOBAL.0 as u32,
            u: STGMEDIUM_0 { hGlobal: hglobal },
            pUnkForRelease: ManuallyDrop::new(None),
        })
    }

    fn QueryGetData(&self, pformatetc: *const FORMATETC) -> HRESULT {
        if pformatetc.is_null() {
            return E_INVALIDARG;
        }
        let fmt = unsafe { &*pformatetc };
        if fmt.cfFormat != CF_HDROP.0 {
            return DV_E_FORMATETC;
        }
        if fmt.tymed & TYMED_HGLOBAL.0 as u32 == 0 {
            return DV_E_TYMED;
        }
        if fmt.dwAspect != DVASPECT_CONTENT.0 {
            return DV_E_DVASPECT;
        }
        HRESULT(0)
    }

    fn GetDataHere(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *mut STGMEDIUM,
    ) -> windows::core::Result<()> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn GetCanonicalFormatEtc(
        &self,
        _pformatetcin: *const FORMATETC,
        _pformatetcout: *mut FORMATETC,
    ) -> HRESULT {
        E_NOTIMPL
    }

    fn SetData(
        &self,
        _pformatetc: *const FORMATETC,
        _pmedium: *const STGMEDIUM,
        _frelease: BOOL,
    ) -> windows::core::Result<()> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn EnumFormatEtc(&self, _dwdirection: u32) -> windows::core::Result<IEnumFORMATETC> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn DAdvise(
        &self,
        _pformatetc: *const FORMATETC,
        _advf: u32,
        _padvsink: Option<&IAdviseSink>,
    ) -> windows::core::Result<u32> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn DUnadvise(&self, _dwconnection: u32) -> windows::core::Result<()> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }

    fn EnumDAdvise(&self) -> windows::core::Result<IEnumSTATDATA> {
        Err(windows::core::Error::from(E_NOTIMPL))
    }
}

#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource_Impl {
    fn QueryContinueDrag(&self, fescapepressed: BOOL, grfkeystate: MODIFIERKEYS_FLAGS) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if grfkeystate.0 & MK_LBUTTON == 0 {
            return DRAGDROP_S_DROP;
        }
        HRESULT(0)
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

pub fn do_drag_file(path: &str) -> bool {
    let data_object: IDataObject = FileDataObject {
        path: path.to_string(),
    }
    .into();
    let drop_source: IDropSource = DropSource.into();
    let mut effect = DROPEFFECT(0);
    let hr = unsafe { DoDragDrop(&data_object, &drop_source, DROPEFFECT_COPY, &mut effect) };
    hr == DRAGDROP_S_DROP
}

fn hdrop_formatetc() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

fn has_hdrop(pdataobj: Option<&IDataObject>) -> bool {
    let Some(obj) = pdataobj else {
        return false;
    };
    let fmt = hdrop_formatetc();
    unsafe { obj.QueryGetData(&fmt).is_ok() }
}

/// OLE `IDropTarget` registered on the Aura window (`nusa.exe` `0x140019BF4`).
/// Immediately opens `IslandState::Tray` on `DragEnter` even when the pill was inactive (`Zen`/`Compact`).
#[implement(IDropTarget)]
struct PillDropTarget {
    hwnd: HWND,
    accepts: Mutex<bool>,
    dropped_files: Arc<Mutex<Vec<String>>>,
}

impl IDropTarget_Impl for PillDropTarget_Impl {
    fn DragEnter(
        &self,
        pdataobj: Option<&IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let ok = has_hdrop(pdataobj);
        if let Ok(mut guard) = self.accepts.lock() {
            *guard = ok;
        }
        unsafe {
            if !pdweffect.is_null() {
                *pdweffect = if ok {
                    DROPEFFECT_COPY
                } else {
                    DROPEFFECT_NONE
                };
            }
            if ok {
                let _ = SendMessageW(self.hwnd, WM_DRAG_ENTER_TRAY, WPARAM(0), LPARAM(0));
            }
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let ok = self.accepts.lock().map(|g| *g).unwrap_or(false);
        unsafe {
            if !pdweffect.is_null() {
                *pdweffect = if ok {
                    DROPEFFECT_COPY
                } else {
                    DROPEFFECT_NONE
                };
            }
            if ok {
                let _ = SendMessageW(self.hwnd, WM_DRAG_ENTER_TRAY, WPARAM(1), LPARAM(0));
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        if let Ok(mut guard) = self.accepts.lock() {
            *guard = false;
        }
        unsafe {
            let _ = PostMessageW(self.hwnd, WM_DRAG_LEAVE_TRAY, WPARAM(0), LPARAM(0));
        }
        Ok(())
    }

    fn Drop(
        &self,
        pdataobj: Option<&IDataObject>,
        _grfkeystate: MODIFIERKEYS_FLAGS,
        _pt: &POINTL,
        pdweffect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        if let Ok(mut guard) = self.accepts.lock() {
            *guard = false;
        }
        let Some(obj) = pdataobj else {
            unsafe {
                if !pdweffect.is_null() {
                    *pdweffect = DROPEFFECT_NONE;
                }
            }
            return Ok(());
        };
        let fmt = hdrop_formatetc();
        let mut paths = Vec::new();
        unsafe {
            if let Ok(mut medium) = obj.GetData(&fmt) {
                let hdrop = HDROP(medium.u.hGlobal.0);
                if !hdrop.0.is_null() {
                    let count = DragQueryFileW(hdrop, u32::MAX, None);
                    for i in 0..count {
                        let len = DragQueryFileW(hdrop, i, None);
                        if len == 0 {
                            continue;
                        }
                        let mut buf = vec![0u16; (len + 1) as usize];
                        let got = DragQueryFileW(hdrop, i, Some(&mut buf));
                        if got > 0 {
                            let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
                            paths.push(String::from_utf16_lossy(&buf[..end]));
                        }
                    }
                }
                ReleaseStgMedium(&mut medium);
            }
            if !pdweffect.is_null() {
                *pdweffect = if !paths.is_empty() {
                    DROPEFFECT_COPY
                } else {
                    DROPEFFECT_NONE
                };
            }
        }
        if !paths.is_empty() {
            if let Ok(mut guard) = self.dropped_files.lock() {
                guard.extend(paths);
            }
            unsafe {
                let _ = SendMessageW(self.hwnd, WM_DRAG_DROP_TRAY, WPARAM(0), LPARAM(0));
            }
        } else {
            unsafe {
                let _ = PostMessageW(self.hwnd, WM_DRAG_LEAVE_TRAY, WPARAM(0), LPARAM(0));
            }
        }
        Ok(())
    }
}

pub fn register_drop_target(hwnd: HWND, dropped_files: Arc<Mutex<Vec<String>>>) {
    let target: IDropTarget = PillDropTarget {
        hwnd,
        accepts: Mutex::new(false),
        dropped_files,
    }
    .into();
    unsafe {
        let _ = RegisterDragDrop(hwnd, &target);
    }
}
