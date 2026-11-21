//! A list and details layout: a list of 200 entries whose rows select on
//! click, and a details pane of field cards with a command bar, a Show
//! button per secret, and a scroll area. `tests/ui/list_details.ps1`
//! drives it. The command bar's Star prints `favorite <id>` to stderr.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use windows_core::Result;
use winui_immediate::{App, Length, ListId, Orientation, Overflow, Ui, run};

fn main() -> Result<()> {
    run(|_| {
        Ok(Notes {
            open: true,
            entries: (1..=200)
                .map(|id| Entry {
                    id,
                    name: format!("Entry {id}"),
                    detail: format!("user{id}@example.com"),
                    secret: format!("secret-{id}"),
                    favorite: id % 7 == 0,
                })
                .collect(),
            selected: None,
            revealed: false,
        })
    })
}

struct Notes {
    open: bool,
    entries: Vec<Entry>,
    /// Index into `entries`.
    selected: Option<usize>,
    /// The selected entry's secret shows; another selection hides it.
    revealed: bool,
}

struct Entry {
    id: u64,
    name: String,
    detail: String,
    secret: String,
    favorite: bool,
}

impl App for Notes {
    type Message = ();

    fn update(&mut self, (): ()) {}

    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        let window = ui.window("main", "winui-immediate list and details", |ui| {
            ui.columns(
                "panes",
                &[Length::Pixels(320.0), Length::Star(1.0)],
                0.0,
                |ui| {
                    ui.list("entries", self.entries.len(), 0, Notes::row)?;
                    ui.scroll("details", |ui| self.details(ui))?;
                    Ok(())
                },
            )?;
            Ok(())
        })?;
        if window.close_requested() {
            self.open = false;
        }
        Ok(())
    }
}

impl Notes {
    /// One row: the entry's name over its detail, selected when it is the
    /// open entry.
    fn row(&mut self, ui: &mut Ui, _: ListId, index: usize) -> Result<()> {
        let entry = &self.entries[index];
        let mut row = ui.pressable("row", |ui| {
            ui.stack("text", Orientation::Vertical, 0.0, |ui| {
                ui.label("name", format_args!("{}", entry.name))?
                    .overflow(Overflow::Ellipsis)?;
                ui.label("detail", format_args!("{}", entry.detail))?
                    .secondary(true)?
                    .style("CaptionTextBlockStyle")?;
                Ok(())
            })?
            .margin([0.0, 8.0, 12.0, 8.0])?;
            Ok(())
        })?;
        row.name(&entry.name)?
            .selected(self.selected == Some(index))?
            .margin([4.0, 2.0, 4.0, 2.0])?;
        if row.clicked()? && self.selected != Some(index) {
            self.selected = Some(index);
            self.revealed = false;
        }
        Ok(())
    }

    fn details(&mut self, ui: &mut Ui) -> Result<()> {
        let Some(index) = self.selected else {
            ui.label("empty", format_args!("Select an entry."))?
                .secondary(true)?
                .margin([24.0, 24.0, 24.0, 24.0])?;
            return Ok(());
        };
        let entry = &mut self.entries[index];
        let mut star = false;
        ui.stack("entry", Orientation::Vertical, 16.0, |ui| {
            ui.command_bar("commands", |ui| {
                let (glyph, label) = match entry.favorite {
                    true => ('\u{E735}', "Unfavorite"),
                    false => ('\u{E734}', "Favorite"),
                };
                star = ui.app_bar_button("favorite", glyph, label)?.clicked()?;
                Ok(())
            })?;
            ui.label("title", format_args!("{}", entry.name))?
                .style("SubtitleTextBlockStyle")?;
            ui.card_group("fields", |ui| {
                ui.card_row("detail", false, &[Length::Star(1.0)], |ui| {
                    field(ui, "Detail", &entry.detail, false)
                })?;
                ui.card_row(
                    "secret",
                    true,
                    &[Length::Star(1.0), Length::Auto],
                    |ui| {
                        let shown = match self.revealed {
                            true => entry.secret.as_str(),
                            false => "••••••••",
                        };
                        field(ui, "Secret", shown, true)?;
                        let (glyph, label) = match self.revealed {
                            true => ('\u{ED1A}', "Hide secret"),
                            false => ('\u{E890}', "Show secret"),
                        };
                        if ui.icon_button("reveal", glyph, label)?.clicked()? {
                            self.revealed = !self.revealed;
                        }
                        Ok(())
                    },
                )?;
                Ok(())
            })?;
            ui.label("note", format_args!("A long note wraps across lines instead of running past the edge of the pane, and keeps\nits line breaks."))?
                .overflow(Overflow::Wrap)?
                .italic(true)?;
            Ok(())
        })?
        .padding([24.0, 16.0, 24.0, 24.0])?;
        if star {
            entry.favorite = !entry.favorite;
            eprintln!("favorite {}", entry.id);
        }
        Ok(())
    }
}

/// A caption over a value, the left column of a card row.
fn field(ui: &mut Ui, caption: &str, value: &str, monospace: bool) -> Result<()> {
    ui.stack("field", Orientation::Vertical, 0.0, |ui| {
        ui.label("caption", format_args!("{caption}"))?
            .secondary(true)?
            .style("CaptionTextBlockStyle")?;
        ui.label("value", format_args!("{value}"))?
            .monospace(monospace)?
            .overflow(Overflow::Ellipsis)?;
        Ok(())
    })?;
    Ok(())
}
