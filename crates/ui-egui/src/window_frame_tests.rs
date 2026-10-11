//! The window-frame service keeps the custom title bar's undecorated window borderless (#2246)
//! and, when maximized, off the taskbar (#3144).
//!
//! winit 0.30 gives an undecorated window a caption and border (`WS_CAPTION | WS_BORDER`), which
//! on some Windows systems offset the drawn content and the pointer. The app shell strips them
//! and re-strips them after every toolkit flag change (maximize, restore, …), driven from
//! `raw_input_hook`; stripped, a maximized window lands on the whole monitor, so the shell also
//! moves it back onto the work area, except in full screen. These tests pin the shell side: the
//! hook must run the service once per frame, before the frame is laid out, telling it whether the
//! window is in full screen.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::PhotocraftApp;
use crate::Services;
use crate::WindowFrameState;

#[test]
fn the_window_frame_service_runs_every_raw_input_hook() {
    let calls = Arc::new(AtomicUsize::new(0));
    let n = calls.clone();
    let services = Services {
        window_frame: Some(Box::new(move |_: WindowFrameState| {
            n.fetch_add(1, Ordering::SeqCst);
        })),
        ..Default::default()
    };
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
    let ctx = egui::Context::default();
    for _ in 0..3 {
        eframe::App::raw_input_hook(&mut app, &ctx, &mut egui::RawInput::default());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[test]
fn the_service_is_told_whether_the_window_is_in_full_screen() {
    // Full screen covers the monitor on purpose: the maximized-window fix must know to stand down.
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let s = seen.clone();
    let services = Services {
        window_frame: Some(Box::new(move |state: WindowFrameState| {
            s.lock().unwrap().push(state.fullscreen);
        })),
        ..Default::default()
    };
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
    let ctx = egui::Context::default();
    // A default input (no viewport info yet) means windowed.
    eframe::App::raw_input_hook(&mut app, &ctx, &mut egui::RawInput::default());
    for fullscreen in [Some(true), Some(false), None] {
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&raw.viewport_id).unwrap().fullscreen = fullscreen;
        eframe::App::raw_input_hook(&mut app, &ctx, &mut raw);
    }
    assert_eq!(*seen.lock().unwrap(), [false, true, false, false]);
}

#[test]
fn no_window_frame_service_is_fine() {
    // Decorated windows (the system title bar preference) and non-Windows platforms leave the
    // service unset; the hook must simply do nothing.
    let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Services::default());
    let ctx = egui::Context::default();
    eframe::App::raw_input_hook(&mut app, &ctx, &mut egui::RawInput::default());
}
