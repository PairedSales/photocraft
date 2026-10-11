//! Keeps the custom (undecorated) title bar window truly borderless (#2246) and, when maximized,
//! on the monitor's work area rather than under the taskbar (#3144, #2509).
//!
//! winit 0.30 gives an undecorated window the `WS_CAPTION | WS_BORDER` styles and hides the
//! non-client area by spreading the client rect over it (`WM_NCCALCSIZE`). On some systems
//! (reported: Windows 10 LTSB, Intel HD Graphics 4600, 1366 × 768) the desktop compositor still
//! reserves that frame for the window surface: the content is drawn offset by the caption and
//! border, leaving black bars at the window's top-left, while the pointer keeps the un-offset
//! client coordinates, so it no longer points at what is drawn under it.
//!
//! winit 0.27 — before rust-windowing/winit#2419 — simply omitted those styles for undecorated
//! windows, and there was no offset. [`service`] restores that: it strips `WS_CAPTION` and
//! `WS_BORDER` and forces Windows to recalculate the frame. winit puts them back whenever it
//! changes the window flags (maximize, restore, …), so the returned guard checks every frame.
//!
//! Stripping the caption has a side effect on maximize. Windows sizes a maximized window with a
//! caption to the monitor's work area, but one without a caption to the whole monitor, taskbar
//! included (the `WM_GETMINMAXINFO` defaults; GLFW and SDL correct them in that message for their
//! undecorated windows). winit maximizes (`ShowWindow(SW_MAXIMIZE)`) before it re-applies its own
//! styles, so the window goes to the whole monitor; winit's `WM_NCCALCSIZE` still limits the
//! client area to the work area, and the strip left under the taskbar is non-client area that
//! nothing paints: black, which the translucent Windows 11 taskbar shows through (#3144, #2509).
//! A window restored as maximized at startup is maximized with its caption, before the first
//! strip, so it lands on the work area: that is why closing and reopening looked right. Handling
//! `WM_GETMINMAXINFO` would need winit's window procedure (and writing the struct needs `unsafe`),
//! so the guard moves a maximized window onto the work area after the fact instead
//! ([`WorkArea`]), once per maximize so it never fights Windows frame after frame.
//!
//! The Windows calls go through safe `winsafe` wrappers; the workspace forbids `unsafe`, and
//! winsafe can't wrap a raw handle without it, so the window is found by its handle instead.

// The placement logic is pure and tested everywhere, but only the Windows guard calls it.
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

pub use platform::service;

/// A screen rectangle in physical pixels, as `GetWindowRect` and `GetMonitorInfo` report them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Rect {
    fn is_empty(self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
}

/// Decides, once per frame, whether a maximized borderless window has to be moved onto its
/// monitor's work area (see the module doc). Pure, so it is tested on every platform.
#[derive(Debug, Default)]
struct WorkArea {
    /// The wrong placement the last move was asked for: if the window is still there next frame,
    /// Windows kept it, and asking again every frame would only fight it.
    tried: Option<Rect>,
}

impl WorkArea {
    /// `window` is the window rect while it is maximized (`None` when it isn't, is minimized, is
    /// in full screen or can't be read), `work` the work area of its monitor (`None` when
    /// unknown). Returns the rect to move the window to, if it needs moving.
    fn fix(&mut self, window: Option<Rect>, work: Option<Rect>) -> Option<Rect> {
        let (Some(window), Some(work)) = (window, work.filter(|w| !w.is_empty())) else {
            self.tried = None;
            return None;
        };
        if window == work {
            self.tried = None;
            return None;
        }
        if self.tried == Some(window) {
            return None;
        }
        self.tried = Some(window);
        Some(work)
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use eframe::wgpu::rwh::{HasWindowHandle, RawWindowHandle};
    use photocraft_ui_egui::{WindowFrameFn, WindowFrameState};
    use winsafe::{HWND, HwndPlace, POINT, RECT, SIZE, co};

    use super::{Rect, WorkArea};

    /// A guard that keeps the window borderless and, when maximized, on the work area each frame,
    /// or `None` when the app uses the system title bar (the window is decorated) or its handle
    /// can't be found.
    pub fn service(cc: &eframe::CreationContext<'_>, custom_titlebar: bool) -> Option<WindowFrameFn> {
        if !custom_titlebar {
            return None;
        }
        let target = match cc.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => h.hwnd.get(),
            _ => return None,
        };
        let hwnd = find_window(target)?;
        strip(&hwnd);
        let mut work_area = WorkArea::default();
        Some(Box::new(move |state: WindowFrameState| {
            strip(&hwnd);
            keep_on_work_area(&hwnd, &mut work_area, state.fullscreen);
        }))
    }

    /// The top-level window with the given raw handle. `EnumWindows` hands out real `HWND`s, so
    /// this matches winit's own window without wrapping a raw handle (which needs `unsafe`).
    fn find_window(target: isize) -> Option<HWND> {
        let mut found = None;
        let _ = winsafe::EnumWindows(|hwnd| {
            if hwnd.ptr() as isize == target {
                found = Some(hwnd);
                false
            } else {
                true
            }
        });
        found
    }

    /// Removes `WS_CAPTION`/`WS_BORDER` and has Windows recalculate the frame. A no-op once they
    /// are gone, so it is cheap to call every frame.
    fn strip(hwnd: &HWND) {
        let style = hwnd.style();
        let without = borderless(style);
        if without.raw() == style.raw() {
            return;
        }
        hwnd.set_style(without);
        let _ = hwnd.SetWindowPos(
            HwndPlace::None,
            POINT::new(),
            SIZE::new(),
            co::SWP::FRAMECHANGED | co::SWP::NOMOVE | co::SWP::NOSIZE | co::SWP::NOZORDER | co::SWP::NOACTIVATE,
        );
    }

    /// `style` without the OS title bar and frame (`WS_CAPTION`, which already includes
    /// `WS_BORDER`). The sizing border (`WS_THICKFRAME`) stays, so edge resizing and Aero snap
    /// keep working, as they did with winit 0.27's undecorated windows.
    fn borderless(style: co::WS) -> co::WS {
        style & !(co::WS::CAPTION | co::WS::BORDER)
    }

    /// Moves a maximized window that Windows put over the whole monitor back onto the monitor's
    /// work area (module doc). Full screen covers the monitor on purpose and is left alone.
    fn keep_on_work_area(hwnd: &HWND, work_area: &mut WorkArea, fullscreen: bool) {
        let maximized = !fullscreen && hwnd.IsZoomed() && !hwnd.IsIconic();
        let window = if maximized { hwnd.GetWindowRect().ok().map(rect) } else { None };
        let work = window.and_then(|_| hwnd.MonitorFromWindow(co::MONITOR::DEFAULTTONEAREST).GetMonitorInfo().ok()).map(|mi| rect(mi.rcWork));
        if let Some(to) = work_area.fix(window, work) {
            let _ = hwnd.SetWindowPos(
                HwndPlace::None,
                POINT::with(to.left, to.top),
                SIZE::with(to.right - to.left, to.bottom - to.top),
                co::SWP::NOZORDER | co::SWP::NOACTIVATE,
            );
        }
    }

    fn rect(r: RECT) -> Rect {
        Rect { left: r.left, top: r.top, right: r.right, bottom: r.bottom }
    }

    #[cfg(test)]
    mod tests {
        use super::borderless;
        use winsafe::co;

        #[test]
        fn the_borderless_style_drops_the_caption_and_border_only() {
            // `WS_CAPTION` is 0x00c0_0000 and already contains `WS_BORDER` (0x0080_0000).
            let decorated = co::WS::CAPTION | co::WS::BORDER | co::WS::SYSMENU | co::WS::THICKFRAME | co::WS::MAXIMIZEBOX | co::WS::MINIMIZEBOX;
            let bare = borderless(decorated);
            assert!(!bare.has(co::WS::CAPTION), "caption gone");
            assert!(!bare.has(co::WS::BORDER), "border gone");
            assert!(bare.has(co::WS::THICKFRAME), "edge resizing kept");
            assert!(bare.has(co::WS::MAXIMIZEBOX) && bare.has(co::WS::SYSMENU), "window buttons kept");
            // Already borderless: unchanged, so the per-frame call does nothing.
            assert_eq!(bare.raw(), borderless(bare).raw());
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod platform {
    use photocraft_ui_egui::WindowFrameFn;

    pub fn service(_cc: &eframe::CreationContext<'_>, _custom_titlebar: bool) -> Option<WindowFrameFn> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::{Rect, WorkArea};

    /// A 2560 × 1440 monitor with a 48 px taskbar at the bottom (the #2509 report).
    const MONITOR: Rect = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    const WORK: Rect = Rect { left: 0, top: 0, right: 2560, bottom: 1392 };

    #[test]
    fn a_window_maximized_over_the_taskbar_moves_onto_the_work_area() {
        let mut g = WorkArea::default();
        // Windows maximized the caption-less window to the whole monitor (#3144).
        assert_eq!(g.fix(Some(MONITOR), Some(WORK)), Some(WORK));
        // Next frame it is on the work area: nothing to do, and the attempt is forgotten.
        assert_eq!(g.fix(Some(WORK), Some(WORK)), None);
        assert_eq!(g.tried, None);
    }

    #[test]
    fn a_window_maximized_with_its_caption_is_moved_too() {
        // Restored as maximized at startup, before the strip: the work area plus the 8 px frame
        // Windows hangs off-screen. Moving it makes both paths end up the same.
        let framed = Rect { left: -8, top: -8, right: 2568, bottom: 1400 };
        assert_eq!(WorkArea::default().fix(Some(framed), Some(WORK)), Some(WORK));
    }

    #[test]
    fn a_placement_windows_keeps_is_asked_for_once() {
        let mut g = WorkArea::default();
        assert_eq!(g.fix(Some(MONITOR), Some(WORK)), Some(WORK));
        // Windows put it straight back: don't fight it every frame.
        assert_eq!(g.fix(Some(MONITOR), Some(WORK)), None);
        assert_eq!(g.fix(Some(MONITOR), Some(WORK)), None);
        // Restore, then maximize again: a fresh attempt.
        assert_eq!(g.fix(None, None), None);
        assert_eq!(g.fix(Some(MONITOR), Some(WORK)), Some(WORK));
    }

    #[test]
    fn a_window_that_is_not_maximized_is_left_alone() {
        let mut g = WorkArea::default();
        // Windowed, minimized, full screen or unreadable: the caller passes `None`.
        assert_eq!(g.fix(None, Some(WORK)), None);
        assert_eq!(g.fix(None, None), None);
        // Full screen covers the monitor on purpose; the caller passes `None` for it, so even a
        // rect equal to the monitor's never gets moved.
        g.tried = Some(MONITOR);
        assert_eq!(g.fix(None, Some(WORK)), None);
        assert_eq!(g.tried, None, "a restore forgets the last attempt");
    }

    #[test]
    fn an_unknown_or_empty_work_area_moves_nothing() {
        let mut g = WorkArea::default();
        assert_eq!(g.fix(Some(MONITOR), None), None);
        let empty = Rect { left: 10, top: 10, right: 10, bottom: 50 };
        assert_eq!(g.fix(Some(MONITOR), Some(empty)), None);
        let inverted = Rect { left: 0, top: 0, right: -2560, bottom: 1392 };
        assert_eq!(g.fix(Some(MONITOR), Some(inverted)), None);
        assert_eq!(g.tried, None);
    }

    #[test]
    fn a_second_monitor_keeps_its_own_coordinates() {
        // A monitor to the left of the primary, taskbar at its top.
        let monitor = Rect { left: -1920, top: 0, right: 0, bottom: 1080 };
        let work = Rect { left: -1920, top: 40, right: 0, bottom: 1080 };
        assert_eq!(WorkArea::default().fix(Some(monitor), Some(work)), Some(work));
    }
}
