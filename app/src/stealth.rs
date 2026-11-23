//! Keep the assistant window out of screen sharing/capture.
//!
//! Windows and macOS have first-class per-window capture exclusion. Linux
//! compositors do not expose such a protocol, so on Linux we hide the window
//! while screen sharing is active:
//!
//! * `dbus-monitor` watches the XDG Desktop Portal `ScreenCast` session —
//!   browsers and conferencing apps on Wayland go through it. This reacts in
//!   milliseconds to a share starting/stopping.
//! * `pw-dump` polling stays as a fallback for clients that create a PipeWire
//!   video stream without the portal (and detects a share that was already
//!   running when the assistant started).
//!
//! If neither source works, `Ctrl/Cmd+Shift+H` always hides the window
//! manually.

#[cfg(target_os = "linux")]
use std::collections::HashSet;
#[cfg(target_os = "linux")]
use std::io::{BufRead, BufReader};
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::Duration;

use tauri::WebviewWindow;
#[cfg(target_os = "linux")]
use tauri::{AppHandle, Emitter, Manager};

/// Apply native OS capture exclusion. No-op on Linux; call
/// `spawn_screen_share_watcher` there instead.
pub fn apply_native_capture_exclusion(window: &WebviewWindow) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetWindowDisplayAffinity, WDA_EXCLUDEFROMCAPTURE,
        };

        let hwnd = window.hwnd().map_err(|err| err.to_string())?;
        unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }
            .map_err(|err| format!("SetWindowDisplayAffinity failed: {err}"))?;
        tracing::info!("window excluded from screen capture (WDA_EXCLUDEFROMCAPTURE)");
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSWindow, NSWindowSharingType};

        let ptr = window.ns_window().map_err(|err| err.to_string())?;
        if ptr.is_null() {
            return Err("NSWindow handle is null".to_owned());
        }
        // SAFETY: Tauri owns the NSWindow for the lifetime of the app and this
        // call runs on the main thread during setup.
        let ns_window = unsafe { &*(ptr as *const NSWindow) };
        ns_window.setSharingType(NSWindowSharingType::None);
        tracing::info!("window sharingType set to NSWindowSharingNone");
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = window;
        Ok(())
    }
}

/// Linux: watch for screen sharing. Always emits `silent://screen-share` so
/// the UI can warn about full-screen capture; when `hide_window` is set, also
/// hides the window and restores it (plus always-on-top) afterwards.
#[cfg(target_os = "linux")]
pub fn spawn_screen_share_watcher(
    app: AppHandle,
    poll_ms: u64,
    always_on_top: bool,
    hide_window: bool,
) {
    tauri::async_runtime::spawn(async move {
        let period = Duration::from_millis(poll_ms.clamp(250, 10_000));
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        let (portal_tx, mut portal_rx) = tokio::sync::mpsc::unbounded_channel();
        spawn_portal_monitor(portal_tx);
        let mut portal_rx_open = true;

        let mut portal_sessions: HashSet<String> = HashSet::new();
        let mut pw_active = false;
        let mut sharing_before = false;
        let mut hidden_by_watcher = false;
        let mut warned = false;

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match tokio::task::spawn_blocking(screen_share_active).await {
                        Ok(Ok(active)) => pw_active = active,
                        Ok(Err(err)) => {
                            if !warned {
                                tracing::warn!(
                                    %err,
                                    "PipeWire screen-share detection unavailable; \
                                     use Ctrl/Cmd+Shift+H to hide manually"
                                );
                                warned = true;
                            }
                        }
                        Err(err) => {
                            if !warned {
                                tracing::warn!(%err, "screen-share detector task failed");
                                warned = true;
                            }
                        }
                    }
                }
                event = portal_rx.recv(), if portal_rx_open => {
                    match event {
                        Some(PortalEvent::Started(path)) => {
                            tracing::debug!(%path, "portal screencast started");
                            portal_sessions.insert(path);
                        }
                        Some(PortalEvent::Stopped(path)) => {
                            tracing::debug!(%path, "portal screencast stopped");
                            portal_sessions.remove(&path);
                        }
                        None => {
                            // dbus-monitor exited; drop stale sessions and let
                            // pw-dump polling decide from now on.
                            portal_rx_open = false;
                            portal_sessions.clear();
                        }
                    }
                }
            }

            let sharing = !portal_sessions.is_empty() || pw_active;
            if sharing == sharing_before {
                continue;
            }
            sharing_before = sharing;

            let _ = app.emit(
                "silent://screen-share",
                serde_json::json!({ "active": sharing }),
            );

            let Some(window) = app.get_webview_window("main") else {
                return;
            };

            if sharing {
                if !hide_window {
                    tracing::info!(
                        "screen share detected; assistant window stays visible \
                         (share a tab/window to keep it out of the stream)"
                    );
                    continue;
                }
                if window.is_visible().unwrap_or(true) {
                    match window.hide() {
                        Ok(()) => {
                            hidden_by_watcher = true;
                            tracing::info!("screen share detected; hiding assistant window");
                        }
                        Err(err) => {
                            tracing::warn!(%err, "failed to hide window during screen share");
                        }
                    }
                }
            } else if hidden_by_watcher {
                match window.show() {
                    Ok(()) => {
                        hidden_by_watcher = false;
                        if always_on_top {
                            let _ = window.set_always_on_top(true);
                        }
                        tracing::info!("screen share ended; restoring assistant window");
                    }
                    Err(err) => {
                        tracing::warn!(%err, "failed to restore window after screen share");
                    }
                }
            }
        }
    });
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
enum PortalEvent {
    Started(String),
    Stopped(String),
}

/// Watch XDG Desktop Portal ScreenCast sessions on the session bus.
/// `Start(session_handle)` is emitted when a screencast actually begins and
/// `Close` on `org.freedesktop.portal.Session` when it ends.
#[cfg(target_os = "linux")]
fn spawn_portal_monitor(tx: tokio::sync::mpsc::UnboundedSender<PortalEvent>) {
    let spawned = std::thread::Builder::new()
        .name("portal-monitor".to_owned())
        .spawn(move || {
            let child = Command::new("dbus-monitor")
                .arg("--session")
                .arg("type='method_call',interface='org.freedesktop.portal.ScreenCast'")
                .arg("type='method_call',interface='org.freedesktop.portal.Session'")
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn();
            let mut child = match child {
                Ok(child) => child,
                Err(err) => {
                    tracing::warn!(
                        %err,
                        "dbus-monitor unavailable; relying on pw-dump polling for screen share"
                    );
                    return;
                }
            };

            if let Some(stdout) = child.stdout.take() {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if let Some(event) = parse_portal_event(&line)
                        && tx.send(event).is_err()
                    {
                        break;
                    }
                }
            }
            let _ = child.kill();
            let _ = child.wait();
        });

    if let Err(err) = spawned {
        tracing::warn!(%err, "failed to start portal monitor thread");
    }
}

#[cfg(target_os = "linux")]
fn parse_portal_event(line: &str) -> Option<PortalEvent> {
    let line = line.trim();
    if !line.starts_with("method call") {
        return None;
    }

    let mut path = None;
    let mut interface = None;
    let mut member = None;
    for field in line.split(|c: char| c.is_whitespace() || c == ';') {
        if let Some(value) = field.strip_prefix("path=") {
            path = Some(value.to_owned());
        } else if let Some(value) = field.strip_prefix("interface=") {
            interface = Some(value.to_owned());
        } else if let Some(value) = field.strip_prefix("member=") {
            member = Some(value.to_owned());
        }
    }

    match (interface.as_deref(), member.as_deref()) {
        (Some("org.freedesktop.portal.ScreenCast"), Some("Start")) => {
            path.map(PortalEvent::Started)
        }
        (Some("org.freedesktop.portal.Session"), Some("Close")) => path.map(PortalEvent::Stopped),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
fn screen_share_active() -> Result<bool, String> {
    let output = Command::new("pw-dump")
        .output()
        .map_err(|err| format!("pw-dump: {err}"))?;
    if !output.status.success() {
        return Err(format!("pw-dump exited with {}", output.status));
    }
    parse_pw_dump(&output.stdout)
}

#[cfg(target_os = "linux")]
fn parse_pw_dump(bytes: &[u8]) -> Result<bool, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|err| format!("invalid pw-dump JSON: {err}"))?;
    let Some(nodes) = value.as_array() else {
        return Ok(false);
    };

    for node in nodes {
        if node.get("type").and_then(serde_json::Value::as_str) != Some("PipeWire:Interface:Node") {
            continue;
        }
        let Some(props) = node.pointer("/info/props") else {
            continue;
        };
        let get = |key: &str| {
            props
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
        };

        let role = get("media.role");
        if role.eq_ignore_ascii_case("screen") || role.eq_ignore_ascii_case("presentation") {
            return Ok(true);
        }

        let class = get("media.class");
        if !class.eq_ignore_ascii_case("Stream/Input/Video") {
            continue;
        }
        // Webcams are video streams too; never hide the assistant for them.
        if role.eq_ignore_ascii_case("camera") {
            continue;
        }

        // Some portal implementations omit media.role. Recognise common
        // screencast naming instead of hiding on every video stream.
        let names = [
            "node.name",
            "media.name",
            "application.name",
            "application.process.binary",
        ]
        .iter()
        .map(|key| get(key))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
        const MARKERS: [&str; 6] = [
            "screencast",
            "screen-cast",
            "screen_share",
            "screenshare",
            "screen share",
            "desktop-capture",
        ];
        if MARKERS.iter().any(|marker| names.contains(marker)) {
            return Ok(true);
        }
    }

    Ok(false)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{PortalEvent, parse_portal_event, parse_pw_dump};

    #[test]
    fn detects_screen_stream() {
        let json = br#"[
          {"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Stream/Input/Video","media.role":"Screen",
            "application.name":"Google Chrome"}}},
          {"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Stream/Output/Audio","media.role":"Movie"}}}
        ]"#;
        assert!(parse_pw_dump(json).expect("valid JSON"));
    }

    #[test]
    fn ignores_webcam_and_audio() {
        let json = br#"[
          {"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Stream/Input/Video","media.role":"Camera",
            "node.name":"v4l2_input"}}},
          {"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Stream/Output/Audio","media.role":"Music"}}}
        ]"#;
        assert!(!parse_pw_dump(json).expect("valid JSON"));
    }

    #[test]
    fn detects_screencast_by_name_without_role() {
        let json = br#"[
          {"type":"PipeWire:Interface:Node","info":{"props":{
            "media.class":"Stream/Input/Video",
            "node.name":"xdg-desktop-portal-gnome-screencast"}}}
        ]"#;
        assert!(parse_pw_dump(json).expect("valid JSON"));
    }

    #[test]
    fn parses_portal_start_and_close() {
        let start = "method call time=1.0 sender=:1.2 -> destination=:1.3 serial=7 \
                     path=/org/freedesktop/portal/desktop/session/1_42/u1; \
                     interface=org.freedesktop.portal.ScreenCast; member=Start";
        assert_eq!(
            parse_portal_event(start),
            Some(PortalEvent::Started(
                "/org/freedesktop/portal/desktop/session/1_42/u1".to_owned()
            ))
        );

        let close = "method call time=1.1 sender=:1.2 -> destination=:1.3 serial=8 \
                     path=/org/freedesktop/portal/desktop/session/1_42/u1; \
                     interface=org.freedesktop.portal.Session; member=Close";
        assert_eq!(
            parse_portal_event(close),
            Some(PortalEvent::Stopped(
                "/org/freedesktop/portal/desktop/session/1_42/u1".to_owned()
            ))
        );
    }

    #[test]
    fn ignores_other_portal_calls() {
        let line = "method call time=1.0 sender=:1.2 -> destination=:1.3 serial=9 \
                    path=/org/freedesktop/portal/desktop; \
                    interface=org.freedesktop.portal.ScreenCast; member=CreateSession";
        assert_eq!(parse_portal_event(line), None);
    }
}
