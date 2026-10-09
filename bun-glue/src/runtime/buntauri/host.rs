//! macOS UI host process entry. AppKit needs the process main thread, which
//! in Bun runs JS, so `bun:buntauri` starts this same executable again as its
//! UI host (see `buntauri_window`'s host.rs). `cli::Command::start` calls
//! `maybe_run` first thing, like Bun's own `BUN_INTERNAL_WEBVIEW_HOST` check:
//! before the standalone graph, argv parsing and JSC. In the UI host it never
//! returns; otherwise it does nothing.
//!
//! Lives in the buntauri repo (bun-glue/) and is copied into the Bun tree.

#[inline]
pub(crate) fn maybe_run() {
    #[cfg(target_os = "macos")]
    buntauri_window::run_ui_host_if_requested(|spec| {
        // A compiled app serves app:// from its own executable: load the
        // graph, as `Command::start` would have done after this point.
        if spec.as_deref().is_some_and(|s| bun_standalone_graph::is_bun_standalone_file_path(s.as_bytes())) {
            let _ = bun_standalone_graph::Graph::from_executable();
        }
        super::window::asset_provider(spec)
    });
}
