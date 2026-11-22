//! The DOM spike app. `spike/index.html` loads it and drives it: clicks,
//! typing, a caret position, and removing the focused input.

#![cfg(target_arch = "wasm32")]

use dom_immediate::{App, Ui, owned, run, sleep};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
fn start() {
    let mount = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id("app"))
        .expect("the page has an #app element");
    run(
        mount,
        Spike {
            clicks: 0,
            name: String::new(),
            editing: true,
            blurs: 0,
            timer: String::from("timer waiting"),
            step: String::from("step none"),
        },
    );
}

struct Spike {
    clicks: u32,
    name: String,
    editing: bool,
    blurs: u32,
    timer: String,
    step: String,
}

impl App for Spike {
    fn view(&mut self, ui: &mut Ui) {
        ui.stack("main", |ui| {
            ui.label("clicks", &format!("clicks: {}", self.clicks));
            if ui.button("add", "Add") {
                self.clicks += 1;
            }
            if self.editing {
                let input = ui.text_input("name", &mut self.name);
                if input.blurred {
                    self.blurs += 1;
                }
            }
            ui.label("echo", &format!("name: {}", self.name));
            // Removes the input while it has focus, so the DOM fires `blur`
            // inside the pass that removes it.
            if ui.button("done", "Done") {
                self.editing = false;
                ui.request_pass();
            }
            ui.label("blurs", &format!("blurs: {}", self.blurs));
            if let Some(text) = ui
                .task("timer", || async {
                    sleep(30).await;
                    "timer fired"
                })
                .take()
            {
                self.timer = String::from(text);
            }
            ui.label("timer-text", &self.timer);
            let mut step = std::mem::take(&mut self.step);
            ui.flow("steps", owned(&mut step), |flow| async move {
                flow.frame(|ui, step| {
                    *step.get_mut() = String::from("step 1");
                    ui.button("next", "Next").then_some(())
                })
                .await;
                flow.frame(|ui, step| {
                    *step.get_mut() = String::from("step 2");
                    ui.label("finished", "flow finished");
                    None::<()>
                })
                .await;
            });
            self.step = step;
            ui.label("step", &self.step);
        });
    }
}
