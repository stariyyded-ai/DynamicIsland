//! Sticky notes: each note from the island becomes its own small window that can live on any monitor.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Mutex;
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, WebviewUrl, WebviewWindowBuilder, WindowEvent};

pub const W: f64 = 280.0;

#[derive(Clone, Serialize, Deserialize)]
pub struct Item {
    pub text: String,
    #[serde(default)]
    pub done: bool,
    /// reminder time, unix ms
    #[serde(default)]
    pub at: Option<u64>,
    #[serde(default)]
    pub fired: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Sticker {
    pub id: u64,
    #[serde(default)]
    pub items: Vec<Item>,
    /// pinned stickers can't be dragged away from where they were left
    #[serde(default)]
    pub locked: bool,
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default)]
    pub x: Option<i32>,
    #[serde(default)]
    pub y: Option<i32>,
    #[serde(default = "default_h")]
    pub h: f64,
}

/// One task per line; a sticker always has at least one (possibly empty) task.
fn items_from(text: &str) -> Vec<Item> {
    let v: Vec<Item> = text.lines().map(|l| l.trim()).filter(|l| !l.is_empty()).map(|l| Item { text: l.to_string(), done: false, at: None, fired: false }).collect();
    if v.is_empty() {
        vec![Item { text: String::new(), done: false, at: None, fired: false }]
    } else {
        v
    }
}

fn is_open(s: &Sticker) -> bool {
    s.items.iter().any(|i| !i.done && !i.text.trim().is_empty())
}

fn default_color() -> String {
    "yellow".into()
}
fn default_h() -> f64 {
    160.0
}

static STICKERS: Mutex<Vec<Sticker>> = Mutex::new(Vec::new());

fn file() -> std::path::PathBuf {
    crate::data_dir().join("stickers.json")
}

fn save(list: &[Sticker]) {
    let _ = std::fs::write(file(), serde_json::to_string_pretty(list).unwrap_or_default());
}

fn label(id: u64) -> String {
    format!("sticker-{}", id)
}

fn broadcast(app: &AppHandle) {
    let n = STICKERS.lock().unwrap().len();
    let open = STICKERS.lock().unwrap().iter().filter(|s| is_open(s)).count();
    let _ = app.emit_to("island", "stickers", json!({ "count": n, "open": open }));
}

fn open_window(app: &AppHandle, s: &Sticker) {
    if app.get_webview_window(&label(s.id)).is_some() {
        return;
    }
    let mut b = WebviewWindowBuilder::new(app, label(s.id), WebviewUrl::App(format!("sticker.html?id={}", s.id).into()))
        .title("Стикер")
        .inner_size(W, s.h)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .resizable(false)
        .skip_taskbar(true)
        .visible(false);
    if s.x.is_none() {
        b = b.center();
    }
    let Ok(win) = b.build() else { return };
    if let (Some(x), Some(y)) = (s.x, s.y) {
        // a sticker left on a monitor that is gone comes back to the main one
        let on_screen = win.available_monitors().unwrap_or_default().iter().any(|m| {
            let p = m.position();
            let sz = m.size();
            x >= p.x - 40 && x < p.x + sz.width as i32 - 40 && y >= p.y - 10 && y < p.y + sz.height as i32 - 40
        });
        if on_screen {
            let _ = win.set_position(PhysicalPosition::new(x, y));
        } else {
            let _ = win.center();
        }
    }
    let id = s.id;
    win.on_window_event(move |ev| {
        if let WindowEvent::Moved(p) = ev {
            let mut list = STICKERS.lock().unwrap();
            if let Some(s) = list.iter_mut().find(|s| s.id == id) {
                s.x = Some(p.x);
                s.y = Some(p.y);
            }
            save(&list);
        }
    });
    let _ = win.show();
}

pub fn restore(app: &AppHandle) {
    // 0.4 kept a single text per sticker; split it into tasks
    let raw: Vec<Value> = std::fs::read_to_string(file()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    let mut list: Vec<Sticker> = raw
        .into_iter()
        .filter_map(|mut v| {
            if v.get("items").is_none() {
                let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                let done = v.get("done").and_then(|d| d.as_bool()).unwrap_or(false);
                let mut items = items_from(&text);
                items.iter_mut().for_each(|i| i.done = done);
                v["items"] = json!(items);
            }
            serde_json::from_value(v).ok()
        })
        .collect();
    // the single note from earlier versions becomes the first sticker
    let old = crate::data_dir().join("notes.txt");
    if let Ok(t) = std::fs::read_to_string(&old) {
        if !t.trim().is_empty() {
            list.push(Sticker { id: now_id(), items: items_from(&t), locked: false, color: default_color(), x: None, y: None, h: default_h() });
        }
        let _ = std::fs::rename(&old, crate::data_dir().join("notes-old.txt"));
        save(&list);
    }
    *STICKERS.lock().unwrap() = list.clone();
    for s in &list {
        open_window(app, s);
    }
    broadcast(app);
}

fn now_id() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

pub fn create(app: &AppHandle, text: String) {
    let s = {
        let mut list = STICKERS.lock().unwrap();
        let mut id = now_id();
        while list.iter().any(|s| s.id == id) {
            id += 1;
        }
        // fan new stickers out a little so they don't land exactly on top of each other
        let n = list.len() as i32;
        let (x, y) = match app.get_webview_window("island").and_then(|w| w.current_monitor().ok().flatten()) {
            Some(m) => {
                let k = m.scale_factor();
                let x = m.position().x + m.size().width as i32 - ((W + 40.0) * k) as i32 - (n % 5) * (24.0 * k) as i32;
                let y = m.position().y + (120.0 * k) as i32 + (n % 5) * (28.0 * k) as i32;
                (Some(x), Some(y))
            }
            None => (None, None),
        };
        let s = Sticker { id, items: items_from(&text), locked: false, color: default_color(), x, y, h: default_h() };
        list.push(s.clone());
        save(&list);
        s
    };
    open_window(app, &s);
    broadcast(app);
}

pub fn get(id: u64) -> Value {
    STICKERS.lock().unwrap().iter().find(|s| s.id == id).map(|s| json!(s)).unwrap_or(Value::Null)
}

pub fn update(app: &AppHandle, id: u64, patch: Value) {
    let mut list = STICKERS.lock().unwrap();
    let Some(s) = list.iter_mut().find(|s| s.id == id) else { return };
    if let Some(mut items) = patch.get("items").and_then(|v| serde_json::from_value::<Vec<Item>>(v.clone()).ok()) {
        // the window may save an older copy right after a reminder went off; don't let it fire twice
        for it in items.iter_mut() {
            if it.at.is_some() && s.items.iter().any(|o| o.fired && o.at == it.at && o.text == it.text) {
                it.fired = true;
            }
        }
        s.items = items;
    }
    if let Some(l) = patch.get("locked").and_then(|v| v.as_bool()) {
        s.locked = l;
    }
    if let Some(c) = patch.get("color").and_then(|v| v.as_str()) {
        s.color = c.to_string();
    }
    save(&list);
    drop(list);
    broadcast(app);
}

pub fn resize(app: &AppHandle, id: u64, h: f64) {
    let h = h.clamp(110.0, 640.0);
    if let Some(w) = app.get_webview_window(&label(id)) {
        let _ = w.set_size(tauri::LogicalSize::new(W, h));
    }
    let mut list = STICKERS.lock().unwrap();
    if let Some(s) = list.iter_mut().find(|s| s.id == id) {
        s.h = h;
    }
    save(&list);
}

pub fn delete(app: &AppHandle, id: u64) {
    let mut list = STICKERS.lock().unwrap();
    list.retain(|s| s.id != id);
    save(&list);
    drop(list);
    if let Some(w) = app.get_webview_window(&label(id)) {
        let _ = w.destroy();
    }
    broadcast(app);
}

pub fn show_all(app: &AppHandle) {
    let list = STICKERS.lock().unwrap().clone();
    for s in &list {
        match app.get_webview_window(&label(s.id)) {
            Some(w) => {
                let _ = w.show();
                let _ = w.unminimize();
                let _ = w.set_always_on_top(true);
                let _ = w.set_always_on_top(false);
                let _ = w.set_focus();
            }
            None => open_window(app, s),
        }
    }
}

pub fn counts() -> Value {
    let list = STICKERS.lock().unwrap();
    json!({ "count": list.len(), "open": list.iter().filter(|s| is_open(s)).count() })
}

/* -------------------------------------------------------------- reminders */

fn push_to_window(app: &AppHandle, s: &Sticker) {
    let _ = app.emit_to(label(s.id).as_str(), "sticker-changed", json!(s));
}

pub fn reminder_loop(app: AppHandle) {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3));
        let now = now_id();
        let mut due: Vec<(Sticker, usize)> = Vec::new();
        {
            let mut list = STICKERS.lock().unwrap();
            for s in list.iter_mut() {
                let mut hit = None;
                for (i, it) in s.items.iter_mut().enumerate() {
                    if let Some(at) = it.at {
                        if at <= now && !it.fired && !it.done && !it.text.trim().is_empty() {
                            it.fired = true;
                            hit.get_or_insert(i);
                        }
                    }
                }
                if let Some(i) = hit {
                    due.push((s.clone(), i));
                }
            }
            if !due.is_empty() {
                save(&list);
            }
        }
        for (s, i) in due {
            push_to_window(&app, &s);
            let it = &s.items[i];
            let _ = app.emit_to("island", "reminder", json!({ "sid": s.id, "idx": i, "text": it.text, "at": it.at }));
            crate::sys::chime();
        }
    }
}

pub fn reminder_action(app: &AppHandle, sid: u64, idx: usize, text: &str, action: &str) {
    let snapshot = {
        let mut list = STICKERS.lock().unwrap();
        let Some(s) = list.iter_mut().find(|s| s.id == sid) else { return };
        let i = if s.items.get(idx).map(|i| i.text == text).unwrap_or(false) { Some(idx) } else { s.items.iter().position(|i| i.text == text) };
        if let Some(it) = i.and_then(|i| s.items.get_mut(i)) {
            match action {
                "done" => it.done = true,
                "snooze" => {
                    it.at = Some(now_id() + 10 * 60 * 1000);
                    it.fired = false;
                }
                _ => {}
            }
        }
        let c = s.clone();
        save(&list);
        c
    };
    push_to_window(app, &snapshot);
    if action == "show" {
        if let Some(w) = app.get_webview_window(&label(sid)) {
            let _ = w.show();
            let _ = w.unminimize();
            let _ = w.set_focus();
        }
    }
    broadcast(app);
}
