//! Every widget in one window, for the UI Automation scripts in `tests/ui`.
//!
//! `WIDGETS_TICK=<ms>` runs a timed pass on that period, so passes run while you
//! type. `WIDGETS_IME=vi` switches this process to Vietnamese Telex.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

mod ime;

use std::time::Duration;
use windows_core::{HSTRING, IInspectable, Interface, Result};
use winui_immediate::{App, Length, Orientation, Severity, Ui, run};

fn main() -> Result<()> {
    let tick = std::env::var("WIDGETS_TICK")
        .ok()
        .and_then(|text| text.parse().ok())
        .map(Duration::from_millis);
    let ime = std::env::var("WIDGETS_IME").as_deref() == Ok("vi");
    run(move || {
        Ok(Widgets {
            tick,
            open: true,
            filter: String::new(),
            edits: 0,
            ticks: 0,
            unlocked: false,
            revealed: false,
            ime_pending: ime,
            color: 0,
            enters: 0,
        })
    })
}

struct Widgets {
    /// The tick period.
    tick: Option<Duration>,
    open: bool,
    filter: String,
    edits: u32,
    ticks: u64,
    unlocked: bool,
    revealed: bool,
    /// Telex is switched on once the window exists; TSF ignores it before.
    ime_pending: bool,
    /// An index into [`COLORS`].
    color: usize,
    /// Enter presses in the filter box.
    enters: u32,
}

const COLORS: [&str; 3] = ["Red", "Green", "Blue"];

impl App for Widgets {
    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        if let Some(every) = self.tick {
            let mut ticks = ui.stream("tick", || winui_executor::interval(every));
            while ticks.next().is_some() {
                self.ticks += 1;
            }
        }
        let window = ui.window("main", "winui-immediate widgets", |ui| {
            ui.grid("rows", &[Length::Auto; 6], |ui| {
                ui.label(
                    "status",
                    format_args!("{} edits, filter '{}'", self.edits, self.filter),
                )?;
                let mut filter = ui.text_box("filter", "Filter", &mut self.filter)?;
                if filter.changed() {
                    self.edits += 1;
                }
                if filter.header("Filter")?.enter_pressed()? {
                    self.enters += 1;
                }
                ui.stack("unlock", Orientation::Horizontal, 8.0, |ui| {
                    let mut field = ui.password_box("password", "Master password")?;
                    // Read on every pass, to show the check leaves the box's
                    // text whole.
                    let empty = field.is_empty()?;
                    field.changed()?;
                    let password = field.handle();
                    let submitted = ui
                        .button("submit", if self.unlocked { "Lock" } else { "Unlock" })?
                        .clicked()?;
                    if submitted {
                        self.unlocked =
                            !self.unlocked && ui.take_password(password)?.as_str() == "hunter2";
                    }
                    ui.label("empty", format_args!("empty {empty}"))?;
                    ui.label(
                        "state",
                        format_args!("{}", if self.unlocked { "Unlocked" } else { "Locked" }),
                    )?;
                    Ok(())
                })?;
                // Rebuilt only when `revealed` changes or Reveal is clicked.
                let revealed = self.revealed;
                let mut toggled = false;
                ui.cached("secret-cache", revealed, |ui| {
                    eprintln!("secret row built");
                    ui.stack("secret", Orientation::Horizontal, 8.0, |ui| {
                        ui.label(
                            "value",
                            format_args!(
                                "{}",
                                if revealed {
                                    "password 1"
                                } else {
                                    "••••••"
                                }
                            ),
                        )?;
                        toggled = ui.button("reveal", "Reveal")?.clicked()?;
                        Ok(())
                    })?;
                    Ok(())
                })?;
                if toggled {
                    self.revealed = !self.revealed;
                }
                ui.native("switch", toggle_switch, |_| Ok(()))?;
                ui.stack("picker", Orientation::Vertical, 8.0, |ui| {
                    ui.combo_box("color", &COLORS, &mut self.color)?
                        .header("Color")?
                        .enabled(self.enters < 3)?;
                    ui.label(
                        "picked",
                        format_args!("picked {}, enters {}", COLORS[self.color], self.enters),
                    )?;
                    if ui.button("reset", "Reset color")?.accent(true)?.clicked()? {
                        self.color = 0;
                    }
                    if self.enters > 0 {
                        ui.info_bar("entered", Severity::Warning, "Enter", "Enter was pressed.")?;
                    }
                    Ok(())
                })?;
                Ok(())
            })?;
            Ok(())
        })?;
        if std::mem::take(&mut self.ime_pending) {
            ime::activate_vietnamese_telex()?;
        }
        if window.close_requested() {
            self.open = false;
        }
        Ok(())
    }
}

windows_core::link!("combase.dll" "system" fn RoActivateInstance(class_id: *mut core::ffi::c_void, instance: *mut *mut core::ffi::c_void) -> windows_core::HRESULT);

/// A `ToggleSwitch` (a sealed class, so `RoActivateInstance` can build it),
/// made without WinUI bindings, to show [`Ui::native`].
fn toggle_switch() -> Result<IInspectable> {
    let class = HSTRING::from("Microsoft.UI.Xaml.Controls.ToggleSwitch");
    let mut instance = core::ptr::null_mut();
    // SAFETY: `class` is a live HSTRING, and on success `instance` receives an
    // owned IInspectable pointer.
    unsafe {
        RoActivateInstance(core::mem::transmute_copy(&class), &mut instance).ok()?;
        Ok(IInspectable::from_raw(instance))
    }
}
