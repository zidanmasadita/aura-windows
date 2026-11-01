//! Lyrics (Phase 1b + Nusa RE upgrade):
//! - Non-blocking async MTA worker thread (`fetch_async`) that posts
//!   `WM_LYRICS_READY` (`0x800F`, matching `nusa.exe` `0x14000B9AA`) to the HWND
//!   when complete so clicking "Lirik" or switching songs never freezes the UI.
//! - LRCLIB `/api/get` + Nusa's `/api/search?track_name=...&artist_name=...`
//!   array fallback (catches remastered/featured track titles where exact `/api/get`
//!   returns 404) + `lyrics.ovh` plain fallback.
//! - Custom `User-Agent` header on `Windows.Web.Http.HttpClient` as required by
//!   LRCLIB API etiquette (`nusa.exe` `0x0b0580`).
//! - Per-song millisecond nudge persisted in `%APPDATA%/Aura/lyric-nudges.json`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use windows::core::{h, HSTRING};
use windows::Foundation::Uri;
use windows::Web::Http::HttpClient;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

/// Posted to the main HWND when background lyrics fetch completes (Nusa `0x800F`).
pub const WM_LYRICS_READY: u32 = 0x800F;

/// One lyric line. `time_ms` is None for unsynced (plain) lyrics.
#[derive(Clone, Debug)]
pub struct LyricLine {
    pub time_ms: Option<i64>,
    pub text: String,
}

/// Fetched lyrics for one song. `lines` may be empty (not available).
#[derive(Clone, Debug)]
pub struct LyricsCache {
    pub key: String,
    pub lines: Vec<LyricLine>,
    pub synced: bool,
    pub nudge_ms: i64,
}

pub fn key_for(title: &str, artist: &str) -> String {
    format!("{}|{}", title, artist)
}

/// Index of the current line for a position (ms): last line whose time
/// is <= position, or 0 when all lines are still ahead (intro).
pub fn current_index(lines: &[LyricLine], position_ms: i64) -> usize {
    let mut current = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if let Some(t) = line.time_ms {
            if t <= position_ms {
                current = i;
            } else {
                break;
            }
        }
    }
    current
}

/// Spawn a non-blocking MTA worker thread to fetch lyrics and post
/// `WM_LYRICS_READY` (`0x800F`) to `hwnd` when done (`nusa.exe` `0x14000B9AA`).
pub fn fetch_async(
    hwnd: HWND,
    slot: Arc<Mutex<Option<LyricsCache>>>,
    title: String,
    artist: String,
    album: String,
    duration_secs: f64,
) {
    let hwnd_raw = hwnd.0 as isize;
    std::thread::spawn(move || {
        crate::media::init_mta();
        let result = fetch(&title, &artist, &album, duration_secs);
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(result);
        }
        unsafe {
            let hwnd = HWND(hwnd_raw as *mut core::ffi::c_void);
            let _ = PostMessageW(hwnd, WM_LYRICS_READY, WPARAM(0), LPARAM(0));
        }
    });
}

/// Fetch lyrics for a song (runs on the MTA worker thread).
/// Order:
/// 1. LRCLIB `/api/get` (synced preferred)
/// 2. LRCLIB `/api/search` (Nusa's primary search endpoint — handles slight title/album diffs)
/// 3. LRCLIB plainLyrics fallback
/// 4. `lyrics.ovh` plain fallback
pub fn fetch(title: &str, artist: &str, album: &str, duration_secs: f64) -> LyricsCache {
    let key = key_for(title, artist);
    let nudge_ms = load_nudge(&key);

    // 1. LRCLIB exact /api/get (with duration: LRCLIB then only matches the
    //    recording within ±2s, i.e. the exact version that is playing).
    if let Ok(text) = http_get(&lrclib_get_url(title, artist, album, duration_secs)) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(synced) = value.get("syncedLyrics").and_then(|v| v.as_str()) {
                if !synced.trim().is_empty() {
                    let lines = parse_lrc(synced);
                    if !lines.is_empty() {
                        return LyricsCache {
                            key,
                            lines,
                            synced: true,
                            nudge_ms,
                        };
                    }
                }
            }
        }
    }

    // 2. LRCLIB /api/search: score every candidate (Mac `Lyrics.score`) instead of
    //    taking the first synced hit, so a remix/live/sped-up version with a
    //    different duration never wins over the recording actually playing
    //    (a version mismatch = constant offset = "lyrics not synced").
    if let Ok(text) = http_get(&lrclib_search_url(title, artist)) {
        if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&text)
        {
            let mut ranked: Vec<(i32, &serde_json::Value)> = items
                .iter()
                .map(|it| (score_candidate(it, title, artist, duration_secs), it))
                .collect();
            ranked.sort_by(|a, b| b.0.cmp(&a.0));
            let has_synced = |it: &serde_json::Value| {
                it.get("syncedLyrics")
                    .and_then(|v| v.as_str())
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            };
            let close_dur = |it: &serde_json::Value| {
                it.get("duration")
                    .and_then(|v| v.as_f64())
                    .map(|d| d <= 0.0 || (d - duration_secs).abs() <= 8.0)
                    .unwrap_or(true)
            };
            // Only reject far-duration candidates when a close synced one exists
            // (a YouTube video with a long intro still gets synced lyrics).
            let strict = duration_secs > 0.0
                && ranked.iter().any(|(_, it)| has_synced(*it) && close_dur(*it));
            for (_, item) in &ranked {
                // Reject candidates whose length is clearly another recording.
                if strict && !close_dur(*item) {
                    continue;
                }
                if let Some(synced) = item.get("syncedLyrics").and_then(|v| v.as_str()) {
                    if !synced.trim().is_empty() {
                        let lines = parse_lrc(synced);
                        if !lines.is_empty() {
                            return LyricsCache {
                                key,
                                lines,
                                synced: true,
                                nudge_ms,
                            };
                        }
                    }
                }
            }
        }
    }

    // No synced lyrics found in LRCLIB: do NOT fall back to plain lyrics per user rule.
    LyricsCache {
        key,
        lines: Vec::new(),
        synced: false,
        nudge_ms,
    }
}

fn lrclib_get_url(title: &str, artist: &str, album: &str, duration_secs: f64) -> String {
    let mut url = format!(
        "https://lrclib.net/api/get?artist_name={}&track_name={}",
        url_encode(artist),
        url_encode(title)
    );
    if !album.trim().is_empty() {
        url.push_str(&format!("&album_name={}", url_encode(album)));
    }
    if duration_secs > 0.0 {
        url.push_str(&format!("&duration={}", duration_secs.round() as i64));
    }
    url
}

/// Candidate score (Mac `Lyrics.score`): title/artist match dominate,
/// synced lyrics and a close duration are bonuses.
fn score_candidate(item: &serde_json::Value, title: &str, artist: &str, duration_secs: f64) -> i32 {
    let norm = |s: &str| s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ");
    let (t, a) = (norm(title), norm(artist));
    let it = norm(item.get("trackName").and_then(|v| v.as_str()).unwrap_or(""));
    let ia = norm(item.get("artistName").and_then(|v| v.as_str()).unwrap_or(""));
    let mut s = 0;
    if it == t {
        s += 100;
    } else if !t.is_empty() && (it.contains(&t) || t.contains(&it)) {
        s += 50;
    }
    if ia == a {
        s += 60;
    } else if !a.is_empty() && (ia.contains(&a) || a.contains(&ia)) {
        s += 30;
    }
    if item
        .get("syncedLyrics")
        .and_then(|v| v.as_str())
        .map(|x| !x.trim().is_empty())
        .unwrap_or(false)
    {
        s += 40;
    }
    if duration_secs > 0.0 {
        if let Some(d) = item.get("duration").and_then(|v| v.as_f64()) {
            if d > 0.0 {
                let delta = (d - duration_secs).abs();
                s += if delta <= 1.0 {
                    60
                } else if delta <= 3.0 {
                    40
                } else if delta <= 8.0 {
                    10
                } else {
                    -80
                };
            }
        }
    }
    s
}

fn lrclib_search_url(title: &str, artist: &str) -> String {
    format!(
        "https://lrclib.net/api/search?track_name={}&artist_name={}",
        url_encode(title),
        url_encode(artist)
    )
}

/// Percent-encode a string for a URL (query value or path segment).
fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for &b in s.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~';
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// HTTP GET via WinRT `HttpClient` on an MTA thread with `User-Agent` header
/// (`nusa.exe` `0x14000B9AA`).
fn http_get(url: &str) -> windows::core::Result<String> {
    let client = HttpClient::new()?;
    if let Ok(headers) = client.DefaultRequestHeaders() {
        let _ = headers.Append(h!("User-Agent"), h!("Aura-Notch/1.0 (Windows)"));
    }
    let uri = Uri::CreateUri(&HSTRING::from(url))?;
    let text = client.GetStringAsync(&uri)?.get()?;
    Ok(text.to_string())
}

/// Parse LRC text into timestamped lines (ported from Mac `Lyrics.parseSynced`):
/// - `[offset:+/-ms]` honored (positive = lyrics sooner, LRC spec).
/// - Multiple stamps per line (`[00:12.00][00:45.00] chorus`) -> one entry each.
/// - Metadata brackets (`[ti:]`, `[ar:]`) skipped; enhanced `<mm:ss.xx>` word tags stripped.
/// - Non-vocal markers (blank, `♪`, `(Instrumental)`) dropped so the last sung
///   line persists through melody parts.
fn parse_lrc(text: &str) -> Vec<LyricLine> {
    let mut offset_ms: i64 = 0;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(rest) = line.strip_prefix("[offset:") {
            if let Some(num) = rest.strip_suffix(']') {
                if let Ok(v) = num.trim().trim_start_matches('+').parse::<i64>() {
                    offset_ms = v;
                }
            }
        }
    }
    let mut lines: Vec<LyricLine> = Vec::new();
    for raw in text.lines() {
        let mut line = raw.trim_end_matches('\r').trim();
        let mut times: Vec<i64> = Vec::new();
        while line.starts_with('[') {
            let Some(close) = line.find(']') else {
                break;
            };
            let inside = &line[1..close];
            let rest = line[close + 1..].trim_start();
            if let Some(t) = parse_lrc_time(inside) {
                times.push((t - offset_ms).max(0));
                line = rest;
            } else if times.is_empty() {
                line = rest;
            } else {
                break;
            }
        }
        if times.is_empty() {
            continue;
        }
        let mut lyric = line.trim().to_string();
        while let Some(start) = lyric.find('<') {
            if let Some(end_rel) = lyric[start..].find('>') {
                let end = start + end_rel + 1;
                lyric.replace_range(start..end, "");
            } else {
                break;
            }
        }
        let lyric = lyric.trim().to_string();
        if is_non_vocal_marker(&lyric) {
            continue;
        }
        for t in times {
            lines.push(LyricLine {
                time_ms: Some(t),
                text: lyric.clone(),
            });
        }
    }
    lines.sort_by_key(|l| l.time_ms.unwrap_or(0));
    lines
}

/// Mac `Lyrics.isNonVocalMarker`.
fn is_non_vocal_marker(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    const SYMBOLS: &str = "♪♫♩♬🎵🎶()[]{}* ";
    if t.chars().all(|c| SYMBOLS.contains(c)) {
        return true;
    }
    let wrappers = "()[]{}";
    let (Some(first), Some(last)) = (t.chars().next(), t.chars().last()) else {
        return false;
    };
    if !wrappers.contains(first) || !wrappers.contains(last) {
        return false;
    }
    let inner: String = t
        .trim_matches(|c| wrappers.contains(c))
        .chars()
        .filter(|c| !"♪♫♩♬🎵🎶* ".contains(*c))
        .collect::<String>()
        .to_lowercase();
    matches!(
        inner.trim(),
        "instrumental" | "music" | "musik" | "interlude" | "intro" | "outro"
    )
}

fn parse_lrc_time(s: &str) -> Option<i64> {
    let (mm, rest) = s.split_once(':')?;
    let minutes: i64 = mm.trim().parse().ok()?;
    let (ss, frac) = match rest.split_once('.') {
        Some((a, b)) => (a, Some(b)),
        None => (rest, None),
    };
    let seconds: i64 = ss.trim().parse().ok()?;
    let millis: i64 = match frac {
        None => 0,
        Some(f) => {
            let digits = f.trim();
            if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let value: i64 = digits.parse().ok()?;
            match digits.len() {
                1 => value * 100,
                2 => value * 10,
                3 => value,
                _ => value / 10i64.pow((digits.len() - 3) as u32),
            }
        }
    };
    Some(minutes * 60_000 + seconds * 1_000 + millis)
}

fn nudges_path() -> Option<PathBuf> {
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            return Some(
                PathBuf::from(appdata)
                    .join("Aura")
                    .join("lyric-nudges.json"),
            );
        }
    }
    if let Ok(profile) = std::env::var("USERPROFILE") {
        if !profile.is_empty() {
            return Some(
                PathBuf::from(profile)
                    .join("AppData")
                    .join("Roaming")
                    .join("Aura")
                    .join("lyric-nudges.json"),
            );
        }
    }
    None
}

pub fn load_nudge(key: &str) -> i64 {
    let Some(path) = nudges_path() else {
        return 0;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    value.get(key).and_then(|v| v.as_i64()).unwrap_or(0)
}

#[allow(dead_code)]
pub fn save_nudge(key: &str, nudge_ms: i64) {
    let Some(path) = nudges_path() else {
        return;
    };
    let mut map: HashMap<String, i64> = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            if let Some(obj) = value.as_object() {
                for (k, v) in obj {
                    if let Some(n) = v.as_i64() {
                        map.insert(k.clone(), n);
                    }
                }
            }
        }
    }
    map.insert(key.to_string(), nudge_ms);
    let mut obj = serde_json::Map::new();
    for (k, v) in map {
        obj.insert(k, serde_json::Value::from(v));
    }
    let text = serde_json::Value::Object(obj).to_string();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, text);
}
