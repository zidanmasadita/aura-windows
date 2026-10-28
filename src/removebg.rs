//! Remove BG section — Windows port of Aura Mac `RimufuRemoveBg`.
//! Backend: the same Rimufu VPS API the Mac section uses
//! (`https://zcal.my.id/rimufu-api`). Uploads run on a one-shot MTA worker
//! thread (mirror of `lyrics::fetch_async`): `init_mta` → WinRT `HttpClient`
//! POST with a hand-built multipart body → result slot → `PostMessage`
//! `WM_REMOVEBG_READY` (`0x8010`). The worker never touches UI state.
//! Model note: the VPS currently forces `u2netp` server-side (same as Mac),
//! so Quality/Balanced results are identical to Fast until the server-side
//! force patch is removed (pending VPS IP from the user).
//! Not yet ported (follow-up slice): Mac's upload resize (images larger
//! than 2000px on a side or 10MB are sent as-is for now).

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::{Interface, HSTRING};
use windows::Foundation::AsyncStatus;
use windows::Storage::Streams::{DataReader, DataWriter};
use windows::Web::Http::{HttpBufferContent, HttpClient};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapFrameDecode,
    IWICBitmapSource, IWICFormatConverter, IWICImagingFactory, IWICStream,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeMedianCut,
    WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Rimufu API base (same as the Mac section).
pub const API_BASE: &str = "https://zcal.my.id/rimufu-api";

/// Posted to the main HWND when an upload worker finishes (mirror of
/// `WM_LYRICS_READY`). Free: `0x8003` media, `0x800F` lyrics, `0x8020`-`0x8022` drag.
pub const WM_REMOVEBG_READY: u32 = 0x8010;

/// Total budget for one upload + response read (Mac: 180s timeout).
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// Budget for the response-body read once headers arrived.
const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Remove BG model (Mac `RemoveBgModel`; default Fast).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RemoveBgModel {
    Quality,
    Balanced,
    Fast,
}

impl RemoveBgModel {
    pub fn param(self) -> &'static str {
        match self {
            RemoveBgModel::Quality => "birefnet-general",
            RemoveBgModel::Balanced => "u2net",
            RemoveBgModel::Fast => "u2netp",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RemoveBgModel::Quality => "Quality",
            RemoveBgModel::Balanced => "Balanced",
            RemoveBgModel::Fast => "Fast",
        }
    }
}

/// Upload result delivered through the pending slot.
pub struct RemoveBgResult {
    pub gen: u64,
    pub result: Result<Vec<u8>, String>,
}

/// Owned Remove BG section state (lives on `App`, never on the worker).
pub struct RemoveBgState {
    pub input_path: Option<String>,
    pub input_name: String,
    pub input_size: u64,
    /// Input thumbnail, PBGRA pixels aspect-fit into 264x108.
    pub input_preview: Option<(Vec<u8>, u32, u32)>,
    pub model: RemoveBgModel,
    pub uploading: bool,
    /// Bumped on every input change and every run; stale worker results
    /// (gen != current) are dropped by the ready handler.
    pub run_gen: u64,
    pub result_bytes: Option<Vec<u8>>,
    /// Result preview, PBGRA pixels aspect-fit into 264x144.
    pub result_preview: Option<(Vec<u8>, u32, u32)>,
    /// Written once per result run, shared by drag-out and Save (Mac
    /// `ensureRemoveBgFile`).
    pub saved_path: Option<String>,
    pub error: Option<String>,
    pub notice: String,
}

impl RemoveBgState {
    pub fn new() -> Self {
        Self {
            input_path: None,
            input_name: String::new(),
            input_size: 0,
            input_preview: None,
            model: RemoveBgModel::Fast,
            uploading: false,
            run_gen: 0,
            result_bytes: None,
            result_preview: None,
            saved_path: None,
            error: None,
            notice: String::new(),
        }
    }

    /// Set a new input (drop, pick, or From Tray). Invalidates any
    /// in-flight upload via the generation bump.
    pub fn set_input(&mut self, path: &str) {
        let p = Path::new(path);
        self.input_path = Some(path.to_string());
        self.input_name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        self.input_size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        self.input_preview = std::fs::read(path)
            .ok()
            .and_then(|b| decode_preview_fit(&b, 264, 108));
        self.run_gen += 1;
        self.uploading = false;
        self.result_bytes = None;
        self.result_preview = None;
        self.saved_path = None;
        self.error = None;
        self.notice.clear();
    }

    /// Start an upload run. Returns `(gen, bytes, filename, model_param)`
    /// for the worker, or `None` when there is no input.
    pub fn start_run(&mut self) -> Option<(u64, Vec<u8>, String, String)> {
        let path = self.input_path.clone()?;
        let bytes = std::fs::read(&path).ok()?;
        self.run_gen += 1;
        self.uploading = true;
        self.result_bytes = None;
        self.result_preview = None;
        self.saved_path = None;
        self.error = None;
        self.notice.clear();
        Some((
            self.run_gen,
            bytes,
            self.input_name.clone(),
            self.model.param().to_string(),
        ))
    }

    /// Write the result to `%USERPROFILE%\Downloads\Aura` once (Finder-style
    /// numbering via `lan::unique_path`), reusing the file afterwards.
    pub fn ensure_saved(&mut self) -> Option<String> {
        if let Some(p) = &self.saved_path {
            return Some(p.clone());
        }
        let bytes = self.result_bytes.as_ref()?;
        let dir = crate::lan::downloads_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        let stem = Path::new(&self.input_name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Remove BG".to_string());
        let name = format!("{} no-bg.png", stem);
        let path = crate::lan::unique_path(&dir, &name);
        std::fs::write(&path, bytes).ok()?;
        let s = path.to_string_lossy().into_owned();
        self.saved_path = Some(s.clone());
        Some(s)
    }
}

/// True for the image extensions Remove BG accepts.
pub fn is_image_path(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif"
    )
}

/// Existing tray files usable as Remove BG input (Mac `From Tray` list).
pub fn tray_images(shelf: &crate::shelf::Shelf) -> Vec<String> {
    shelf
        .files
        .iter()
        .filter(|p| is_image_path(p) && Path::new(p).exists())
        .cloned()
        .collect()
}

/// Spawn the upload worker (mirror of `lyrics::fetch_async`).
pub fn run_async(
    hwnd: HWND,
    slot: Arc<Mutex<Option<RemoveBgResult>>>,
    gen: u64,
    bytes: Vec<u8>,
    filename: String,
    model_param: String,
) {
    let hwnd_raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        crate::media::init_mta();
        let result = upload(&bytes, &filename, &model_param);
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(RemoveBgResult { gen, result });
        }
        unsafe {
            let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
            let _ = PostMessageW(hwnd, WM_REMOVEBG_READY, WPARAM(0), LPARAM(0));
        }
    });
}

/// Map a WinRT error to a readable message.
fn err_str(e: windows::core::Error) -> String {
    let m = e.message().to_string();
    if m.trim().is_empty() {
        format!("Windows error 0x{:08X}", e.code().0 as u32)
    } else {
        m
    }
}

/// Map a WinRT result to `Result<T, String>`.
fn werr<T>(r: windows::core::Result<T>) -> Result<T, String> {
    r.map_err(err_str)
}

/// Wait for a WinRT async-with-progress operation (poll `Status()`, like
/// the old `get_with_timeout` but without STA pumping: this runs on an MTA
/// worker, where completions need no message pump).
fn wait_op<T, P>(
    op: &windows::Foundation::IAsyncOperationWithProgress<T, P>,
    deadline: Instant,
) -> Result<T, String>
where
    T: windows::core::RuntimeType,
    P: windows::core::RuntimeType,
{
    loop {
        if let Ok(status) = op.Status() {
            match status {
                AsyncStatus::Completed => return werr(op.GetResults()),
                AsyncStatus::Error | AsyncStatus::Canceled => {
                    let code = op.ErrorCode().map(|c| c.0).unwrap_or(-1);
                    return Err(format!("Windows error 0x{:08X}", code as u32));
                }
                _ => {}
            }
        }
        if Instant::now() >= deadline {
            return Err("Request timed out.".to_string());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Sanitize a filename for the multipart `Content-Disposition` header.
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Build a `multipart/form-data` body with a single `file` part.
fn build_multipart(boundary: &str, filename: &str, file_bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    body.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
            sanitize_filename(filename)
        )
        .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(file_bytes);
    body.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());
    body
}

/// Extract the server error message from a JSON error body (Mac: `detail`
/// or `error`), falling back to a generic HTTP message.
fn parse_error_body(text: &str, code: i32) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        for key in ["detail", "error"] {
            if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    format!("Server error (HTTP {}).", code)
}

/// POST the image to `/api/remove-bg` and return the PNG result bytes.
fn upload(bytes: &[u8], filename: &str, model_param: &str) -> Result<Vec<u8>, String> {
    let boundary = "----AuraRemoveBg7d3f9a";
    let body = build_multipart(boundary, filename, bytes);
    let writer = werr(DataWriter::new())?;
    werr(writer.WriteBytes(&body))?;
    let buffer = werr(writer.DetachBuffer())?;
    let content = werr(HttpBufferContent::CreateFromBuffer(&buffer))?;
    let headers = werr(content.Headers())?;
    werr(headers.Append(
        &HSTRING::from("Content-Type"),
        &HSTRING::from(format!("multipart/form-data; boundary={}", boundary)),
    ))?;
    let client = werr(HttpClient::new())?;
    if let Ok(req_headers) = client.DefaultRequestHeaders() {
        let _ = req_headers.Append(
            &HSTRING::from("User-Agent"),
            &HSTRING::from("Aura-Notch/1.0 (Windows)"),
        );
    }
    let url = format!(
        "{}/api/remove-bg?model={}&alpha_matting=false",
        API_BASE, model_param
    );
    let uri = werr(windows::Foundation::Uri::CreateUri(&HSTRING::from(url)))?;
    let http_content: windows::Web::Http::IHttpContent = werr(content.cast())?;
    let op = werr(client.PostAsync(&uri, &http_content))?;
    let resp = wait_op(&op, Instant::now() + UPLOAD_TIMEOUT)?;
    let ok = werr(resp.IsSuccessStatusCode())?;
    let code = werr(resp.StatusCode())?.0;
    let resp_content = werr(resp.Content())?;
    if !ok {
        let text = resp_content
            .ReadAsStringAsync()
            .ok()
            .and_then(|o| o.get().ok())
            .map(|t| t.to_string())
            .unwrap_or_default();
        return Err(parse_error_body(&text, code));
    }
    let stream_op = werr(resp_content.ReadAsInputStreamAsync())?;
    let stream = wait_op(&stream_op, Instant::now() + READ_TIMEOUT)?;
    let reader = werr(DataReader::CreateDataReader(&stream))?;
    let mut out: Vec<u8> = Vec::new();
    loop {
        let loaded = reader
            .LoadAsync(65536)
            .ok()
            .and_then(|o| o.get().ok())
            .unwrap_or(0) as usize;
        if loaded == 0 {
            break;
        }
        let mut chunk = vec![0u8; loaded];
        werr(reader.ReadBytes(&mut chunk))?;
        out.extend_from_slice(&chunk);
    }
    let _ = reader.Close();
    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if out.len() < 8 || out[0..8] != PNG_MAGIC {
        return Err("Result is not a PNG image.".to_string());
    }
    Ok(out)
}

/// Decode encoded image bytes into PBGRA pixels aspect-fit into a
/// `box_w`x`box_h` box (mirror of `media::decode_image_bytes_wic`, but with
/// aspect-preserving fit instead of a fixed target size).
pub fn decode_preview_fit(
    bytes: &[u8],
    box_w: u32,
    box_h: u32,
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
        let src: IWICBitmapSource = frame.cast().ok()?;
        let mut nat_w = 0u32;
        let mut nat_h = 0u32;
        src.GetSize(&mut nat_w, &mut nat_h).ok()?;
        if nat_w == 0 || nat_h == 0 {
            return None;
        }
        let scale = ((box_w as f32) / (nat_w as f32)).min((box_h as f32) / (nat_h as f32));
        let target_w = ((nat_w as f32) * scale).round() as u32;
        let target_h = ((nat_h as f32) * scale).round() as u32;
        if target_w == 0 || target_h == 0 {
            return None;
        }
        let scaler = factory.CreateBitmapScaler().ok()?;
        scaler
            .Initialize(&src, target_w, target_h, WICBitmapInterpolationModeFant)
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
        let mut pixels = vec![0u8; (stride * target_h) as usize];
        converter
            .CopyPixels(std::ptr::null(), stride, &mut pixels)
            .ok()?;
        Some((pixels, target_w, target_h))
    }
}
