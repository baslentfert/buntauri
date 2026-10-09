//! UI host process (macOS, Linux/BSD).
//!
//! The host starts its own executable again with `BUNTAURI_UI_HOST=1`, the
//! way Bun's `Bun.WebView` does, and the child runs the UI event loop
//! ([`crate::ui_main`]) on its main thread.
//! - macOS: AppKit only runs on the process main thread, which in Bun runs JS.
//! - Linux: GTK/WebKitGTK, and the JavaScriptCore inside WebKitGTK, never
//!   enter the host process; one JavaScriptCore per process. With stub
//!   linking (scripts/linux-stubs) the child loads them on demand.
//!
//! Startup handshake: the child's first line is `{"type":"__ready"}` once its
//! event loop runs, or `{"type":"__fatal","message":...}` (e.g. no WebKitGTK);
//! [`Client::spawn`] waits for it, so such failures are its error.
//!
//! The two talk over a Unix socket that is the child's stdin, one JSON value
//! per line: [`Command`]s go down, [`HostEvent`]s (`HostEvent::to_json`) and
//! window state snapshots come back. stdout/stderr stay the host's, so logs
//! and panics of the child show up as usual.
//!
//! The child exits when the socket closes, so it never outlives the host
//! (also not when the host is killed). If the child dies, the host gets
//! `closed` for its windows, `trayfailed` for its trays, then `exited`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::{AssetProvider, Command, Emit, HostEvent, StateSink, States};

/// Set (to `1`) in the environment of the UI host process.
pub const ENV: &str = "BUNTAURI_UI_HOST";
/// `AssetProvider::host_spec` of the host's assets, if any.
pub const ENV_ASSETS: &str = "BUNTAURI_UI_HOST_ASSETS";

/// A state snapshot line from the child: `{"type": "__state", "window": 1, "state": {...}}`.
const STATE: &str = "__state";
/// Handshake lines (see the module docs).
const READY: &str = "__ready";
const FATAL: &str = "__fatal";
/// How long the child may take to bring up its UI.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

/// Host side: the running UI host process.
pub(crate) struct Client {
    lines: Mutex<Option<mpsc::Sender<String>>>,
    /// Windows (false) and trays (true) the host still expects events for.
    owners: Arc<Mutex<HashMap<u32, bool>>>,
    child: Arc<Mutex<Child>>,
    threads: Vec<JoinHandle<()>>,
}

impl Client {
    pub(crate) fn spawn(on_event: Emit, assets: Option<String>, states: States) -> std::io::Result<Client> {
        let (ours, theirs) = UnixStream::pair()?;
        let mut cmd = std::process::Command::new(std::env::current_exe()?);
        cmd.env(ENV, "1").stdin(Stdio::from(OwnedFd::from(theirs)));
        match &assets {
            Some(spec) => cmd.env(ENV_ASSETS, spec),
            None => cmd.env_remove(ENV_ASSETS),
        };
        let mut child = cmd.spawn()?;
        // `cmd` still holds the child's end of the socket; without it a child
        // that dies during startup is an EOF here right away.
        drop(cmd);

        // Handshake: wait for the child's first line.
        ours.set_read_timeout(Some(STARTUP_TIMEOUT))?;
        let mut lines = BufReader::new(ours.try_clone()?);
        let mut first = String::new();
        let got = lines.read_line(&mut first);
        ours.set_read_timeout(None)?;
        let first: serde_json::Value = serde_json::from_str(first.trim()).unwrap_or_default();
        if first["type"] != READY {
            // A timeout leaves the child running; otherwise it is exiting.
            if got.is_err() {
                let _ = child.kill();
            }
            let status = child.wait();
            let message = match first["type"].as_str() {
                Some(FATAL) => first["message"].as_str().unwrap_or("UI host failed").to_string(),
                _ => match (got, status) {
                    (Err(e), _) => format!("UI host did not start: {e}"),
                    (_, Ok(s)) => format!("UI host exited during startup ({s})"),
                    (_, Err(e)) => format!("UI host exited during startup: {e}"),
                },
            };
            return Err(std::io::Error::other(message));
        }
        let child = Arc::new(Mutex::new(child));
        let owners: Arc<Mutex<HashMap<u32, bool>>> = Default::default();

        // Writer: commands are serialized on the caller's thread and written
        // here, so a busy child never blocks the host.
        let (tx, rx) = mpsc::channel::<String>();
        let mut out = ours.try_clone()?;
        let writer = std::thread::Builder::new().name("buntauri-ui-tx".into()).spawn(move || {
            for line in rx {
                if out.write_all(line.as_bytes()).is_err() {
                    break;
                }
            }
            // All senders gone (shutdown): EOF tells the child to exit.
            let _ = out.shutdown(std::net::Shutdown::Write);
        })?;

        let reader = {
            let owners = owners.clone();
            let child = child.clone();
            std::thread::Builder::new().name("buntauri-ui-rx".into()).spawn(move || {
                let mut exited = false;
                for line in lines.lines() {
                    let Ok(line) = line else { break };
                    if let Some(state) = parse_state(&line) {
                        let (id, state) = state;
                        let mut map = states.lock().unwrap();
                        match state {
                            Some(v) => map.insert(id, v),
                            None => map.remove(&id),
                        };
                        continue;
                    }
                    let ev = match HostEvent::from_json(&line) {
                        Ok(ev) => ev,
                        Err(message) => HostEvent::Error { window: None, message: format!("UI host: {message}") },
                    };
                    if ev.ends_owner() {
                        if let Some(id) = ev.owner() {
                            owners.lock().unwrap().remove(&id);
                        }
                    }
                    exited |= matches!(ev, HostEvent::Exited);
                    on_event(ev);
                }
                // The child is gone (or closed its end): settle what is still open.
                let status = child.lock().unwrap().wait();
                let left: Vec<(u32, bool)> = owners.lock().unwrap().drain().collect();
                if !left.is_empty() {
                    let why = match status {
                        Ok(s) => format!("UI host process exited ({s})"),
                        Err(e) => format!("UI host process lost: {e}"),
                    };
                    on_event(HostEvent::Error { window: None, message: why.clone() });
                    for (id, tray) in left {
                        states.lock().unwrap().remove(&id);
                        on_event(if tray {
                            HostEvent::TrayFailed { tray: id, message: why.clone() }
                        } else {
                            HostEvent::Closed { window: id }
                        });
                    }
                }
                if !exited {
                    on_event(HostEvent::Exited);
                }
            })?
        };

        Ok(Client { lines: Mutex::new(Some(tx)), owners, child, threads: vec![writer, reader] })
    }

    pub(crate) fn send(&self, cmd: &Command) {
        match cmd {
            Command::Create(id, _) => {
                self.owners.lock().unwrap().insert(*id, false);
            }
            Command::TrayCreate(id, ..) => {
                self.owners.lock().unwrap().insert(*id, true);
            }
            Command::TrayRemove(id) => {
                self.owners.lock().unwrap().remove(id);
            }
            _ => {}
        }
        let mut line = match serde_json::to_string(cmd) {
            Ok(l) => l,
            Err(_) => return,
        };
        line.push('\n');
        let shutdown = matches!(cmd, Command::Shutdown);
        let mut lines = self.lines.lock().unwrap();
        if let Some(tx) = lines.as_ref() {
            let _ = tx.send(line);
        }
        if shutdown {
            // Nothing is sent after Shutdown; closing lets the writer finish.
            *lines = None;
        }
    }

    /// After `Shutdown`: wait for the child to exit and the threads to finish.
    pub(crate) fn wait(&mut self) {
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
        let _ = self.child.lock().unwrap().wait();
    }
}

fn parse_state(line: &str) -> Option<(u32, Option<serde_json::Value>)> {
    // Cheap check first: most lines are events.
    if !line.contains(STATE) {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    if v["type"] != STATE {
        return None;
    }
    let id = v["window"].as_u64()? as u32;
    Some((id, if v["state"].is_null() { None } else { Some(v["state"].clone()) }))
}

/// Child side: run the UI on this (main) thread until the host goes away.
pub(crate) fn run(assets: Arc<dyn AssetProvider>) -> ! {
    // SAFETY: stdin is the socket the host gave us; we own it from here on.
    let socket = UnixStream::from(unsafe { OwnedFd::from_raw_fd(0) });
    let out = Arc::new(Mutex::new(socket.try_clone().expect("UI host: clone socket")));
    let write = move |line: String| {
        let mut out = out.lock().unwrap();
        // The host is gone if this fails; the reader sees EOF and shuts down.
        let _ = out.write_all(line.as_bytes()).and_then(|_| out.write_all(b"\n"));
    };
    #[cfg(not(target_os = "macos"))]
    if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
        let message = "cannot open windows: no display (neither DISPLAY nor WAYLAND_DISPLAY is set)";
        write(serde_json::json!({ "type": FATAL, "message": message }).to_string());
        std::process::exit(1);
    }
    #[cfg(not(target_os = "macos"))]
    if let Err(message) = crate::load_gtk() {
        write(serde_json::json!({ "type": FATAL, "message": message }).to_string());
        std::process::exit(1);
    }
    let emit: Emit = {
        let write = write.clone();
        Arc::new(move |ev: HostEvent| write(ev.to_json()))
    };
    let on_state: StateSink = {
        let write = write.clone();
        Arc::new(move |id, state| {
            write(serde_json::json!({ "type": STATE, "window": id, "state": state }).to_string());
        })
    };

    let (ready_tx, ready_rx) = mpsc::channel();
    {
        let emit = emit.clone();
        let ready = write.clone();
        std::thread::Builder::new()
            .name("buntauri-ui-host-rx".into())
            .spawn(move || {
                let Ok(proxy) = ready_rx.recv() else { return };
                let proxy: tao::event_loop::EventLoopProxy<Command> = proxy;
                ready(serde_json::json!({ "type": READY }).to_string());
                for line in BufReader::new(socket).lines() {
                    let Ok(line) = line else { break };
                    match serde_json::from_str::<Command>(&line) {
                        Ok(cmd) => {
                            if proxy.send_event(cmd).is_err() {
                                return;
                            }
                        }
                        Err(e) => emit(HostEvent::Error { window: None, message: format!("UI host: bad command: {e}") }),
                    }
                }
                // Host closed the socket (or died): stop.
                let _ = proxy.send_event(Command::Shutdown);
            })
            .expect("UI host: spawn reader");
    }

    crate::ui_main(ready_tx, emit, assets, on_state);
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_lines() {
        assert_eq!(parse_state(r#"{"type":"__state","window":3,"state":{"visible":true}}"#).unwrap().0, 3);
        assert!(parse_state(r#"{"type":"__state","window":3,"state":null}"#).unwrap().1.is_none());
        assert!(parse_state(r#"{"type":"closed","window":3}"#).is_none());
    }
}
