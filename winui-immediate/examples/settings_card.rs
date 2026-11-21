//! Settings cards in each layout, at 360px, where `Right` content wraps,
//! and at 520px, where it stays beside the header. The clickable card counts
//! clicks in its description. `tests/ui/settings_card.ps1` drives it.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use winui_immediate::{App, ContentAlignment, Handle, Orientation, SettingsCard, Ui, run};

fn main() -> windows_core::Result<()> {
    run(|_: Handle<()>| {
        Ok(Gallery {
            clicks: 0,
            checked: false,
            text: String::new(),
            open: true,
        })
    })
}

struct Gallery {
    clicks: u32,
    checked: bool,
    text: String,
    open: bool,
}

impl App for Gallery {
    type Message = ();

    fn update(&mut self, (): ()) {}

    fn view(&mut self, ui: &mut Ui) -> windows_core::Result<()> {
        if !self.open {
            return Ok(());
        }
        let mut main = ui.window("main", "winui-immediate settings card", |ui| {
            ui.stack("columns", Orientation::Horizontal, 24.0, |ui| {
                for width in [360.0, 520.0] {
                    ui.stack(("column", width as u32), Orientation::Vertical, 4.0, |ui| {
                        let clicks = format!("Clicked {} times", self.clicks);
                        let clicked = ui
                            .settings_card(
                                "clickable",
                                SettingsCard {
                                    header: "Authenticator app",
                                    description: &clicks,
                                    icon: "\u{E8EA}",
                                    clickable: true,
                                    ..SettingsCard::default()
                                },
                                |_| Ok(()),
                            )?
                            .clicked();
                        if clicked {
                            eprintln!("clicked in column {width}");
                            self.clicks += 1;
                        }
                        ui.settings_card(
                            "right",
                            SettingsCard {
                                header: "Right",
                                description: "A button on the right, or under the header when narrow",
                                icon: "\u{E713}",
                                ..SettingsCard::default()
                            },
                            |ui| {
                                ui.button("action", "Action")?;
                                Ok(())
                            },
                        )?;
                        ui.settings_card(
                            "left",
                            SettingsCard {
                                header: "Hidden header",
                                alignment: ContentAlignment::Left,
                                ..SettingsCard::default()
                            },
                            |ui| {
                                ui.check_box("check", "Left: only the check box", &mut self.checked)?;
                                Ok(())
                            },
                        )?;
                        ui.settings_card(
                            "vertical",
                            SettingsCard {
                                header: "Vertical",
                                description: "A text box under the header",
                                icon: "\u{E70F}",
                                alignment: ContentAlignment::Vertical,
                                ..SettingsCard::default()
                            },
                            |ui| {
                                ui.text_box("text", "Type here", &mut self.text)?;
                                Ok(())
                            },
                        )?;
                        Ok(())
                    })?
                    .width(width)?;
                }
                Ok(())
            })?
            .padding([16.0, 16.0, 16.0, 16.0])?;
            Ok(())
        })?;
        main.fit_to_content()?;
        if main.close_requested() {
            self.open = false;
        }
        Ok(())
    }
}
