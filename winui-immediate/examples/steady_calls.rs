//! Counts the WinUI calls of a pass with no changes, and of a pass that
//! changes one label, over a window of about 20,000 nodes. A timer drives
//! the passes; the app closes its window after the last count and prints
//! both, with the time `view` took in each. Exits with an error when the
//! pass with no changes made any call, or the pass with one change made
//! other than one, leaving out the dispatcher call that schedules each turn.
//!
//! Run it with `cargo run --release --example steady_calls --features
//! count-calls`. `STEADY_ROWS=<n>` sets the row count; each row is 4 nodes.
//! For a profiler, `STEADY_PASS=<n>` runs `n` steady passes before the
//! counted ones, and `STEADY_TICK_MS=<ms>` sets the timer period.
//!
//! At the default 4,800 rows, WinUI's first layout of the unvirtualized
//! stack holds the UI thread for about 95 s, and closing the window takes
//! about 9 s of WinUI teardown; the passes themselves take milliseconds.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

use std::time::Duration;
use windows_core::Result;
use winui_immediate::{App, Length, Orientation, Severity, Ui, calls, owned, run};

fn main() -> Result<()> {
    // `RUST_LOG=winui_immediate=debug` prints each turn's time as its span
    // closes.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE)
        .with_timer(tracing_subscriber::fmt::time::uptime())
        .with_writer(std::io::stderr)
        .init();
    run(|| {
        Ok(Steady {
            open: true,
            steady_pass: setting("STEADY_PASS", STEADY_PASS),
            tick: Duration::from_millis(setting("STEADY_TICK_MS", 50)),
            passes: 0,
            changed: false,
            text: String::from("some text"),
            choice: 1,
            checks: vec![
                false;
                std::env::var("STEADY_ROWS")
                    .ok()
                    .and_then(|rows| rows.parse().ok())
                    .unwrap_or(ROWS)
            ],
        })
    })?;
    for (name, time) in VIEW_TIMES
        .lock()
        .expect("no thread panicked holding it")
        .iter()
    {
        eprintln!("view of the {name}: {:.2} ms", time.as_secs_f64() * 1e3);
    }
    // The timer's wake asks the dispatcher for the next turn with
    // `TryEnqueue`. That is the cost of driving passes by a timer, which a
    // pass after a click or a task pays too, so it does not count against
    // the pass.
    let total = |calls: &[(&str, u64)]| {
        calls
            .iter()
            .filter(|(method, _)| *method != "IDispatcherQueue::TryEnqueue")
            .map(|(_, count)| count)
            .sum::<u64>()
    };
    let report = |name: &str, calls: &[(&str, u64)]| {
        eprintln!("{name}: {} calls", total(calls));
        for (method, count) in calls {
            eprintln!("  {count:>6} {method}");
        }
    };
    let steady = std::mem::take(&mut *STEADY.lock().expect("no thread panicked holding it"));
    let one_change =
        std::mem::take(&mut *ONE_CHANGE.lock().expect("no thread panicked holding it"));
    report("pass with no changes", &steady);
    report("pass that changes one label", &one_change);
    match (total(&steady), total(&one_change)) {
        (0, 1) => Ok(()),
        _ => std::process::exit(1),
    }
}

/// The counts, handed from the app to `main` after `run` returns.
static STEADY: std::sync::Mutex<Vec<(&str, u64)>> = std::sync::Mutex::new(Vec::new());
static ONE_CHANGE: std::sync::Mutex<Vec<(&str, u64)>> = std::sync::Mutex::new(Vec::new());
/// How long `view` took in each counted pass. It leaves out the close of
/// each container, which runs after `view` returns.
static VIEW_TIMES: std::sync::Mutex<Vec<(&str, Duration)>> = std::sync::Mutex::new(Vec::new());

/// Each row is a stack of a label, a check box and a button: 4 nodes.
const ROWS: usize = 4_800;
/// Labels inside one cache, which a pass skips.
const CACHED: usize = 800;

/// The pass that starts counting the pass with no changes. The passes
/// before it let the window open and settle.
const STEADY_PASS: u64 = 10;

/// `name` from the environment, or `default` when it is unset or does not
/// parse.
fn setting<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

struct Steady {
    open: bool,
    /// From `STEADY_PASS`. A profiler raises it to run many steady passes.
    steady_pass: u64,
    /// From `STEADY_TICK_MS`.
    tick: Duration,
    /// Passes in which a tick arrived. A slow pass can take several ticks
    /// at once, so counting passes keeps each count to one pass.
    passes: u64,
    /// The one label differs from the last pass.
    changed: bool,
    text: String,
    choice: usize,
    checks: Vec<bool>,
}

impl App for Steady {
    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        let tick = self.tick;
        let mut ticks = ui.stream("tick", || winui_executor::interval(tick));
        let mut ticked = false;
        while ticks.next().is_some() {
            ticked = true;
        }
        let mut timed = None;
        if ticked {
            self.passes += 1;
            // Each count runs from the top of one ticked pass to the top of
            // the next, so it holds one whole pass and the turn around it.
            let steady = self.steady_pass;
            if self.passes == steady {
                timed = Some("pass with no changes");
                calls::start();
            } else if self.passes == steady + 1 {
                *STEADY.lock().expect("no thread panicked holding it") = calls::take();
                self.changed = true;
                timed = Some("pass that changes one label");
                calls::start();
            } else if self.passes == steady + 2 {
                *ONE_CHANGE.lock().expect("no thread panicked holding it") = calls::take();
                self.open = false;
                return Ok(());
            }
        }
        let start = std::time::Instant::now();
        self.declare(ui)?;
        if let Some(name) = timed {
            VIEW_TIMES
                .lock()
                .expect("no thread panicked holding it")
                .push((name, start.elapsed()));
        }
        Ok(())
    }
}

impl Steady {
    fn declare(&mut self, ui: &mut Ui) -> Result<()> {
        let changed = self.changed;
        ui.window("main", "winui-immediate steady calls", |ui| {
            ui.grid(
                "rows",
                &[Length::Auto, Length::Auto, Length::Star(1.0)],
                |ui| {
                    ui.stack("header", Orientation::Horizontal, 8.0, |ui| {
                        ui.label(
                            "changing",
                            format_args!("{}", if changed { "after" } else { "before" }),
                        )?;
                        ui.text_box("text", "Text", &mut self.text)?;
                        ui.combo_box("choice", ["One", "Two", "Three"], &mut self.choice)?
                            .header("Choice")?;
                        ui.button("accent", "Accent")?.accent(true)?;
                        ui.link("link", "A link")?;
                        ui.font_icon("icon", '\u{E713}')?;
                        ui.icon_button("settings", '\u{E713}', "Settings")?;
                        // A copy button as chaveiro-winui draws it: a flow
                        // around a button whose glyphs morph.
                        let mut clicked = false;
                        ui.flow("copy", owned(&mut clicked), |flow| async move {
                            loop {
                                flow.frame(|ui, clicked| {
                                    let hit = ui
                                        .content_button("button", "Copy", |ui| {
                                            ui.morph("glyphs", 0, |ui| {
                                                ui.font_icon("copy", '\u{E8C8}')?;
                                                ui.font_icon("check mark", '\u{E73E}')?;
                                                Ok(())
                                            })?
                                            .margin([2.0; 4])?;
                                            Ok(())
                                        })?
                                        .clicked()?;
                                    *clicked.get_mut() = hit;
                                    Ok(hit.then_some(()))
                                })
                                .await;
                            }
                        })?;
                        Ok(())
                    })?;
                    ui.info_bar(
                        "info",
                        Severity::Informational,
                        "Info",
                        "Nothing changes here.",
                    )?;
                    ui.cached("rows-cache", (), |ui| {
                        ui.stack("cached", Orientation::Vertical, 2.0, |ui| {
                            for row in 0..CACHED {
                                ui.label(row, format_args!("cached row {row}"))?;
                            }
                            Ok(())
                        })?;
                        Ok(())
                    })?;
                    ui.stack("rows", Orientation::Vertical, 2.0, |ui| {
                        for (row, checked) in self.checks.iter_mut().enumerate() {
                            ui.stack(row, Orientation::Horizontal, 8.0, |ui| {
                                ui.label("name", format_args!("row {row}"))?
                                    .secondary(row % 2 == 0)?;
                                ui.check_box("check", "Check", checked)?;
                                // An app reads its buttons' clicks.
                                ui.button("open", "Open")?.clicked()?;
                                Ok(())
                            })?;
                        }
                        Ok(())
                    })?;
                    Ok(())
                },
            )?;
            Ok(())
        })?;
        Ok(())
    }
}
