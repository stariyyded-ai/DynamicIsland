//! Version 0.3 watchers: app notifications, Explorer copy progress and the volume keys.

use base64::Engine;
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread::sleep;
use std::time::{Duration, Instant};
use tauri::AppHandle;

use windows::core::{w, GUID, PCWSTR};
use windows::Win32::Storage::EnhancedStorage::{PKEY_AppUserModel_ID, PKEY_AppUserModel_ToastActivatorCLSID};
use windows::Win32::System::Com::StructuredStorage::{PropVariantToGUID, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CoTaskMemFree, CLSCTX_LOCAL_SERVER};
use windows::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};
use windows::Win32::UI::Notifications::{INotificationActivationCallback, NOTIFICATION_USER_INPUT_DATA};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreFromParsingName, GPS_DEFAULT};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation, TreeScope_Descendants};
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_VOLUME_DOWN, VK_VOLUME_MUTE, VK_VOLUME_UP};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, FindWindowExW, GetMessageW, GetWindowTextW, IsIconic, IsWindowVisible,
    SetForegroundWindow, SetWindowsHookExW, ShowWindow, KBDLLHOOKSTRUCT, MSG, SW_RESTORE,
    SW_SHOWNORMAL, WH_KEYBOARD_LL, WM_KEYDOWN, WM_SYSKEYDOWN,
};

use crate::sys::{com_init, emit, endpoint, keep, log};

pub fn spawn_all(app: &AppHandle) {
    let a = app.clone();
    keep("notify_loop", move || notify_loop(a.clone()));
    let a = app.clone();
    keep("copy_loop", move || copy_loop(a.clone()));
    keep("volume_keys_loop", volume_keys_loop);
}

/* ---------------------------------------------------------- notifications */

// Windows keeps every toast in this database, including ones from Telegram, Outlook and browser sites.
fn wpn_path() -> Option<PathBuf> {
    let base = std::env::var("LOCALAPPDATA").ok()?;
    let p = PathBuf::from(base).join("Microsoft\\Windows\\Notifications\\wpndatabase.db");
    p.exists().then_some(p)
}

fn open_db(p: &PathBuf) -> rusqlite::Result<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let c = rusqlite::Connection::open_with_flags(p, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    c.busy_timeout(Duration::from_millis(300))?;
    Ok(c)
}

// When the live file can't be opened, read a fresh copy of it instead.
fn open_copy(p: &PathBuf) -> rusqlite::Result<rusqlite::Connection> {
    let dir = std::env::temp_dir().join("IslandWpn");
    let _ = std::fs::create_dir_all(&dir);
    for ext in ["", "-wal"] {
        let src = PathBuf::from(format!("{}{}", p.display(), ext));
        let dst = dir.join(format!("wpn.db{}", ext));
        let _ = std::fs::remove_file(&dst);
        if src.exists() {
            let _ = std::fs::copy(&src, &dst);
        }
    }
    let _ = std::fs::remove_file(dir.join("wpn.db-shm"));
    rusqlite::Connection::open(dir.join("wpn.db"))
}

struct Toast {
    id: i64,
    aumid: String,
    xml: String,
}

// Newest toasts only: Telegram replaces and removes its toasts, so rows are compared by content, not by time.
const QUERY: &str = "SELECT n.Id, h.PrimaryId, n.Payload FROM Notification n \
     JOIN NotificationHandler h ON h.RecordId = n.HandlerId \
     WHERE n.Type = 'toast' ORDER BY n.Id DESC LIMIT 80";

fn read_toasts(c: &rusqlite::Connection) -> rusqlite::Result<Vec<Toast>> {
    let mut st = c.prepare_cached(QUERY)?;
    let rows = st.query_map([], |r| {
        let xml = match r.get_ref(2)? {
            rusqlite::types::ValueRef::Blob(b) => String::from_utf8_lossy(b).to_string(),
            rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).to_string(),
            _ => String::new(),
        };
        Ok(Toast { id: r.get(0)?, aumid: r.get::<_, Option<String>>(1)?.unwrap_or_default(), xml })
    })?;
    let mut v: Vec<Toast> = rows.flatten().collect();
    v.reverse();
    Ok(v)
}

fn hash_of(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(j) = rest.find(';').filter(|&j| j <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..j];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => u32::from_str_radix(&ent[2..], 16).ok().and_then(char::from_u32),
            _ if ent.starts_with('#') => ent[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match ch {
            Some(c) => out.push(c),
            None => out.push_str(&rest[..=j]),
        }
        rest = &rest[j + 1..];
    }
    out.push_str(rest);
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{}=\"", name);
    let i = tag.find(&key)? + key.len();
    let j = tag[i..].find('"')?;
    Some(unescape(&tag[i..i + j]))
}

/// Every `<tag ...>` with its inner text (empty for self-closing tags).
fn elements<'a>(xml: &'a str, tag: &str) -> Vec<(&'a str, String)> {
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find(&open) {
        rest = &rest[i + open.len()..];
        if !rest.starts_with([' ', '>', '/', '\t', '\n', '\r']) {
            continue;
        }
        let Some(gt) = rest.find('>') else { break };
        let head = &rest[..gt];
        rest = &rest[gt + 1..];
        if head.ends_with('/') {
            out.push((head, String::new()));
            continue;
        }
        let end = rest.find(&close).unwrap_or(rest.len());
        out.push((head, unescape(rest[..end].trim())));
        rest = &rest[end..];
    }
    out
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn image_data(src: &str) -> Option<String> {
    let path = if let Some(p) = src.strip_prefix("file:///") {
        percent_decode(p).replace('/', "\\")
    } else if src.len() > 2 && src.as_bytes()[1] == b':' {
        src.to_string()
    } else {
        return None;
    };
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > 600_000 {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    let lower = path.to_lowercase();
    let mime = if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "image/jpeg"
    };
    Some(format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(bytes)))
}

const MAIL_APPS: [&str; 6] = ["windowscommunicationsapps", "outlook", "thunderbird", "mailbird", "spark", "mail"];
const MAIL_SITES: [&str; 6] = ["mail.google", "mail.yandex", "mail.ru", "outlook.live", "outlook.office", "proton.me"];
const BROWSERS: [&str; 8] = ["chrome", "msedge", "edge", "firefox", "opera", "yandex", "brave", "vivaldi"];

fn app_name(aumid: &str) -> String {
    let s = aumid.rsplit('\\').next().unwrap_or(aumid);
    let s = s.split('!').next().unwrap_or(s);
    let s = s.split('_').next().unwrap_or(s);
    let s = s.trim_end_matches(".exe");
    let s = s.rsplit('.').next().filter(|t| !t.is_empty()).unwrap_or(s);
    if s.is_empty() {
        "Уведомление".into()
    } else {
        s.to_string()
    }
}

fn parse_toast(t: &Toast) -> Option<serde_json::Value> {
    let id = t.aumid.to_lowercase();
    if id.starts_with("windows.systemtoast") || id.contains("microsoft.windows.shellexperiencehost") {
        return None;
    }
    let toast_tag = elements(&t.xml, "toast").first().map(|e| e.0.to_string()).unwrap_or_default();
    let mut attribution = String::new();
    let mut lines: Vec<String> = Vec::new();
    for (head, text) in elements(&t.xml, "text") {
        if text.is_empty() {
            continue;
        }
        if attr(head, "placement").as_deref() == Some("attribution") {
            attribution = text;
        } else {
            lines.push(text);
        }
    }
    if lines.is_empty() {
        return None;
    }
    let images = elements(&t.xml, "image");
    let logo = images
        .iter()
        .find(|(h, _)| attr(h, "placement").as_deref() == Some("appLogoOverride"))
        .or_else(|| images.iter().find(|(h, _)| attr(h, "placement").is_none()))
        .and_then(|(h, _)| attr(h, "src"))
        .and_then(|s| image_data(&s));

    let all = lines.join(" ").to_lowercase();
    let attr_l = attribution.to_lowercase();
    let browser = BROWSERS.iter().any(|b| id.contains(b));
    let telegram = id.contains("telegram");
    let call = toast_tag.contains("incomingCall") || (telegram && (all.contains("звон") || all.contains("call")));
    let mail = !telegram && (MAIL_APPS.iter().any(|m| id.contains(m)) || MAIL_SITES.iter().any(|m| attr_l.contains(m)));

    let (kind, app) = if call {
        ("call", if telegram { "Telegram".to_string() } else { app_name(&t.aumid) })
    } else if telegram {
        ("telegram", "Telegram".to_string())
    } else if mail {
        let label = if attribution.is_empty() { "Почта".to_string() } else { attribution.clone() };
        ("mail", label)
    } else {
        let label = if !attribution.is_empty() { attribution.clone() } else { app_name(&t.aumid) };
        ("other", label)
    };
    let title = lines.remove(0);

    // what the toast lets you do: the click target, a quick reply box and buttons such as "mark as read"
    let launch = attr(&toast_tag, "launch").unwrap_or_default();
    let input = elements(&t.xml, "input")
        .iter()
        .find(|(h, _)| attr(h, "type").as_deref() != Some("selection"))
        .and_then(|(h, _)| attr(h, "id"));
    let mut reply: Option<String> = None;
    let mut acts: Vec<serde_json::Value> = Vec::new();
    for (h, _) in elements(&t.xml, "action") {
        let kind = attr(h, "activationType").unwrap_or_default();
        if kind == "system" || kind == "protocol" {
            continue;
        }
        let Some(args) = attr(h, "arguments") else { continue };
        let hint = attr(h, "hint-inputId");
        if input.is_some() && (hint.is_some() && hint == input || (reply.is_none() && args.contains("reply"))) {
            reply = Some(args);
            continue;
        }
        let label = attr(h, "content").unwrap_or_default();
        if !label.is_empty() && acts.len() < 2 {
            acts.push(json!({ "label": label, "args": args }));
        }
    }
    let can_act = !browser && activator(&t.aumid).is_some();
    Some(json!({
        "kind": kind, "app": app, "title": title, "body": lines.join("\n"),
        "icon": logo, "aumid": t.aumid, "browser": browser, "id": t.id,
        "launch": if can_act { launch } else { String::new() },
        "reply": if can_act { reply.map(|a| json!({ "args": a, "input": input })) } else { None },
        "acts": if can_act { acts } else { vec![] },
    }))
}

/// Size and time of the database and its journal: when they don't move, nothing new arrived.
fn file_sig(p: &PathBuf) -> String {
    ["", "-wal"]
        .iter()
        .map(|e| {
            std::fs::metadata(format!("{}{}", p.display(), e))
                .map(|m| format!("{}:{:?}", m.len(), m.modified().ok()))
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("|")
}

fn notify_loop(app: AppHandle) {
    let Some(path) = wpn_path() else {
        log("notifications: wpndatabase.db not found");
        return;
    };
    let mut live = true;
    let mut fails = 0;
    let mut conn: Option<rusqlite::Connection> = None;
    let mut known: HashMap<i64, u64> = HashMap::new();
    let mut primed = false;
    let mut last_ver: Option<i64> = None;
    let mut last_sig = String::new();
    let mut last_err = Instant::now() - Duration::from_secs(600);
    loop {
        sleep(Duration::from_millis(if live { 300 } else { 700 }));
        if live {
            if conn.is_none() {
                match open_db(&path) {
                    Ok(c) => conn = Some(c),
                    Err(e) => {
                        log(&format!("notifications: live open failed: {}", e));
                        live = false;
                        continue;
                    }
                }
            }
            // data_version changes only when another process (the shell) commits something
            let ver = conn.as_ref().unwrap().query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0));
            match ver {
                Ok(v) if primed && last_ver == Some(v) => continue,
                Ok(v) => last_ver = Some(v),
                Err(e) => {
                    log(&format!("notifications: {}", e));
                    conn = None;
                    fails += 1;
                    if fails > 3 {
                        live = false;
                    }
                    continue;
                }
            }
        } else {
            let sig = file_sig(&path);
            if primed && sig == last_sig {
                continue;
            }
            last_sig = sig;
            conn = open_copy(&path).ok();
        }
        let Some(c) = conn.as_ref() else { continue };
        match read_toasts(c) {
            Ok(list) => {
                fails = 0;
                let mut next = HashMap::with_capacity(list.len());
                for t in &list {
                    let h = hash_of(&t.xml);
                    next.insert(t.id, h);
                    if primed && known.get(&t.id) != Some(&h) {
                        if let Some(p) = parse_toast(t) {
                            emit(&app, "notify", p);
                        }
                    }
                }
                known = next;
                if !primed {
                    primed = true;
                    log(&format!("notifications: watching ({})", if live { "live" } else { "copy" }));
                }
            }
            Err(e) => {
                if last_err.elapsed() > Duration::from_secs(60) {
                    log(&format!("notifications: query failed: {}", e));
                    last_err = Instant::now();
                }
                conn = None;
                if live {
                    fails += 1;
                    if fails > 3 {
                        live = false;
                    }
                }
            }
        }
    }
}

/* ------------------------------------------------- acting on a toast */

static ACTIVATORS: Mutex<Option<HashMap<String, Option<GUID>>>> = Mutex::new(None);

fn guid_from_str(s: &str) -> Option<GUID> {
    let h: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (h.len() == 32).then(|| u128::from_str_radix(&h, 16).ok().map(GUID::from_u128)).flatten()
}

fn activator_from_store(path: &str, aumid: Option<&str>) -> Option<GUID> {
    unsafe {
        let p: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let store: IPropertyStore = SHGetPropertyStoreFromParsingName(PCWSTR(p.as_ptr()), None, GPS_DEFAULT).ok()?;
        if let Some(want) = aumid {
            let v = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
            let s = PropVariantToStringAlloc(&v).ok()?;
            let got = s.to_string().unwrap_or_default();
            CoTaskMemFree(Some(s.0 as _));
            if !got.eq_ignore_ascii_case(want) {
                return None;
            }
        }
        let v = store.GetValue(&PKEY_AppUserModel_ToastActivatorCLSID).ok()?;
        PropVariantToGUID(&v).ok().filter(|g| *g != GUID::zeroed())
    }
}

fn find_links(dir: &std::path::Path, out: &mut Vec<PathBuf>, depth: u32) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && depth < 4 {
            find_links(&p, out, depth + 1);
        } else if p.extension().and_then(|x| x.to_str()).map(|x| x.eq_ignore_ascii_case("lnk")).unwrap_or(false) {
            out.push(p);
        }
    }
}

/// The COM class an app registered to receive clicks and replies from its own toasts.
fn activator(aumid: &str) -> Option<GUID> {
    if aumid.is_empty() {
        return None;
    }
    if let Some(m) = ACTIVATORS.lock().unwrap().as_ref() {
        if let Some(v) = m.get(aumid) {
            return *v;
        }
    }
    com_init();
    let key: Vec<u16> = format!("Software\\Classes\\AppUserModelId\\{}", aumid).encode_utf16().chain(Some(0)).collect();
    let from_reg = unsafe {
        let mut buf = [0u16; 128];
        let mut len = (buf.len() * 2) as u32;
        RegGetValueW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr()), w!("CustomActivator"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut len))
            .ok()
            .ok()
            .and_then(|_| guid_from_str(&String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)])))
    };
    let found = from_reg
        .or_else(|| activator_from_store(&format!("shell:AppsFolder\\{}", aumid), None))
        .or_else(|| {
            let mut links = Vec::new();
            for base in [std::env::var("APPDATA").ok().map(|a| format!("{}\\Microsoft\\Windows\\Start Menu\\Programs", a)), std::env::var("ProgramData").ok().map(|a| format!("{}\\Microsoft\\Windows\\Start Menu\\Programs", a))].into_iter().flatten() {
                find_links(std::path::Path::new(&base), &mut links, 0);
            }
            links.iter().find_map(|l| activator_from_store(&l.to_string_lossy(), Some(aumid)))
        });
    log(&format!("notifications: activator for {} = {:?}", aumid, found));
    ACTIVATORS.lock().unwrap().get_or_insert_with(HashMap::new).insert(aumid.to_string(), found);
    found
}

/// Does what clicking a button on the toast would do: open the chat, send a quick reply, mark as read.
pub fn notif_act(aumid: &str, args: &str, input: Option<&str>, text: Option<&str>) -> bool {
    com_init();
    let Some(clsid) = activator(aumid) else { return false };
    unsafe {
        let cb: INotificationActivationCallback = match CoCreateInstance(&clsid, None, CLSCTX_LOCAL_SERVER) {
            Ok(c) => c,
            Err(e) => {
                log(&format!("notifications: activator start failed: {}", e));
                return false;
            }
        };
        let w = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
        let (a, g) = (w(aumid), w(args));
        let (k, v) = (w(input.unwrap_or("")), w(text.unwrap_or("")));
        let data = [NOTIFICATION_USER_INPUT_DATA { Key: PCWSTR(k.as_ptr()), Value: PCWSTR(v.as_ptr()) }];
        let r = cb.Activate(PCWSTR(a.as_ptr()), PCWSTR(g.as_ptr()), if input.is_some() && text.is_some() { &data[..] } else { &[] });
        if let Err(e) = &r {
            log(&format!("notifications: activate failed: {}", e));
        }
        r.is_ok()
    }
}

/// Opens what a notification is about: the chat it came from when the app lets us activate it,
/// otherwise the app's own window, and as a last resort the app is started.
pub fn notif_open(aumid: &str, launch: &str) {
    if aumid.is_empty() {
        return;
    }
    let activated = !launch.is_empty() && notif_act(aumid, launch, None, None);
    let win = crate::sys::app_window(aumid);
    log(&format!("notif open {}: activated {}, window {}", aumid, activated, win.is_some()));
    if let Some(h) = win {
        // activation opens the right chat but often leaves the window behind the others
        if activated {
            sleep(Duration::from_millis(250));
        }
        crate::sys::bring_front(h);
        return;
    }
    if activated {
        return;
    }
    let target: Vec<u16> = format!("shell:AppsFolder\\{}", aumid).encode_utf16().chain(Some(0)).collect();
    unsafe {
        ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(target.as_ptr()), None, SW_SHOWNORMAL);
    }
}

/* ------------------------------------------------------- explorer copying */

static COPY_HWND: AtomicIsize = AtomicIsize::new(0);

fn copy_window() -> Option<HWND> {
    unsafe {
        let mut after: Option<HWND> = None;
        loop {
            let h = FindWindowExW(None, after, w!("OperationStatusWindow"), PCWSTR::null()).ok()?;
            if h.0.is_null() {
                return None;
            }
            if IsWindowVisible(h).as_bool() || IsIconic(h).as_bool() {
                return Some(h);
            }
            after = Some(h);
        }
    }
}

fn window_title(h: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { GetWindowTextW(h, &mut buf) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn percent_in(s: &str) -> Option<u32> {
    let i = s.find('%')?;
    let digits: String = s[..i].trim_end().chars().rev().take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
    digits.parse().ok().filter(|v: &u32| *v <= 100)
}

/// Texts inside the dialog: "Копирование 12 элементов из A в B", speed and time left.
fn dialog_texts(ua: &IUIAutomation, h: HWND) -> Vec<String> {
    unsafe {
        let Ok(el) = ua.ElementFromHandle(h) else { return vec![] };
        let Ok(cond) = ua.CreateTrueCondition() else { return vec![] };
        let Ok(arr) = el.FindAll(TreeScope_Descendants, &cond) else { return vec![] };
        let n = arr.Length().unwrap_or(0).min(200);
        let mut out = Vec::new();
        for i in 0..n {
            if let Ok(e) = arr.GetElement(i) {
                if let Ok(name) = e.CurrentName() {
                    let s = name.to_string();
                    let s = s.trim();
                    if !s.is_empty() && !out.iter().any(|o: &String| o == s) {
                        out.push(s.to_string());
                    }
                }
            }
        }
        out
    }
}

const OP_WORDS: [&str; 8] = ["Копирование", "Перемещение", "Удаление", "Извлечение", "Copying", "Moving", "Deleting", "Extracting"];

fn copy_loop(app: AppHandle) {
    com_init();
    let ua: Option<IUIAutomation> = unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok() };
    let mut active = false;
    let mut last_pct = 0u32;
    let mut last_head = String::new();
    let mut tick = 0u32;
    loop {
        sleep(Duration::from_millis(500));
        tick += 1;
        match copy_window() {
            Some(h) => {
                COPY_HWND.store(h.0 as isize, Ordering::SeqCst);
                let title = window_title(h);
                let pct = percent_in(&title);
                // reading the dialog through UI Automation is heavier, so only every second tick
                if tick % 2 == 0 || !active {
                    let texts = ua.as_ref().map(|u| dialog_texts(u, h)).unwrap_or_default();
                    if let Some(head) = texts.iter().find(|t| OP_WORDS.iter().any(|w| t.starts_with(w))) {
                        last_head = head.clone();
                    }
                    let speed = texts.iter().find(|t| t.contains("Скорость") || t.contains("Speed")).cloned();
                    let left = texts.iter().find(|t| t.contains("Осталось") || t.to_lowercase().contains("remaining")).cloned();
                    let pct = pct.or_else(|| texts.iter().find_map(|t| percent_in(t)));
                    if let Some(p) = pct {
                        last_pct = p;
                    }
                    emit(&app, "copy", json!({ "active": true, "pct": pct, "head": last_head, "speed": speed, "left": left, "title": title }));
                }
                active = true;
            }
            None => {
                COPY_HWND.store(0, Ordering::SeqCst);
                if active {
                    active = false;
                    emit(&app, "copy", json!({ "active": false, "done": last_pct >= 90, "head": last_head }));
                    last_pct = 0;
                    last_head.clear();
                }
            }
        }
    }
}

pub fn copy_show() {
    let h = COPY_HWND.load(Ordering::SeqCst);
    if h == 0 {
        return;
    }
    let h = HWND(h as _);
    unsafe {
        if IsIconic(h).as_bool() {
            let _ = ShowWindow(h, SW_RESTORE);
        }
        let _ = SetForegroundWindow(h);
    }
}

/* ------------------------------------------------------------ volume keys */

/// When on, the volume keys are handled here and shown in the island instead of the Windows flyout.
pub static REPLACE_OSD: AtomicBool = AtomicBool::new(true);
static KEY_TX: OnceLock<Mutex<Sender<u32>>> = OnceLock::new();

unsafe extern "system" fn kb_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == 0
        && REPLACE_OSD.load(Ordering::Relaxed)
        // in a game the Windows flyout is the one that can actually be seen
        && !(crate::sys::FULL_RAW.load(Ordering::Relaxed) && crate::sys::FULL_MODE.load(Ordering::Relaxed) != 2)
        && !crate::sys::USER_HIDDEN.load(Ordering::Relaxed)
    {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        let vk = k.vkCode;
        if vk == VK_VOLUME_UP.0 as u32 || vk == VK_VOLUME_DOWN.0 as u32 || vk == VK_VOLUME_MUTE.0 as u32 {
            let msg = wp.0 as u32;
            if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
                if let Some(tx) = KEY_TX.get() {
                    if let Ok(tx) = tx.lock() {
                        let _ = tx.send(vk);
                    }
                }
            }
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

fn volume_keys_loop() {
    let (tx, rx) = channel::<u32>();
    let _ = KEY_TX.set(Mutex::new(tx));
    std::thread::spawn(move || {
        com_init();
        for vk in rx {
            let Some(e) = endpoint() else { continue };
            unsafe {
                if vk == VK_VOLUME_MUTE.0 as u32 {
                    let m = e.GetMute().map(|b| b.as_bool()).unwrap_or(false);
                    let _ = e.SetMute(!m, std::ptr::null());
                    continue;
                }
                let v = e.GetMasterVolumeLevelScalar().unwrap_or(0.0);
                let step = if vk == VK_VOLUME_UP.0 as u32 { 2.0 } else { -2.0 };
                let nv = (((v * 100.0 / 2.0).round() * 2.0 + step).clamp(0.0, 100.0)) / 100.0;
                let _ = e.SetMute(false, std::ptr::null());
                let _ = e.SetMasterVolumeLevelScalar(nv, std::ptr::null());
            }
        }
    });
    unsafe {
        let hmod = GetModuleHandleW(None).ok().map(|m| m.into());
        if let Err(e) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), hmod, 0) {
            log(&format!("volume keys: hook failed: {}", e));
            return;
        }
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {}
    }
}
