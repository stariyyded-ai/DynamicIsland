#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod extra;
mod shelf;
mod stickers;
mod sys;
mod update;

use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, HWND};
use windows::Win32::System::Registry::{
    RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, SetWindowLongPtrW, GWL_EXSTYLE, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
};

const WIN_W: f64 = 660.0;
const WIN_H: f64 = 420.0;
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

static SETTINGS: Mutex<Option<Value>> = Mutex::new(None);

fn data_dir() -> PathBuf {
    let base = std::env::var("APPDATA").map(PathBuf::from).unwrap_or_else(|_| std::env::temp_dir());
    let d = base.join("Island");
    let _ = std::fs::create_dir_all(&d);
    d
}

fn default_settings() -> Value {
    json!({
        "theme": "dark", "size": "m", "op": 100, "top": 8, "mon": 1,
        "hideFull": true, "dnd": false, "osd": true, "notifyOther": true,
        "mods": { "music": 1, "volume": 1, "brightness": 1, "layout": 1, "download": 1,
                  "clip": 1, "notes": 1, "notify": 1, "copy": 1 }
    })
}

fn merge(base: &mut Value, over: &Value) {
    if let (Some(b), Some(o)) = (base.as_object_mut(), over.as_object()) {
        for (k, v) in o {
            match b.get_mut(k) {
                Some(bv) if bv.is_object() && v.is_object() => merge(bv, v),
                _ => {
                    b.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

fn load_settings() -> Value {
    let mut s = default_settings();
    if let Ok(txt) = std::fs::read_to_string(data_dir().join("settings.json")) {
        if let Ok(v) = serde_json::from_str::<Value>(&txt) {
            merge(&mut s, &v);
        }
    }
    s
}

fn current_settings() -> Value {
    let mut g = SETTINGS.lock().unwrap();
    if g.is_none() {
        *g = Some(load_settings());
    }
    g.clone().unwrap()
}

/* ---------------------------------------------------------------- autostart */

fn exe_path() -> String {
    std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn autostart_get() -> bool {
    unsafe {
        let mut buf = [0u16; 1024];
        let mut len = (buf.len() * 2) as u32;
        let r = RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Island"), RRF_RT_REG_SZ, None, Some(buf.as_mut_ptr() as _), Some(&mut len));
        if r.is_err() {
            return false;
        }
        let s = String::from_utf16_lossy(&buf[..(len as usize / 2).saturating_sub(1)]);
        s.contains(&exe_path())
    }
}

fn autostart_set(on: bool) {
    unsafe {
        if on {
            let v = wide(&format!("\"{}\"", exe_path()));
            let _ = RegSetKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Island"), REG_SZ.0, Some(v.as_ptr() as _), (v.len() * 2) as u32);
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Island"));
        }
    }
}

/* ----------------------------------------------------------------- window */

fn position_island(app: &AppHandle) {
    let Some(win) = app.get_webview_window("island") else { return };
    let s = current_settings();
    let idx = s["mon"].as_u64().unwrap_or(1).max(1) as usize - 1;
    let mons = win.available_monitors().unwrap_or_default();
    let mon = mons.get(idx).cloned().or_else(|| win.primary_monitor().ok().flatten()).or_else(|| mons.first().cloned());
    let Some(m) = mon else { return };
    // the window spans the monitor's full width so the island sits at its true centre on any DPI
    let w = m.size().width;
    let h = (WIN_H * m.scale_factor()).round() as u32;
    let _ = win.set_position(PhysicalPosition::new(m.position().x, m.position().y));
    let _ = win.set_size(tauri::PhysicalSize::new(w, h));
    let _ = win.set_position(PhysicalPosition::new(m.position().x, m.position().y));
}

pub fn apply_visibility(app: &AppHandle) {
    let hide = sys::FULL_NOW.load(Ordering::SeqCst) || sys::USER_HIDDEN.load(Ordering::SeqCst);
    let _ = app.emit_to("island", "visibility", json!({ "hidden": hide }));
    let Some(win) = app.get_webview_window("island") else { return };
    if hide {
        // let the slide-up animation finish, then take the window out of the compositor
        let a = app.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(650));
            let still = sys::FULL_NOW.load(Ordering::SeqCst) || sys::USER_HIDDEN.load(Ordering::SeqCst);
            if still {
                if let Some(w) = a.get_webview_window("island") {
                    let _ = w.hide();
                }
            }
        });
    } else {
        let _ = win.show();
        let _ = win.set_always_on_top(true);
    }
}

fn open_settings(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
        return;
    }
    let _ = WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
        .title("Настройки острова")
        .inner_size(480.0, 760.0)
        .min_inner_size(400.0, 500.0)
        .center()
        .build();
}

fn apply_flags(s: &Value) {
    sys::HIDE_FULL.store(s["hideFull"].as_bool().unwrap_or(true), Ordering::SeqCst);
    let m = &s["mods"]["volume"];
    let vol_on = !(m == &json!(0) || m == &json!(false));
    extra::REPLACE_OSD.store(vol_on && s["osd"].as_bool().unwrap_or(true), Ordering::SeqCst);
}

/* --------------------------------------------------------------- commands */

#[tauri::command]
async fn get_settings() -> Value {
    let mut s = current_settings();
    s["autostart"] = json!(autostart_get());
    s
}

#[tauri::command]
async fn save_settings(app: AppHandle, settings: Value) {
    let mut s = current_settings();
    merge(&mut s, &settings);
    if let Some(a) = settings.get("autostart").and_then(|v| v.as_bool()) {
        autostart_set(a);
    }
    if let Some(o) = s.as_object_mut() {
        o.remove("autostart");
    }
    let _ = std::fs::write(data_dir().join("settings.json"), serde_json::to_string_pretty(&s).unwrap_or_default());
    *SETTINGS.lock().unwrap() = Some(s.clone());
    apply_flags(&s);
    position_island(&app);
    let mut out = s.clone();
    out["autostart"] = json!(autostart_get());
    let _ = app.emit("settings", out);
}

#[tauri::command]
async fn monitors(app: AppHandle) -> Value {
    let Some(win) = app.get_webview_window("island") else { return json!([]) };
    let list: Vec<Value> = win
        .available_monitors()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(i, m)| json!({ "index": i + 1, "w": m.size().width, "h": m.size().height }))
        .collect();
    json!(list)
}

#[tauri::command]
async fn set_hit(rects: Vec<sys::HitRect>) {
    *sys::HIT.lock().unwrap() = rects;
}

#[tauri::command]
async fn set_solid(on: bool) {
    sys::FORCE_SOLID.store(on, Ordering::SeqCst);
}

#[tauri::command]
async fn media_cmd(cmd: String, pos: Option<f64>) {
    sys::media_cmd(&cmd, pos);
}

#[tauri::command]
async fn set_volume(v: u32) {
    std::thread::spawn(move || sys::set_volume(v));
}

#[tauri::command]
async fn clip_list() -> Value {
    json!(sys::CLIPS.lock().unwrap().clone())
}

#[tauri::command]
async fn clip_set(index: usize) -> bool {
    sys::clip_set(index)
}

#[tauri::command]
async fn note_load() -> String {
    std::fs::read_to_string(data_dir().join("notes.txt")).unwrap_or_default()
}

#[tauri::command]
async fn note_save(text: String) {
    let _ = std::fs::write(data_dir().join("notes.txt"), text);
}

#[tauri::command]
async fn open_settings_cmd(app: AppHandle) {
    open_settings(&app);
}

#[tauri::command]
async fn notif_open(aumid: String, launch: Option<String>) {
    extra::notif_open(&aumid, launch.as_deref().unwrap_or(""));
}

#[tauri::command]
async fn notif_act(aumid: String, args: String, input: Option<String>, text: Option<String>) -> bool {
    extra::notif_act(&aumid, &args, input.as_deref(), text.as_deref())
}

#[tauri::command]
async fn copy_show() {
    extra::copy_show();
}

#[tauri::command]
async fn sticker_create(app: AppHandle, text: String) {
    stickers::create(&app, text);
}

#[tauri::command]
async fn sticker_get(id: u64) -> Value {
    stickers::get(id)
}

#[tauri::command]
async fn sticker_update(app: AppHandle, id: u64, patch: Value) {
    stickers::update(&app, id, patch);
}

#[tauri::command]
async fn sticker_resize(app: AppHandle, id: u64, h: f64) {
    stickers::resize(&app, id, h);
}

#[tauri::command]
async fn sticker_delete(app: AppHandle, id: u64) {
    stickers::delete(&app, id);
}

#[tauri::command]
async fn stickers_show_all(app: AppHandle) {
    stickers::show_all(&app);
}

#[tauri::command]
async fn reminder_action(app: AppHandle, sid: u64, idx: usize, text: String, action: String) {
    stickers::reminder_action(&app, sid, idx, &text, &action);
}

#[tauri::command]
async fn shelf_list() -> Value {
    shelf::list()
}

#[tauri::command]
async fn shelf_add(app: AppHandle, paths: Vec<String>) {
    shelf::add(&app, paths);
}

#[tauri::command]
async fn shelf_remove(app: AppHandle, path: String) {
    shelf::remove(&app, &path);
}

#[tauri::command]
async fn shelf_clear(app: AppHandle) {
    shelf::clear(&app);
}

#[tauri::command]
async fn shelf_open(path: String, reveal: bool) {
    shelf::open(&path, reveal);
}

#[tauri::command]
async fn shelf_drag(app: AppHandle, paths: Vec<String>) {
    shelf::drag_out(&app, paths);
}

#[tauri::command]
async fn update_check() -> Value {
    tauri::async_runtime::spawn_blocking(update::check).await.unwrap_or(Value::Null)
}

#[tauri::command]
async fn update_install(app: AppHandle) {
    std::thread::spawn(move || update::install(app));
}

#[tauri::command]
async fn app_version() -> String {
    update::CURRENT.to_string()
}

#[tauri::command]
async fn stickers_count() -> Value {
    stickers::counts()
}

/* ------------------------------------------------------------------- main */

fn main() {
    unsafe {
        let _ = CreateMutexW(None, true, w!("Local\\IslandForWindows"));
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return;
        }
    }
    let first_run = !data_dir().join("settings.json").exists();

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            get_settings, save_settings, monitors, set_hit, set_solid, media_cmd, set_volume,
            clip_list, clip_set, note_load, note_save, open_settings_cmd, notif_open, notif_act, copy_show,
            sticker_create, sticker_get, sticker_update, sticker_resize, sticker_delete, stickers_show_all, stickers_count, reminder_action,
            shelf_list, shelf_add, shelf_remove, shelf_clear, shelf_open, shelf_drag,
            update_check, update_install, app_version
        ])
        .setup(move |app| {
            let handle = app.handle().clone();
            let s = current_settings();
            apply_flags(&s);
            if first_run {
                autostart_set(true);
                let _ = std::fs::write(data_dir().join("settings.json"), serde_json::to_string_pretty(&s).unwrap_or_default());
            }

            let win = WebviewWindowBuilder::new(app, "island", WebviewUrl::App("index.html".into()))
                .title("Island")
                .inner_size(WIN_W, WIN_H)
                .transparent(true)
                .decorations(false)
                .always_on_top(true)
                .skip_taskbar(true)
                .resizable(false)
                .shadow(false)
                .focused(false)
                .visible(false)
                .build()?;
            if let Ok(hwnd) = win.hwnd() {
                unsafe {
                    let h = HWND(hwnd.0 as _);
                    let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
                    SetWindowLongPtrW(h, GWL_EXSTYLE, (ex | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize));
                }
            }
            let _ = win.set_ignore_cursor_events(true);
            position_island(&handle);
            let _ = win.show();

            let m_settings = MenuItem::with_id(app, "settings", "Настройки…", true, None::<&str>)?;
            let m_toggle = MenuItem::with_id(app, "toggle", "Скрыть остров", true, None::<&str>)?;
            let m_quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
            let sep = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&m_settings, &m_toggle, &sep, &m_quit])?;
            let toggle_item = m_toggle.clone();
            let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
            TrayIconBuilder::with_id("tray")
                .icon(icon)
                .tooltip("Остров")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, ev| match ev.id().as_ref() {
                    "settings" => open_settings(app),
                    "toggle" => {
                        let hidden = !sys::USER_HIDDEN.load(Ordering::SeqCst);
                        sys::USER_HIDDEN.store(hidden, Ordering::SeqCst);
                        let _ = toggle_item.set_text(if hidden { "Показать остров" } else { "Скрыть остров" });
                        apply_visibility(app);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                        open_settings(tray.app_handle());
                    }
                })
                .build(app)?;

            sys::spawn_all(&handle);
            extra::spawn_all(&handle);
            shelf::load();
            let h5 = handle.clone();
            std::thread::spawn(move || update::watch(h5));
            stickers::restore(&handle);
            let h4 = handle.clone();
            std::thread::spawn(move || stickers::reminder_loop(h4));

            // follow monitor layout changes (docking, resolution)
            let h2 = handle.clone();
            std::thread::spawn(move || {
                let mut last = String::new();
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    if let Some(w) = h2.get_webview_window("island") {
                        let sig = format!("{:?}", w.available_monitors().map(|v| v.iter().map(|m| (m.position().x, m.position().y, m.size().width, m.scale_factor().to_bits())).collect::<Vec<_>>()));
                        if sig != last {
                            if !last.is_empty() {
                                position_island(&h2);
                            }
                            last = sig;
                        }
                    }
                }
            });
            if first_run {
                let h3 = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(1500));
                    let _ = h3.emit_to("island", "welcome", json!({}));
                });
            }
            Ok(())
        })
        .on_window_event(|w, ev| {
            if w.label() == "island" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = ev {
                    api.prevent_close();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running app");
}
