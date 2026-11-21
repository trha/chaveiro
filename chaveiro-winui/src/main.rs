//! Chaveiro for Windows: a winui-immediate [`App`] over the vault-core
//! [`Core`], with [`Io`] running the core's effects.
//!
//! Only the login screen is built. The other screens show a placeholder
//! with the one action that leads back to login.

#![windows_subsystem = "windows"]

mod bindings;
mod io;
mod login;
mod wipe;

use io::{Answer, Io, Settings, SettingsError, TaskId};
use login::{LoginForm, LoginState, Submit};
use rand_core::OsRng;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vault_core::core::{Config, Core, Input, LoginError, Screen, UserAction};
use windows_core::Result;
use winui_immediate::{App, Handle, Orientation, Ui, run};

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
    if let Err(error) = &result {
        tracing::error!(%error, "Chaveiro stopped");
    }
    result
}

/// A message to the UI thread.
pub enum Message {
    /// The answer of a task of [`Io`]. `task` is [`None`] for file work,
    /// which [`Io`] does not track per job.
    Io {
        task: Option<TaskId>,
        answer: Answer,
    },
}

/// The app.
///
/// Contract: `pending` is [`Some`] only while the login screen is shown, and
/// it is sent, or dropped, as soon as the core has booted and shows the
/// login form.
struct Chaveiro {
    open: bool,
    io: Io,
    /// Made once the settings load, because it needs their device id.
    core: Option<Core>,
    /// Why the settings file did not load. While set, the file is never
    /// written, so a share that was offline for a moment keeps its settings.
    settings_error: Option<String>,
    login: LoginForm,
    /// A login asked for before the core booted.
    pending: Option<Submit>,
    /// The last pass showed the core's login screen.
    showed_login: bool,
}

/// What the user asked for in a pass, applied after the screen's borrow
/// ends.
enum Next {
    Submit(Submit),
    User(UserAction),
}

/// Lock after this long with no user action.
const LOCK_AFTER: Duration = Duration::from_secs(15 * 60);

impl App for Chaveiro {
    type Message = Message;

    fn update(&mut self, message: Message) {
        match message {
            Message::Io { task, answer } => {
                if let Some(task) = task {
                    self.io.finish(task);
                }
                match answer {
                    Answer::Core(input) => self.handle_input(input),
                    Answer::Settings(result) => self.settings_loaded(result),
                    Answer::None => {}
                }
            }
        }
    }

    fn view(&mut self, ui: &mut Ui) -> Result<()> {
        if !self.open {
            return Ok(());
        }
        let mut next = None;
        let mut main = ui.window("main", "Chaveiro", |ui| {
            next = self.body(ui)?;
            Ok(())
        })?;
        main.mica()?.compact()?.fit_to_content()?;
        if self.io.window().is_none() {
            let id = main.id()?;
            self.io.set_window(id);
        }
        if main.close_requested() {
            self.open = false;
            self.io.close();
            return Ok(());
        }
        match next {
            None => {}
            Some(Next::Submit(submit)) => {
                self.pending = Some(submit);
                self.advance();
            }
            Some(Next::User(action)) => self.handle_input(Input::User(action)),
        }
        Ok(())
    }
}

impl Chaveiro {
    /// Makes the effect runner and starts reading the settings; the core
    /// comes once they load.
    fn new(handle: Handle<Message>) -> Result<Self> {
        let mut io = Io::new(handle)?;
        io.load_settings();
        Ok(Chaveiro {
            open: true,
            io,
            core: None,
            settings_error: None,
            login: LoginForm::new(),
            pending: None,
            showed_login: false,
        })
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
        let io = &mut self.io;
        for effect in core.handle(now_ms, &mut OsRng, input) {
            io.run(effect);
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

/// A screen this app does not draw yet, with the action that leads back to
/// login.
fn placeholder(ui: &mut Ui, screen: &Screen<'_>) -> Result<Option<Next>> {
    let (name, button, action) = match screen {
        Screen::SecondFactor { .. } => ("Two-step login", "Cancel", UserAction::Cancel),
        Screen::Locked { .. } => ("Unlock", "Log out", UserAction::Logout),
        _ => ("The vault", "Lock", UserAction::Lock),
    };
    let mut next = None;
    form(ui, "placeholder", |ui| {
        ui.label("title", format_args!("{name}"))?
            .style("TitleTextBlockStyle")?;
        ui.label("note", format_args!("This screen is not built yet."))?;
        if ui.button("action", button)?.clicked()? {
            next = Some(Next::User(action));
        }
        Ok(())
    })?;
    Ok(next)
}

/// The window's one control: the title bar row with the app name, over a
/// vertical stack of the controls `body` declares, `space-2` apart. The
/// window fits itself to this, so it is as wide as the login card.
pub fn form(ui: &mut Ui, id: &'static str, body: impl FnOnce(&mut Ui) -> Result<()>) -> Result<()> {
    ui.stack("window", Orientation::Vertical, 0.0, |ui| {
        // The system keeps the top 32px as the drag area and draws the close
        // button there.
        ui.stack("title bar", Orientation::Horizontal, 0.0, |ui| {
            ui.label("name", format_args!("Chaveiro"))?
                .style("CaptionTextBlockStyle")?;
            Ok(())
        })?
        .padding([16.0, 8.0, 16.0, 8.0])?;
        ui.stack(id, Orientation::Vertical, 8.0, body)?
            .padding([24.0, 8.0, 24.0, 24.0])?;
        Ok(())
    })?
    .width(FORM_WIDTH)?;
    Ok(())
}

/// The login card's width, from the LoginScreen design.
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
