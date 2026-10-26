//! Windows integrations. Each watcher runs on its own thread and pushes events to the island webview.

use base64::Engine;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

use windows::core::{BSTR, PCWSTR, PWSTR, w};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as SessionManager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as PlaybackStatus,
};
use windows::Storage::Streams::DataReader;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HGLOBAL, HWND, POINT, RECT};
use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoSetProxyBlanket, CLSCTX_ALL,
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Variant::{VariantToInt32, VARIANT};
use windows::Win32::System::Wmi::{
    IWbemLocator, WbemLocator, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayout, RegisterHotKey, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::Shell::{
    DragQueryFileW, SHQueryUserNotificationState, HDROP, QUNS_BUSY, QUNS_PRESENTATION_MODE,
    QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowRect,
    GetWindowThreadProcessId, GetWindowLongW, IsIconic, GWL_STYLE, WS_CAPTION, MB_ICONASTERISK, MSG, WM_HOTKEY,
};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::Globalization::{GetLocaleInfoW, LOCALE_SABBREVLANGNAME};
use windows::Win32::System::Diagnostics::Debug::MessageBeep;

pub(crate) fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

pub(crate) fn emit(app: &AppHandle, ev: &str, payload: serde_json::Value) {
    let _ = app.emit_to("island", ev, payload);
}

pub fn spawn_all(app: &AppHandle) {
    let a = app.clone();
    std::thread::spawn(move || media_loop(a));
    let a = app.clone();
    std::thread::spawn(move || volume_loop(a));
    let a = app.clone();
    std::thread::spawn(move || layout_loop(a));
    let a = app.clone();
    std::thread::spawn(move || brightness_loop(a));
    let a = app.clone();
    std::thread::spawn(move || clipboard_loop(a));
    let a = app.clone();
    std::thread::spawn(move || downloads_loop(a));
    let a = app.clone();
    std::thread::spawn(move || fullscreen_loop(a));
    let a = app.clone();
    std::thread::spawn(move || hotkey_loop(a));
    let a = app.clone();
    std::thread::spawn(move || hit_loop(a));
}

/* ------------------------------------------------------------------ media */

fn now_ticks() -> i64 {
    unsafe {
        let ft = GetSystemTimeAsFileTime();
        ((ft.dwHighDateTime as i64) << 32) | ft.dwLowDateTime as i64
    }
}

fn source_of(app_id: &str, album: &str) -> (&'static str, String) {
    let l = app_id.to_lowercase();
    if l.contains("spotify") {
        return ("sp", "Spotify".into());
    }
    if l.contains("yandex") && l.contains("music") {
        return ("ym", "Яндекс Музыка".into());
    }
    let browsers = ["chrome", "msedge", "firefox", "opera", "browser", "yandex", "brave", "vivaldi"];
    if browsers.iter().any(|b| l.contains(b)) {
        if album.is_empty() {
            return ("yt", "YouTube".into());
        }
        return ("web", "Браузер".into());
    }
    let name = app_id
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(app_id)
        .split('!')
        .next()
        .unwrap_or(app_id)
        .trim_end_matches(".exe")
        .trim_end_matches(".EXE");
    let name = name.split('_').next().unwrap_or(name).rsplit('.').next().unwrap_or(name);
    ("app", name.to_string())
}

fn read_thumbnail(props: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionMediaProperties) -> Option<String> {
    let thumb = props.Thumbnail().ok()?;
    let stream = thumb.OpenReadAsync().ok()?.join().ok()?;
    let size = stream.Size().ok()? as u32;
    if size == 0 || size > 8_000_000 {
        return None;
    }
    let ctype = stream.ContentType().map(|s| s.to_string()).unwrap_or_default();
    let reader = DataReader::CreateDataReader(&stream).ok()?;
    reader.LoadAsync(size).ok()?.join().ok()?;
    let mut buf = vec![0u8; size as usize];
    reader.ReadBytes(&mut buf).ok()?;
    let ctype = if ctype.starts_with("image/") { ctype } else { "image/png".into() };
    Some(format!("data:{};base64,{}", ctype, base64::engine::general_purpose::STANDARD.encode(buf)))
}

fn manager() -> Option<SessionManager> {
    SessionManager::RequestAsync().ok()?.join().ok()
}


pub static MEDIA_TX: Mutex<Option<std::sync::mpsc::Sender<(String, Option<f64>)>>> = Mutex::new(None);

pub fn log(msg: &str) {
    use std::io::Write;
    let dir = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join("Island");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")) {
        let _ = writeln!(f, "{} {}", unix_ms(), msg);
    }
}

fn run_media_cmd(s: &Session, cmd: &str, pos: Option<f64>) {
    let r = match cmd {
        "toggle" => s.TryTogglePlayPauseAsync().and_then(|o| o.join()),
        "next" => s.TrySkipNextAsync().and_then(|o| o.join()),
        "prev" => s.TrySkipPreviousAsync().and_then(|o| o.join()),
        "seek" => {
            let start = s.GetTimelineProperties().and_then(|t| t.StartTime()).map(|d| d.Duration).unwrap_or(0);
            let ticks = start + (pos.unwrap_or(0.0).max(0.0) * 1e7) as i64;
            s.TryChangePlaybackPositionAsync(ticks).and_then(|o| o.join())
        }
        _ => Ok(false),
    };
    log(&format!("media {} -> {:?}", cmd, r));
}

fn media_loop(app: AppHandle) {
    com_init();
    let (tx, rx) = std::sync::mpsc::channel::<(String, Option<f64>)>();
    *MEDIA_TX.lock().unwrap() = Some(tx);
    let mut mgr = None;
    let mut last_key = String::new();
    let mut last_payload = String::new();
    let mut last_send = Instant::now();
    loop {
        if mgr.is_none() {
            mgr = manager();
            if mgr.is_none() {
                sleep(Duration::from_secs(5));
                continue;
            }
        }
        let session = mgr.as_ref().and_then(|m| {
            m.GetCurrentSession().ok().or_else(|| m.GetSessions().ok().and_then(|l| l.GetAt(0).ok()))
        });
        let Some(s) = session else {
            if !last_key.is_empty() || last_payload.is_empty() {
                last_key.clear();
                last_payload = "null".into();
                emit(&app, "media", serde_json::Value::Null);
            }
            if let Ok((c, p)) = rx.recv_timeout(Duration::from_millis(900)) {
                log(&format!("media {} -> no session", c));
                let _ = p;
            }
            continue;
        };
        let app_id = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
        let props = s.TryGetMediaPropertiesAsync().ok().and_then(|o| o.join().ok());
        let (title, artist, album) = match &props {
            Some(p) => (
                p.Title().map(|h| h.to_string()).unwrap_or_default(),
                p.Artist().map(|h| h.to_string()).unwrap_or_default(),
                p.AlbumTitle().map(|h| h.to_string()).unwrap_or_default(),
            ),
            None => Default::default(),
        };
        let playing = s
            .GetPlaybackInfo()
            .and_then(|i| i.PlaybackStatus())
            .map(|st| st == PlaybackStatus::Playing)
            .unwrap_or(false);
        let (mut pos, dur) = match s.GetTimelineProperties() {
            Ok(t) => {
                let p = t.Position().map(|d| d.Duration).unwrap_or(0) as f64 / 1e7;
                let start = t.StartTime().map(|d| d.Duration).unwrap_or(0) as f64 / 1e7;
                let end = t.EndTime().map(|d| d.Duration).unwrap_or(0) as f64 / 1e7;
                let mut p = p - start;
                if playing {
                    if let Ok(upd) = t.LastUpdatedTime() {
                        let el = (now_ticks() - upd.UniversalTime) as f64 / 1e7;
                        if el > 0.0 && el < 3600.0 {
                            p += el;
                        }
                    }
                }
                (p, end - start)
            }
            Err(_) => (0.0, 0.0),
        };
        if dur > 0.0 && pos > dur {
            pos = dur;
        }
        let (src, label) = source_of(&app_id, &album);
        let key = format!("{}|{}|{}", app_id, title, artist);
        let payload = json!({
            "key": key, "src": src, "label": label, "title": title, "artist": artist,
            "playing": playing, "pos": pos, "dur": dur
        });
        let sig = format!("{}|{}|{}", key, playing, (dur as i64));
        // resend every few seconds so the webview can correct drift; immediately on any change
        if sig != last_payload || last_send.elapsed() > Duration::from_secs(4) {
            last_payload = sig;
            last_send = Instant::now();
            emit(&app, "media", payload);
        }
        if key != last_key {
            last_key = key.clone();
            let cover = props.as_ref().and_then(read_thumbnail);
            emit(&app, "media-cover", json!({ "key": key, "data": cover }));
        }
        if let Ok((c, p)) = rx.recv_timeout(Duration::from_millis(700)) {
            run_media_cmd(&s, &c, p);
            // re-read right away so the island reflects the change without waiting a full tick
            last_payload.clear();
            sleep(Duration::from_millis(150));
        }
    }
}

pub fn media_cmd(cmd: &str, pos: Option<f64>) {
    if let Some(tx) = MEDIA_TX.lock().unwrap().as_ref() {
        let _ = tx.send((cmd.to_string(), pos));
    }
}

/* ----------------------------------------------------------------- volume */

pub(crate) fn endpoint() -> Option<IAudioEndpointVolume> {
    unsafe {
        let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let dev = en.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()
    }
}

fn volume_loop(app: AppHandle) {
    com_init();
    let mut ep = None;
    let mut last: Option<(u32, bool)> = None;
    let mut refresh = Instant::now();
    loop {
        if ep.is_none() || refresh.elapsed() > Duration::from_secs(3) {
            ep = endpoint();
            refresh = Instant::now();
        }
        if let Some(e) = &ep {
            let v = unsafe { e.GetMasterVolumeLevelScalar() };
            let m = unsafe { e.GetMute() };
            match (v, m) {
                (Ok(v), Ok(m)) => {
                    let cur = ((v * 100.0).round() as u32, m.as_bool());
                    if last != Some(cur) {
                        emit(&app, "volume", json!({ "v": cur.0, "muted": cur.1, "initial": last.is_none() }));
                        last = Some(cur);
                    }
                }
                _ => ep = None,
            }
        }
        sleep(Duration::from_millis(120));
    }
}

pub fn set_volume(v: u32) {
    com_init();
    if let Some(e) = endpoint() {
        unsafe {
            let _ = e.SetMute(false, std::ptr::null());
            let _ = e.SetMasterVolumeLevelScalar((v.min(100) as f32) / 100.0, std::ptr::null());
        }
    }
}

/* ----------------------------------------------------------------- layout */

fn lang_label(langid: u16) -> String {
    match langid & 0x3ff {
        0x19 => "РУС".into(),
        0x09 => "ENG".into(),
        0x22 => "УКР".into(),
        0x23 => "БЕЛ".into(),
        0x3f => "ҚАЗ".into(),
        _ => {
            let mut buf = [0u16; 16];
            let n = unsafe { GetLocaleInfoW(langid as u32, LOCALE_SABBREVLANGNAME, Some(&mut buf)) };
            if n > 1 {
                String::from_utf16_lossy(&buf[..(n as usize - 1)]).to_uppercase()
            } else {
                format!("{:04X}", langid)
            }
        }
    }
}

fn layout_loop(app: AppHandle) {
    let mut last_hwnd = HWND::default();
    let mut last_lang: u16 = 0;
    let my_pid = std::process::id();
    loop {
        unsafe {
            let hwnd = GetForegroundWindow();
            let mut pid = 0u32;
            let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if !hwnd.0.is_null() && tid != 0 && pid != my_pid {
                let hkl = GetKeyboardLayout(tid);
                let lang = (hkl.0 as usize & 0xffff) as u16;
                if lang != 0 {
                    if hwnd == last_hwnd && lang != last_lang && last_lang != 0 {
                        emit(&app, "layout", json!({ "from": lang_label(last_lang), "to": lang_label(lang) }));
                    }
                    last_hwnd = hwnd;
                    last_lang = lang;
                }
            }
        }
        sleep(Duration::from_millis(150));
    }
}



/* ------------------------------------------------------------- brightness */

fn brightness_loop(app: AppHandle) {
    com_init();
    unsafe {
        let _ = CoInitializeSecurity(
            None, -1, None, None, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE, None,
        );
        let Ok(loc) = CoCreateInstance::<_, IWbemLocator>(&WbemLocator, None, CLSCTX_INPROC_SERVER) else { return };
        let Ok(svc) = loc.ConnectServer(&BSTR::from("ROOT\\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None) else { return };
        let _ = CoSetProxyBlanket(
            &svc, RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE, PCWSTR::null(), RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE, None, EOAC_NONE,
        );
        let mut last: Option<i32> = None;
        let mut fails = 0;
        loop {
            let mut val = None;
            if let Ok(en) = svc.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from("SELECT CurrentBrightness FROM WmiMonitorBrightness"),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            ) {
                let mut objs = [None];
                let mut ret = 0u32;
                if en.Next(WBEM_INFINITE, &mut objs, &mut ret).is_ok() && ret == 1 {
                    if let Some(o) = &objs[0] {
                        let mut v = VARIANT::default();
                        if o.Get(w!("CurrentBrightness"), 0, &mut v, None, None).is_ok() {
                            val = VariantToInt32(&v).ok();
                        }
                    }
                }
            }
            match val {
                Some(b) => {
                    fails = 0;
                    if last.is_some() && last != Some(b) {
                        emit(&app, "brightness", json!({ "v": b }));
                    }
                    last = Some(b);
                }
                None => {
                    fails += 1;
                    // desktop monitors have no WMI brightness; stop asking
                    if fails > 3 && last.is_none() {
                        return;
                    }
                }
            }
            sleep(Duration::from_millis(700));
        }
    }
}

/* -------------------------------------------------------------- clipboard */

#[derive(Clone, serde::Serialize)]
pub struct ClipItem {
    kind: String,
    text: String,
    ts: u64,
}

pub static CLIPS: Mutex<Vec<ClipItem>> = Mutex::new(Vec::new());
static OWN_SEQ: AtomicU32 = AtomicU32::new(0);

fn unix_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

unsafe fn read_clipboard() -> Option<ClipItem> {
    const CF_BITMAP: u32 = 2;
    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;
    let mut opened = false;
    for _ in 0..10 {
        if OpenClipboard(None).is_ok() {
            opened = true;
            break;
        }
        sleep(Duration::from_millis(30));
    }
    if !opened {
        return None;
    }
    let exclude = RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing"));
    let mut item = None;
    if exclude != 0 && IsClipboardFormatAvailable(exclude).is_ok() {
        // password managers ask clipboard watchers to skip their content
    } else if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if !p.is_null() {
                let mut n = 0;
                while *p.add(n) != 0 && n < 200_000 {
                    n += 1;
                }
                let s = String::from_utf16_lossy(std::slice::from_raw_parts(p, n));
                let _ = GlobalUnlock(g);
                let t = s.trim();
                if !t.is_empty() {
                    let kind = if t.starts_with("http://") || t.starts_with("https://") { "link" } else { "text" };
                    item = Some(ClipItem { kind: kind.into(), text: s.chars().take(2000).collect(), ts: unix_ms() });
                }
            }
        }
    } else if IsClipboardFormatAvailable(CF_HDROP).is_ok() {
        if let Ok(h) = GetClipboardData(CF_HDROP) {
            let drop = HDROP(h.0);
            let count = DragQueryFileW(drop, 0xFFFFFFFF, None);
            let mut buf = [0u16; 520];
            let n = DragQueryFileW(drop, 0, Some(&mut buf));
            let first = String::from_utf16_lossy(&buf[..n as usize]);
            let name = first.rsplit('\\').next().unwrap_or(&first).to_string();
            let text = if count > 1 { format!("{} и ещё {}", name, count - 1) } else { name };
            item = Some(ClipItem { kind: "files".into(), text, ts: unix_ms() });
        }
    } else if IsClipboardFormatAvailable(CF_DIB).is_ok() || IsClipboardFormatAvailable(CF_BITMAP).is_ok() {
        let mut text = "Изображение".to_string();
        if let Ok(h) = GetClipboardData(CF_DIB) {
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const i32;
            if !p.is_null() {
                // BITMAPINFOHEADER: biSize, biWidth, biHeight
                let wdt = *p.add(1);
                let hgt = (*p.add(2)).abs();
                let _ = GlobalUnlock(g);
                text = format!("Изображение · {} × {}", wdt, hgt);
            }
        }
        item = Some(ClipItem { kind: "image".into(), text, ts: unix_ms() });
    }
    let _ = CloseClipboard();
    item
}

fn clipboard_loop(app: AppHandle) {
    let mut seq = unsafe { GetClipboardSequenceNumber() };
    loop {
        sleep(Duration::from_millis(250));
        let s = unsafe { GetClipboardSequenceNumber() };
        if s == seq {
            continue;
        }
        seq = s;
        sleep(Duration::from_millis(60));
        let Some(item) = (unsafe { read_clipboard() }) else { continue };
        let own = OWN_SEQ.swap(0, Ordering::SeqCst) != 0;
        let items = {
            let mut c = CLIPS.lock().unwrap();
            c.retain(|x| !(x.kind == item.kind && x.text == item.text));
            c.insert(0, item);
            c.truncate(25);
            c.clone()
        };
        emit(&app, "clip", json!({ "items": items, "added": !own }));
    }
}

pub fn clip_set(index: usize) -> bool {
    let item = {
        let c = CLIPS.lock().unwrap();
        match c.get(index) {
            Some(i) if i.kind == "text" || i.kind == "link" => i.clone(),
            _ => return false,
        }
    };
    unsafe {
        let wide: Vec<u16> = item.text.encode_utf16().chain(std::iter::once(0)).collect();
        if OpenClipboard(None).is_err() {
            return false;
        }
        let _ = EmptyClipboard();
        let ok = (|| -> Option<()> {
            let g = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).ok()?;
            let p = GlobalLock(g) as *mut u16;
            if p.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(g);
            OWN_SEQ.store(1, Ordering::SeqCst);
            SetClipboardData(13, Some(HANDLE(g.0))).ok()?;
            Some(())
        })()
        .is_some();
        let _ = CloseClipboard();
        ok
    }
}

/* -------------------------------------------------------------- downloads */

fn downloads_dir() -> Option<PathBuf> {
    use windows::Win32::UI::Shell::{FOLDERID_Downloads, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
    unsafe {
        let p = SHGetKnownFolderPath(&FOLDERID_Downloads, KF_FLAG_DEFAULT, None).ok()?;
        let s = p.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
    .or_else(|| std::env::var("USERPROFILE").ok().map(|h| PathBuf::from(h).join("Downloads")))
}

const PARTIAL: [&str; 5] = ["crdownload", "part", "partial", "opdownload", "download"];

/// Browsers start with a placeholder name ("Unconfirmed 123", "Не подтвержден 123") before the real one is known.
fn is_placeholder(stem: &str) -> bool {
    if stem.starts_with("Unconfirmed") || stem.starts_with("Не подтвержден") {
        return true;
    }
    !stem.contains('.') && stem.rsplit(' ').next().map(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())).unwrap_or(false)
}

struct Partial {
    size: u64,
    seen: Instant,
    grew: Instant,
    speed: f64,
}

fn downloads_loop(app: AppHandle) {
    let Some(dir) = downloads_dir() else { return };
    let mut prev: HashMap<PathBuf, Partial> = HashMap::new();
    let mut was_active = false;
    let stale = Duration::from_secs(30);
    loop {
        let mut now: HashMap<PathBuf, Partial> = HashMap::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let p = e.path();
                let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("").to_lowercase();
                if !PARTIAL.contains(&ext.as_str()) {
                    continue;
                }
                let meta = e.metadata().ok();
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let entry = match prev.get(&p) {
                    Some(o) => {
                        let dt = o.seen.elapsed().as_secs_f64().max(0.05);
                        let inst = size.saturating_sub(o.size) as f64 / dt;
                        let grew = if size != o.size { Instant::now() } else { o.grew };
                        Partial { size, seen: Instant::now(), grew, speed: o.speed * 0.6 + inst * 0.4 }
                    }
                    None => {
                        // a leftover from a cancelled download starts out already stale
                        let age = meta.and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).unwrap_or(stale);
                        let grew = Instant::now().checked_sub(age).unwrap_or_else(Instant::now);
                        Partial { size, seen: Instant::now(), grew, speed: 0.0 }
                    }
                };
                now.insert(p, entry);
            }
        }
        for (p, o) in prev.iter() {
            if !now.contains_key(p) && o.grew.elapsed() < stale {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
                let final_path = dir.join(&stem);
                if !is_placeholder(&stem) && final_path.exists() {
                    let sz = std::fs::metadata(&final_path).map(|m| m.len()).unwrap_or(o.size);
                    emit(&app, "download", json!({ "done": true, "name": stem, "bytes": sz }));
                }
            }
        }
        let live: Vec<(&PathBuf, &Partial)> = now.iter().filter(|(_, o)| o.grew.elapsed() < stale).collect();
        if let Some((p, o)) = live.iter().max_by(|a, b| a.1.size.cmp(&b.1.size)) {
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
            let name = if is_placeholder(&stem) { "Загрузка файла".to_string() } else { stem };
            emit(&app, "download", json!({ "active": true, "name": name, "bytes": o.size, "speed": o.speed, "count": live.len() }));
            was_active = true;
        } else if was_active {
            was_active = false;
            emit(&app, "download", json!({ "active": false }));
        }
        prev = now;
        sleep(Duration::from_millis(700));
    }
}

/* ------------------------------------------------------------- fullscreen */

pub static HIDE_FULL: AtomicBool = AtomicBool::new(true);
pub static USER_HIDDEN: AtomicBool = AtomicBool::new(false);
pub static FULL_NOW: AtomicBool = AtomicBool::new(false);
pub static SLIM_BROWSER: AtomicBool = AtomicBool::new(true);
pub static SLIM_NOW: AtomicBool = AtomicBool::new(false);

const BROWSERS: &[&str] = &[
    "chrome.exe", "msedge.exe", "firefox.exe", "opera.exe", "opera_gx.exe", "brave.exe", "browser.exe",
    "vivaldi.exe", "arc.exe", "yandex.exe", "chromium.exe", "librewolf.exe", "waterfox.exe", "zen.exe", "thorium.exe",
];

fn exe_name(pid: u32) -> Option<String> {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; 520];
        let mut n = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut n).is_ok();
        let _ = CloseHandle(h);
        if !ok {
            return None;
        }
        let path = String::from_utf16_lossy(&buf[..n as usize]);
        path.rsplit('\\').next().map(|s| s.to_lowercase())
    }
}

/// Some(true) when a browser window reaches the top of the island's monitor, so its tabs sit under the island.
/// None when the island itself (or a sticker) has focus, so the current mode is kept.
unsafe fn browser_under_island(island: HWND) -> Option<bool> {
    let hwnd = GetForegroundWindow();
    if hwnd.0.is_null() {
        return Some(false);
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == std::process::id() {
        return None;
    }
    if IsIconic(hwnd).as_bool() {
        return Some(false);
    }
    let Some(name) = exe_name(pid) else { return Some(false) };
    if !BROWSERS.contains(&name.as_str()) {
        return Some(false);
    }
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    if mon != MonitorFromWindow(island, MONITOR_DEFAULTTONEAREST) {
        return Some(false);
    }
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    let mut r = RECT::default();
    if !GetMonitorInfoW(mon, &mut mi).as_bool() || GetWindowRect(hwnd, &mut r).is_err() {
        return Some(false);
    }
    let w = mi.rcWork;
    let cx = (mi.rcMonitor.left + mi.rcMonitor.right) / 2;
    Some(r.top <= w.top + 60 && r.left < cx - 60 && r.right > cx + 60)
}

unsafe fn foreground_is_fullscreen() -> bool {
    let hwnd = GetForegroundWindow();
    if hwnd.0.is_null() {
        return false;
    }
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid == std::process::id() {
        return false;
    }
    let mut cls = [0u16; 64];
    let n = GetClassNameW(hwnd, &mut cls);
    let cls = String::from_utf16_lossy(&cls[..n as usize]);
    if cls == "Progman" || cls == "WorkerW" || cls == "Shell_TrayWnd" {
        return false;
    }
    // ordinary maximized windows keep their caption; real fullscreen apps drop it
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if style & WS_CAPTION.0 == WS_CAPTION.0 {
        return false;
    }
    let mut r = RECT::default();
    if GetWindowRect(hwnd, &mut r).is_err() {
        return false;
    }
    let mon = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if !GetMonitorInfoW(mon, &mut mi).as_bool() {
        return false;
    }
    let m = mi.rcMonitor;
    r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
}

fn fullscreen_loop(app: AppHandle) {
    let mut last = false;
    let mut slim = false;
    let mut first = true;
    loop {
        let island = app.get_webview_window("island").and_then(|w| w.hwnd().ok());
        if let Some(h) = island {
            let want = SLIM_BROWSER.load(Ordering::SeqCst) && unsafe { browser_under_island(HWND(h.0)) }.unwrap_or(slim);
            if want != slim || first {
                first = false;
                slim = want;
                SLIM_NOW.store(slim, Ordering::SeqCst);
                emit(&app, "slim", json!({ "on": slim }));
            }
        }
        let st = unsafe { SHQueryUserNotificationState() }.unwrap_or_default();
        let full = st == QUNS_BUSY || st == QUNS_RUNNING_D3D_FULL_SCREEN || st == QUNS_PRESENTATION_MODE
            || unsafe { foreground_is_fullscreen() };
        let full = full && HIDE_FULL.load(Ordering::SeqCst);
        if full != last {
            last = full;
            FULL_NOW.store(full, Ordering::SeqCst);
            crate::apply_visibility(&app);
        }
        sleep(Duration::from_millis(500));
    }
}

/* ---------------------------------------------------------------- hotkeys */

fn hotkey_loop(app: AppHandle) {
    unsafe {
        let _ = RegisterHotKey(None, 1, MOD_WIN | MOD_SHIFT | MOD_NOREPEAT, 'N' as u32);
        let _ = RegisterHotKey(None, 2, MOD_WIN | MOD_SHIFT | MOD_NOREPEAT, 'V' as u32);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_HOTKEY {
                let id = msg.wParam.0;
                if let Some(w) = app.get_webview_window("island") {
                    let _ = w.set_ignore_cursor_events(false);
                    let _ = w.set_focus();
                }
                emit(&app, "hotkey", json!({ "id": if id == 1 { "notes" } else { "clip" } }));
            }
        }
    }
}

/* ------------------------------------------------------- click-through hit */

#[derive(Clone, Copy, serde::Deserialize)]
pub struct HitRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

pub static HIT: Mutex<Vec<HitRect>> = Mutex::new(Vec::new());
pub static FORCE_SOLID: AtomicBool = AtomicBool::new(false);

fn hit_loop(app: AppHandle) {
    let mut last_inside: Option<bool> = None;
    loop {
        sleep(Duration::from_millis(25));
        let Some(win) = app.get_webview_window("island") else { continue };
        let (Ok(pos), Ok(scale)) = (win.outer_position(), win.scale_factor()) else { continue };
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            continue;
        }
        let lx = (pt.x - pos.x) as f64 / scale;
        let ly = (pt.y - pos.y) as f64 / scale;
        let pad = 4.0;
        let inside = FORCE_SOLID.load(Ordering::SeqCst)
            || HIT.lock().unwrap().iter().any(|r| lx >= r.x - pad && lx <= r.x + r.w + pad && ly >= r.y - pad && ly <= r.y + r.h + pad);
        if last_inside != Some(inside) {
            last_inside = Some(inside);
            let _ = win.set_ignore_cursor_events(!inside);
            emit(&app, "cursor", json!({ "inside": inside }));
        }
    }
}


pub fn chime() {
    unsafe {
        let _ = MessageBeep(MB_ICONASTERISK);
    }
}
