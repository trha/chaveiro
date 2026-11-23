//! "Choose a version" from the ItemEdit design: the dialog that shows both
//! copies of an item when a save meets a newer server copy, and asks which
//! one to keep.

use chaveiro_core::{
    core::Conflict,
    crypto::SecretString,
    vault::{CipherContent, CipherView, CustomField, VaultIndex},
};
use windows_core::Result;
use winui_immediate::{Align, DialogAnswer, DialogButtons, Length, Orientation, Overflow, Ui};

/// What the user picked in the dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Keep the user's copy and save it over the server's.
    Mine,
    /// Keep the server's copy and drop the user's.
    Theirs,
    /// Back to the form, with the conflict still open.
    Back,
}

/// The secret cells the user showed, by row and side (`true` for the
/// server's). They belong to one dialog; start a new list for the next.
pub type Shown = Vec<(usize, bool)>;

/// Draws the dialog for `conflict`. `id` names this showing of it, so a
/// later conflict opens a dialog of its own. Gives the user's pick once
/// the dialog closes.
pub fn conflict_dialog(
    ui: &mut Ui,
    id: u32,
    conflict: &Conflict<'_>,
    index: &VaultIndex,
    shown: &mut Shown,
) -> Result<Option<Choice>> {
    let name = conflict.mine.name.as_str();
    let differing: Vec<Row<'_>> = rows(conflict.mine, conflict.theirs, index)
        .into_iter()
        .filter(|row| row.mine != row.theirs)
        .collect();
    let buttons = DialogButtons {
        primary: "Keep my changes",
        // Shorter than the design's "Keep the server version", which the
        // dialog's equal-width buttons cut off.
        secondary: Some("Keep server version"),
        close: "Back to editing",
        default: DialogAnswer::Primary,
    };
    let title = format!("Choose a version of {name}");
    let dialog = ui.content_dialog(("conflict", id), &title, buttons, |ui| {
        // The dialog gives its body the height it has, so the table takes
        // what is left and scrolls when the window is too short.
        ui.grid(
            "conflict",
            &[Length::Auto, Length::Star(1.0), Length::Auto],
            |ui| {
                ui.label(
                    "explain",
                    format_args!(
                        "{name} changed on another device while you were editing it. Pick the \
                     version to keep."
                    ),
                )?
                .overflow(Overflow::Wrap)?
                .margin([0.0, 0.0, 0.0, 8.0])?;

                ui.scroll("table", |ui| {
                    ui.card_group("table", |ui| {
                        // The card's corners less its 1px stroke, so the fill stays
                        // inside the stroke.
                        row(ui, "head", |ui| {
                            ui.label("field", format_args!("Field"))?
                                .style("BodyStrongTextBlockStyle")?;
                            for (id, text) in
                                [("mine", "Your changes"), ("theirs", "On the server")]
                            {
                                ui.label(id, format_args!("{text}"))?
                                    .style("BodyStrongTextBlockStyle")?;
                            }
                            Ok(())
                        })?
                        .fill(
                            Some("CardBackgroundFillColorSecondaryBrush"),
                            [7.0, 7.0, 0.0, 0.0],
                        )?;

                        for (position, field) in differing.iter().enumerate() {
                            row(ui, ("row", position), |ui| {
                                ui.label("field", format_args!("{}", field.label))?
                                    .overflow(Overflow::Wrap)?;
                                for (side, theirs) in [(&field.mine, false), (&field.theirs, true)]
                                {
                                    cell(ui, theirs, side.as_ref(), (position, theirs), shown)?;
                                }
                                Ok(())
                            })?
                            .dividers([false, true, false, false])?;
                        }

                        row(ui, "changed", |ui| {
                            ui.label("field", format_args!("Changed"))?;
                            ui.label("mine", format_args!("Not saved yet"))?;
                            ui.label("theirs", format_args!("{}", when(conflict.theirs_changed)))?;
                            Ok(())
                        })?
                        .dividers([false, true, false, false])?;
                        Ok(())
                    })?;
                    Ok(())
                })?;

                let hint = match differing.is_empty() {
                    true => "No field differs; only the server's copy is newer.",
                    false => {
                        "Only the fields that differ are shown. Secrets stay hidden until \
                          you show them."
                    }
                };
                ui.label("hint", format_args!("{hint}"))?
                    .secondary(true)?
                    .overflow(Overflow::Wrap)?
                    .style("CaptionTextBlockStyle")?
                    .margin([0.0, 8.0, 0.0, 0.0])?;
                Ok(())
            },
        )?;
        Ok(())
    })?;
    Ok(dialog.answer().map(|answer| match answer {
        DialogAnswer::Primary => Choice::Mine,
        DialogAnswer::Secondary => Choice::Theirs,
        DialogAnswer::Close => Choice::Back,
    }))
}

/// The field names, then the user's copy and the server's.
const COLUMNS: [Length; 3] = [Length::Pixels(120.0), Length::Star(1.0), Length::Star(1.0)];

/// A row of the table: the three cells `body` declares, in a stack that
/// can take a fill and dividers.
fn row(
    ui: &mut Ui,
    id: impl std::hash::Hash,
    body: impl FnOnce(&mut Ui) -> Result<()>,
) -> Result<winui_immediate::Response<'_>> {
    let mut row = ui.stack(id, Orientation::Vertical, 0.0, |ui| {
        ui.columns("cells", &COLUMNS, 12.0, body)?;
        Ok(())
    })?;
    row.padding([12.0, 8.0, 12.0, 8.0])?;
    Ok(row)
}

/// One side of a row; a secret shows as dots until its key is in `shown`.
fn cell(
    ui: &mut Ui,
    id: bool,
    value: Option<&Value<'_>>,
    key: (usize, bool),
    shown: &mut Shown,
) -> Result<()> {
    match value {
        None => {
            ui.label(id, format_args!(""))?;
        }
        Some(Value::Text(text)) => {
            ui.label(id, format_args!("{text}"))?
                .overflow(Overflow::Wrap)?;
        }
        Some(Value::Lines(lines)) => {
            ui.stack(id, Orientation::Vertical, 0.0, |ui| {
                for (line, text) in lines.iter().enumerate() {
                    ui.label(line, format_args!("{text}"))?
                        .overflow(Overflow::Wrap)?;
                }
                Ok(())
            })?;
        }
        Some(Value::Date { month, year }) => {
            ui.label(id, format_args!("{month} / {year}"))?;
        }
        Some(Value::Secret(text)) => {
            let visible = shown.contains(&key);
            let mut toggled = false;
            ui.columns(id, &[Length::Star(1.0), Length::Auto], 4.0, |ui| {
                let mut label = match visible {
                    true => ui.label("value", format_args!("{text}"))?,
                    false => ui.label("value", format_args!("••••••••"))?,
                };
                label
                    .monospace(true)?
                    .overflow(Overflow::Wrap)?
                    .valign(Align::Center)?;
                let (glyph, tip) = match visible {
                    true => ('\u{ED1A}', "Hide"),
                    false => ('\u{E890}', "Show"),
                };
                toggled = ui.icon_button("show", glyph, tip)?.clicked()?;
                Ok(())
            })?;
            if toggled {
                match visible {
                    true => shown.retain(|shown| *shown != key),
                    false => shown.push(key),
                }
                ui.request_pass();
            }
        }
    }
    Ok(())
}

/// A field's value as the dialog shows it.
#[derive(Debug, PartialEq, Eq)]
enum Value<'a> {
    Text(&'a str),
    /// Hidden until the user shows it.
    Secret(&'a str),
    /// Several values, one per line, as a login's websites.
    Lines(Vec<&'a str>),
    /// A card's expiration as stored.
    Date {
        month: &'a str,
        year: &'a str,
    },
}

/// One field of the table, with each copy's value, or [`None`] where that
/// copy has no value.
struct Row<'a> {
    label: &'a str,
    mine: Option<Value<'a>>,
    theirs: Option<Value<'a>>,
}

/// The rows of the table: the fields of `mine` in their order, then the
/// fields only `theirs` has.
fn rows<'a>(mine: &'a CipherView, theirs: &'a CipherView, index: &'a VaultIndex) -> Vec<Row<'a>> {
    let mut theirs = fields(theirs, index);
    let mut rows: Vec<Row<'a>> = fields(mine, index)
        .into_iter()
        .map(|(label, value)| {
            let theirs = theirs
                .iter()
                .position(|(other, _)| *other == label)
                .map(|position| theirs.remove(position).1);
            Row {
                label,
                mine: Some(value),
                theirs,
            }
        })
        .collect();
    rows.extend(theirs.into_iter().map(|(label, value)| Row {
        label,
        mine: None,
        theirs: Some(value),
    }));
    rows
}

/// The labeled values of `view` that are set, in the order of the details
/// pane.
fn fields<'a>(view: &'a CipherView, index: &'a VaultIndex) -> Vec<(&'a str, Value<'a>)> {
    let mut out = Vec::new();
    out.push(("Name", Value::Text(view.name.as_str())));
    let folder = view
        .folder_id
        .and_then(|id| index.folders.iter().find(|folder| folder.id == id))
        .map_or("No folder", |folder| match folder.readable {
            true => folder.name.as_str(),
            false => "Unreadable folder",
        });
    out.push(("Folder", Value::Text(folder)));

    /// Adds the field `label` when it is set.
    fn set<'a>(
        out: &mut Vec<(&'a str, Value<'a>)>,
        label: &'static str,
        value: &'a Option<SecretString>,
        secret: bool,
    ) {
        if let Some(value) = value.as_deref().filter(|value| !value.is_empty()) {
            let value = match secret {
                true => Value::Secret(value.as_str()),
                false => Value::Text(value.as_str()),
            };
            out.push((label, value));
        }
    }
    match &view.content {
        CipherContent::Login(login) => {
            set(&mut out, "Username", &login.username, false);
            set(&mut out, "Password", &login.password, true);
            set(&mut out, "Authenticator key", &login.totp, true);
            let websites: Vec<&str> = login
                .uris
                .iter()
                .filter_map(|uri| uri.uri.as_deref().map(String::as_str))
                .collect();
            if !websites.is_empty() {
                out.push(("Websites", Value::Lines(websites)));
            }
        }
        CipherContent::Card(card) => {
            set(&mut out, "Cardholder name", &card.cardholder_name, false);
            set(&mut out, "Brand", &card.brand, false);
            set(&mut out, "Number", &card.number, true);
            if card.exp_month.is_some() || card.exp_year.is_some() {
                let part =
                    |value: &'a Option<SecretString>| value.as_deref().map_or("", String::as_str);
                out.push((
                    "Expiration",
                    Value::Date {
                        month: part(&card.exp_month),
                        year: part(&card.exp_year),
                    },
                ));
            }
            set(&mut out, "Security code", &card.code, true);
        }
        CipherContent::Identity(identity) => {
            for (label, value) in [
                ("Title", &identity.title),
                ("First name", &identity.first_name),
                ("Middle name", &identity.middle_name),
                ("Last name", &identity.last_name),
                ("Username", &identity.username),
                ("Company", &identity.company),
                ("Passport number", &identity.passport_number),
                ("Tax number", &identity.ssn),
                ("Driver's license", &identity.license_number),
                ("Email", &identity.email),
                ("Phone", &identity.phone),
                ("Address", &identity.address1),
                ("Address line 2", &identity.address2),
                ("Address line 3", &identity.address3),
                ("City", &identity.city),
                ("State or region", &identity.state),
                ("Postal code", &identity.postal_code),
                ("Country", &identity.country),
            ] {
                set(&mut out, label, value, false);
            }
        }
        CipherContent::SshKey(key) => {
            set(&mut out, "Public key", &key.public_key, false);
            set(&mut out, "Fingerprint", &key.key_fingerprint, false);
            set(&mut out, "Private key", &key.private_key, true);
        }
        _ => {}
    }

    for field in &view.fields {
        let Some(name) = field.name.as_deref() else {
            continue;
        };
        if let Some(value) = custom_value(field) {
            out.push((name.as_str(), value));
        }
    }
    if let Some(notes) = view.notes.as_deref().filter(|notes| !notes.is_empty()) {
        out.push(("Notes", Value::Text(notes.as_str())));
    }
    out
}

/// A custom field's value: hidden for a hidden field, Yes or No for a
/// boolean one.
fn custom_value(field: &CustomField) -> Option<Value<'_>> {
    let value = field.value.as_deref().map_or("", String::as_str);
    match field.kind {
        1 if !value.is_empty() => Some(Value::Secret(value)),
        2 => Some(Value::Text(match value {
            "true" => "Yes",
            _ => "No",
        })),
        _ if !value.is_empty() => Some(Value::Text(value)),
        _ => None,
    }
}

/// An ISO 8601 date and time as `2026-11-21 14:02 UTC`, or as given when
/// it is shorter.
fn when(iso: &str) -> String {
    match (iso.get(..10), iso.get(11..16)) {
        (Some(day), Some(time)) => format!("{day} {time} UTC"),
        _ => iso.to_owned(),
    }
}

#[cfg(test)]
mod tests;
