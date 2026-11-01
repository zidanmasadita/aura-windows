//! Convert section — Windows port of Aura Mac `RimufuConvert`.
//! Backend: Rimufu VPS API (`https://zcal.my.id/rimufu-api`).
//! Supports:
//!   - Photo (convert to AVIF, WebP, PNG)
//!   - Merge PDF (multi-file reorderable queue to merged PDF)
//!   - PDF to Word (.docx)
//!   - Compress PDF (.pdf)
//!   - PDF to MD (Markdown)
//!
//! Uploads run on a one-shot MTA worker thread using WinRT `HttpClient`,
//! posting `WM_CONVERT_READY` (`0x8011`) when finished.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::core::HSTRING;
use windows::Foundation::AsyncStatus;
use windows::Storage::Streams::{DataReader, DataWriter};
use windows::Web::Http::{HttpBufferContent, HttpClient};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::removebg::decode_preview_fit;

/// Rimufu API base (same as Mac).
pub const API_BASE: &str = "https://zcal.my.id/rimufu-api";

/// Posted to main HWND when convert finishes.
pub const WM_CONVERT_READY: u32 = 0x8011;

const UPLOAD_TIMEOUT: Duration = Duration::from_secs(180);
const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ConvertTool {
    Photo,
    MergePdf,
    PdfToWord,
    CompressPdf,
    PdfToMd,
}

impl ConvertTool {
    pub const ALL: [ConvertTool; 5] = [
        ConvertTool::Photo,
        ConvertTool::MergePdf,
        ConvertTool::PdfToWord,
        ConvertTool::CompressPdf,
        ConvertTool::PdfToMd,
    ];

    #[allow(dead_code)]
    pub fn display_name(self) -> &'static str {
        match self {
            ConvertTool::Photo => "Photo",
            ConvertTool::MergePdf => "Merge PDF",
            ConvertTool::PdfToWord => "PDF to Word",
            ConvertTool::CompressPdf => "Compress PDF",
            ConvertTool::PdfToMd => "PDF to MD",
        }
    }

    pub fn default_status(self) -> &'static str {
        match self {
            ConvertTool::Photo => "Drop an image, or click to pick one.",
            ConvertTool::MergePdf => "Add at least 2 PDFs, then press Merge.",
            _ => "Drop a PDF, or click to pick one.",
        }
    }

    pub fn accepts(self, path: &str) -> bool {
        let ext = Path::new(path)
            .extension()
            .map(|s| s.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match self {
            ConvertTool::Photo => matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "heic" | "webp" | "gif" | "bmp" | "tiff"
            ),
            _ => ext == "pdf",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConvertFormat {
    Avif,
    Webp,
    Png,
}

impl ConvertFormat {
    #[allow(dead_code)]
    pub const ALL: [ConvertFormat; 3] = [
        ConvertFormat::Avif,
        ConvertFormat::Webp,
        ConvertFormat::Png,
    ];

    #[allow(dead_code)]
    pub fn display_name(self) -> &'static str {
        match self {
            ConvertFormat::Avif => "AVIF",
            ConvertFormat::Webp => "WebP",
            ConvertFormat::Png => "PNG",
        }
    }

    pub fn form_value(self) -> &'static str {
        match self {
            ConvertFormat::Avif => "avif",
            ConvertFormat::Webp => "webp",
            ConvertFormat::Png => "png",
        }
    }

    pub fn file_extension(self) -> &'static str {
        match self {
            ConvertFormat::Avif => "avif",
            ConvertFormat::Webp => "webp",
            ConvertFormat::Png => "png",
        }
    }
}

pub struct ConvertResult {
    pub tool: ConvertTool,
    pub gen: u64,
    pub result: Result<Vec<u8>, String>,
}

#[derive(Clone)]
pub struct SingleToolState {
    pub input_path: Option<String>,
    pub input_name: String,
    pub input_size: u64,
    pub input_preview: Option<(Vec<u8>, u32, u32)>,
    pub result_bytes: Option<Vec<u8>>,
    pub result_preview: Option<(Vec<u8>, u32, u32)>,
    pub result_file_name: Option<String>,
    pub result_lines: Option<Vec<String>>,
    pub saved_path: Option<String>,
    pub uploading: bool,
    pub status_text: String,
    pub status_is_error: bool,
    pub run_gen: u64,
    pub queue: Vec<String>,
    pub queue_idx: usize,
}

impl SingleToolState {
    pub fn new(tool: ConvertTool) -> Self {
        Self {
            input_path: None,
            input_name: String::new(),
            input_size: 0,
            input_preview: None,
            result_bytes: None,
            result_preview: None,
            result_file_name: None,
            result_lines: None,
            saved_path: None,
            uploading: false,
            status_text: tool.default_status().to_string(),
            status_is_error: false,
            run_gen: 0,
            queue: Vec::new(),
            queue_idx: 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConvertView {
    AppGrid,
    ToolDetail,
}

pub struct ConvertState {
    pub view: ConvertView,
    pub active_tool: ConvertTool,
    pub photo_format: ConvertFormat,
    pub states: HashMap<ConvertTool, SingleToolState>,
    pub merge_files: Vec<(String, Vec<u8>)>,
    #[allow(dead_code)]
    pub saving_tool: Option<ConvertTool>,
    #[allow(dead_code)]
    pub drop_highlight: bool,
}

impl ConvertState {
    pub fn new() -> Self {
        let mut states = HashMap::new();
        for &t in &ConvertTool::ALL {
            states.insert(t, SingleToolState::new(t));
        }
        Self {
            view: ConvertView::AppGrid,
            active_tool: ConvertTool::Photo,
            photo_format: ConvertFormat::Webp,
            states,
            merge_files: Vec::new(),
            saving_tool: None,
            drop_highlight: false,
        }
    }

    pub fn tool_state(&self, tool: ConvertTool) -> &SingleToolState {
        self.states.get(&tool).expect("all tools populated")
    }

    pub fn tool_state_mut(&mut self, tool: ConvertTool) -> &mut SingleToolState {
        self.states.get_mut(&tool).expect("all tools populated")
    }

    pub fn current_state(&self) -> &SingleToolState {
        self.tool_state(self.active_tool)
    }

    #[allow(dead_code)]
    pub fn current_state_mut(&mut self) -> &mut SingleToolState {
        self.tool_state_mut(self.active_tool)
    }

    pub fn add_to_queue(&mut self, tool: ConvertTool, paths: &[String]) {
        if tool == ConvertTool::MergePdf {
            for p in paths {
                self.add_merge_file(p);
            }
            return;
        }
        let valid: Vec<String> = paths.iter().filter(|p| tool.accepts(p)).cloned().collect();
        let st = self.tool_state_mut(tool);
        for p in valid {
            if !st.queue.contains(&p) {
                st.queue.push(p);
            }
        }
        let st = self.tool_state(tool);
        if st.input_path.is_none() && !st.queue.is_empty() {
            let first = st.queue[0].clone();
            self.set_input(tool, &first);
        }
    }

    pub fn set_queue_index(&mut self, tool: ConvertTool, idx: usize) {
        let st = self.tool_state(tool);
        if idx < st.queue.len() {
            let path = st.queue[idx].clone();
            self.set_input(tool, &path);
            self.tool_state_mut(tool).queue_idx = idx;
        }
    }

    pub fn next_in_queue(&mut self, tool: ConvertTool) {
        let st = self.tool_state(tool);
        if !st.queue.is_empty() {
            let next = (st.queue_idx + 1) % st.queue.len();
            self.set_queue_index(tool, next);
        }
    }

    pub fn prev_in_queue(&mut self, tool: ConvertTool) {
        let st = self.tool_state(tool);
        if !st.queue.is_empty() {
            let prev = if st.queue_idx == 0 {
                st.queue.len() - 1
            } else {
                st.queue_idx - 1
            };
            self.set_queue_index(tool, prev);
        }
    }

    pub fn set_input(&mut self, tool: ConvertTool, path: &str) {
        let p = Path::new(path);
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let preview = if tool == ConvertTool::Photo {
            std::fs::read(path).ok().and_then(|b| decode_preview_fit(&b, 164, 140))
        } else {
            None
        };

        let st = self.tool_state_mut(tool);
        st.input_path = Some(path.to_string());
        st.input_name = name;
        st.input_size = size;
        st.input_preview = preview;
        st.result_bytes = None;
        st.result_preview = None;
        st.result_file_name = None;
        st.result_lines = None;
        st.saved_path = None;
        st.uploading = false;
        st.status_text = "File ready. Press Convert to start.".to_string();
        st.status_is_error = false;
        st.run_gen += 1;
    }

    pub fn has_active_content(&self) -> bool {
        if self.view == ConvertView::AppGrid {
            return false;
        }
        let st = self.current_state();
        st.input_path.is_some() || st.result_bytes.is_some() || !self.merge_files.is_empty()
    }

    pub fn add_merge_file(&mut self, path: &str) -> bool {
        let p = Path::new(path);
        if !ConvertTool::MergePdf.accepts(path) {
            return false;
        }
        let Ok(data) = std::fs::read(path) else { return false };
        let name = p
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        self.merge_files.push((name, data));

        let count = self.merge_files.len();
        let st = self.tool_state_mut(ConvertTool::MergePdf);
        st.result_bytes = None;
        st.result_preview = None;
        st.result_file_name = None;
        st.result_lines = None;
        st.saved_path = None;
        st.status_is_error = false;
        st.status_text = if count == 1 {
            "1 PDF - add at least 1 more, then press Merge.".to_string()
        } else {
            format!("{} PDFs ready - press Merge.", count)
        };
        true
    }

    pub fn remove_merge_file(&mut self, index: usize) {
        if index < self.merge_files.len() {
            self.merge_files.remove(index);
            let count = self.merge_files.len();
            let st = self.tool_state_mut(ConvertTool::MergePdf);
            st.result_bytes = None;
            st.result_preview = None;
            st.result_file_name = None;
            st.result_lines = None;
            st.saved_path = None;
            st.status_is_error = false;
            st.status_text = if count == 0 {
                ConvertTool::MergePdf.default_status().to_string()
            } else if count == 1 {
                "1 PDF - add at least 1 more, then press Merge.".to_string()
            } else {
                format!("{} PDFs ready - press Merge.", count)
            };
        }
    }

    #[allow(dead_code)]
    pub fn swap_merge_files(&mut self, a: usize, b: usize) {
        if a < self.merge_files.len() && b < self.merge_files.len() && a != b {
            self.merge_files.swap(a, b);
            let st = self.tool_state_mut(ConvertTool::MergePdf);
            st.result_bytes = None;
            st.result_preview = None;
            st.result_file_name = None;
            st.result_lines = None;
            st.saved_path = None;
        }
    }

    pub fn result_file_name(tool: ConvertTool, input_name: &str, format: ConvertFormat) -> String {
        let stem = Path::new(input_name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "result".to_string());
        match tool {
            ConvertTool::Photo => format!("{}.{}", stem, format.file_extension()),
            ConvertTool::MergePdf => "merged_document.pdf".to_string(),
            ConvertTool::PdfToWord => format!("{}.docx", stem),
            ConvertTool::CompressPdf => format!("{}_compressed.pdf", stem),
            ConvertTool::PdfToMd => format!("{}.md", stem),
        }
    }

    pub fn ensure_saved(&mut self, tool: ConvertTool) -> Option<String> {
        let st = self.tool_state_mut(tool);
        if let Some(p) = &st.saved_path {
            if Path::new(p).exists() {
                return Some(p.clone());
            }
        }
        let bytes = st.result_bytes.as_ref()?;
        let name = st.result_file_name.clone().unwrap_or_else(|| "converted".to_string());
        let dir = crate::lan::downloads_dir()?;
        std::fs::create_dir_all(&dir).ok()?;
        let path = crate::lan::unique_path(&dir, &name);
        std::fs::write(&path, bytes).ok()?;
        let s = path.to_string_lossy().into_owned();
        st.saved_path = Some(s.clone());
        Some(s)
    }
}

// ---------------------------------------------------------------------------
// Background Upload / Worker
// ---------------------------------------------------------------------------

pub struct WorkerArgs {
    pub tool: ConvertTool,
    pub gen: u64,
    pub file_data: Vec<u8>,
    pub file_name: String,
    pub format: ConvertFormat,
    pub merge_files: Option<Vec<(String, Vec<u8>)>>,
}

pub fn spawn_convert_worker(
    args: WorkerArgs,
    hwnd: HWND,
    slot: Arc<Mutex<Option<ConvertResult>>>,
) {
    let hwnd_raw = hwnd.0 as isize;
    std::thread::Builder::new()
        .name(format!("aura-convert-{:?}", args.tool))
        .spawn(move || {
            crate::media::init_mta();
            let hwnd = HWND(hwnd_raw as *mut _);
            let tool = args.tool;
            let gen = args.gen;
            let res = run_convert_http(&args);
            {
                let mut guard = slot.lock().unwrap();
                *guard = Some(ConvertResult {
                    tool,
                    gen,
                    result: res,
                });
            }
            unsafe {
                let _ = PostMessageW(hwnd, WM_CONVERT_READY, WPARAM(0), LPARAM(0));
            }
        })
        .expect("spawn convert thread");
}

fn mime_for_name(name: &str) -> &'static str {
    let ext = Path::new(name)
        .extension()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn has_prefix(data: &[u8], prefix: &[u8]) -> bool {
    data.len() >= prefix.len() && &data[..prefix.len()] == prefix
}

fn is_png(data: &[u8]) -> bool {
    has_prefix(data, &[0x89, 0x50, 0x4E, 0x47])
}

fn is_webp(data: &[u8]) -> bool {
    data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP"
}

fn is_avif(data: &[u8]) -> bool {
    data.len() >= 12 && &data[4..8] == b"ftyp" && &data[8..12] == b"avif"
}

fn is_pdf(data: &[u8]) -> bool {
    has_prefix(data, b"%PDF")
}

fn is_docx(data: &[u8]) -> bool {
    has_prefix(data, b"PK")
}

fn run_convert_http(args: &WorkerArgs) -> Result<Vec<u8>, String> {
    let boundary = format!("Aura-Win-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());

    let (path, body, validate_fn): (&str, Vec<u8>, fn(&[u8]) -> bool) = match args.tool {
        ConvertTool::Photo => {
            let mut b = Vec::new();
            b.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            b.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
                    args.file_name
                )
                .as_bytes(),
            );
            b.extend_from_slice(
                format!("Content-Type: {}\r\n\r\n", mime_for_name(&args.file_name)).as_bytes(),
            );
            b.extend_from_slice(&args.file_data);
            b.extend_from_slice(b"\r\n");

            b.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            b.extend_from_slice(b"Content-Disposition: form-data; name=\"format\"\r\n\r\n");
            b.extend_from_slice(args.format.form_value().as_bytes());
            b.extend_from_slice(b"\r\n");
            b.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());

            let v: fn(&[u8]) -> bool = match args.format {
                ConvertFormat::Avif => is_avif,
                ConvertFormat::Webp => is_webp,
                ConvertFormat::Png => is_png,
            };
            ("/api/convert", b, v)
        }
        ConvertTool::PdfToWord => {
            let mut b = Vec::new();
            b.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            b.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
                    args.file_name
                )
                .as_bytes(),
            );
            b.extend_from_slice(b"Content-Type: application/pdf\r\n\r\n");
            b.extend_from_slice(&args.file_data);
            b.extend_from_slice(b"\r\n");
            b.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
            ("/api/pdf/to-word", b, is_docx)
        }
        ConvertTool::CompressPdf => {
            let mut b = Vec::new();
            b.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            b.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
                    args.file_name
                )
                .as_bytes(),
            );
            b.extend_from_slice(b"Content-Type: application/pdf\r\n\r\n");
            b.extend_from_slice(&args.file_data);
            b.extend_from_slice(b"\r\n");
            b.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
            ("/api/pdf/compress", b, is_pdf)
        }
        ConvertTool::PdfToMd => {
            let mut b = Vec::new();
            b.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
            b.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n",
                    args.file_name
                )
                .as_bytes(),
            );
            b.extend_from_slice(b"Content-Type: application/pdf\r\n\r\n");
            b.extend_from_slice(&args.file_data);
            b.extend_from_slice(b"\r\n");
            b.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
            // Handled separately below because returns JSON {"result": text}
            ("/api/pdf/to-md", b, |_| true)
        }
        ConvertTool::MergePdf => {
            let files = args.merge_files.as_ref().ok_or_else(|| "No files to merge".to_string())?;
            if files.len() < 2 {
                return Err("Add at least 2 PDFs to merge.".to_string());
            }
            let mut b = Vec::new();
            for (name, data) in files {
                b.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
                b.extend_from_slice(
                    format!(
                        "Content-Disposition: form-data; name=\"files\"; filename=\"{}\"\r\n",
                        name
                    )
                    .as_bytes(),
                );
                b.extend_from_slice(b"Content-Type: application/pdf\r\n\r\n");
                b.extend_from_slice(data);
                b.extend_from_slice(b"\r\n");
            }
            b.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
            ("/api/pdf/merge", b, is_pdf)
        }
    };

    let client = HttpClient::new().map_err(|e| format!("HttpClient error: {:?}", e))?;
    let headers = client.DefaultRequestHeaders().map_err(|e| format!("{:?}", e))?;
    let _ = headers.Append(
        &HSTRING::from("User-Agent"),
        &HSTRING::from("Aura-Island/1.0 Windows"),
    );

    let writer = DataWriter::new().map_err(|e| format!("{:?}", e))?;
    writer
        .WriteBytes(&body)
        .map_err(|e| format!("{:?}", e))?;
    let buffer = writer.DetachBuffer().map_err(|e| format!("{:?}", e))?;
    let content = HttpBufferContent::CreateFromBuffer(&buffer).map_err(|e| format!("{:?}", e))?;

    let content_headers = content.Headers().map_err(|e| format!("{:?}", e))?;
    let ctype = format!("multipart/form-data; boundary={}", boundary);
    let _ = content_headers.Append(
        &HSTRING::from("Content-Type"),
        &HSTRING::from(&ctype),
    );

    let url_str = format!("{}{}", API_BASE, path);
    let uri = windows::Foundation::Uri::CreateUri(&HSTRING::from(&url_str))
        .map_err(|e| format!("Invalid URI {}: {:?}", url_str, e))?;

    let async_op = client
        .PostAsync(&uri, &content)
        .map_err(|e| format!("PostAsync failed: {:?}", e))?;

    let t0 = Instant::now();
    while async_op.Status().unwrap_or(AsyncStatus::Started) == AsyncStatus::Started {
        if t0.elapsed() > UPLOAD_TIMEOUT {
            let _ = async_op.Cancel();
            return Err("Upload timed out.".to_string());
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let response = async_op
        .GetResults()
        .map_err(|e| format!("Upload error: {:?}", e))?;

    let status = response
        .StatusCode()
        .map_err(|e| format!("{:?}", e))?;

    let resp_content = response
        .Content()
        .map_err(|e| format!("{:?}", e))?;

    let read_op = resp_content
        .ReadAsBufferAsync()
        .map_err(|e| format!("{:?}", e))?;

    let t_read = Instant::now();
    while read_op.Status().unwrap_or(AsyncStatus::Started) == AsyncStatus::Started {
        if t_read.elapsed() > READ_TIMEOUT {
            let _ = read_op.Cancel();
            return Err("Response read timed out.".to_string());
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    let resp_buf = read_op
        .GetResults()
        .map_err(|e| format!("Read buffer failed: {:?}", e))?;

    let reader = DataReader::FromBuffer(&resp_buf).map_err(|e| format!("{:?}", e))?;
    let len = reader.UnconsumedBufferLength().map_err(|e| format!("{:?}", e))? as usize;
    let mut bytes = vec![0u8; len];
    reader
        .ReadBytes(&mut bytes)
        .map_err(|e| format!("{:?}", e))?;

    if status.0 != 200 {
        let msg = String::from_utf8_lossy(&bytes);
        return Err(format!("Server error (HTTP {}): {}", status.0, msg));
    }

    if args.tool == ConvertTool::PdfToMd {
        let text = String::from_utf8_lossy(&bytes);
        // Parse JSON {"result": "..."}
        let parsed_result = if let Some(idx) = text.find("\"result\":") {
            let rest = text[idx + 9..].trim();
            if rest.starts_with('"') {
                let end = rest[1..].find('"').unwrap_or(rest.len() - 1);
                rest[1..=end].replace("\\n", "\n").replace("\\\"", "\"")
            } else {
                rest.to_string()
            }
        } else {
            text.to_string()
        };
        if parsed_result.trim().is_empty() {
            return Err("No text found in this PDF.".to_string());
        }
        return Ok(parsed_result.into_bytes());
    }

    if !validate_fn(&bytes) {
        return Err("Server returned a result Aura could not read.".to_string());
    }

    Ok(bytes)
}
