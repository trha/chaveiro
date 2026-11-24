//! "Password history" from the design: a dialog that lists a login's old
//! passwords, newest first, each hidden until the user shows it, with a
//! Copy button.

use crate::copy::copy_button;
use chaveiro_core::{crypto::SecretString, vault::CipherView};
use jiff::{Timestamp, tz::TimeZone};
use uuid::Uuid;
use windows_core::Result;
use winui_immediate::{Align, DialogAnswer, DialogButtons, Length, Orientation, Overflow, Ui};

/// The open dialog, for the login `item`.
pub struct History {
    pub item: Uuid,
    /// Names this showing of the dialog, so the next one is a dialog of its
    /// own.
    serial: u64,
    /// The rows the user showed, by position. A new dialog shows none.
    shown: Vec<usize>,
    /// A copy was made from the dialog, so it says the clipboard clears.
    copied: bool,
}

impl History {
    pub fn new(item: Uuid, serial: u64) -> Self {
        History {
            item,
            serial,
            shown: Vec::new(),
            copied: false,
        }
    }
}

/// What the user did in the dialog in a pass.
pub enum HistoryPass {
    Stay,
    /// The dialog closed.
    Close,
    /// Copy this old password.
    Copy(SecretString),
}

/// Draws the dialog for `view`'s password history.
pub fn history_dialog(
    ui: &mut Ui,
    history: &mut History,
    view: &CipherView,
) -> Result<HistoryPass> {
    let mut pass = HistoryPass::Stay;
    let buttons = DialogButtons {
        primary: None,
        secondary: None,
        close: "Close",
        default: DialogAnswer::Close,
    };
    let entries = &view.password_history;
    let dialog = ui.content_dialog(
        ("history", history.serial),
        "Password history",
        buttons,
        |ui| {
            // The dialog gives its body the height it has, so the list takes
            // what is left and scrolls when the window is too short.
            ui.grid(
                "history",
                &[Length::Auto, Length::Star(1.0), Length::Auto],
                |ui| {
                    ui.label(
                    "explain",
                    format_args!(
                        "Old passwords for {}, newest first. The current password isn't listed.",
                        view.name.as_str()
                    ),
                )?
                .overflow(Overflow::Wrap)?
                .margin([0.0, 0.0, 0.0, 12.0])?;

                    ui.scroll("list", |ui| {
                        ui.card_group("list", |ui| {
                            for (position, entry) in entries.iter().enumerate() {
                                let visible = history.shown.contains(&position);
                                let columns = [Length::Star(1.0), Length::Auto, Length::Auto];
                                let mut toggled = false;
                                ui.card_row(position, position > 0, &columns, |ui| {
                                    ui.stack("text", Orientation::Vertical, 2.0, |ui| {
                                        ui.label(
                                            "replaced",
                                            format_args!(
                                                "Replaced {}",
                                                when(&entry.last_used_date)
                                            ),
                                        )?
                                        .secondary(true)?
                                        .style("CaptionTextBlockStyle")?;
                                        let mut password = match visible {
                                            true => ui.label(
                                                "password",
                                                format_args!("{}", entry.password.as_str()),
                                            )?,
                                            false => {
                                                ui.label("password", format_args!("••••••••••"))?
                                            }
                                        };
                                        password.monospace(true)?.overflow(Overflow::Wrap)?;
                                        Ok(())
                                    })?
                                    .valign(Align::Center)?;
                                    let (glyph, tip) = match visible {
                                        true => ('\u{ED1A}', "Hide"),
                                        false => ('\u{E890}', "Show"),
                                    };
                                    toggled = ui.icon_button("show", glyph, tip)?.clicked()?;
                                    if copy_button(ui, "copy", "Copy")? {
                                        pass = HistoryPass::Copy(entry.password.clone());
                                    }
                                    Ok(())
                                })?;
                                if toggled {
                                    match visible {
                                        true => history.shown.retain(|shown| *shown != position),
                                        false => history.shown.push(position),
                                    }
                                    ui.request_pass();
                                }
                            }
                            Ok(())
                        })?;
                        Ok(())
                    })?;

                    // The vault's own copy message sits behind the dialog's
                    // smoke layer, so the dialog says it too.
                    let copied = match history.copied {
                        true => "Copied. Clears in 30 seconds.",
                        false => "",
                    };
                    ui.label("copied", format_args!("{copied}"))?
                        .secondary(true)?
                        .style("CaptionTextBlockStyle")?
                        .margin([0.0, 8.0, 0.0, 0.0])?;
                    Ok(())
                },
            )?;
            Ok(())
        },
    )?;
    if matches!(pass, HistoryPass::Copy(_)) {
        history.copied = true;
        ui.request_pass();
    }
    if dialog.answer().is_some() {
        pass = HistoryPass::Close;
    }
    Ok(pass)
}

/// An ISO 8601 time as the dialog shows it, in this computer's time zone:
/// `14 Oct 2026, 09:12`. Text that does not read as a time shows as given.
fn when(iso: &str) -> String {
    match iso.parse::<Timestamp>() {
        Ok(time) => time
            .to_zoned_datetime(TimeZone::system())
            .strftime("%-d %b %Y, %H:%M")
            .to_string(),
        Err(_) => iso.to_owned(),
    }
}

#[cfg(test)]
mod tests;
