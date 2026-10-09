//! Stand-in for Bun: the main thread plays the JS thread, the window layer
//! runs on its own UI thread. Events come back over a channel, just as they
//! will come back over Bun's concurrent task queue.
//!
//!   cargo run --example demo

use std::borrow::Cow;
use std::sync::{mpsc, Arc};

use base64::Engine as _;
use buntauri_window::{mime_for, Asset, AssetProvider, HostEvent, UiThread, WindowOptions};

const ICON: &[u8] = include_bytes!("assets/icon.png");

/// Assets baked into the binary, like `bun build --compile` will do.
struct Embedded;

impl AssetProvider for Embedded {
    fn get(&self, path: &str) -> Option<Asset> {
        let bytes: &'static [u8] = match path {
            "index.html" => include_bytes!("assets/index.html"),
            "app.js" => include_bytes!("assets/app.js"),
            _ => return None,
        };
        Some(Asset { bytes: Cow::Borrowed(bytes), mime: Cow::Borrowed(mime_for(path)) })
    }
}

fn main() {
    let (tx, rx) = mpsc::channel();
    let ui = UiThread::spawn(move |ev| { let _ = tx.send(ev); }, Arc::new(Embedded)).expect("spawn UI thread");

    let mut open = 0;
    let selftest = std::env::var_os("BUNTAURI_SELFTEST").is_some();
    // With ?selftest the page runs the round-trip test itself and reports via invoke("done").
    let url = if selftest { "index.html?selftest" } else { "index.html" };
    // Same shape as a window in tauri.conf.json; Bun JS will send exactly this.
    let opts = serde_json::json!({
        "label": "main", "title": "buntauri demo", "url": url,
        "width": 640, "height": 480, "minWidth": 400, "minHeight": 300,
        "center": true, "theme": "dark", "backgroundColor": "#111111",
        "icon": base64::engine::general_purpose::STANDARD.encode(ICON),
        "menu": [
            { "text": "File", "items": [
                { "id": "greet", "text": "Say hello", "accelerator": "CmdOrCtrl+H" },
                { "type": "separator" },
                { "id": "quit", "text": "Quit", "accelerator": "CmdOrCtrl+Q" },
            ]},
            { "text": "Edit", "items": [{ "predefined": "copy" }, { "predefined": "paste" }, { "predefined": "selectAll" }] },
            { "text": "View", "items": [{ "id": "dark", "text": "Dark mode", "type": "check", "checked": true }] },
        ],
    });
    let main_window = ui.create_window_json(&opts.to_string()).expect("window options");
    let tray = serde_json::json!({
        "icon": base64::engine::general_purpose::STANDARD.encode(ICON),
        "tooltip": "buntauri demo",
        "menu": [{ "id": "show", "text": "Say hello" }, { "type": "separator" }, { "id": "quit", "text": "Quit" }],
    });
    let tray_id = ui.create_tray_json(&tray.to_string()).expect("tray");
    let mut problems = 0;
    // Selftest phase 2: after the IPC round trip, resize and check the state.
    let mut resize_check = false;
    open += 1;
    if selftest {
        // A page outside app:// must NOT be able to invoke.
        let w = ui.create_window(WindowOptions { label: "untrusted".into(), title: "untrusted".into(), url: Some("data:text/html,<p>untrusted</p>".into()), ..Default::default() });
        ui.eval(w, "window.__BUNTAURI__.invoke('greet', {name: 'evil'})");
        open += 1;
    }

    // The "JS thread" event loop.
    let started = std::time::Instant::now();
    for ev in rx {
        match ev {
            HostEvent::Created { window } => println!("[host] window {window} created"),
            HostEvent::Invoke { window, call, cmd, args, origin, remote } => {
                let how = if remote { ", remote" } else { "" };
                println!("[host] invoke #{call} {cmd}({args}) from window {window} ({origin}{how})");
                match cmd.as_str() {
                    "greet" => {
                        let args: serde_json::Value = serde_json::from_str(&args).unwrap();
                        let name = args["name"].as_str().unwrap_or("stranger");
                        let reply = serde_json::json!(format!("Hello {name}, greetings from the host thread!"));
                        ui.resolve(window, call, &reply.to_string());
                    }
                    "uptime" => ui.resolve(window, call, &started.elapsed().as_secs_f64().to_string()),
                    "title" => {
                        let args: serde_json::Value = serde_json::from_str(&args).unwrap();
                        ui.set_title(window, args["title"].as_str().unwrap_or(""));
                        ui.resolve(window, call, "null");
                    }
                    "tick" => {
                        // Push events to the page from the host.
                        for i in 1..=3 {
                            ui.emit(window, "tick", &i.to_string());
                        }
                        ui.resolve(window, call, "null");
                    }
                    "done" => {
                        println!("[host] selftest result: {args}");
                        println!("[host] native problems: {problems}");
                        ui.remove_tray(tray_id);
                        resize_check = true;
                        ui.window_op_json(window, r#"{"op":"setSize","width":700,"height":500}"#).unwrap();
                        ui.resolve(window, call, "null");
                    }
                    other => ui.reject(window, call, &format!("unknown command: {other}")),
                }
            }
            HostEvent::Menu { window, tray, id } => {
                println!("[host] menu {id:?} (window {window:?}, tray {tray:?})");
                match id.as_str() {
                    "greet" | "show" => ui.emit(main_window, "tick", "\"hello from the menu\""),
                    "quit" => {
                        ui.remove_tray(tray_id);
                        ui.close(main_window);
                    }
                    _ => {}
                }
            }
            HostEvent::Window { window, kind, data } => {
                println!("[host] window {window} {kind} {data}");
                if resize_check && kind == "resized" {
                    let state: serde_json::Value = serde_json::from_str(&ui.window_state_json(window)).unwrap();
                    println!("[host] state after setSize: {}x{} visible={}", state["width"], state["height"], state["visible"]);
                    resize_check = false;
                    ui.close(window);
                }
            }
            HostEvent::Tray { tray, kind, button, .. } => println!("[host] tray {tray} {kind} {button}"),
            HostEvent::TrayFailed { tray, message } => {
                problems += 1;
                eprintln!("[host] tray {tray} failed: {message}");
            }
            HostEvent::DragDrop { window, kind, paths, x, y } => println!("[host] drag {kind} {paths:?} at {x},{y} in window {window}"),
            HostEvent::CreateFailed { window, message } => {
                eprintln!("[host] window {window} failed: {message}");
                open -= 1;
                if open == 0 {
                    break;
                }
            }
            HostEvent::Warning { window, message } => eprintln!("[host] warning (window {window:?}): {message}"),
            HostEvent::Closed { window } => {
                println!("[host] window {window} closed");
                open -= 1;
                if open == 0 {
                    break;
                }
            }
            HostEvent::Error { window, message } => {
                if !message.starts_with("ipc blocked") {
                    problems += 1;
                }
                eprintln!("[host] error (window {window:?}): {message}");
                if let (Some(w), true) = (window, message.starts_with("ipc blocked")) {
                    ui.close(w);
                }
            }
            HostEvent::Exited => break,
        }
    }

    ui.shutdown();
    println!("[host] bye");
}
