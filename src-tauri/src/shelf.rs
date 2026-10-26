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

// The island window lets the mouse through, so Windows never offers it a dragged file.
// While something is dragged near the island, a nearly transparent native window sits over it and takes the drop.

use std::sync::atomic::{AtomicIsize, Ordering};
use windows::core::{implement, Ref};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, POINTL, WPARAM};
use windows::Win32::System::Com::{IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop, ReleaseStgMedium, CF_HDROP, DROPEFFECT,
    DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetCursorPos, GetMessageW, DispatchMessageW, TranslateMessage, RegisterClassW,
    SetLayeredWindowAttributes, SetWindowPos, ShowWindow, HWND_TOPMOST, LWA_ALPHA, MSG, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SW_HIDE, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

const ZONE_W: f64 = 480.0;
const ZONE_H: f64 = 140.0;
static ZONE: AtomicIsize = AtomicIsize::new(0);

fn pick_effect(e: *mut DROPEFFECT) {
    unsafe {
        if e.is_null() {
            return;
        }
        let allowed = (*e).0;
        // never MOVE: the file stays where it is, the shelf only remembers it
        *e = if allowed & DROPEFFECT_COPY.0 != 0 {
            DROPEFFECT_COPY
        } else if allowed & DROPEFFECT_LINK.0 != 0 {
            DROPEFFECT_LINK
        } else {
            DROPEFFECT_NONE
        };
    }
}

fn paths_of(obj: &IDataObject) -> Vec<String> {
    let mut out = Vec::new();
    unsafe {
        let fmt = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
        let Ok(mut med) = obj.GetData(&fmt) else { return out };
        let hdrop = HDROP(med.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(hdrop, i, Some(&mut buf));
            out.push(String::from_utf16_lossy(&buf[..len]));
        }
        ReleaseStgMedium(&mut med);
    }
    out
}

#[implement(IDropTarget)]
struct Catcher {
    app: AppHandle,
}

impl IDropTarget_Impl for Catcher_Impl {
    fn DragEnter(&self, _d: Ref<IDataObject>, _k: MODIFIERKEYS_FLAGS, _p: &POINTL, e: *mut DROPEFFECT) -> windows::core::Result<()> {
        pick_effect(e);
        let _ = self.app.emit_to("island", "drag-near", json!({ "on": true }));
        Ok(())
    }
    fn DragOver(&self, _k: MODIFIERKEYS_FLAGS, _p: &POINTL, e: *mut DROPEFFECT) -> windows::core::Result<()> {
        pick_effect(e);
        Ok(())
    }
    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn Drop(&self, d: Ref<IDataObject>, _k: MODIFIERKEYS_FLAGS, _p: &POINTL, e: *mut DROPEFFECT) -> windows::core::Result<()> {
        pick_effect(e);
        let paths = d.as_ref().map(paths_of).unwrap_or_default();
        crate::sys::log(&format!("shelf: {} file(s) dropped", paths.len()));
        hide_zone();
        if !paths.is_empty() {
            add(&self.app, paths);
            let _ = self.app.emit_to("island", "shelf-dropped", json!({}));
        }
        Ok(())
    }
}

unsafe extern "system" fn zone_proc(h: HWND, m: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    DefWindowProcW(h, m, w, l)
}

fn hide_zone() {
    let h = ZONE.load(Ordering::SeqCst);
    if h != 0 {
        unsafe {
            let _ = ShowWindow(HWND(h as _), SW_HIDE);
        }
    }
}

/// Owns the catcher window: OLE needs it on its own single-threaded thread with a message loop.
fn zone_thread(app: AppHandle) {
    unsafe {
        if let Err(e) = OleInitialize(None) {
            crate::sys::log(&format!("shelf: OleInitialize failed: {}", e));
        }
        let Ok(hinst) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSW { lpfnWndProc: Some(zone_proc), hInstance: hinst.into(), lpszClassName: w!("IslandDropCatcher"), ..Default::default() };
        RegisterClassW(&wc);
        let hwnd = match CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("IslandDropCatcher"),
            w!(""),
            WS_POPUP,
            0,
            0,
            10,
            10,
            None,
            None,
            Some(hinst.into()),
            None,
        ) {
            Ok(h) => h,
            Err(e) => {
                crate::sys::log(&format!("shelf: drop window failed: {}", e));
                return;
            }
        };
        // alpha 1 of 255: invisible to the eye, but still a real target for the mouse and for drops
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA);
        let target: IDropTarget = Catcher { app }.into();
        if let Err(e) = RegisterDragDrop(hwnd, &target) {
            crate::sys::log(&format!("shelf: RegisterDragDrop failed: {}", e));
            return;
        }
        ZONE.store(hwnd.0 as isize, Ordering::SeqCst);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

pub fn create_zone(app: &AppHandle) {
    let a = app.clone();
    std::thread::spawn(move || zone_thread(a));
}

pub fn zone_loop(app: AppHandle) {
    let mut shown = false;
    let mut was_down = false;
    let mut started_inside = false;
    let mut released_at: Option<std::time::Instant> = None;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let h = ZONE.load(Ordering::SeqCst);
        let Some(isl) = app.get_webview_window("island") else { continue };
        if h == 0 {
            continue;
        }
        let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } as u16 & 0x8000 != 0;
        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        let Ok(Some(m)) = isl.current_monitor() else { continue };
        let k = m.scale_factor();
        let cx = m.position().x as f64 + m.size().width as f64 / 2.0;
        let top = m.position().y as f64;
        let (w, hh) = (ZONE_W * k, ZONE_H * k);
        let near = (pt.x as f64 - cx).abs() < w / 2.0 && (pt.y as f64) < top + hh && (pt.y as f64) >= top - 2.0;
        if down && !was_down {
            // a press that begins on the island is a click on it, not a drag from somewhere else
            started_inside = near;
        }
        was_down = down;
        let want = down && !started_inside && near && !crate::sys::FULL_NOW.load(Ordering::SeqCst);
        if want && !shown {
            shown = true;
            released_at = None;
            unsafe {
                let _ = SetWindowPos(HWND(h as _), Some(HWND_TOPMOST), (cx - w / 2.0).round() as i32, top as i32, w.round() as i32, hh.round() as i32, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
            // the island lights up only once a real file drag enters (DragEnter), not when a window is dragged past
        } else if shown && !want {
            // give the drop a moment to land before the window goes away
            let t = *released_at.get_or_insert_with(std::time::Instant::now);
            if t.elapsed() > std::time::Duration::from_millis(if down { 250 } else { 500 }) {
                shown = false;
                released_at = None;
                hide_zone();
                let _ = app.emit_to("island", "drag-near", json!({ "on": false }));
            }
        } else if want {
            released_at = None;
        }
    }
}
