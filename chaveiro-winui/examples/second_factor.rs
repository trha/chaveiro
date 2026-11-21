//! The two-step login screen on its own, against a fake server, so it can be
//! seen and driven without an account. `PREVIEW_METHOD` picks the challenge:
//! `app` (the default), `email`, `yubikey`, `device` or `unsupported`. The
//! fake server takes 1 s per code and accepts `123456` (or `cccc` for the
//! YubiKey); every other code is refused. Accepted codes print to stderr.
//! `tests/ui/second_factor.ps1` drives it.

#![allow(dead_code, reason = "shares modules with the app")]

#[path = "../src/screen.rs"]
mod screen;
#[path = "../src/second_factor.rs"]
mod second_factor;

use second_factor::{SecondFactorForm, SecondFactorState};
use std::time::Duration;
use vault_core::core::{Challenge, LoginError, UserAction};
use windows_core::Result;
use winui_immediate::{App, Handle, Orientation, Ui, run};

fn main() -> Result<()> {
    let challenge = match std::env::var("PREVIEW_METHOD").as_deref() {
        Ok("email") => Challenge::TwoFactor {
            providers: vec![1],
            masked_email: Some("f***@example.com".to_owned()),
        },
        Ok("yubikey") => Challenge::TwoFactor {
            providers: vec![3],
            masked_email: None,
        },
        Ok("device") => Challenge::NewDevice,
        Ok("unsupported") => Challenge::TwoFactor {
            providers: vec![2, 7],
            masked_email: None,
        },
        _ => Challenge::TwoFactor {
            providers: vec![0, 1],
            masked_email: None,
        },
    };
    run(|handle: Handle<Answer>| {
        Ok(Preview {
            handle,
            challenge,
            form: SecondFactorForm::default(),
            error: None,
            busy: false,
            sending: false,
            open: true,
        })
    })
}

/// The fake server's answer.
enum Answer {
    Code { accepted: bool },
    Sent,
}

struct Preview {
    handle: Handle<Answer>,
    challenge: Challenge,
    form: SecondFactorForm,
    error: Option<LoginError>,
    busy: bool,
    sending: bool,
    open: bool,
}

impl App for Preview {
    type Message = Answer;

    fn update(&mut self, answer: Answer) {
        match answer {
            Answer::Code { accepted } => {
                self.busy = false;
                self.error = (!accepted).then_some(LoginError::InvalidCode);
            }
            Answer::Sent => self.sending = false,
        }
    }

    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        let mut asked = None;
        let mut main = ui.window("main", "Chaveiro preview", |ui| {
            ui.stack("window", Orientation::Vertical, 0.0, |ui| {
                ui.title_bar("title bar", "Chaveiro", true)?;
                let state = SecondFactorState {
                    challenge: &self.challenge,
                    email: "fixture-user@example.com",
                    error: self.error.as_ref(),
                    busy: self.busy,
                    sending: self.sending,
                };
                asked = second_factor::view(ui, &mut self.form, state)?;
                Ok(())
            })?
            .width(360.0)?;
            Ok(())
        })?;
        main.mica()?.compact()?.fit_to_content()?;
        if main.close_requested() {
            self.open = false;
        }
        match asked {
            Some(UserAction::SubmitCode {
                code,
                provider,
                remember,
            }) => {
                eprintln!("submitted provider {provider} remember {remember}");
                self.answer_later(code.as_str() == "123456" || code.as_str() == "cccc");
            }
            Some(UserAction::SubmitNewDeviceOtp { code }) => {
                eprintln!("submitted new device code");
                self.answer_later(code.as_str() == "123456");
            }
            Some(UserAction::SendEmailCode) => {
                eprintln!("send email");
                self.sending = true;
                let handle = self.handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(1));
                    // The window may be gone, which needs no answer.
                    let _ = handle.send(Answer::Sent);
                });
            }
            _ => {}
        }
        Ok(())
    }
}

impl Preview {
    fn answer_later(&mut self, accepted: bool) {
        if accepted {
            eprintln!("accepted");
        }
        self.busy = true;
        self.error = None;
        let handle = self.handle.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(1));
            // The window may be gone, which needs no answer.
            let _ = handle.send(Answer::Code { accepted });
        });
    }
}

fn login_error(error: &LoginError) -> String {
    format!("{error:?}")
}
