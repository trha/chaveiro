//! Chaveiro for Windows: a winui-immediate [`App`] over the chaveiro-core
//! [`Core`], with [`Io`] running the core's effects.
//!
//! The login and two-step screens are built, and the vault window has its
//! navigation pane. The unlock screen shows a placeholder with the one
//! action that leads back to login.

// The program is Windows only. On other targets it is empty, so it checks
// but does not link.
#![cfg_attr(not(windows), no_main)]
#![cfg(windows)]
#![windows_subsystem = "windows"]

mod bindings;
mod conflict;
mod copy;
mod edit;
mod history;
mod io;
mod login;
mod route;
mod screen;
mod second_factor;
mod vault;
mod wipe;

use chaveiro_core::{
    core::{Config, Core, Effect, Input, LoginError, OpenItem, Screen, UserAction},
    vault::VaultIndex,
};
use copy::Copies;
use io::{Answer, Io, Settings, SettingsError, clipboard};
use login::{LoginForm, LoginState, Submit};
use rand_core::OsRng;
use route::Route;
use screen::{form, title};
use second_factor::{SecondFactorForm, SecondFactorState};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vault::{VaultAction, VaultApp, VaultForm};
use windows_core::Result;
use winui_immediate::{App, Orientation, Slide, Ui, run};

/// Logs go to stderr, filtered by `CHAVEIRO_LOG` (such as `debug` or
/// `winui_immediate=trace`), warnings and up by default. The app has no
/// console, so redirect stderr to read them.
fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CHAVEIRO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    let result = run(Chaveiro::new);
    // The app dropped its tasks with the tree, which cancelled their WinRT
    // work; wait a moment for it to end, so none outlives the process
    // unnoticed.
    let left = winui_executor::shutdown(SHUTDOWN_LIMIT);
    if left > 0 {
        tracing::warn!(left, "async work still ran at exit");
    }
    if let Err(error) = &result {
        tracing::error!(%error, "Chaveiro stopped");
    }
    result
}


/// The app.
///
/// Contract: `pending` is [`Some`] only while the login screen is shown, and
/// it is sent, or dropped, as soon as the core has booted and shows the
/// login form.
struct Chaveiro {
    open: bool,
    io: Io,
    /// The clipboard copies, which the vault's status runs as a flow.
    copies: Copies,
    /// Made once the settings load, because it needs their device id.
    core: Option<Core>,
    /// Why the settings file did not load. While set, the file is never
    /// written, so a share that was offline for a moment keeps its settings.
    settings_error: Option<String>,
    login: LoginForm,
    /// A login asked for before the core booted.
    pending: Option<Submit>,
    /// The last pass showed the core's login screen, so the prefill runs only
    /// when the login screen first appears.
    showed_login: bool,
    second_factor: SecondFactorForm,
    /// The screen the last pass showed.
    route: Route,
    /// How the last change of `route` moved.
    slide: Slide,
    vault: VaultForm,

}

/// What the user asked for in a pass, applied after the screen's borrow
/// ends.
enum Next {
    Submit(Submit),
    User(UserAction),
    /// The title bar's back button.
    Back,
    /// The window's close button.
    Close,
}

/// How often the core gets [`Input::Tick`]. Its timeouts are minutes, so a
/// lock or a check lands at most this late. The core reads the clock for
/// each, so a tick after sleep catches up at once.
const TICK_EVERY: Duration = Duration::from_secs(30);

/// How long exit waits for cancelled async work to end.
const SHUTDOWN_LIMIT: Duration = Duration::from_secs(2);

/// Lock after this long with no user action.
const LOCK_AFTER: Duration = Duration::from_secs(15 * 60);

impl App for Chaveiro {
    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        // Ticked before the route is read, so this pass shows the tick's
        // result. One interval for the app's life, from the core's start;
        // ticks that came together count once, because the core reads the
        // clock for each.
        if self.core.is_some() {
            let mut ticks = ui.stream("tick", || winui_executor::interval(TICK_EVERY));
            let mut ticked = false;
            while ticks.next().is_some() {
                ticked = true;
            }
            if ticked {
                tracing::debug!("tick");
                self.handle_input(Input::Tick);
            }
        }

        // Effects run as tasks at the root, so a route change does not end
        // them. Answers are applied after the loop, because applying one
        // may add effects.
        let mut answers = Vec::new();
        for (task, work) in self.io.pending() {
            let answer = ui
                .task(("io", *task), || {
                    work.take().expect("a pending effect starts on its first pass")
                })
                .take();
            if let Some(answer) = answer {
                tracing::debug!(?task, "effect answered");
                answers.push((*task, answer));
            }
        }
        for (task, answer) in answers {
            self.io.answered(task);
            self.answer(answer);
        }
        // File work answers in push order. The stream lives as long as the
        // view runs, like the pending effects.
        let mut stored = Vec::new();
        let io = &mut self.io;
        let mut queue = ui.stream("storage", || io.stored_answers());
        while let Some(answer) = queue.next() {
            stored.push(answer);
        }
        for answer in stored {
            self.answer(answer);
        }

        let route = Route::of(self.core.as_ref().map(Core::screen).as_ref(), |challenge| {
            self.second_factor.choosing(challenge)
        });
        if route != self.route {
            // Each visit to the second factor starts with an empty form,
            // which its code screen and method list share.
            if self.route.has_back() && !route.has_back() {
                self.second_factor = SecondFactorForm::default();
            }
            // The vault starts on All items each time it opens.
            if route == Route::Vault {
                self.vault.reset();
            }
            // Leaving the vault means it locked or logged out.
            if self.route == Route::Vault {
                self.copies.clear();
            }
            self.slide = self.route.slide_to(route);
            self.route = route;
        }
        let next = match route {
            Route::Vault => {
                let Some(Screen::Vault(screen)) = self.core.as_ref().map(Core::screen) else {
                    unreachable!("Route::Vault comes from Screen::Vault")
                };
                let open = screen.open.map(|item| &item.view);
                let conflict = screen.open.and_then(OpenItem::conflict);
                let (copies, window) = (&mut self.copies, self.io.window());
                let pass = vault::view::<Self>(ui, &mut self.vault, &screen, open, conflict, |ui| {
                    copies.status(ui, window)
                })?;
                self.io.set_window(pass.window);
                pass.action.and_then(|action| match action {
                    VaultAction::Close => Some(Next::Close),
                    VaultAction::User(action) => Some(Next::User(action)),
                    VaultAction::OpenWebsite(url) => {
                        io::open_website(&url);
                        None
                    }
                    VaultAction::Copy(text) => {
                        self.copies.start(text);
                        None
                    }
                    VaultAction::ReadKey(source) => {
                        self.vault
                            .key_read(source, io::key_file::read(source, self.io.window()));
                        None
                    }
                })
            }
            Route::Login | Route::Methods | Route::SecondFactor | Route::Locked => {
                let mut next = None;
                let mut main = ui.window("main", "Chaveiro", |ui| {
                    ui.stack("window", Orientation::Vertical, 0.0, |ui| {
                        let mut title_bar =
                            ui.title_bar("title bar", "Chaveiro", route.has_back(), |_| Ok(()))?;
                        if title_bar.back_requested()? && route.has_back() {
                            next = Some(Next::Back);
                        }
                        ui.navigator("screens", route, self.slide, |ui| {
                            if let Some(asked) = self.body(ui)? {
                                next = Some(asked);
                            }
                            Ok(())
                        })?;
                        Ok(())
                    })?
                    .width(FORM_WIDTH)?;
                    Ok(())
                })?;
                main.mica()?.compact()?.fit_to_content()?;
                self.io.set_window(main.id()?);
                match main.close_requested() {
                    true => Some(Next::Close),
                    false => next,
                }
            }
        };
        match next {
            None => {}
            Some(Next::Close) => {
                self.open = false;
                self.copies.clear();
                self.io.close();
            }
            Some(Next::Submit(submit)) => {
                self.pending = Some(submit);
                self.advance();
            }
            Some(Next::User(action)) => self.handle_input(Input::User(action)),
            Some(Next::Back) => {
                // The code screen goes back to the method list when there
                // is one; otherwise back leaves the login.
                let to_list = match self.core.as_ref().map(Core::screen) {
                    Some(Screen::SecondFactor { challenge, .. }) => {
                        self.second_factor.back(challenge)
                    }
                    _ => false,
                };
                if !to_list {
                    self.handle_input(Input::User(UserAction::Cancel));
                }
            }
        }
        // An effect added after the tasks were declared starts next pass.
        if self.io.unstarted() {
            ui.request_pass();
        }
        Ok(())
    }
}

impl VaultApp for Chaveiro {
    fn vault(&mut self) -> Option<(&mut VaultForm, &VaultIndex)> {
        match self.core.as_ref().map(Core::screen) {
            Some(Screen::Vault(screen)) => Some((&mut self.vault, screen.index)),
            _ => None,
        }
    }
}

impl Chaveiro {
    /// Makes the effect runner and starts reading the settings; the core
    /// comes once they load.
    fn new() -> Result<Self> {
        let mut io = Io::new()?;
        io.load_settings();
        Ok(Chaveiro {
            open: true,
            io,
            copies: Copies::default(),
            core: None,
            settings_error: None,
            login: LoginForm::new(),
            pending: None,
            showed_login: false,
            second_factor: SecondFactorForm::default(),
            route: Route::Login,
            slide: Slide::Instant,
            vault: VaultForm::default(),

        })
    }

    /// Applies the answer of an effect.
    fn answer(&mut self, answer: Answer) {
        match answer {
            Answer::Core(input) => self.handle_input(input),
            Answer::Settings(result) => self.settings_loaded(result),
            Answer::None => {}
        }
    }

    /// Takes the loaded settings, or fresh ones and a warning, then makes
    /// the core and boots the stored account.
    fn settings_loaded(&mut self, result: std::result::Result<Option<Settings>, SettingsError>) {
        let settings = match result {
            Ok(Some(settings)) => settings,
            // A first start: the new device id is a changed setting.
            Ok(None) => {
                let settings = Settings::fresh();
                self.io.save_settings(&settings);
                settings
            }
            Err(error) => {
                self.settings_error = Some(format!(
                    "Chaveiro could not read its settings file, so it started with defaults: {error}"
                ));
                Settings::fresh()
            }
        };
        self.core = Some(Core::new(Config {
            client_id: "desktop".to_owned(),
            client_name: "desktop".to_owned(),
            client_version: "2025.1.0".to_owned(),
            device_type: 6,
            device_id: settings.device_id,
            device_name: "windows".to_owned(),
            lock_after: LOCK_AFTER.as_millis() as u64,
        }));
        self.io.boot();

    }

    /// Sends `pending` once the core has booted. A stored account that
    /// boots locked drops it, because the typed password is then not a
    /// login; the unlock screen asks again.
    fn advance(&mut self) {
        let Some(core) = &self.core else {
            return;
        };
        match (core.screen(), &self.pending) {
            (_, None) | (Screen::Starting, _) => {}
            (Screen::Login { busy: false, .. }, Some(_)) => {
                let submit = self.pending.take().expect("matched above");
                self.handle_input(Input::User(UserAction::Login {
                    server: submit.server,
                    email: submit.email,
                    password: submit.password,
                }));
            }
            (_, Some(_)) => self.pending = None,
        }
    }

    /// Feeds one input to the core and starts its effects.
    fn handle_input(&mut self, input: Input) {
        let Some(core) = &mut self.core else {
            return;
        };
        let now_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| since.as_millis() as u64);
        for effect in core.handle(now_ms, &mut OsRng, input) {
            match effect {
                // A copy is UI: it shows a countdown, so the app runs it.
                Effect::Copy { text } => self.copies.start(text),
                effect => self.io.run(effect),
            }
        }
        if !matches!(core.screen(), Screen::Vault(_)) {
            wipe::wipe_stack();
        }
        self.advance();
    }

    /// The window's one control for the current screen.
    fn body(&mut self, ui: &mut Ui) -> Result<Option<Next>> {
        let screen = self.core.as_ref().map(Core::screen);
        let (busy, error) = match &screen {
            None | Some(Screen::Starting) => (false, None),
            Some(Screen::Login {
                server,
                busy,
                error,
                email,
            }) => {
                // Fill in the last account once, when the core first shows
                // the form.
                if !self.showed_login {
                    if self.login.email.is_empty() {
                        (*email).clone_into(&mut self.login.email);
                    }
                    if let Some(server) = server {
                        self.login.set_server(server);
                    }
                }
                (*busy, error.map(login_error))
            }
            Some(Screen::SecondFactor {
                email,
                challenge,
                error,
                busy,
                sending,
            }) => {
                self.showed_login = false;
                let state = SecondFactorState {
                    challenge,
                    email,
                    error: *error,
                    busy: *busy,
                    sending: *sending,
                };
                return Ok(second_factor::view(ui, &mut self.second_factor, state)?.map(Next::User));
            }
            Some(other) => {
                self.showed_login = false;
                return placeholder(ui, other);
            }
        };
        self.showed_login = matches!(screen, Some(Screen::Login { .. }));

        let state = LoginState {
            busy: busy || self.pending.is_some(),
            error: error.as_deref(),

            warning: self.settings_error.as_deref(),
        };
        Ok(login::view(ui, &mut self.login, state)?.map(Next::Submit))
    }
}

fn placeholder(ui: &mut Ui, screen: &Screen<'_>) -> Result<Option<Next>> {
    // The vault has its own window, so only the unlock screen lands here.
    let (name, button, action) = match screen {
        Screen::Locked { .. } => ("Unlock", "Log out", UserAction::Logout),
        _ => ("This screen", "Lock", UserAction::Lock),
    };
    let mut next = None;
    form(ui, "placeholder", |ui| {
        title(ui, name)?;
        ui.label("note", format_args!("This screen is not built yet."))?;
        if ui.button("action", button)?.clicked()? {
            next = Some(Next::User(action));
        }
        Ok(())
    })?;
    Ok(next)
}

/// The window's width, from the LoginScreen design. The window fits itself
/// to its content, so this sets its width.
const FORM_WIDTH: f64 = 360.0;

/// The InfoBar sentence for a failed login.
fn login_error(error: &LoginError) -> String {
    match error {
        LoginError::Rejected(message) if message.is_empty() => {
            "Wrong email or master password. Try again.".to_owned()
        }
        LoginError::Rejected(message) => message.clone(),
        LoginError::InvalidCode => "That code is not valid.".to_owned(),
        LoginError::Server { status } => {
            format!("The server answered with error {status}. Try again later.")
        }
        LoginError::Network(message) => format!("Chaveiro could not reach the server: {message}"),
        LoginError::BadResponse => "The server sent an answer Chaveiro does not read.".to_owned(),
        LoginError::Kdf(error) => format!("Key derivation failed: {error:?}"),
        LoginError::Key(error) => format!("The account key did not open: {error}"),
        LoginError::SessionExpired => "The session ended. Log in again.".to_owned(),
        LoginError::ServerUrl => "The server URL must start with http:// or https://.".to_owned(),
    }
}
