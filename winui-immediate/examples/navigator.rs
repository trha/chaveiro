//! A navigator over three screens. Next and Back slide between the first
//! two; Lock jumps from the second to the first with no animation, as a
//! lock must. The first screen's text box shows that a screen's state is
//! dropped when the screen goes. `tests/ui/navigator.ps1` drives it.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use std::{cell::RefCell, rc::Rc};
use winui_immediate::{App, Handle, Orientation, Slide, Ui, run};

fn main() -> windows_core::Result<()> {
    run(|_: Handle<()>| {
        Ok(Navigation {
            screen: Screen::First,
            slide: Slide::Instant,
            open: true,
        })
    })
}

#[derive(Clone, Copy, Hash, PartialEq, Eq)]
enum Screen {
    First,
    Second,
}

struct Navigation {
    screen: Screen,
    /// How the last change of `screen` asked to move.
    slide: Slide,
    open: bool,
}

impl App for Navigation {
    type Message = ();

    fn update(&mut self, (): ()) {}

    fn view(&mut self, ui: &mut Ui) -> windows_core::Result<()> {
        if !self.open {
            return Ok(());
        }
        let mut next = None;
        let mut main = ui.window("main", "winui-immediate navigator", |ui| {
            ui.stack("window", Orientation::Vertical, 0.0, |ui| {
                let back = self.screen == Screen::Second;
                if ui
                    .title_bar("title bar", "Navigator", back)?
                    .back_requested()?
                    && back
                {
                    next = Some((Screen::First, Slide::Back));
                }
                ui.navigator("screens", self.screen, self.slide, |ui| {
                    ui.stack("screen", Orientation::Vertical, 8.0, |ui| {
                        match self.screen {
                            Screen::First => {
                                ui.label("title", format_args!("First screen"))?;
                                // The text lives in this screen's state, so
                                // leaving the screen drops it. navigator.ps1
                                // checks the box is empty after Next and Back.
                                let text = ui
                                    .state("note text", || Rc::new(RefCell::new(String::new())))?
                                    .clone();
                                ui.text_box("note", "Type here", &mut text.borrow_mut())?;
                                if ui.button("next", "Next")?.clicked()? {
                                    next = Some((Screen::Second, Slide::Forward));
                                }
                            }
                            Screen::Second => {
                                ui.label("title", format_args!("Second screen"))?;
                                // A taller screen, so the slide shows the window
                                // growing.
                                for line in 0..6 {
                                    ui.label(("line", line), format_args!("Line {line}"))?;
                                }
                                if ui.button("back", "Back")?.clicked()? {
                                    next = Some((Screen::First, Slide::Back));
                                }
                                if ui.button("lock", "Lock")?.clicked()? {
                                    next = Some((Screen::First, Slide::Instant));
                                }
                            }
                        }
                        Ok(())
                    })?
                    .padding([24.0, 24.0, 24.0, 24.0])?
                    .width(360.0)?;
                    Ok(())
                })?;
                Ok(())
            })?;
            Ok(())
        })?;
        main.compact()?.fit_to_content()?;
        if main.close_requested() {
            self.open = false;
        }
        if let Some((screen, slide)) = next {
            eprintln!("navigate {slide:?}");
            self.screen = screen;
            self.slide = slide;
        }
        Ok(())
    }
}
