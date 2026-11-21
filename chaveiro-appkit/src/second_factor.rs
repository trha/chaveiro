//! The two-step login screen, from the "Two-step" and "New device check"
//! boards of the Chaveiro macOS design: a title and a line on where the
//! code comes from, then a form with the method, the code and Remember
//! this Mac, then a row with the resend link, Back and Continue.
//!
//! Authenticator, email and the new-device check take six digit boxes;
//! YubiKey OTP takes one text field, because the key types a long code and
//! Return. A missing code shows under the entry on Continue; a code the
//! server refuses shows there too, and empties the entry. Other errors show
//! left of the buttons, as on the login screen.

use appkit_immediate::{Align, CodeDigits, Orientation, Style, TextColor, Ui};
use chaveiro_core::core::{CODE_PROVIDERS, Challenge, LoginError, UserAction};
use std::array;
use zeroize::Zeroizing;

/// What the user entered on the screen. Make a new one for each challenge.
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
    /// The method picked in the pop-up. [`None`] for the challenge's
    /// default.
    provider: Option<u8>,
    /// The user asked for an email code since picking email.
    asked_email: bool,
}

/// What the screen shows besides the form.
pub struct SecondFactorState<'a> {
    pub challenge: &'a Challenge,
    /// The account's email, named by the new-device check.
    pub email: &'a str,
    pub error: Option<&'a str>,
    /// The error is a refused code.
    pub refused: bool,
    /// A code is being checked.
    pub busy: bool,
    /// The email code is being sent.
    pub sending: bool,
}

/// What the user asked for in a pass.
pub enum SecondFactorAction {
    User(UserAction),
    /// Leave the login.
    Back,
}

const MISSING_DIGITS: &str = "Enter all 6 digits";
const MISSING_YUBIKEY: &str = "Touch your YubiKey to enter the code";
const REFUSED: &str = "That code didn’t work. Check it and try again.";
const REFUSED_YUBIKEY: &str = "That code didn’t work. Touch your YubiKey again.";

/// The providers the method pop-up lists, in its order: authenticator,
/// email, YubiKey OTP, security key (FIDO2), Duo, and Duo for an
/// organization.
const LISTED: [u8; 6] = [0, 1, 3, 7, 2, 6];

/// Draws the window and returns what the user asked for.
pub fn view(
    ui: &mut Ui,
    form: &mut SecondFactorForm,
    state: SecondFactorState<'_>,
) -> Option<SecondFactorAction> {
    let mut action = None;
    // The answer to the last code: a refusal empties the entry.
    let answered = form.waiting && !state.busy;
    if answered {
        form.waiting = false;
    }
    let method = method(&state, form);
    if answered && state.refused {
        form.digits = CodeDigits::default();
        form.long_code.clear();
        form.code_error = Some(match method.six {
            true => REFUSED,
            false => REFUSED_YUBIKEY,
        });
    }
    let enabled = !state.busy;
    let mut window = ui.window("main", "Chaveiro", |ui| {
        ui.stack_view("second factor", Orientation::Vertical, 16.0, |ui| {
            ui.stack_view("heading", Orientation::Vertical, 4.0, |ui| {
                ui.label("title", format_args!("{}", method.title))
                    .style(Style::Heading);
                ui.label("intro", format_args!("{}", method.intro))
                    .text_color(TextColor::Secondary);
            })
            .padding([0.0, 0.0, 0.0, 2.0]);

            let mut enter = false;
            let mut entry = None;
            ui.form("fields", HEADER_WIDTH, |ui| {
                // The method, when the account has a choice.
                if let Challenge::TwoFactor { providers, .. } = state.challenge {
                    // The offered providers fill the front of `slots`.
                    let mut slots = [0; LISTED.len()];
                    let mut count = 0;
                    for provider in LISTED.into_iter().filter(|p| providers.contains(p)) {
                        slots[count] = provider;
                        count += 1;
                    }
                    let listed = &slots[..count];
                    if listed.len() > 1 {
                        let names: [&str; LISTED.len()] = array::from_fn(|i| name(slots[i]));
                        let names = &names[..count];
                        let supported: [bool; LISTED.len()] =
                            array::from_fn(|i| CODE_PROVIDERS.contains(&slots[i]));
                        let supported = &supported[..count];
                        let mut index = method
                            .provider
                            .and_then(|current| listed.iter().position(|p| *p == current))
                            .unwrap_or(usize::MAX);
                        let mut pop_up = ui.pop_up_button("method", names, &mut index);
                        let picked = pop_up.changed().then(|| listed[index]);
                        pop_up
                            .enabled_items(supported)
                            .header("Method")
                            .enabled(enabled);
                        if let Some(provider) = picked
                            && Some(provider) != method.provider
                        {
                            // Each pick starts with an empty entry that takes
                            // focus. Email waits for Send Code.
                            form.provider = Some(provider);
                            form.digits = CodeDigits::default();
                            form.long_code.clear();
                            form.code_error = None;
                            form.focused = false;
                            form.asked_email = false;
                            // The heading above shows the old method.
                            ui.request_pass();
                        }
                    }
                }
                // A pick above changes the entry, so it is read again.
                let method = self::method(&state, form);
                if method.unsupported {
                    return;
                }

                // The code.
                let refocus = !form.focused || (answered && form.code_error.is_some());
                let ready = enabled && method.sent;
                if method.six {
                    let mut boxes = ui.code_boxes("code", &mut form.digits);
                    if boxes.changed() {
                        form.code_error = None;
                    }
                    enter |= boxes.return_pressed();
                    boxes.header("Code").error(form.code_error).enabled(ready);
                    entry = Some((boxes.field(), form.digits.contains(&None)));
                } else {
                    let mut field =
                        ui.text_field("yubikey", "Touch your YubiKey", &mut form.long_code);
                    if field.changed() {
                        form.code_error = None;
                    }
                    enter |= field.return_pressed();
                    field.header("Code").error(form.code_error).enabled(ready);
                    entry = Some((field.field(), form.long_code.trim().is_empty()));
                }
                let (field, _) = entry.expect("set above");
                if refocus && ready {
                    ui.make_first_responder(field);
                    form.focused = true;
                }

                if method.remember {
                    ui.check_box("remember", "Remember this Mac", &mut form.remember)
                        .header("")
                        .enabled(enabled);
                }
            });
            let method = self::method(&state, form);

            let mut clicked = false;
            ui.stack_view("buttons", Orientation::Horizontal, 10.0, |ui| {
                if method.resend && method.sent {
                    let label = match state.sending {
                        true => "Sending…",
                        false => "Send the code again",
                    };
                    let mut link = ui.link("resend", label);
                    if link.clicked() && !state.sending {
                        action = Some(SecondFactorAction::User(UserAction::SendEmailCode));
                    }
                    link.enabled(!state.sending && enabled).align(Align::Center);
                }
                // Errors that name no field.
                let error = state.error.filter(|_| !state.refused);
                ui.alert_line("error", error).align(Align::Center);
                if ui.button("back", "Back").clicked() {
                    action = Some(SecondFactorAction::Back);
                }
                let label = match (state.busy, method.sent) {
                    (true, _) => "Verifying…",
                    (false, true) => "Continue",
                    (false, false) => "Send Code",
                };
                let mut button = ui.button("continue", label);
                button
                    .default_button(true)
                    .busy(state.busy || state.sending)
                    .enabled(enabled && !method.unsupported && !state.sending);
                clicked = button.clicked();
            })
            .padding([0.0; 4]);

            if !(clicked || enter) || state.busy || method.unsupported {
                return;
            }
            if !method.sent {
                form.asked_email = true;
                action = Some(SecondFactorAction::User(UserAction::SendEmailCode));
                return;
            }
            let Some((field, missing)) = entry else {
                return;
            };
            if missing {
                let message = match method.six {
                    true => MISSING_DIGITS,
                    false => MISSING_YUBIKEY,
                };
                form.code_error = Some(message);
                ui.set_error(field, Some(message));
                ui.make_first_responder(field);
                return;
            }
            let code = Zeroizing::new(match method.six {
                true => form
                    .digits
                    .iter()
                    .flatten()
                    .map(|digit| char::from(b'0' + digit))
                    .collect(),
                false => form.long_code.trim().to_owned(),
            });
            form.code_error = None;
            ui.set_error(field, None);
            form.waiting = true;
            action = Some(SecondFactorAction::User(match method.provider {
                Some(provider) => UserAction::SubmitCode {
                    provider,
                    code,
                    remember: form.remember,
                },
                None => UserAction::SubmitNewDeviceOtp { code },
            }));
        })
        .padding([20.0, 36.0, 20.0, 20.0])
        .width(WIDTH);
    });
    window.compact().glass().size_to_fit();
    if window.should_close() {
        action = Some(SecondFactorAction::Back);
    }
    action
}

/// The window's width, from the design.
const WIDTH: f64 = 480.0;

/// The width of the form's header column, from the design.
const HEADER_WIDTH: f64 = 116.0;

/// How the pop-up names a provider.
fn name(provider: u8) -> &'static str {
    match provider {
        0 => "Authenticator app",
        1 => "Email",
        3 => "YubiKey OTP",
        7 => "Security key (FIDO2)",
        2 => "Duo",
        _ => "Duo (organization)",
    }
}

/// One way to answer the challenge, as the screen shows it.
struct Method {
    /// [`None`] for the new-device check, and for an account with no
    /// method this app can answer.
    provider: Option<u8>,
    title: &'static str,
    intro: String,
    /// Six digit boxes, or one text field.
    six: bool,
    resend: bool,
    remember: bool,
    /// The code is on its way, so the entry is on. False only for email
    /// before the user asked for it.
    sent: bool,
    /// The account has no method this app can answer.
    unsupported: bool,
}

/// The method the screen shows: the one picked in `form`, else the
/// challenge's default.
fn method(state: &SecondFactorState<'_>, form: &SecondFactorForm) -> Method {
    let Challenge::TwoFactor { masked_email, .. } = state.challenge else {
        return Method {
            provider: None,
            title: "Verify this Mac",
            intro: format!(
                "Enter the 6-digit code sent to {} to confirm this is you.",
                state.email
            ),
            six: true,
            // The core's resend asks for a two-step email, which is not the
            // new-device code.
            resend: false,
            remember: false,
            sent: true,
            unsupported: false,
        };
    };
    let address = masked_email.as_deref().unwrap_or("your email");
    let Some(provider) = form.provider.or(state.challenge.default_provider()) else {
        return Method {
            provider: None,
            title: "Two-step login",
            intro: "This account’s two-step login methods are not supported in this app yet. \
                    Go back to log in another way."
                .to_owned(),
            six: true,
            resend: false,
            remember: false,
            sent: false,
            unsupported: true,
        };
    };
    let intro = match provider {
        0 => "Enter the 6-digit code from your authenticator app.".to_owned(),
        // With email alone the core sent the code as the screen opened.
        1 if state.challenge.only_email() || form.asked_email => {
            format!("Enter the 6-digit code sent to {address}.")
        }
        1 => format!("Chaveiro will email a 6-digit code to {address}."),
        _ => "Insert your YubiKey and touch its button.".to_owned(),
    };
    Method {
        provider: Some(provider),
        title: "Two-step login",
        intro,
        six: provider != 3,
        resend: provider == 1,
        remember: true,
        sent: provider != 1 || state.challenge.only_email() || form.asked_email,
        unsupported: false,
    }
}

/// Whether `error` is a code the server refused, which shows under the
/// entry.
pub fn refused(error: Option<&LoginError>) -> bool {
    error == Some(&LoginError::InvalidCode)
}
