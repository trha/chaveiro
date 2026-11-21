//! The two-step login screen: the method in use, the code entry, Remember
//! me, one accent Continue button, and a link to email the code again.
//!
//! Authenticator, email and the new-device check take six digit boxes;
//! YubiKey OTP takes one text box, because the key types a long code and
//! Enter. A missing code shows under the entry on Continue; a code the
//! server refuses shows there too, and empties the entry. Other errors go
//! in the InfoBar.

use vault_core::core::{Challenge, LoginError, UserAction};
use windows_core::Result;
use winui_immediate::{Align, CodeDigits, SettingsCard, Severity, Ui};
use zeroize::Zeroizing;

/// What the user entered on the screen.
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
}

/// What the screen shows besides the form.
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

const MISSING_DIGITS: &str = "Enter all 6 digits";
const MISSING_YUBIKEY: &str = "Touch your YubiKey to enter the code";
const REFUSED: &str = "That code didn’t work. Check it and try again.";

/// Draws the screen and returns what the user asked for: a code to check,
/// or another email.
pub fn view(
    ui: &mut Ui,
    form: &mut SecondFactorForm,
    state: SecondFactorState<'_>,
) -> Result<Option<UserAction>> {
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
    let Some(method) = method(&state) else {
        crate::screen::form(ui, "second factor", |ui| {
            crate::screen::title(ui, "Two-step login")?;
            ui.info_bar(
                "unsupported",
                Severity::Warning,
                "Not supported yet",
                "This account’s two-step login methods are not supported in this app yet. Use the back button to log in another way.",
            )?;
            Ok(())
        })?;
        return Ok(None);
    };
    let enabled = !state.busy;
    crate::screen::form(ui, "second factor", |ui| {
        crate::screen::title(ui, method.title)?;

        // The method in use.
        ui.settings_card(
            "method",
            SettingsCard {
                header: method.name,
                description: &method.caption,
                icon: method.icon,
                ..SettingsCard::default()
            },
            |_| Ok(()),
        )?
        .margin(crate::screen::GROUP_GAP_BELOW)?;

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

/// One way to answer the challenge, as the screen names it.
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
}

/// The method the screen opens with, or [`None`] when this app can answer
/// none of the account's methods.
fn method(state: &SecondFactorState<'_>) -> Option<Method> {
    let provider = match state.challenge {
        Challenge::NewDevice => {
            return Some(Method {
                provider: None,
                title: "Verify this device",
                name: "New device check",
                caption: format!("Code sent to {}", state.email),
                icon: DEVICE,
                six: true,
                resend: false,
                remember: false,
            });
        }
        Challenge::TwoFactor { .. } => state.challenge.default_provider()?,
    };
    let Challenge::TwoFactor { masked_email, .. } = state.challenge else {
        unreachable!("matched above");
    };
    let (name, caption, icon, six) = match provider {
        0 => (
            "Authenticator app",
            "Code from your authenticator app".to_owned(),
            CELL_PHONE,
            true,
        ),
        1 => (
            "Email",
            match masked_email {
                Some(address) => format!("Code sent to {address}"),
                None => "Code sent to your email".to_owned(),
            },
            MAIL,
            true,
        ),
        _ => (
            "YubiKey OTP",
            "Insert your key, then touch it".to_owned(),
            PERMISSIONS,
            false,
        ),
    };
    Some(Method {
        provider: Some(provider),
        title: "Two-step login",
        name,
        caption,
        icon,
        six,
        resend: provider == 1,
        remember: true,
    })
}
