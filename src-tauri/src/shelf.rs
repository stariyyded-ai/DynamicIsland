//! The shelf: files dropped on the island wait here until they are dragged out somewhere else.

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager};

use windows::core::{w, PCWSTR};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    path: String,
}

static SHELF: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
const MAX: usize = 30;

fn file() -> PathBuf {
    crate::data_dir().join("shelf.json")
}

fn save(list: &[Entry]) {
    let _ = std::fs::write(file(), serde_json::to_string_pretty(list).unwrap_or_default());
}

pub fn load() {
    let list: Vec<Entry> = std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    *SHELF.lock().unwrap() = list;
}

fn thumb(p: &PathBuf) -> Option<String> {
    let ext = p.extension()?.to_str()?.to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        _ => return None,
    };
    if std::fs::metadata(p).ok()?.len() > 4_000_000 {
        return None;
    }
    let bytes = std::fs::read(p).ok()?;
    Some(format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(bytes)))
}

/// Current shelf, dropping entries whose files were moved or deleted in the meantime.
pub fn list() -> Value {
    let mut l = SHELF.lock().unwrap();
    let before = l.len();
    l.retain(|e| std::path::Path::new(&e.path).exists());
    if l.len() != before {
        save(&l);
    }
    let items: Vec<Value> = l
        .iter()
        .map(|e| {
            let p = PathBuf::from(&e.path);
            let meta = std::fs::metadata(&p).ok();
            let dir = meta.as_ref().map(|m| m.is_dir()).unwrap_or(false);
            json!({
                "path": e.path,
                "name": p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| e.path.clone()),
                "ext": if dir { String::new() } else { p.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default() },
                "dir": dir,
                "size": meta.map(|m| m.len()).unwrap_or(0),
                "thumb": if dir { None } else { thumb(&p) },
            })
        })
        .collect();
    json!(items)
}

fn changed(app: &AppHandle) {
    let _ = app.emit_to("island", "shelf", list());
}

pub fn add(app: &AppHandle, paths: Vec<String>) {
    {
        let mut l = SHELF.lock().unwrap();
        for p in paths {
            l.retain(|e| !e.path.eq_ignore_ascii_case(&p));
            l.insert(0, Entry { path: p });
        }
        l.truncate(MAX);
        save(&l);
    }
    changed(app);
}

pub fn remove(app: &AppHandle, path: &str) {
    {
        let mut l = SHELF.lock().unwrap();
        l.retain(|e| e.path != path);
        save(&l);
    }
    changed(app);
}

pub fn clear(app: &AppHandle) {
    {
        let mut l = SHELF.lock().unwrap();
        l.clear();
        save(&l);
    }
    changed(app);
}

pub fn open(path: &str, reveal: bool) {
    let p: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if reveal {
            let arg: Vec<u16> = format!("/select,\"{}\"", path).encode_utf16().chain(Some(0)).collect();
            ShellExecuteW(None, w!("open"), w!("explorer.exe"), PCWSTR(arg.as_ptr()), None, SW_SHOWNORMAL);
        } else {
            ShellExecuteW(None, w!("open"), PCWSTR(p.as_ptr()), None, None, SW_SHOWNORMAL);
        }
    }
}

/// Starts a real Windows drag with these files, so they can be dropped into Explorer, Telegram, a browser…
pub fn drag_out(app: &AppHandle, paths: Vec<String>) {
    let a = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(win) = a.get_webview_window("island") else { return };
        let files: Vec<PathBuf> = paths.iter().map(PathBuf::from).filter(|p| p.exists()).collect();
        if files.is_empty() {
            return;
        }
        let a2 = a.clone();
        let moved = paths.clone();
        let r = drag::start_drag(
            &win,
            drag::DragItem::Files(files),
            drag::Image::Raw(include_bytes!("../icons/tray.png").to_vec()),
            move |res, _| {
                // what was dragged out has found its place and leaves the shelf
                if let drag::DragResult::Dropped = res {
                    {
                        let mut l = SHELF.lock().unwrap();
                        l.retain(|e| !moved.contains(&e.path));
                        save(&l);
                    }
                    changed(&a2);
                }
                let _ = a2.emit_to("island", "shelf-drag-end", json!({}));
            },
            drag::Options::default(),
        );
        if let Err(e) = r {
            crate::sys::log(&format!("shelf: drag failed: {:?}", e));
        }
    });
}

/* ------------------------------------------------------------ drop zone */

// The island window lets the mouse through, and Windows doesn't reliably offer it file drops that way.
// While a file is being dragged near the island, a plain invisible window sits on top of it and takes the drop.

const ZONE_W: f64 = 460.0;
const ZONE_H: f64 = 130.0;

pub fn create_zone(app: &AppHandle) {
    let built = tauri::WebviewWindowBuilder::new(app, "dropzone", tauri::WebviewUrl::App("dropzone.html".into()))
        .title("Island drop")
        .inner_size(ZONE_W, ZONE_H)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .visible(false)
        .build();
    if let Err(e) = built {
        crate::sys::log(&format!("shelf: drop zone failed: {}", e));
    }
}

fn dragging_now() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsWindowVisible};
    unsafe {
        if GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000 == 0 {
            return false;
        }
        // Explorer and most apps show their drag picture in a window of this class
        match FindWindowW(w!("SysDragImage"), PCWSTR::null()) {
            Ok(h) if !h.0.is_null() => IsWindowVisible(h).as_bool(),
            _ => false,
        }
    }
}

pub fn zone_loop(app: AppHandle) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut shown = false;
    let mut away_since: Option<std::time::Instant> = None;
    let mut logged = false;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(60));
        let (Some(isl), Some(zone)) = (app.get_webview_window("island"), app.get_webview_window("dropzone")) else { continue };
        let drag = dragging_now();
        if drag && !logged {
            logged = true;
            crate::sys::log("shelf: file drag noticed");
        }
        let mut near = false;
        if drag && !crate::sys::FULL_NOW.load(std::sync::atomic::Ordering::SeqCst) {
            if let (Ok(Some(m)), Ok(_)) = (isl.current_monitor(), isl.outer_position()) {
                let mut pt = POINT::default();
                if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                    let k = m.scale_factor();
                    let cx = m.position().x as f64 + m.size().width as f64 / 2.0;
                    let top = m.position().y as f64;
                    let w = ZONE_W * k;
                    let h = ZONE_H * k;
                    // a generous catch area: the zone appears a bit before the cursor reaches the island
                    near = (pt.x as f64 - cx).abs() < w / 2.0 + 60.0 * k && (pt.y as f64) < top + h + 80.0 * k && (pt.y as f64) >= top - 2.0;
                    if near && !shown {
                        let _ = zone.set_position(tauri::PhysicalPosition::new((cx - w / 2.0).round() as i32, top as i32));
                        let _ = zone.set_size(tauri::PhysicalSize::new(w.round() as u32, h.round() as u32));
                    }
                }
            }
        }
        if near {
            away_since = None;
            if !shown {
                shown = true;
                let _ = zone.show();
                let _ = zone.set_always_on_top(true);
                let _ = app.emit_to("island", "drag-near", json!({ "on": true }));
            }
        } else if shown {
            let t = *away_since.get_or_insert_with(std::time::Instant::now);
            if !drag || t.elapsed() > std::time::Duration::from_millis(350) {
                shown = false;
                away_since = None;
                let _ = zone.hide();
                let _ = app.emit_to("island", "drag-near", json!({ "on": false }));
            }
        }
    }
}
