// Discuss.TM v3.0 — desktop shell: native screen capture + global hotkey.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{io::Cursor, sync::Mutex, thread, time::Duration};

use image::{codecs::jpeg::JpegEncoder, imageops::FilterType, DynamicImage};
use tauri::{ipc::Response, AppHandle, Emitter, Manager, WebviewWindow};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
use xcap::Monitor;

pub const VERSION: &str = "3.0";
const MAX_WIDTH: u32 = 2560; // keep screenshots light on memory and disk

/// Screenshot taken by the global hotkey, waiting for the page to collect it.
struct Pending(Mutex<Option<Vec<u8>>>);

fn pick_monitor(x: i32, y: i32) -> Result<Monitor, String> {
    if let Ok(m) = Monitor::from_point(x, y) {
        return Ok(m);
    }
    let mut first = None;
    for m in Monitor::all().map_err(|e| e.to_string())? {
        if m.is_primary().unwrap_or(false) {
            return Ok(m);
        }
        if first.is_none() {
            first = Some(m);
        }
    }
    first.ok_or_else(|| "No monitor found".to_string())
}

fn encode(img: image::RgbaImage) -> Result<Vec<u8>, String> {
    let mut dynimg = DynamicImage::ImageRgba8(img);
    if dynimg.width() > MAX_WIDTH {
        let h = (dynimg.height() as f64 * MAX_WIDTH as f64 / dynimg.width() as f64).round() as u32;
        dynimg = dynimg.resize_exact(MAX_WIDTH, h.max(1), FilterType::Triangle);
    }
    let rgb = dynimg.to_rgb8();
    let mut buf = Cursor::new(Vec::with_capacity(1 << 20));
    let enc = JpegEncoder::new_with_quality(&mut buf, 88);
    rgb.write_with_encoder(enc).map_err(|e| e.to_string())?;
    Ok(buf.into_inner())
}

/// Capture the monitor at `at` (or under the window), hiding our window first if asked.
fn capture_now(win: &WebviewWindow, hide: bool, at: Option<(i32, i32)>) -> Result<Vec<u8>, String> {
    let (cx, cy) = match at {
        Some(p) => p,
        None => {
            let p = win.outer_position().map_err(|e| e.to_string())?;
            let s = win.outer_size().map_err(|e| e.to_string())?;
            (p.x + s.width as i32 / 2, p.y + s.height as i32 / 2)
        }
    };
    let visible = win.is_visible().unwrap_or(false) && !win.is_minimized().unwrap_or(false);
    let must_hide = hide && visible;
    if must_hide {
        let _ = win.hide();
        thread::sleep(Duration::from_millis(300));
    }
    let shot = pick_monitor(cx, cy).and_then(|m| m.capture_image().map_err(|e| e.to_string()));
    if must_hide {
        let _ = win.show();
        let _ = win.set_focus();
    }
    encode(shot?)
}

#[tauri::command]
async fn capture_screen(window: WebviewWindow) -> Result<Response, String> {
    let w = window.clone();
    let bytes = tauri::async_runtime::spawn_blocking(move || capture_now(&w, true, None))
        .await
        .map_err(|e| e.to_string())??;
    Ok(Response::new(bytes))
}

#[tauri::command]
fn take_pending(state: tauri::State<'_, Pending>) -> Response {
    let bytes = state.0.lock().map(|mut g| g.take()).ok().flatten().unwrap_or_default();
    Response::new(bytes)
}

fn hotkey_capture(app: AppHandle) {
    thread::spawn(move || {
        let Some(win) = app.get_webview_window("main") else { return };
        let focused = win.is_focused().unwrap_or(false);
        let at = win.cursor_position().ok().map(|p| (p.x as i32, p.y as i32));
        match capture_now(&win, focused, at) {
            Ok(bytes) => {
                if let Ok(mut g) = app.state::<Pending>().0.lock() {
                    *g = Some(bytes);
                }
                let _ = win.unminimize();
                let _ = win.show();
                let _ = win.set_focus();
                let _ = app.emit("screen-captured", ());
            }
            Err(e) => {
                let _ = app.emit("capture-failed", e);
            }
        }
    });
}

fn main() {
    let hotkey = Shortcut::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyD);
    tauri::Builder::default()
        .manage(Pending(Mutex::new(None)))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, sc, ev| {
                    if ev.state == ShortcutState::Pressed && sc == &hotkey {
                        hotkey_capture(app.clone());
                    }
                })
                .build(),
        )
        .setup(move |app| {
            // If another program already owns Ctrl+Shift+D, the app still runs without the hotkey.
            let _ = app.global_shortcut().register(hotkey);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![capture_screen, take_pending])
        .run(tauri::generate_context!())
        .expect("error while running Discuss.TM");
}
