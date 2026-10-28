//! OLE drag-out for tray tiles (Phase 3b): drag a tile out of the pill
//! into Explorer (or any CF_HDROP target) as a COPY.
//!
//! Minimal IDataObject (CF_HDROP only) + IDropSource, driven by
//! DoDragDrop on the UI thread (STA after OleInitialize in app::run).
//! The data object builds a FRESH DROPFILES HGLOBAL on every GetData,
//! so the drop target owns the copy it receives and repeated drags
//! never share one handle. No DropSource / IDataObject use beyond
//! what DoDragDrop needs is implemented; the rest return E_NOTIMPL.
//!
//! windows-rs 0.58 import quirks (fixed from the first build errors):
//! STGMEDIUM/IDataObject_Impl need feature Win32_System_Com_StructuredStorage,
//! #[implement] needs windows-core as a direct dependency, GlobalFree lives
//! in Win32::Foundation (not System::Memory), and DV_E_* are literals above.
//! _Impl traits need the windows feature "implement" (it gates the
//! Com/Ole impl.rs include); they live at the namespace root, NOT
//! in an r#impl submodule (that guess failed 2026-10-28).
//! 0.58 _Impl shapes (proven by compiler 2026-10-28): GetData returns
//! STGMEDIUM by value; QueryGetData/Canonical/QueryContinue/GiveFeedback
//! return raw HRESULT; Enum* return the enumerator by value; DAdvise
//! takes u32 + Option sink and returns Result<u32>; FORMATETC/STGMEDIUM
//! fields are primitives (u16/u32), and GlobalAlloc returns Result.
//! TYMED itself is i32-based in 0.58, so TYMED_HGLOBAL.0 is cast to u32.

use std::mem::ManuallyDrop;

use windows::core::{implement, HRESULT};
use windows::Win32::Foundation::{BOOL, E_INVALIDARG, E_NOTIMPL, E_OUTOFMEMORY, GlobalFree, HGLOBAL, POINT};
use windows::Win32::System::Com::{
    DVASPECT_CONTENT, FORMATETC, IAdviseSink,
    IDataObject, IDataObject_Impl, IEnumFORMATETC, IEnumSTATDATA, STGMEDIUM, STGMEDIUM_0,
    TYMED_HGLOBAL,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Ole::{
    DoDragDrop, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, IDropSource, IDropSource_Impl,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::DROPFILES;

// DoDragDrop result codes (windows-rs 0.58 does not export these as
// named constants, so they are spelled out as literals here).
const DRAGDROP_S_DROP: HRESULT = HRESULT(0x00040100);
const DRAGDROP_S_CANCEL: HRESULT = HRESULT(0x00040101);
const DRAGDROP_S_USEDEFAULTCURSORS: HRESULT = HRESULT(0x00040102);
// DV_E_* error codes (objidl): windows-rs 0.58 does not export them
// from System::Com, so they are spelled out as literals here.
const DV_E_FORMATETC: HRESULT = HRESULT(0x80040064u32 as i32);
const DV_E_DVASPECT: HRESULT = HRESULT(0x80040066u32 as i32);
const DV_E_TYMED: HRESULT = HRESULT(0x80040069u32 as i32);
// MK_LBUTTON bit in QueryContinueDrag's grfKeyState (Win32 docs).
const MK_LBUTTON: u32 = 0x0001;

/// Build the DROPFILES payload for one file: a DROPFILES header
/// (pFiles = size of the header, fWide = TRUE) followed by the path
/// as UTF-16, terminated by a double null (end of the file list).
fn hdrop_bytes(path: &str) -> Vec<u8> {
    let mut wide: Vec<u16> = path.encode_utf16().collect();
    wide.push(0);
    wide.push(0); // double-null terminator for the file list
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

/// Allocate a moveable global block holding the DROPFILES payload.
/// Returns None on allocation/lock failure (caller reports E_OUTOFMEMORY).
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

/// One file offered as CF_HDROP. The path is stored as a String;
/// every GetData builds a fresh DROPFILES HGLOBAL from it.
#[implement(IDataObject)]
struct FileDataObject {
    path: String,
}

impl IDataObject_Impl for FileDataObject {
    fn GetData(
        &self,
        pformatetc: *const FORMATETC,
    ) -> windows::core::Result<STGMEDIUM> {
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

    fn EnumFormatEtc(
        &self,
        _dwdirection: u32,
    ) -> windows::core::Result<IEnumFORMATETC> {
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

/// Minimal drag source: drop when the left button is released, cancel
/// on Escape, keep dragging otherwise; always use default cursors.
#[implement(IDropSource)]
struct DropSource;

impl IDropSource_Impl for DropSource {
    fn QueryContinueDrag(
        &self,
        fescapepressed: BOOL,
        grfkeystate: MODIFIERKEYS_FLAGS,
    ) -> HRESULT {
        if fescapepressed.as_bool() {
            return DRAGDROP_S_CANCEL;
        }
        if grfkeystate.0 & MK_LBUTTON == 0 {
            // Left button released: drop.
            return DRAGDROP_S_DROP;
        }
        HRESULT(0)
    }

    fn GiveFeedback(&self, _dweffect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Run a blocking OLE drag for one file on the calling (UI) thread.
/// Returns true if the target accepted the drop, false if the drag
/// was cancelled or the target refused it. Callers must have called
/// OleInitialize on this thread first (app::run does).
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
