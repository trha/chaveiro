//! The two-step login screens: the code screen and the method list.
//!
//! The code screen shows the method in use, the code entry, Remember me, one
//! accent Continue button, and a link to email the code again. When the
//! account offers more than one method, the method card opens the list.
//!
//! Authenticator, email and the new-device check take six digit boxes;
//! YubiKey OTP takes one text box, because the key types a long code and
//! Enter. A missing code shows under the entry on Continue; a code the
//! server refuses shows there too, and empties the entry. Other errors go
//! in the InfoBar.

use vault_core::core::{CODE_PROVIDERS, Challenge, LoginError, UserAction};
use windows_core::Result;
use winui_immediate::{Align, CodeDigits, Orientation, SettingsCard, Severity, Ui};
use zeroize::Zeroizing;

/// What the user entered on the screens.
#[derive(Default)]
pub struct SecondFactorForm {
    digits: CodeDigits,
    /// The YubiKey's code.
    long_code: String,
    remember: bool,
    /// The error under the code entry, until the user types again.
    code_error: Option<&'static str>,
    /// A code went out and its answer has not been seen yet.
    waiting: bool,
    /// The code entry took focus when the screen opened.
    focused: bool,
    /// The method picked from the list. [`None`] while the list shows, or
    /// for the default when the account has one method.
    provider: Option<u8>,
}

impl SecondFactorForm {
    /// Whether the window shows the method list for `challenge`: until the
    /// user picks a method, when the account has more than one. It also
    /// shows when the app can answer none of them, because the code screen
    /// would then have nothing to show.
    pub fn choosing(&self, challenge: &Challenge) -> bool {
        match challenge {
            Challenge::TwoFactor { providers, .. } => {
                self.provider.is_none()
                    && (has_choices(providers) || challenge.default_provider().is_none())
            }
            Challenge::NewDevice => false,
        }
    }

    /// Takes the title bar's back button on the code screen. Returns true
    /// when it went back to the method list, and false when there is no
    /// list to go back to, so the back button leaves the login.
    pub fn back(&mut self, challenge: &Challenge) -> bool {
        let to_list = match challenge {
            Challenge::TwoFactor { providers, .. } => {
                self.provider.is_some() && has_choices(providers)
            }
            Challenge::NewDevice => false,
        };
        self.provider = None;
        to_list
    }
}

/// The account offers more than one method, so the user picks one. This
/// counts every provider, as the core does when it decides to wait for the
/// pick before it emails a code.
fn has_choices(providers: &[u8]) -> bool {
    providers.len() > 1
}

/// What the screens show besides the form.
pub struct SecondFactorState<'a> {
    pub challenge: &'a Challenge,
    /// The account's email, named by the new-device check.
    pub email: &'a str,
    pub error: Option<&'a LoginError>,
    /// A code is being checked.
    pub busy: bool,
    /// The email code is being sent.
    pub sending: bool,
}

// Segoe Fluent Icons glyphs for the methods, as the design shows them.
const CELL_PHONE: &str = "\u{E8EA}";
const MAIL: &str = "\u{E715}";
const PERMISSIONS: &str = "\u{E8D7}";
const DEVICE: &str = "\u{E7F8}";
const LOCK: &str = "\u{E72E}";
const SHIELD: &str = "\u{EA18}";

const MISSING_DIGITS: &str = "Enter all 6 digits";
const MISSING_YUBIKEY: &str = "Touch your YubiKey to enter the code";
const REFUSED: &str = "That code didn’t work. Check it and try again.";
const UNSUPPORTED: &str = "Not supported in this app yet";

/// The providers the method list shows, in its order: authenticator,
/// email, YubiKey OTP, security key (FIDO2), Duo, and Duo for an
/// organization.
const LISTED: [u8; 6] = [0, 1, 3, 7, 2, 6];

/// Draws the code screen, or the method list while it is open, and returns
/// what the user asked for: a code to check, or another email.
pub fn view(
    ui: &mut Ui,
    form: &mut SecondFactorForm,
    state: SecondFactorState<'_>,
) -> Result<Option<UserAction>> {
    if form.choosing(state.challenge) {
        return methods(ui, form, &state);
    }
    let mut asked = None;
    // The answer to the last code: a refusal empties the entry.
    let refused = form.waiting && !state.busy;
    if refused {
        form.waiting = false;
        if state.error == Some(&LoginError::InvalidCode) {
            form.digits = CodeDigits::default();
            form.long_code.clear();
            form.code_error = Some(REFUSED);
        }
    }
    let method = method(&state, form.provider).expect("choosing covers a challenge with no method");
    let enabled = !state.busy;
    crate::screen::form(ui, "second factor", |ui| {
        crate::screen::title(ui, method.title)?;

        // The method in use. A card is clickable or not for life, so the
        // two kinds take different ids.
        let card = SettingsCard {
            header: method.name,
            description: &method.caption,
            icon: method.icon,
            clickable: method.choices,
            ..SettingsCard::default()
        };
        let id = match method.choices {
            true => "method choice",
            false => "method",
        };
        let mut card = ui.settings_card(id, card, |_| Ok(()))?;
        if card.clicked() && enabled {
            form.provider = None;
        }
        card.enabled(enabled)?.margin(crate::screen::GROUP_GAP_BELOW)?;

        // The code.
        let mut first_empty = None;
        let mut enter = false;
        if method.six {
            ui.label("code label", format_args!("Enter the 6-digit code"))?;
            let boxes = ui.code_boxes("code", &mut form.digits, enabled, form.code_error)?;
            if boxes.changed() {
                form.code_error = None;
            }
            enter |= boxes.enter_pressed();
            let empty = form.digits.iter().position(Option::is_none);
            first_empty = empty.map(|index| boxes.field(index));
            if !form.focused || (refused && form.code_error.is_some()) {
                form.focused |= ui.focus(boxes.field(0))?;
            }
        } else {
            let mut field = ui.text_box("code", "", &mut form.long_code)?;
            field.header("YubiKey code")?.enabled(enabled)?;
            if field.changed() {
                form.code_error = None;
            }
            enter |= field.enter_pressed()?;
            let handle = field.field();
            ui.set_error(handle, form.code_error)?;
            if form.long_code.trim().is_empty() {
                first_empty = Some(handle);
            }
            if !form.focused || (refused && form.code_error.is_some()) {
                form.focused |= ui.focus(handle)?;
            }
        }

        if method.remember {
            ui.check_box("remember", "Remember me on this device", &mut form.remember)?
                .enabled(enabled)?;
        }

        let label = match state.busy {
            true => "Verifying…",
            false => "Continue",
        };
        let mut button = ui.button("continue", label)?;
        let clicked = button.clicked()?;
        button
            .accent(true)?
            .busy(state.busy)?
            .align(Align::Stretch)?
            .margin(crate::screen::GROUP_GAP_ABOVE)?
            .access_key("C")?;

        if (clicked || enter) && !state.busy {
            match first_empty {
                Some(field) => {
                    form.code_error = Some(match method.six {
                        true => MISSING_DIGITS,
                        false => MISSING_YUBIKEY,
                    });
                    ui.focus(field)?;
                }
                None => {
                    let code = match method.six {
                        true => form
                            .digits
                            .iter()
                            .flatten()
                            .map(|digit| char::from(b'0' + digit))
                            .collect(),
                        false => form.long_code.trim().to_owned(),
                    };
                    let code = Zeroizing::new(code);
                    form.code_error = None;
                    form.waiting = true;
                    asked = Some(match method.provider {
                        Some(provider) => UserAction::SubmitCode {
                            provider,
                            code,
                            remember: form.remember,
                        },
                        None => UserAction::SubmitNewDeviceOtp { code },
                    });
                }
            }
        }

        if method.resend {
            let label = match state.sending {
                true => "Sending…",
                false => "Send the code again",
            };
            let mut link = ui.link("resend", label)?;
            if link.clicked() && !state.sending {
                asked = Some(UserAction::SendEmailCode);
            }
            link.enabled(!state.sending && enabled)?;
        }

        // Errors that name no field.
        if let Some(error) = state
            .error
            .filter(|error| **error != LoginError::InvalidCode)
        {
            ui.info_bar(
                "error",
                Severity::Error,
                "Two-step login failed",
                &crate::login_error(error),
            )?;
        }
        Ok(())
    })?;
    Ok(asked)
}

/// Draws the method list: one card per method the account offers, with the
/// ones this app cannot answer disabled. Picking another method returns to
/// the code screen with an empty entry; picking email also asks for a code.
fn methods(
    ui: &mut Ui,
    form: &mut SecondFactorForm,
    state: &SecondFactorState<'_>,
) -> Result<Option<UserAction>> {
    let Challenge::TwoFactor {
        providers,
        masked_email,
    } = state.challenge
    else {
        unreachable!("the new-device check has no list");
    };
    let current = form.provider.or(state.challenge.default_provider());
    let mut picked = None;
    crate::screen::form(ui, "methods", |ui| {
        crate::screen::title(ui, "Choose a method")?;
        if current.is_none() {
            ui.info_bar(
                "unsupported",
                Severity::Warning,
                "Not supported yet",
                "This account’s two-step login methods are not supported in this app yet. Use the back button to log in another way.",
            )?;
        }
        ui.stack("list", Orientation::Vertical, 4.0, |ui| {
            for provider in LISTED.into_iter().filter(|provider| providers.contains(provider)) {
                let row = row(provider, masked_email.as_deref());
                let supported = CODE_PROVIDERS.contains(&provider);
                let card = SettingsCard {
                    header: row.name,
                    description: match supported {
                        true => &row.caption,
                        false => UNSUPPORTED,
                    },
                    icon: row.icon,
                    clickable: true,
                    ..SettingsCard::default()
                };
                let mut card = ui.settings_card(provider, card, |_| Ok(()))?;
                if card.clicked() && supported {
                    picked = Some(provider);
                }
                card.enabled(supported)?;
            }
            Ok(())
        })?;
        Ok(())
    })?;
    let Some(provider) = picked else {
        return Ok(None);
    };
    // Each pick starts with an empty entry that takes focus.
    form.provider = Some(provider);
    form.digits = CodeDigits::default();
    form.long_code.clear();
    form.code_error = None;
    form.focused = false;
    // The core waits for this pick before it emails a code.
    Ok((provider == 1).then_some(UserAction::SendEmailCode))
}

/// How the screens name one provider.
struct Row {
    name: &'static str,
    /// Where the code comes from, as the method list says it.
    caption: String,
    /// A Segoe Fluent Icons glyph.
    icon: &'static str,
}

fn row(provider: u8, masked_email: Option<&str>) -> Row {
    let (name, caption, icon) = match provider {
        0 => (
            "Authenticator app",
            "A code from an app such as Authy or Microsoft Authenticator".to_owned(),
            CELL_PHONE,
        ),
        1 => (
            "Email",
            match masked_email {
                Some(address) => format!("Send a code to {address}"),
                None => "Send a code to your email".to_owned(),
            },
            MAIL,
        ),
        3 => (
            "YubiKey OTP",
            "Touch your YubiKey to type a code".to_owned(),
            PERMISSIONS,
        ),
        7 => ("Security key (FIDO2)", String::new(), LOCK),
        2 => ("Duo", String::new(), SHIELD),
        _ => ("Duo (organization)", String::new(), SHIELD),
    };
    Row { name, caption, icon }
}

/// One way to answer the challenge, as the code screen names it.
struct Method {
    /// [`None`] for the new-device check.
    provider: Option<u8>,
    title: &'static str,
    name: &'static str,
    caption: String,
    /// A Segoe Fluent Icons glyph.
    icon: &'static str,
    /// Six digit boxes, or one text box.
    six: bool,
    resend: bool,
    remember: bool,
    /// The account offers other methods, so the card opens the list.
    choices: bool,
}

/// The method the code screen shows: `chosen`, else the challenge's
/// default. [`None`] when this app can answer none of the account's
/// methods.
fn method(state: &SecondFactorState<'_>, chosen: Option<u8>) -> Option<Method> {
    let Challenge::TwoFactor {
        providers,
        masked_email,
    } = state.challenge
    else {
        return Some(Method {
            provider: None,
            title: "Verify this device",
            name: "New device check",
            caption: format!("Code sent to {}", state.email),
            icon: DEVICE,
            six: true,
            resend: false,
            remember: false,
            choices: false,
        });
    };
    let provider = chosen.or(state.challenge.default_provider())?;
    let caption = match provider {
        0 => "Code from your authenticator app".to_owned(),
        1 => match masked_email {
            Some(address) => format!("Code sent to {address}"),
            None => "Code sent to your email".to_owned(),
        },
        _ => "Insert your key, then touch it".to_owned(),
    };
    let row = row(provider, masked_email.as_deref());
    let listed = LISTED.into_iter().filter(|provider| providers.contains(provider));
    Some(Method {
        provider: Some(provider),
        title: "Two-step login",
        name: row.name,
        caption,
        icon: row.icon,
        six: provider != 3,
        resend: provider == 1,
        remember: true,
        choices: listed.count() > 1,
    })
}
