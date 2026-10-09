//! Stand-in for Bun: the main thread plays the JS thread, the window layer
//! runs on its own UI thread. Events come back over a channel, just as they
//! will come back over Bun's concurrent task queue.
//!
//!   cargo run --example demo

use std::borrow::Cow;
use std::sync::{mpsc, Arc};

use buntauri_window::{mime_for, Asset, AssetProvider, HostEvent, UiThread, WindowOptions};

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
    ui.create_window(WindowOptions { title: "buntauri demo".into(), width: 640.0, height: 480.0, url: Some(url.into()), ..Default::default() });
    open += 1;
    if selftest {
        // A page outside app:// must NOT be able to invoke.
        let w = ui.create_window(WindowOptions { title: "untrusted".into(), url: Some("data:text/html,<p>untrusted</p>".into()), ..Default::default() });
        ui.eval(w, "window.__BUNTAURI__.invoke('greet', {name: 'evil'})");
        open += 1;
    }

    // The "JS thread" event loop.
    let started = std::time::Instant::now();
    for ev in rx {
        match ev {
            HostEvent::Created { window } => println!("[host] window {window} created"),
            HostEvent::Invoke { window, call, cmd, args } => {
                println!("[host] invoke #{call} {cmd}({args}) from window {window}");
                match cmd.as_str() {
                    "greet" => {
                        let args: serde_json::Value = serde_json::from_str(&args).unwrap();
                        let name = args["name"].as_str().unwrap_or("onbekende");
                        let reply = serde_json::json!(format!("Hallo {name}, groeten van de host-thread!"));
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
                        ui.resolve(window, call, "null");
                        ui.close(window);
                    }
                    other => ui.reject(window, call, &format!("unknown command: {other}")),
                }
            }
            HostEvent::Closed { window } => {
                println!("[host] window {window} closed");
                open -= 1;
                if open == 0 {
                    break;
                }
            }
            HostEvent::Error { window, message } => {
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
