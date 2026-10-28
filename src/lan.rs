//! LAN share (Phase 3b): Aura-to-Aura file sharing, protocol-identical
//! to Aura Mac (LANShare.swift): UDP broadcast discovery on port 48124
//! + plain HTTP receive on the first free port of 48125-48130, receiver
//! approval dialog + one-time token, files land in
//! %USERPROFILE%\Downloads\Aura with Finder-style numbering
//! ("name 1.ext"). Plaintext, same-network only, one file per send.
//! No new crates: std::net + serde_json only.
//!
//! Threading: one UDP listener thread, one announcer thread, one HTTP
//! accept thread + one thread per connection. The approval MessageBox
//! runs on the connection thread (Mac runs it on its main thread; on
//! Windows a MessageBox has its own modal loop, so the pill never
//! freezes). If no HTTP port binds, receiving is off and nothing is
//! announced (Mac behavior: it can still send).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use windows::core::PCWSTR;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDYES, MB_ICONERROR, MB_ICONQUESTION, MB_OK, MB_YESNO,
};

pub const DISCOVERY_PORT: u16 = 48124;
const HTTP_PORTS: [u16; 6] = [48125, 48126, 48127, 48128, 48129, 48130];
const PEER_TTL: Duration = Duration::from_secs(10);
const ANNOUNCE_INTERVAL: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const UPLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(300);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CHUNK: usize = 64 * 1024;
const MAX_HEADER_BYTES: usize = 16384;

/// A discovered Aura peer (Mac or Windows).
#[derive(Clone, Debug)]
pub struct LanPeer {
    pub id: String,
    pub name: String,
    pub address: String, // IPv4 as seen in the announce packet's source
    pub port: u16,
    pub last_seen: Instant,
}

/// An approved-but-not-yet-uploaded transfer. The token is removed
/// from the table the moment the /aura-share/file request arrives
/// (single use), before anything is written to disk.
struct PendingTransfer {
    file_name: String,
}

pub struct LanShare {
    own_id: String,
    own_name: String,
    http_port: Option<u16>,
    peers: Arc<Mutex<HashMap<String, LanPeer>>>,
    tokens: Arc<Mutex<HashMap<String, PendingTransfer>>>,
    token_counter: AtomicU64,
}

// ---------------------------------------------------------------- paths

/// %APPDATA%\Aura (same rule as shelf.rs: APPDATA, else
/// %USERPROFILE%\AppData\Roaming). Duplicated here because shelf.rs
/// is not shared with this module.
fn aura_dir() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("AppData").join("Roaming"))
        })?;
    Some(base.join("Aura"))
}

/// %USERPROFILE%\Downloads\Aura (same rule as Aura Mac's Downloads/Aura).
fn downloads_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        return Some(home.join("Downloads").join("Aura"));
    }
    if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
        return Some(PathBuf::from(format!("{}{}", drive, path)).join("Downloads").join("Aura"));
    }
    None
}

/// Persistent peer id in %APPDATA%\Aura\peer.json, created once.
/// No uuid crate: timestamp + pid in hex (format is free, stability
/// is what matters). Falls back to a fresh id if the file is
/// unreadable or unwritable.
fn load_or_create_peer_id() -> String {
    if let Some(dir) = aura_dir() {
        let path = dir.join("peer.json");
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(id) = v.get("id").and_then(|x| x.as_str()) {
                    if !id.is_empty() {
                        return id.to_string();
                    }
                }
            }
        }
        let id = format!("{:x}-{:x}", now_millis(), std::process::id());
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(&path, serde_json::json!({ "id": id }).to_string());
        }
        return id;
    }
    format!("{:x}-{:x}", now_millis(), std::process::id())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mac-identical size string for the approval dialog.
fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1048576.0)
    } else {
        format!("{:.1} GB", bytes as f64 / 1073741824.0)
    }
}

/// Finder-style unique path: "name.ext", then "name 1.ext",
/// "name 2.ext", ... Directory components in a peer-supplied name
/// are stripped first (a peer name must never traverse out of
/// Downloads\Aura); an empty name becomes "shared-file".
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let base_name = Path::new(name)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "shared-file".to_string());
    let candidate = dir.join(&base_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = Path::new(&base_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = Path::new(&base_name)
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut i = 1;
    loop {
        let numbered = if ext.is_empty() {
            format!("{} {}", stem, i)
        } else {
            format!("{} {}.{}", stem, i, ext)
        };
        let c = dir.join(numbered);
        if !c.exists() {
            return c;
        }
        i += 1;
    }
}

// ------------------------------------------------------------ dialogs

/// Plain error popup (used by the send worker thread; success shows
/// nothing, per the plan: the receiver dialog + the landed file are
/// the confirmation).
pub fn show_error(msg: &str) {
    unsafe {
        let text: Vec<u16> = msg.encode_utf16().chain(std::iter::once(0)).collect();
        let caption: Vec<u16> = "Aura".encode_utf16().chain(std::iter::once(0)).collect();
        let _ = MessageBoxW(
            HWND::default(),
            PCWSTR::from_raw(text.as_ptr()),
            PCWSTR::from_raw(caption.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

/// Receiver approval dialog. Returns true only on Yes. The message
/// format matches Aura Mac exactly.
fn ask_approval(sender: &str, file: &str, size: u64) -> bool {
    let text = format!(
        "{} wants to send '{}' ({}). Accept this file?",
        sender,
        file,
        format_size(size)
    );
    let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let cap_w: Vec<u16> = "Aura".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(
            HWND::default(),
            PCWSTR::from_raw(text_w.as_ptr()),
            PCWSTR::from_raw(cap_w.as_ptr()),
            MB_YESNO | MB_ICONQUESTION,
        ) == IDYES
    }
}

// --------------------------------------------------------- http utils

struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>, // keys lowercased
    body_prefix: Vec<u8>,             // bytes already read past the header
    content_length: usize,
}

fn find_subslice(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Read until the header terminator \r\n\r\n (headers capped at
/// 16KB; anything after the terminator is returned as body_prefix).
/// None on EOF / read error / oversize headers.
fn read_request(stream: &mut TcpStream) -> Option<HttpRequest> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if buf.len() > MAX_HEADER_BYTES {
            return None;
        }
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).into_owned();
            let body_prefix = buf[pos + 4..].to_vec();
            let mut lines = head.lines();
            let request_line = lines.next()?;
            let mut parts = request_line.split_whitespace();
            let method = parts.next()?.to_string();
            let path = parts.next()?.to_string();
            let mut headers = HashMap::new();
            for line in lines {
                if let Some((k, v)) = line.split_once(':') {
                    headers.insert(k.trim().to_lowercase(), v.trim().to_string());
                }
            }
            let content_length = headers
                .get("content-length")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            return Some(HttpRequest {
                method,
                path,
                headers,
                body_prefix,
                content_length,
            });
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    }
}

/// Read exactly content_length body bytes (body_prefix counts toward
/// it; a longer prefix is truncated). None on EOF / read error.
fn read_body(stream: &mut TcpStream, prefix: Vec<u8>, content_length: usize) -> Option<Vec<u8>> {
    let mut body = prefix;
    if body.len() > content_length {
        body.truncate(content_length);
        return Some(body);
    }
    let mut chunk = [0u8; 8192];
    while body.len() < content_length {
        let want = (content_length - body.len()).min(chunk.len());
        match stream.read(&mut chunk[..want]) {
            Ok(0) => return None,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    }
    Some(body)
}

fn send_json(stream: &mut TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status,
        reason,
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

// ------------------------------------------------------------- server

impl LanShare {
    /// Bind the first free HTTP port (48125-48130), then start the
    /// UDP listener, the announcer, and the HTTP accept loop. If no
    /// HTTP port binds, receiving is off and nothing is announced,
    /// but the returned handle can still send.
    pub fn start() -> Arc<Self> {
        let own_id = load_or_create_peer_id();
        let own_name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".to_string());
        let mut listener: Option<(u16, TcpListener)> = None;
        for port in HTTP_PORTS {
            if let Ok(l) = TcpListener::bind(("0.0.0.0", port)) {
                listener = Some((port, l));
                break;
            }
        }
        let http_port = listener.as_ref().map(|(p, _)| *p);
        let share = Arc::new(Self {
            own_id,
            own_name,
            http_port,
            peers: Arc::new(Mutex::new(HashMap::new())),
            tokens: Arc::new(Mutex::new(HashMap::new())),
            token_counter: AtomicU64::new(0),
        });
        if let Some((_, l)) = listener {
            let s = share.clone();
            std::thread::spawn(move || accept_loop(s, l));
            let s = share.clone();
            std::thread::spawn(move || udp_listener(s));
            let s = share.clone();
            std::thread::spawn(move || announcer(s));
        }
        share
    }

    /// Live peers (TTL-pruned), sorted by name, for the Share submenu.
    pub fn peers(&self) -> Vec<LanPeer> {
        let mut out = Vec::new();
        if let Ok(peers) = self.peers.lock() {
            for p in peers.values() {
                if p.last_seen.elapsed() <= PEER_TTL {
                    out.push(p.clone());
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    fn prune_peers(&self) {
        if let Ok(mut peers) = self.peers.lock() {
            peers.retain(|_, p| p.last_seen.elapsed() <= PEER_TTL);
        }
    }

    fn new_token(&self) -> String {
        let n = self.token_counter.fetch_add(1, Ordering::Relaxed);
        format!("{:x}-{:x}-{:x}", now_millis(), std::process::id(), n)
    }

    fn handle_connection(&self, mut stream: TcpStream) {
        // Bounds both the header read and the body stream (receiver
        // side of the 300s upload stall timeout).
        let _ = stream.set_read_timeout(Some(UPLOAD_STALL_TIMEOUT));
        let Some(req) = read_request(&mut stream) else {
            return;
        };
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/aura-share/request") => {
                let Some(body) = read_body(&mut stream, req.body_prefix, req.content_length)
                else {
                    return;
                };
                self.handle_share_request(&mut stream, &body);
            }
            ("POST", "/aura-share/file") => {
                self.handle_share_file(&mut stream, &req);
            }
            _ => {
                let body = serde_json::json!({"ok": false, "error": "unknown path"}).to_string();
                send_json(&mut stream, 404, &body);
            }
        }
    }

    fn handle_share_request(&self, stream: &mut TcpStream, body: &[u8]) {
        let mut bad = || {
            let b = serde_json::json!({"ok": false, "error": "bad request"}).to_string();
            send_json(stream, 400, &b);
        };
        let v: serde_json::Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => {
                bad();
                return;
            }
        };
        let (Some(sender), Some(file_name), Some(file_size)) = (
            v.get("senderName").and_then(|x| x.as_str()),
            v.get("fileName").and_then(|x| x.as_str()),
            v.get("fileSize").and_then(|x| x.as_u64()),
        ) else {
            bad();
            return;
        };
        // senderId is required by the protocol but not displayed or
        // stored (the token carries the approval); a missing one is
        // still a bad request, like on Mac.
        if v.get("senderId").and_then(|x| x.as_str()).is_none() {
            bad();
            return;
        }
        if !ask_approval(sender, file_name, file_size) {
            let b = serde_json::json!({"ok": true, "accepted": false}).to_string();
            send_json(stream, 200, &b);
            return;
        }
        let token = self.new_token();
        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert(
                token.clone(),
                PendingTransfer {
                    file_name: file_name.to_string(),
                },
            );
        }
        let b = serde_json::json!({"ok": true, "accepted": true, "token": token}).to_string();
        send_json(stream, 200, &b);
    }

    fn handle_share_file(&self, stream: &mut TcpStream, req: &HttpRequest) {
        let mut forbidden = |err: &str| {
            let b = serde_json::json!({"ok": false, "error": err}).to_string();
            send_json(stream, 403, &b);
        };
        // Token is checked (and consumed) BEFORE anything is written.
        let Some(token) = req.headers.get("x-aura-token") else {
            forbidden("missing token");
            return;
        };
        let pending = {
            let Ok(mut tokens) = self.tokens.lock() else {
                forbidden("invalid token");
                return;
            };
            tokens.remove(token)
        };
        let Some(pending) = pending else {
            forbidden("invalid token");
            return;
        };
        let Some(dir) = downloads_dir() else {
            let b = serde_json::json!({"ok": false, "error": "cannot create folder"}).to_string();
            send_json(stream, 500, &b);
            return;
        };
        if std::fs::create_dir_all(&dir).is_err() {
            let b = serde_json::json!({"ok": false, "error": "cannot create folder"}).to_string();
            send_json(stream, 500, &b);
            return;
        }
        let dest = unique_path(&dir, &pending.file_name);
        let mut file = match std::fs::File::create(&dest) {
            Ok(f) => f,
            Err(_) => {
                let b = serde_json::json!({"ok": false, "error": "cannot create file"}).to_string();
                send_json(stream, 500, &b);
                return;
            }
        };
        // Stream the body to disk; on any failure mid-transfer the
        // partial file is deleted and the connection is closed
        // without a response (Mac behavior).
        let mut remaining = req.content_length;
        let mut prefix = req.body_prefix.clone();
        if prefix.len() > remaining {
            prefix.truncate(remaining);
        }
        if !prefix.is_empty() {
            if file.write_all(&prefix).is_err() {
                let _ = std::fs::remove_file(&dest);
                return;
            }
            remaining -= prefix.len();
        }
        let mut chunk = [0u8; 8192];
        while remaining > 0 {
            let want = remaining.min(chunk.len());
            match stream.read(&mut chunk[..want]) {
                Ok(0) => {
                    let _ = std::fs::remove_file(&dest);
                    return;
                }
                Ok(n) => {
                    if file.write_all(&chunk[..n]).is_err() {
                        let _ = std::fs::remove_file(&dest);
                        return;
                    }
                    remaining -= n;
                }
                Err(_) => {
                    let _ = std::fs::remove_file(&dest);
                    return;
                }
            }
        }
        let _ = file.flush();
        let b = serde_json::json!({"ok": true}).to_string();
        send_json(stream, 200, &b);
    }

    /// Send one file to a peer: request (approval) first, then a
    /// streaming 64KB-chunk upload with Content-Length. Errors are
    /// human-readable strings for the MessageBox; success is silent.
    pub fn send_file(&self, peer: &LanPeer, path: &Path) -> Result<(), String> {
        let meta = std::fs::metadata(path).map_err(|_| "Cannot read file.".to_string())?;
        if !meta.is_file() {
            return Err("Only files can be shared.".to_string());
        }
        let size = meta.len();
        let file_name = path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".to_string());
        let req_body = serde_json::json!({
            "senderName": self.own_name,
            "senderId": self.own_id,
            "fileName": file_name,
            "fileSize": size,
        })
        .to_string();
        let resp = http_post(
            &peer.address,
            peer.port,
            "/aura-share/request",
            &[("Content-Type", "application/json")],
            req_body.as_bytes(),
            REQUEST_TIMEOUT,
        )?;
        let v: serde_json::Value =
            serde_json::from_slice(&resp).map_err(|_| "Bad response from peer.".to_string())?;
        if v.get("ok").and_then(|x| x.as_bool()) != Some(true) {
            return Err("Connection failed.".to_string());
        }
        if v.get("accepted").and_then(|x| x.as_bool()) != Some(true) {
            return Err("Declined.".to_string());
        }
        let token = v
            .get("token")
            .and_then(|x| x.as_str())
            .ok_or_else(|| "Peer did not return a token.".to_string())?
            .to_string();
        let mut file = std::fs::File::open(path).map_err(|_| "Cannot open file.".to_string())?;
        let mut stream = connect_peer(&peer.address, peer.port)?;
        let _ = stream.set_read_timeout(Some(UPLOAD_STALL_TIMEOUT));
        let _ = stream.set_write_timeout(Some(UPLOAD_STALL_TIMEOUT));
        let head = format!(
            "POST /aura-share/file HTTP/1.1\r\nHost: {}:{}\r\nX-Aura-Token: {}\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            peer.address, peer.port, token, size
        );
        stream
            .write_all(head.as_bytes())
            .map_err(|_| "Send failed.".to_string())?;
        let mut buf = [0u8; CHUNK];
        loop {
            let n = file.read(&mut buf).map_err(|_| "Send failed.".to_string())?;
            if n == 0 {
                break;
            }
            stream
                .write_all(&buf[..n])
                .map_err(|_| "Send failed.".to_string())?;
        }
        let _ = stream.flush();
        // Upload-phase read errors collapse to "Send failed." (Mac
        // string); the request phase keeps the timeout distinction.
        let resp = read_response_body(&mut stream).map_err(|_| "Send failed.".to_string())?;
        let v: serde_json::Value =
            serde_json::from_slice(&resp).map_err(|_| "Send failed.".to_string())?;
        if v.get("ok").and_then(|x| x.as_bool()) == Some(true) {
            Ok(())
        } else {
            Err("Send failed.".to_string())
        }
    }
}

// ----------------------------------------------------------- threads

fn accept_loop(share: Arc<LanShare>, listener: TcpListener) {
    loop {
        let Ok((stream, _)) = listener.accept() else {
            continue;
        };
        let s = share.clone();
        std::thread::spawn(move || s.handle_connection(stream));
    }
}

/// UDP discovery listener. A bind failure (port already held) just
/// ends this thread: announcing still works, so peers can find us
/// and send even if we cannot discover them.
fn udp_listener(share: Arc<LanShare>) {
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) else {
        return;
    };
    let mut buf = [0u8; 2048];
    loop {
        let Ok((n, src)) = sock.recv_from(&mut buf) else {
            continue;
        };
        // Mac listens on AF_INET only, so IPv6 sources are ignored.
        if !src.ip().is_ipv4() {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("aura-hello") {
            continue;
        }
        let (Some(id), Some(name), Some(port)) = (
            v.get("id").and_then(|x| x.as_str()),
            v.get("name").and_then(|x| x.as_str()),
            v.get("port").and_then(|x| x.as_u64()),
        ) else {
            continue;
        };
        if id == share.own_id {
            continue;
        }
        let peer = LanPeer {
            id: id.to_string(),
            name: name.to_string(),
            address: src.ip().to_string(),
            port: port as u16,
            last_seen: Instant::now(),
        };
        if let Ok(mut peers) = share.peers.lock() {
            peers.insert(peer.id.clone(), peer);
        }
    }
}

fn announcer(share: Arc<LanShare>) {
    let Ok(sock) = UdpSocket::bind(("0.0.0.0", 0)) else {
        return;
    };
    let _ = sock.set_broadcast(true);
    loop {
        if let Some(port) = share.http_port {
            let msg = serde_json::json!({
                "type": "aura-hello",
                "id": share.own_id,
                "name": share.own_name,
                "port": port,
            });
            let _ = sock.send_to(
                msg.to_string().as_bytes(),
                ("255.255.255.255", DISCOVERY_PORT),
            );
        }
        share.prune_peers();
        std::thread::sleep(ANNOUNCE_INTERVAL);
    }
}

// ------------------------------------------------------------- client

fn connect_peer(address: &str, port: u16) -> Result<TcpStream, String> {
    let ip: std::net::IpAddr = address
        .parse()
        .map_err(|_| "Connection failed.".to_string())?;
    let addr = std::net::SocketAddr::new(ip, port);
    TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|_| "Connection failed.".to_string())
}

/// Read a response until the server closes (Connection: close) and
/// return its body. A read timeout maps to Mac's request-timeout
/// string; other errors map to a connection failure.
fn read_response_body(stream: &mut TcpStream) -> Result<Vec<u8>, String> {
    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => raw.extend_from_slice(&chunk[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                return Err("Timed out (no answer)".to_string());
            }
            Err(_) => return Err("Connection failed.".to_string()),
        }
    }
    if let Some(pos) = find_subslice(&raw, b"\r\n\r\n") {
        Ok(raw[pos + 4..].to_vec())
    } else {
        Err("Bad response from peer.".to_string())
    }
}

fn http_post(
    address: &str,
    port: u16,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let mut stream = connect_peer(address, port)?;
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let mut head = format!(
        "POST {} HTTP/1.1\r\nHost: {}:{}\r\nContent-Length: {}\r\nConnection: close\r\n",
        path,
        address,
        port,
        body.len()
    );
    for (k, v) in extra_headers {
        head.push_str(&format!("{}: {}\r\n", k, v));
    }
    head.push_str("\r\n");
    stream
        .write_all(head.as_bytes())
        .map_err(|_| "Connection failed.".to_string())?;
    stream
        .write_all(body)
        .map_err(|_| "Connection failed.".to_string())?;
    let _ = stream.flush();
    read_response_body(&mut stream)
}
