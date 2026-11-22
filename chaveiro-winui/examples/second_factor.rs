//! The two-step login screen on its own, against a fake server, so it can be
//! seen and driven without an account. `PREVIEW_METHOD` picks the challenge:
//! `app` (the default), `email`, `yubikey`, `device` or `unsupported`. The
//! fake server takes 1 s per code and accepts `123456` (or `cccc` for the
//! YubiKey); every other code is refused. Accepted codes print to stderr.
//! `tests/ui/second_factor.ps1` drives it.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]
#![allow(dead_code, reason = "shares modules with the app")]

#[path = "../src/screen.rs"]
mod screen;
#[path = "../src/second_factor.rs"]
mod second_factor;

use chaveiro_core::core::{Challenge, LoginError, UserAction};
use second_factor::{SecondFactorForm, SecondFactorState};
use std::time::{Duration, Instant};
use windows_core::Result;
use winui_immediate::{App, Orientation, Ui, run};

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
    run(|| {
        Ok(Preview {
            answer: None,
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
    /// The fake server's next answer and when it arrives.
    answer: Option<(Instant, Answer)>,
    challenge: Challenge,
    form: SecondFactorForm,
    error: Option<LoginError>,
    busy: bool,
    sending: bool,
    open: bool,
}

impl App for Preview {
    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        // Applied before the screen reads the state, so this pass shows it.
        if self.answer.as_ref().is_some_and(|(due, _)| Instant::now() >= *due) {
            match self.answer.take().expect("checked above").1 {
                Answer::Code { accepted } => {
                    self.busy = false;
                    self.error = (!accepted).then_some(LoginError::InvalidCode);
                }
                Answer::Sent => self.sending = false,
            }
        }
        let mut asked = None;
        let mut main = ui.window("main", "Chaveiro preview", |ui| {
            ui.stack("window", Orientation::Vertical, 0.0, |ui| {
                ui.title_bar("title bar", "Chaveiro", true, |_| Ok(()))?;
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
                self.answer_code(code.as_str() == "123456" || code.as_str() == "cccc");
            }
            Some(UserAction::SubmitNewDeviceOtp { code }) => {
                eprintln!("submitted new device code");
                self.answer_code(code.as_str() == "123456");
            }
            Some(UserAction::SendEmailCode) => {
                eprintln!("send email");
                self.sending = true;
                self.answer_later(Answer::Sent);
            }
            _ => {}
        }
        // Declared after the actions, so an answer asked for in this pass
        // starts its wait now. Keyed by the deadline, so each answer waits
        // in its own task.
        if let Some((due, _)) = &self.answer {
            let due = *due;
            ui.task(("answer", due), move || winui_executor::sleep_until(due));
        }
        Ok(())
    }
}

impl Preview {
    fn answer_code(&mut self, accepted: bool) {
        if accepted {
            eprintln!("accepted");
        }
        self.busy = true;
        self.error = None;
        self.answer_later(Answer::Code { accepted });
    }

    /// The fake server answers with `answer` a second from now.
    fn answer_later(&mut self, answer: Answer) {
        self.answer = Some((Instant::now() + Duration::from_secs(1), answer));
    }
}

fn login_error(error: &LoginError) -> String {
    format!("{error:?}")
}
