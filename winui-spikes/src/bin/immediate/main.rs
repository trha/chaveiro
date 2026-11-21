//! Spike: an immediate-mode UI over the WinUI retained tree.
//!
//! `SPIKE_BENCH=idle|one|all|scroll` runs 300 builds back to back and prints
//! per-build time, allocations and WinUI calls, then exits.
//! `SPIKE_TICK=<ms>` rebuilds on a timer while you type, to check that caret,
//! selection and IME survive rebuilds.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]

mod ime;
mod ui;
mod winui;

use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
use ui::{Inbox, Scheduler, Ui};
use windows_core::Result;

struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

unsafe impl std::alloc::GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: std::alloc::Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { std::alloc::System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: std::alloc::Layout) {
        // SAFETY: same contract as the caller's.
        unsafe { std::alloc::System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: std::alloc::Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: same contract as the caller's.
        unsafe { std::alloc::System.realloc(pointer, layout, size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy, PartialEq)]
enum Bench {
    Idle,
    One,
    All,
    Scroll,
}

struct Entry {
    id: u64,
    name: String,
    password: String,
}

struct App {
    entries: Vec<Entry>,
    /// Indexes into `entries` that match `filter`. Rebuilt only when the
    /// filter changes.
    visible: Vec<usize>,
    filter: String,
    revealed: Option<u64>,
    unlocked: bool,
    /// Total characters typed, shown so a rebuild is visible while typing.
    edits: usize,
    list_mode: ListMode,
    /// Changes whenever `visible` changes, so the repeater resets its rows.
    revision: u64,
}

/// `SPIKE_LIST=canvas|pool|repeater` picks how the entry list is built.
#[derive(Clone, Copy, PartialEq)]
enum ListMode {
    Canvas,
    Pool,
    Repeater,
}

impl App {
    fn refilter(&mut self) {
        self.revision += 1;
        self.visible.clear();
        let filter = &self.filter;
        self.visible.extend(
            (0..self.entries.len())
                .filter(|&index| self.entries[index].name.contains(filter.as_str())),
        );
    }
}

/// The whole UI, written as straight-line code over `&mut App`.
fn view(ui: &mut Ui, app: &mut App) -> Result<()> {
    ui.stack("root", false, 8.0, |ui| {
        ui.label(
            "status",
            format_args!(
                "{} of {} entries, {} edits",
                app.visible.len(),
                app.entries.len(),
                app.edits
            ),
        )?;
        ui.stack("unlock", true, 8.0, |ui| {
            let password = ui.password_box("password", "Master password")?;
            if ui.button("submit", if app.unlocked { "Lock" } else { "Unlock" })? {
                let typed = ui.take_password(password)?;
                app.unlocked = !app.unlocked && !typed.is_empty();
            }
            Ok(())
        })?;
        if ui.text_box("filter", "Filter", &mut app.filter)? {
            app.edits += 1;
            app.refilter();
        }
        let visible = &app.visible;
        let entries = &app.entries;
        let mut revealed = app.revealed;
        let row = |ui: &mut Ui, row: usize| entry_row(ui, &entries[visible[row]], &mut revealed);
        if app.list_mode == ListMode::Repeater {
            ui.repeater_list("entries", 600.0, visible.len(), app.revision, row)?;
        } else {
            ui.list(
                "entries",
                600.0,
                visible.len(),
                32.0,
                |row| entries[visible[row]].id,
                row,
            )?;
        }
        app.revealed = revealed;
        Ok(())
    })
}

/// One entry row. Toggles `revealed` when its Reveal button was clicked.
fn entry_row(ui: &mut Ui, entry: &Entry, revealed: &mut Option<u64>) -> Result<()> {
    ui.label("name", format_args!("{}", entry.name))?;
    let shown = if *revealed == Some(entry.id) {
        entry.password.as_str()
    } else {
        "••••••"
    };
    ui.label("password", format_args!("{shown}"))?;
    if ui.button("reveal", "Reveal")? {
        *revealed = if *revealed == Some(entry.id) {
            None
        } else {
            Some(entry.id)
        };
    }
    Ok(())
}

struct Runner {
    ui: RefCell<Ui>,
    app: RefCell<App>,
    window: winui::Window,
    scheduler: Rc<Scheduler>,
    build: RefCell<Option<Rc<dyn Fn()>>>,
    bench: Option<Bench>,
    /// `SPIKE_LOG=1`: print one line per build.
    log: bool,
    samples: RefCell<Vec<(f64, usize, ui::Stats)>>,
}

thread_local! {
    static RUNNER: RefCell<Option<Rc<Runner>>> = const { RefCell::new(None) };
}

const BENCH_BUILDS: usize = 300;

impl Runner {
    fn request(&self) {
        let build = self.build.borrow().clone().expect("set at launch");
        self.scheduler.request(&build);
    }

    fn build_once(&self) {
        let mut ui = self.ui.borrow_mut();
        let mut app = self.app.borrow_mut();
        if let Some(bench) = self.bench {
            let tick = self.samples.borrow().len();
            match bench {
                Bench::Idle => {}
                Bench::One => {
                    app.entries[tick % 20].name =
                        format!("entry {} rev {tick}", app.entries[tick % 20].id)
                }
                Bench::All => {
                    for entry in &mut app.entries {
                        entry.name.clear();
                        std::fmt::Write::write_fmt(
                            &mut entry.name,
                            format_args!("entry {} rev {tick}", entry.id),
                        )
                        .expect("writing to a String cannot fail");
                    }
                }
                Bench::Scroll => ui.scroll_override = Some(tick as f64 * 32.0),
            }
        }
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let started = Instant::now();
        let consumed = ui
            .run(&self.window, |ui| view(ui, &mut app))
            .expect("build failed");
        let first_pass = ui.stats;
        if consumed {
            // A second pass shows state changed by events read late in the first.
            ui.run(&self.window, |ui| view(ui, &mut app))
                .expect("build failed");
            ui.stats.created += first_pass.created;
            ui.stats.dropped += first_pass.dropped;
            ui.stats.calls += first_pass.calls;
        }
        let elapsed = started.elapsed().as_secs_f64() * 1e6;
        let allocations = ALLOCATIONS.load(Ordering::Relaxed) - allocations;
        if self.log {
            let stats = ui.stats;
            eprintln!(
                "build {elapsed:.0} us, passes {}, {allocations} allocations, {} widgets, {} created, {} dropped, {} calls, scroll {:?}",
                1 + usize::from(consumed),
                stats.widgets,
                stats.created,
                stats.dropped,
                stats.calls,
                ui.last_scroll
            );
        }
        let stats = ui.stats;
        let sources = ui.take_pending_sources();
        drop(ui);
        drop(app);
        // WinUI may call the row factory inside SetItemsSource, which borrows
        // `ui` and `app` again, so both are released first.
        for (repeater, list, row_count) in sources {
            let source = ui::index_source(row_count).expect("index source");
            repeater
                .SetItemsSource(
                    &windows_core::Interface::cast::<windows_core::IInspectable>(&source)
                        .expect("vectors are inspectable"),
                )
                .expect("set items source");
            self.ui.borrow_mut().set_source(list, source);
        }
        let Some(bench) = self.bench else { return };
        let mut samples = self.samples.borrow_mut();
        samples.push((elapsed, allocations, stats));
        if samples.len() < BENCH_BUILDS {
            drop(samples);
            self.request();
            return;
        }
        report(bench, &samples);
        std::process::exit(0);
    }

    /// `IElementFactory::GetElement`: builds row `index` of `list` now, so
    /// WinUI gets a filled row in the same layout pass.
    fn realize(&self, list: ui::Key, index: usize) -> Result<winui::UIElement> {
        let (Ok(mut ui), Ok(app)) = (self.ui.try_borrow_mut(), self.app.try_borrow()) else {
            panic!("the repeater asked for a row during a build");
        };
        let created = ui.stats.created;
        let started = Instant::now();
        let mut revealed = app.revealed;
        let element = ui.realize(list, index, |ui, row| {
            entry_row(ui, &app.entries[app.visible[row]], &mut revealed)
        })?;
        if self.log {
            eprintln!(
                "realize {:.0} us, index {index}, {} created",
                started.elapsed().as_secs_f64() * 1e6,
                ui.stats.created - created
            );
        }
        Ok(element)
    }
}

fn report(bench: Bench, samples: &[(f64, usize, ui::Stats)]) {
    let name = match bench {
        Bench::Idle => "idle",
        Bench::One => "one",
        Bench::All => "all",
        Bench::Scroll => "scroll",
    };
    let first = samples[0];
    eprintln!(
        "first build: {:.0} us, {} allocations, {} widgets, {} created, {} calls",
        first.0, first.1, first.2.widgets, first.2.created, first.2.calls
    );
    // Skip the first build (mount) and the second (layout settles the viewport).
    let steady = &samples[2..];
    let mut times: Vec<f64> = steady.iter().map(|sample| sample.0).collect();
    times.sort_by(f64::total_cmp);
    let percentile = |p: f64| times[((times.len() - 1) as f64 * p) as usize];
    let mean = |pick: fn(&(f64, usize, ui::Stats)) -> usize| {
        steady.iter().map(pick).sum::<usize>() as f64 / steady.len() as f64
    };
    eprintln!(
        "{name}: build p50 {:.0} us, p90 {:.0} us, max {:.0} us; per build: {:.1} allocations, {:.1} widgets, {:.1} created, {:.1} dropped, {:.1} WinUI calls",
        percentile(0.5),
        percentile(0.9),
        times[times.len() - 1],
        mean(|sample| sample.1),
        mean(|sample| sample.2.widgets),
        mean(|sample| sample.2.created),
        mean(|sample| sample.2.dropped),
        mean(|sample| sample.2.calls),
    );
}

fn main() {
    let bench = match std::env::var("SPIKE_BENCH").as_deref() {
        Ok("idle") => Some(Bench::Idle),
        Ok("one") => Some(Bench::One),
        Ok("all") => Some(Bench::All),
        Ok("scroll") => Some(Bench::Scroll),
        _ => None,
    };
    let tick_ms: Option<u64> = std::env::var("SPIKE_TICK")
        .ok()
        .and_then(|text| text.parse().ok());
    winui::run(move || {
        let window = winui::Window::new()?;
        window.SetTitle("Immediate spike")?;
        let inbox = Rc::new(Inbox::default());
        let mut app = App {
            entries: (1..=1000)
                .map(|id| Entry {
                    id,
                    name: format!("entry {id}"),
                    password: format!("password {id}"),
                })
                .collect(),
            visible: Vec::new(),
            filter: String::new(),
            revealed: None,
            unlocked: false,
            edits: 0,
            list_mode: match std::env::var("SPIKE_LIST").as_deref() {
                Ok("pool") => ListMode::Pool,
                Ok("repeater") => ListMode::Repeater,
                _ => ListMode::Canvas,
            },
            revision: 0,
        };
        app.refilter();
        let mut ui = Ui::new(Rc::clone(&inbox));
        ui.pool_rows = app.list_mode == ListMode::Pool;
        let runner = Rc::new(Runner {
            ui: RefCell::new(ui),
            app: RefCell::new(app),
            window: window.clone(),
            scheduler: Scheduler::new()?,
            build: RefCell::new(None),
            bench,
            log: std::env::var("SPIKE_LOG").is_ok(),
            samples: RefCell::new(Vec::new()),
        });
        let weak = Rc::downgrade(&runner);
        let build: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(runner) = weak.upgrade() {
                runner.build_once();
            }
        });
        *runner.build.borrow_mut() = Some(build);
        let wake_runner = Rc::downgrade(&runner);
        ui::connect(
            &inbox,
            Rc::new(move || {
                if let Some(runner) = wake_runner.upgrade() {
                    runner.request();
                }
            }),
        );
        RUNNER.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&runner)));
        let realize_runner = Rc::downgrade(&runner);
        let recycle_runner = Rc::downgrade(&runner);
        ui::REPEATER_HOOKS.with(|hooks| {
            *hooks.borrow_mut() = Some(ui::RepeaterHooks {
                realize: Rc::new(move |list, index| {
                    realize_runner
                        .upgrade()
                        .expect("runner outlives the window")
                        .realize(list, index)
                }),
                recycle: Rc::new(move |list, element| {
                    let runner = recycle_runner
                        .upgrade()
                        .expect("runner outlives the window");
                    runner
                        .ui
                        .try_borrow_mut()
                        .expect("the repeater recycled a row during a build")
                        .recycle_row(list, element);
                    Ok(())
                }),
            });
        });
        runner.build_once();
        window.Activate()?;
        if std::env::var("SPIKE_IME").as_deref() == Ok("vi") {
            ime::activate_vietnamese_telex()?;
        }
        if let Some(tick_ms) = tick_ms {
            let dispatcher = winui::DispatcherQueue::GetForCurrentThread()?;
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(tick_ms));
                    // The handler captures nothing, so it is safe to create here
                    // and run on the UI thread.
                    let handler = winui::DispatcherQueueHandler::new(|| {
                        RUNNER.with(|slot| {
                            if let Some(runner) = slot.borrow().as_ref() {
                                runner.request();
                            }
                        });
                    });
                    if !matches!(dispatcher.TryEnqueue(&handler), Ok(true)) {
                        break;
                    }
                }
            });
        }
        if bench.is_some() {
            runner.request();
        }
        Ok(())
    })
    .expect("WinUI failed to start");
}
