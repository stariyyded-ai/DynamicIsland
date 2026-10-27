//! Self-update: new builds are published to the DynamicIsland repository with a small latest.json next to them.

use serde_json::{json, Value};
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Mutex;
use tauri::{AppHandle, Emitter};

const MANIFEST: &str = "https://raw.githubusercontent.com/stariyyded-ai/DynamicIsland/main/latest.json";
const NO_WINDOW: u32 = 0x0800_0000;
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

static LATEST: Mutex<Option<Value>> = Mutex::new(None);

fn powershell(script: &str) -> Option<String> {
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .creation_flags(NO_WINDOW)
        .output()
        .ok()?;
    if !out.status.success() {
        crate::sys::log(&format!("update: powershell failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Single quotes are the only thing that needs escaping inside a PowerShell '...' string.
fn q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn parse_ver(v: &str) -> Vec<u32> {
    v.trim_start_matches('v').split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
}

fn newer(a: &str, b: &str) -> bool {
    parse_ver(a) > parse_ver(b)
}

pub fn check() -> Value {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let script = format!(
        "[Console]::OutputEncoding=[Text.Encoding]::UTF8; [Net.ServicePointManager]::SecurityProtocol='Tls12'; (Invoke-WebRequest -UseBasicParsing -TimeoutSec 20 -Uri {}).Content",
        q(&format!("{}?t={}", MANIFEST, t))
    );
    let manifest = powershell(&script).and_then(|s| serde_json::from_str::<Value>(s.trim()).ok());
    let Some(m) = manifest else {
        return json!({ "current": CURRENT, "ok": false });
    };
    let latest = m["version"].as_str().unwrap_or("0").to_string();
    let available = newer(&latest, CURRENT) && m["url"].is_string();
    if available {
        *LATEST.lock().unwrap() = Some(m.clone());
    }
    json!({ "current": CURRENT, "ok": true, "latest": latest, "available": available, "notes": m["notes"] })
}

pub fn watch(app: AppHandle) {
    std::thread::sleep(std::time::Duration::from_secs(25));
    let mut announced = String::new();
    loop {
        let r = check();
        if r["available"].as_bool().unwrap_or(false) {
            let v = r["latest"].as_str().unwrap_or("").to_string();
            if v != announced {
                announced = v;
                let _ = app.emit("update", r.clone());
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(6 * 3600));
    }
}

/// Downloads the new build, then hands over to a tiny script that swaps the files once this process has exited.
pub fn install(app: AppHandle) {
    let state = |s: &str| {
        let _ = app.emit("update-state", json!({ "state": s }));
    };
    let m = match LATEST.lock().unwrap().clone() {
        Some(m) => m,
        None => {
            if !check()["available"].as_bool().unwrap_or(false) {
                state("none");
                return;
            }
            LATEST.lock().unwrap().clone().unwrap()
        }
    };
    let url = m["url"].as_str().unwrap_or("").to_string();
    state("downloading");
    let tmp = std::env::temp_dir().join("IslandUpdate");
    let _ = std::fs::remove_dir_all(&tmp);
    let _ = std::fs::create_dir_all(&tmp);
    let zip = tmp.join("update.zip");
    let unpacked = tmp.join("x");
    let script = format!(
        "$ProgressPreference='SilentlyContinue'; [Net.ServicePointManager]::SecurityProtocol='Tls12'; Invoke-WebRequest -UseBasicParsing -TimeoutSec 300 -Uri {} -OutFile {}; Expand-Archive -Force -Path {} -DestinationPath {}",
        q(&url),
        q(&zip.to_string_lossy()),
        q(&zip.to_string_lossy()),
        q(&unpacked.to_string_lossy())
    );
    let new_exe = unpacked.join("Island").join("Island.exe");
    if powershell(&script).is_none() || !new_exe.exists() {
        crate::sys::log("update: download failed");
        state("failed");
        return;
    }
    let Ok(exe) = std::env::current_exe() else {
        state("failed");
        return;
    };
    let dir = exe.parent().map(PathBuf::from).unwrap_or_default();
    let exe_name = exe.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Island.exe".into());
    let replacer = format!(
        "$ErrorActionPreference='SilentlyContinue'\r\n\
         $p = {pid}\r\n\
         while (Get-Process -Id $p) {{ Start-Sleep -Milliseconds 300 }}\r\n\
         Start-Sleep -Milliseconds 400\r\n\
         Copy-Item -Force -Path {src_dll} -Destination {dir}\r\n\
         Copy-Item -Force -Path {src_readme} -Destination {dir}\r\n\
         Copy-Item -Force -Path {src_exe} -Destination {dst_exe}\r\n\
         $k = 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Island'\r\n\
         if ((Get-ItemProperty -Path $k -Name InstallLocation).InstallLocation -eq {dir}) {{ Set-ItemProperty -Path $k -Name DisplayVersion -Value {ver} }}\r\n\
         Start-Process -FilePath {dst_exe}\r\n",
        pid = std::process::id(),
        src_dll = q(&unpacked.join("Island").join("WebView2Loader.dll").to_string_lossy()),
        src_readme = q(&unpacked.join("Island").join("README.txt").to_string_lossy()),
        src_exe = q(&new_exe.to_string_lossy()),
        dir = q(&dir.to_string_lossy()),
        dst_exe = q(&dir.join(&exe_name).to_string_lossy()),
        ver = q(m["version"].as_str().unwrap_or("")),
    );
    // a BOM makes Windows PowerShell read Cyrillic folder names correctly
    let ps1 = tmp.join("replace.ps1");
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(replacer.as_bytes());
    if std::fs::write(&ps1, bytes).is_err() {
        state("failed");
        return;
    }
    let started = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File", &ps1.to_string_lossy()])
        .creation_flags(NO_WINDOW)
        .spawn();
    if started.is_err() {
        state("failed");
        return;
    }
    crate::sys::log(&format!("update: installing {}", m["version"]));
    state("restarting");
    std::thread::sleep(std::time::Duration::from_millis(600));
    app.exit(0);
}
