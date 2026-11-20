//! Every widget in one window, for the UI Automation scripts in `tests/ui`.
//!
//! `WIDGETS_TICK=<ms>` sends a message on a timer, so passes run while you
//! type. `WIDGETS_IME=vi` switches this process to Vietnamese Telex.

mod ime;

use std::time::Duration;
use windows_core::{HSTRING, IInspectable, Interface, Result};
use winui_immediate::{App, Handle, Length, Orientation, Ui, run};

fn main() -> Result<()> {
    let tick = std::env::var("WIDGETS_TICK")
        .ok()
        .and_then(|text| text.parse().ok())
        .map(Duration::from_millis);
    let ime = std::env::var("WIDGETS_IME").as_deref() == Ok("vi");
    run(
        Widgets {
            open: true,
            filter: String::new(),
            edits: 0,
            ticks: 0,
            unlocked: false,
            revealed: false,
            ime_pending: ime,
        },
        move |_, handle: Handle<Tick>| {
            if let Some(tick) = tick {
                std::thread::spawn(move || {
                    while handle.send(Tick) {
                        std::thread::sleep(tick);
                    }
                });
            }
        },
    )
}

struct Tick;

struct Widgets {
    open: bool,
    filter: String,
    edits: u32,
    ticks: u64,
    unlocked: bool,
    revealed: bool,
    /// Telex is switched on once the window exists; TSF ignores it before.
    ime_pending: bool,
}

impl App for Widgets {
    type Message = Tick;

    fn update(&mut self, _: Tick) {
        self.ticks += 1;
    }

    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        let window = ui.window("main", "winui-immediate widgets", |ui| {
            ui.grid("rows", &[Length::Auto; 5], |ui| {
                ui.label(
                    "status",
                    format_args!("{} edits, filter '{}'", self.edits, self.filter),
                )?;
                if ui.text_box("filter", "Filter", &mut self.filter)?.changed() {
                    self.edits += 1;
                }
                ui.stack("unlock", Orientation::Horizontal, 8.0, |ui| {
                    let password = ui.password_box("password", "Master password")?.handle();
                    let submitted = ui
                        .button("submit", if self.unlocked { "Lock" } else { "Unlock" })?
                        .clicked()?;
                    if submitted {
                        self.unlocked = !self.unlocked && !ui.take_password(password)?.is_empty();
                    }
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
