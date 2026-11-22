//! A flow that runs a 3 s job with a Cancel button, written as one `async`
//! block that loops: it shows Start, then the countdown, racing the job
//! against Cancel, then the result until Again is clicked. The app keeps no
//! flags for the steps; their order lives in the loop.
//!
//! Widgets declared before and after the flow sit around it in the same
//! stack, and the button after it keeps working while the flow runs.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use futures::{
    StreamExt,
    future::{Either, select},
    pin_mut,
};
use std::time::{Duration, Instant};
use winui_immediate::{App, Orientation, Ui, owned, run};

fn main() -> windows_core::Result<()> {
    run(|| {
        Ok(Example {
            open: true,
            done: 0,
            clicks: 0,
        })
    })
}

struct Example {
    open: bool,
    /// Jobs that ran to the end; the flow writes it through the lent value.
    done: u32,
    /// Clicks on the button after the flow.
    clicks: u32,
}

impl App for Example {
    fn view(&mut self, ui: &mut Ui) -> windows_core::Result<()> {
        if !self.open {
            return Ok(());
        }
        let window = ui.window("main", "winui-immediate flow", |ui| {
            ui.stack("content", Orientation::Vertical, 8.0, |ui| {
                ui.label("done", format_args!("{} jobs done", self.done))?;
                ui.flow("jobs", owned(&mut self.done), |flow| async move {
                    loop {
                        flow.frame(|ui, _| Ok(ui.button("start", "Start")?.clicked()?.then_some(())))
                            .await;
                        let started = Instant::now();
                        let job = winui_executor::sleep(Duration::from_secs(3));
                        // The countdown frame never completes on its own; it
                        // redraws on each pass and reads the Cancel button.
                        let watch = flow.frame(move |ui, _| {
                            let left = Duration::from_secs(3).saturating_sub(started.elapsed());
                            ui.label("left", format_args!("Working… {:.1} s", left.as_secs_f32()))?;
                            Ok(ui.button("cancel", "Cancel")?.clicked()?.then_some(()))
                        });
                        // A tick makes passes while the job runs, so the
                        // countdown moves.
                        let tick = async {
                            let mut ticks = winui_executor::interval(Duration::from_millis(100));
                            while ticks.next().await.is_some() {}
                        };
                        // `futures::future::select` takes futures that are
                        // `Unpin`, so they are pinned on the stack first.
                        pin_mut!(job, tick);
                        let cancelled = match select(job, select(watch, tick)).await {
                            Either::Left(_) => false,
                            Either::Right(_) => true,
                        };
                        // The result frame counts a finished job on its first
                        // draw, so no frame is spent on the count alone.
                        let mut counted = cancelled;
                        flow.frame(move |ui, done| {
                            if !counted {
                                *done.get_mut() += 1;
                                counted = true;
                            }
                            let result = if cancelled { "Cancelled" } else { "Done" };
                            ui.label("result", format_args!("{result}"))?;
                            Ok(ui.button("again", "Again")?.clicked()?.then_some(()))
                        })
                        .await;
                    }
                })?;
                // After the flow in the same stack: its controls sit below
                // whatever step the flow shows, and it reads its own clicks.
                if ui.button("after", "Click me")?.clicked()? {
                    self.clicks += 1;
                }
                ui.label("clicks", format_args!("{} clicks after the flow", self.clicks))?;
                Ok(())
            })?;
            Ok(())
        })?;
        if window.close_requested() {
            self.open = false;
        }
        Ok(())
    }
}
