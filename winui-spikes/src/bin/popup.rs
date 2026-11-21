//! Spike: a popup window with no minimize or maximize button and no resize
//! border, only the system close button, like the Windows emoji picker.
//!
//! Reactor has no API for this, so after the first publication the app finds
//! its own window handle and changes the Win32 window styles directly. A
//! `TitleBar` element extends the content into the caption area.
//!
//! `SPIKE_TOPMOST` also makes the window topmost and a tool window (no
//! taskbar button).

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use std::time::Duration;
use windows_reactor::*;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM},
    System::Threading::GetCurrentThreadId,
    UI::WindowsAndMessaging::{
        EnumThreadWindows, GWL_EXSTYLE, GWL_STYLE, GetClassNameW, GetWindowLongPtrW, HWND_TOPMOST,
        SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos,
        WS_EX_TOOLWINDOW, WS_MAXIMIZEBOX, WS_MINIMIZEBOX, WS_THICKFRAME,
    },
};

/// Returns the WinUI desktop window that belongs to the calling thread.
fn own_window() -> Option<HWND> {
    unsafe extern "system" fn visit(window: HWND, found: LPARAM) -> i32 {
        let mut class = [0u16; 64];
        // SAFETY: `class` is a writable buffer of the stated length.
        let length = unsafe { GetClassNameW(window, class.as_mut_ptr(), class.len() as i32) };
        let class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
        if class == "WinUIDesktopWin32WindowClass" {
            // SAFETY: `found` points at the caller's `Option<HWND>`.
            unsafe { *(found as *mut Option<HWND>) = Some(window) };
            return 0;
        }
        1
    }
    let mut found: Option<HWND> = None;
    // SAFETY: the callback only writes through the pointer it is given, and
    // `found` outlives the call.
    unsafe {
        EnumThreadWindows(
            GetCurrentThreadId(),
            Some(visit),
            &mut found as *mut Option<HWND> as LPARAM,
        )
    };
    found
}

/// Removes minimize, maximize and the resize border, and optionally makes the
/// window a topmost tool window. Returns the style values before and after.
fn make_popup(window: HWND, topmost: bool) -> (isize, isize) {
    // SAFETY: `window` is a live window owned by this thread.
    unsafe {
        let before = GetWindowLongPtrW(window, GWL_STYLE);
        let after = before & !((WS_MINIMIZEBOX | WS_MAXIMIZEBOX | WS_THICKFRAME) as isize);
        SetWindowLongPtrW(window, GWL_STYLE, after);
        if topmost {
            let extended = GetWindowLongPtrW(window, GWL_EXSTYLE);
            SetWindowLongPtrW(window, GWL_EXSTYLE, extended | WS_EX_TOOLWINDOW as isize);
        }
        // The frame is cached until SWP_FRAMECHANGED.
        SetWindowPos(
            window,
            if topmost {
                HWND_TOPMOST
            } else {
                std::ptr::null_mut()
            },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_FRAMECHANGED | if topmost { 0 } else { SWP_NOZORDER },
        );
        (before, GetWindowLongPtrW(window, GWL_STYLE))
    }
}

#[derive(Clone)]
enum PopupMessage {
    /// Changes the content after a delay, to check that a re-render keeps the
    /// styles.
    Tick,
}

struct Popup {
    ticks: u32,
}

impl Component for Popup {
    type Input = ();
    type Message = PopupMessage;

    fn create(_input: &(), context: &ComponentContext<Self>) -> Self {
        context.spawn_background(|_cancel| {
            std::thread::sleep(Duration::from_secs(2));
            PopupMessage::Tick
        });
        Self { ticks: 0 }
    }

    fn update(&mut self, message: PopupMessage, _context: &ComponentContext<Self>) {
        match message {
            PopupMessage::Tick => {
                self.ticks += 1;
                if let Some(window) = own_window() {
                    // SAFETY: `window` is a live window owned by this thread.
                    let style = unsafe { GetWindowLongPtrW(window, GWL_STYLE) };
                    eprintln!("after re-render: style {style:#x}");
                }
            }
        }
    }

    fn view(&self, _input: &(), context: &mut ViewContext<Self>) -> View {
        context.window_title("Popup spike");
        context.window_visuals(
            WindowVisuals::new()
                .backdrop(WindowBackdrop::Acrylic)
                .client_size(360.0, 420.0),
        );
        // Runs once, after the window exists.
        context.use_effect("popup", (), || {
            match own_window() {
                Some(window) => {
                    let topmost = std::env::var_os("SPIKE_TOPMOST").is_some();
                    let (before, after) = make_popup(window, topmost);
                    eprintln!("popup styles: before {before:#x}, after {after:#x}");
                }
                None => eprintln!("popup: no window found on this thread"),
            }
            None
        });
        // A colored band spans both rows, so it draws under the title bar.
        // The TitleBar comes later in the children, so it sits on top, and
        // its Content slot holds a search box like the emoji picker's.
        Grid::new()
            .rows([GridLength::Auto, GridLength::Star(1.0)])
            .children((
                Grid::new()
                    .background(Color {
                        a: 0x60,
                        r: 0x30,
                        g: 0x80,
                        b: 0xff,
                    })
                    .grid_row(0)
                    .grid_row_span(2)
                    .children((StackPanel::new()
                        .spacing(8.0)
                        .margin(16.0)
                        .vertical_alignment(VerticalAlignment::Bottom)
                        .children((
                            TextBlock::new().text("This band starts at the top edge."),
                            TextBlock::new().text(format!("Re-renders: {}", self.ticks)),
                        )),)),
                TitleBar::new().grid_row(0).slot(
                    TitleBarSlot::Content,
                    TextBox::new().placeholder_text("Search"),
                ),
            ))
    }
}

fn main() {
    App::run_component::<Popup>(()).expect("WinUI app failed to start");
}
