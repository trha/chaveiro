//! Code boxes in a form, with a pop-up that has a disabled item, a check
//! box in the fields' column, a link and a button. A thread plays a script
//! of edits through the window's field editor, prints the digits after
//! each step, and saves the window as a PDF to the path in the first argument
//! (`code_boxes.pdf` by default) before it quits.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

use appkit_immediate::{App, CodeDigits, Orientation, Ui, run};
use objc2::{MainThreadMarker, rc::Retained, sel};
use objc2_app_kit::{NSApplication, NSTextInputClient, NSTextView};
use objc2_foundation::{NSNotFound, NSRange, NSString};
use std::{thread, time::Duration};

fn main() {
    run(|handle| {
        thread::spawn(move || {
            for step in SCRIPT {
                thread::sleep(Duration::from_millis(300));
                handle.send(*step);
            }
        });
        Boxes {
            open: true,
            digits: CodeDigits::default(),
            method: 0,
            remember: true,
            focused: false,
            error: None,
        }
    });
}

#[derive(Clone, Copy, Debug)]
enum Step {
    Type(&'static str),
    Backspace,
    Left,
    Error,
    Snapshot,
    Quit,
}

const SCRIPT: &[Step] = &[
    Step::Type("4"),
    Step::Type("8"),
    Step::Backspace,
    Step::Backspace,
    Step::Type("x"),
    Step::Type("12 34 56"),
    Step::Left,
    Step::Type("9"),
    Step::Error,
    Step::Snapshot,
    Step::Quit,
];

struct Boxes {
    open: bool,
    digits: CodeDigits,
    method: usize,
    remember: bool,
    focused: bool,
    error: Option<&'static str>,
}

impl App for Boxes {
    type Message = Step;

    fn update(&mut self, step: Step) {
        let mtm = MainThreadMarker::new().expect("update runs on the main thread");
        let application = NSApplication::sharedApplication(mtm);
        let window = application
            .windows()
            .firstObject()
            .expect("the window is open");
        let editor = window
            .firstResponder()
            .and_then(|responder| responder.downcast::<NSTextView>().ok());
        match step {
            Step::Type(text) => {
                let editor = editor.expect("a box is being edited");
                // Typed text has no range to replace.
                let nowhere = NSRange::new(NSNotFound as usize, 0);
                unsafe { editor.insertText_replacementRange(&NSString::from_str(text), nowhere) };
            }
            Step::Backspace => unsafe {
                editor
                    .expect("a box is being edited")
                    .doCommandBySelector(sel!(deleteBackward:))
            },
            Step::Left => unsafe {
                editor
                    .expect("a box is being edited")
                    .doCommandBySelector(sel!(moveLeft:))
            },
            Step::Error => self.error = Some("That code didn’t work. Check it and try again."),
            Step::Snapshot => {
                let view = window.contentView().expect("the window has a content view");
                // A PDF draws every view's text, which a cached bitmap of
                // layer-backed fields leaves out.
                let pdf = view.dataWithPDFInsideRect(view.bounds());
                let path = std::env::args()
                    .nth(1)
                    .unwrap_or_else(|| "code_boxes.pdf".to_owned());
                std::fs::write(&path, pdf.to_vec()).expect("the picture saves");
                println!("saved {path}");
            }
            Step::Quit => self.open = false,
        }
        let shown: String = self
            .digits
            .iter()
            .map(|digit| digit.map_or('_', |digit| char::from(b'0' + digit)))
            .collect();
        let focus = window
            .firstResponder()
            .and_then(|responder| responder.downcast::<NSTextView>().ok())
            .and_then(|editor| editor.delegate())
            .map(|delegate| {
                let delegate: Retained<objc2::runtime::AnyObject> = delegate.into();
                let label: Option<Retained<NSString>> =
                    unsafe { objc2::msg_send![&delegate, accessibilityLabel] };
                label.map(|label| label.to_string()).unwrap_or_default()
            });
        println!("{step:?}: {shown} focus {focus:?}");
    }

    fn view(&mut self, ui: &mut Ui) {
        if ui.should_terminate() {
            self.open = false;
        }
        if !self.open {
            return;
        }
        let mut window = ui.window("main", "code boxes", |ui| {
            ui.stack_view("body", Orientation::Vertical, 16.0, |ui| {
                ui.form("fields", 116.0, |ui| {
                    let items = ["Authenticator app", "Email", "Duo"];
                    ui.pop_up_button("method", &items, &mut self.method)
                        .enabled_items(&[true, true, false])
                        .header("Method");
                    let mut boxes = ui.code_boxes("code", &mut self.digits);
                    if boxes.changed() {
                        self.error = None;
                    }
                    boxes.header("Code").error(self.error);
                    let field = boxes.field();
                    if !self.focused {
                        ui.make_first_responder(field);
                        self.focused = true;
                    }
                    ui.check_box("remember", "Remember this Mac", &mut self.remember)
                        .header("");
                });
                ui.stack_view("buttons", Orientation::Horizontal, 10.0, |ui| {
                    ui.link("resend", "Send the code again");
                    ui.alert_line("error", None);
                    ui.button("continue", "Continue").default_button(true);
                });
            })
            .padding([20.0, 20.0, 20.0, 20.0])
            .width(480.0);
        });
        window.compact().glass().size_to_fit();
        if window.should_close() {
            self.open = false;
        }
    }
}
