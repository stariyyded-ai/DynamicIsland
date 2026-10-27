//! Soft interface sounds. They are synthesised on first use and played from memory, so the app ships no audio files.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::sync::Mutex;
use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

const RATE: u32 = 44_100;

/// "soft", "drops", "bell" or "off"
pub static SET: Mutex<String> = Mutex::new(String::new());
/// PlaySound reads the buffer while it plays, so every clip stays alive here once made.
static CACHE: Mutex<Option<HashMap<String, Vec<u8>>>> = Mutex::new(None);

struct Clip(Vec<f32>);

impl Clip {
    fn new(secs: f32) -> Self {
        Clip(vec![0.0; (secs * RATE as f32) as usize])
    }

    /// A sine whose pitch glides from f0 to f1, with a quick attack and an exponential fade.
    fn tone(&mut self, at: f32, f0: f32, f1: f32, glide: f32, decay: f32, amp: f32) {
        let start = (at * RATE as f32) as usize;
        let mut phase = 0.0f32;
        for i in start..self.0.len() {
            let t = (i - start) as f32 / RATE as f32;
            let k = (t / glide.max(0.001)).min(1.0);
            let f = f0 + (f1 - f0) * k;
            phase += TAU * f / RATE as f32;
            let env = (t / 0.006).min(1.0) * (-t / decay).exp();
            if env < 0.0005 && t > 0.05 {
                break;
            }
            self.0[i] += phase.sin() * env * amp;
        }
    }

    /// A small bell: a few inharmonic partials that die away at different speeds.
    fn bell(&mut self, at: f32, f: f32, amp: f32) {
        for (ratio, a, d) in [(1.0, 1.0, 0.9), (2.76, 0.45, 0.45), (5.4, 0.22, 0.22), (8.93, 0.1, 0.12)] {
            self.tone(at, f * ratio, f * ratio, 0.0, d, amp * a);
        }
    }

    fn wav(&self) -> Vec<u8> {
        let peak = self.0.iter().fold(0.0f32, |m, v| m.max(v.abs())).max(1.0);
        let data: Vec<u8> = self
            .0
            .iter()
            .flat_map(|v| ((((v / peak) * 0.55).clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
            .collect();
        let mut out = Vec::with_capacity(44 + data.len());
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&RATE.to_le_bytes());
        out.extend_from_slice(&(RATE * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&data);
        out
    }
}

/// kind: "notify", "drop", "done" or "remind"
fn make(set: &str, kind: &str) -> Clip {
    let mut c = Clip::new(1.4);
    match (set, kind) {
        ("drops", "notify") => {
            c.tone(0.0, 500.0, 1500.0, 0.035, 0.07, 0.8);
            c.tone(0.13, 650.0, 1900.0, 0.035, 0.07, 0.7);
        }
        ("drops", "drop") => c.tone(0.0, 380.0, 1150.0, 0.045, 0.09, 0.9),
        ("drops", "done") => {
            for (i, f) in [1300.0, 1650.0, 2100.0].iter().enumerate() {
                c.tone(i as f32 * 0.09, f * 0.4, *f, 0.03, 0.07, 0.7);
            }
        }
        ("drops", _) => {
            for i in 0..3 {
                c.tone(i as f32 * 0.2, 500.0, 1600.0, 0.035, 0.08, 0.8);
            }
        }
        ("bell", "notify") => c.bell(0.0, 1320.0, 0.8),
        ("bell", "drop") => c.bell(0.0, 880.0, 0.8),
        ("bell", "done") => {
            c.bell(0.0, 1047.0, 0.6);
            c.bell(0.12, 1568.0, 0.6);
        }
        ("bell", _) => {
            c.bell(0.0, 1175.0, 0.6);
            c.bell(0.25, 1175.0, 0.6);
            c.bell(0.5, 1568.0, 0.6);
        }
        (_, "notify") => {
            c.tone(0.0, 880.0, 880.0, 0.0, 0.16, 0.7);
            c.tone(0.11, 1175.0, 1175.0, 0.0, 0.22, 0.6);
        }
        (_, "drop") => c.tone(0.0, 1250.0, 520.0, 0.11, 0.12, 0.9),
        (_, "done") => {
            for (i, f) in [1047.0, 1319.0, 1568.0].iter().enumerate() {
                c.tone(i as f32 * 0.075, *f, *f, 0.0, 0.18, 0.55);
            }
        }
        _ => {
            for (i, f) in [988.0, 1319.0, 988.0].iter().enumerate() {
                c.tone(i as f32 * 0.22, *f, *f, 0.0, 0.25, 0.6);
            }
        }
    }
    c
}

/// Plays with the chosen set, or with `set_override` when the settings page previews one.
pub fn play(kind: &str, set_override: Option<&str>) {
    let set = match set_override {
        Some(s) => s.to_string(),
        None => SET.lock().unwrap().clone(),
    };
    if set.is_empty() || set == "off" {
        return;
    }
    let key = format!("{}|{}", set, kind);
    let mut cache = CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    let wav = cache.entry(key).or_insert_with(|| make(&set, kind).wav());
    unsafe {
        let _ = PlaySoundW(PCWSTR(wav.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}
