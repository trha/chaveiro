//! Chaveiro for macOS: an appkit-immediate [`App`] over the chaveiro-core
//! [`Core`], with [`Io`] running the core's effects.
//!
//! The login and two-step login screens are built, and the vault window has
//! its sidebar, list and details. The unlock screen shows a placeholder with the one action
//! that leads back to login.

// The program is macOS only. On other targets it is empty, so it checks but
// does not link.
#![cfg_attr(not(target_os = "macos"), no_main)]
#![cfg(target_os = "macos")]

mod io;
mod login;
mod second_factor;
mod vault;
mod wipe;

use appkit_immediate::{App, Orientation, Style, Ui, run};
use chaveiro_core::{
    core::{Config, Core, Input, LoginError, Screen, StoredAccount, UserAction},
    vault::VaultIndex,
};
use io::{Io, Settings};
use login::{LoginAction, LoginForm, LoginState};
use rand_core::OsRng;
use second_factor::{SecondFactorAction, SecondFactorForm, SecondFactorState};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use vault::{VaultAction, VaultApp, VaultForm};

/// Logs go to stderr, filtered by `CHAVEIRO_LOG` (such as `debug`),
/// warnings and up by default.
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("CHAVEIRO_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();
    run(Chaveiro::new);
}

struct Chaveiro {
    open: bool,
    io: Io,
    core: Core,
    /// Why the settings file did not load. While set, the file is never
    /// written, so a broken file is kept for the user to look at.
    settings_error: Option<String>,
    login: LoginForm,
    /// The last pass showed the core's login screen.
    showed_login: bool,
    /// The two-step form while its screen shows; a new challenge starts a
    /// new one.
    second_factor: Option<SecondFactorForm>,
    vault: VaultForm,
    /// The last pass showed the vault, so its form is current.
    showed_vault: bool,
}

/// Lock after this long with no user action.
const LOCK_AFTER: Duration = Duration::from_secs(15 * 60);

impl App for Chaveiro {
    fn view(&mut self, ui: &mut Ui) {
        if ui.should_terminate() {
            self.quit();
        }
        if !self.open {
            return;
        }
        // Effects run as tasks at the root, so a screen change does not end
        // them. Answers are applied after the loop, because applying one
        // may add effects, and before the screen is read, so this pass
        // shows them.
        let mut answers = Vec::new();
        for (task, work) in self.io.pending() {
            let answer = ui
                .task(("io", *task), || {
                    work.take()
                        .expect("a pending effect starts on its first pass")
                })
                .take();
            if let Some(answer) = answer {
                tracing::debug!(?task, "effect answered");
                answers.push((*task, answer));
            }
        }
        for (task, answer) in answers {
            self.io.answered(task);
            // A derivation that panicked has no answer; its message is on
            // stderr, and the core keeps waiting, as for a lost request.
            if let Some(input) = answer {
                self.handle_input(input);
            }
        }
        match self.body(ui) {
            None => {}
            Some(Next::Quit) => self.quit(),
            Some(Next::User(action)) => self.handle_input(Input::User(action)),
        }
        // An effect added after the tasks were declared starts next pass.
        if self.io.unstarted() {
            ui.request_pass();
        }
    }
}

impl VaultApp for Chaveiro {
    fn vault(&mut self) -> Option<(&mut VaultForm, &VaultIndex)> {
        match self.core.screen() {
            Screen::Vault(screen) => Some((&mut self.vault, screen.index)),
            _ => None,
        }
    }
}

/// What the user asked for in a pass, applied after the screen's borrow
/// ends.
enum Next {
    User(UserAction),
    Quit,
}

impl Chaveiro {
    /// Reads the settings, makes the core and boots it.
    fn new() -> Self {
        let io = Io::new();
        let (settings, settings_error) = match io.load_settings() {
            Ok(Some(settings)) => (settings, None),
            // A first start: the new device id is a changed setting.
            Ok(None) => {
                let settings = Settings::fresh();
                io.save_settings(&settings);
                (settings, None)
            }
            Err(error) => (
                Settings::fresh(),
                Some(format!(
                    "Chaveiro could not read its settings file, so it started with defaults: {error}"
                )),
            ),
        };
        let core = Core::new(Config {
            client_id: "desktop".to_owned(),
            client_name: "desktop".to_owned(),
            client_version: "2025.1.0".to_owned(),
            device_type: 7,
            device_id: settings.device_id,
            device_name: "macos".to_owned(),
            lock_after: LOCK_AFTER.as_millis() as u64,
        });
        let mut app = Chaveiro {
            open: true,
            io,
            core,
            settings_error,
            login: LoginForm::new(),
            showed_login: false,
            second_factor: None,
            vault: VaultForm::default(),
            showed_vault: false,
        };
        // The store keeps nothing yet, so every start is a first one.
        app.handle_input(Input::Started {
            stored: StoredAccount::default(),
        });
        app
    }

    fn quit(&mut self) {
        self.open = false;
        self.io.close();
    }

    /// Feeds one input to the core and starts its effects.
    fn handle_input(&mut self, input: Input) {
        let now_ms = now_ms();
        for effect in self.core.handle(now_ms, &mut OsRng, input) {
            self.io.run(effect);
        }
        if !matches!(self.core.screen(), Screen::Vault(_)) {
            wipe::wipe_stack();
        }
    }

    /// The window for the current screen.
    fn body(&mut self, ui: &mut Ui) -> Option<Next> {
        let screen = self.core.screen();
        if let Screen::Vault(vault) = &screen {
            // The vault starts on All Items each time it opens.
            if !std::mem::replace(&mut self.showed_vault, true) {
                self.vault.reset();
            }
            self.showed_login = false;
            self.second_factor = None;
            let open = vault.open.map(|item| &item.view);
            return match vault::view::<Self>(ui, &mut self.vault, vault, open, now_ms())? {
                VaultAction::User(action) => Some(Next::User(action)),
                VaultAction::Close => Some(Next::Quit),
            };
        }
        self.showed_vault = false;
        if let Screen::SecondFactor {
            email,
            challenge,
            error,
            busy,
            sending,
        } = &screen
        {
            self.showed_login = false;
            let form = self.second_factor.get_or_insert_default();
            let message = error.map(login_error);
            let state = SecondFactorState {
                challenge,
                email,
                error: message.as_deref(),
                refused: second_factor::refused(*error),
                busy: *busy,
                sending: *sending,
            };
            return match second_factor::view(ui, form, state)? {
                SecondFactorAction::User(action) => Some(Next::User(action)),
                SecondFactorAction::Back => Some(Next::User(UserAction::Cancel)),
            };
        }
        self.second_factor = None;
        let (busy, error) = match &screen {
            Screen::Starting => (false, None),
            Screen::Login {
                server,
                busy,
                error,
                email,
            } => {
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
            other => {
                self.showed_login = false;
                return placeholder(ui, other);
            }
        };
        self.showed_login = matches!(screen, Screen::Login { .. });
        let state = LoginState {
            busy,
            error: error.as_deref(),
            warning: self.settings_error.as_deref(),
        };
        match login::view(ui, &mut self.login, state)? {
            LoginAction::Submit(submit) => Some(Next::User(UserAction::Login {
                server: submit.server,
                email: submit.email,
                password: submit.password,
            })),
            LoginAction::Cancel => Some(Next::User(UserAction::Cancel)),
            LoginAction::Quit => Some(Next::Quit),
        }
    }
}

/// Our clock, in ms since the epoch; 0 for a clock set before 1970.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as u64)
}

/// A screen this app does not draw yet, with the action that leads back to
/// login.
fn placeholder(ui: &mut Ui, screen: &Screen<'_>) -> Option<Next> {
    let (name, button, action) = match screen {
        Screen::Locked { .. } => ("Unlock", "Log Out", UserAction::Logout),
        _ => ("This screen", "Lock", UserAction::Lock),
    };
    let mut next = None;
    let mut window = ui.window("main", "Chaveiro", |ui| {
        ui.stack_view("placeholder", Orientation::Vertical, 8.0, |ui| {
            ui.label("title", format_args!("{name}"))
                .style(Style::Title2);
            ui.label("note", format_args!("This screen is not built yet."));
            if ui.button("action", button).clicked() {
                next = Some(Next::User(action));
            }
        })
        .padding([20.0, 36.0, 20.0, 20.0]);
    });
    window.compact().glass().size_to_fit();
    if window.should_close() {
        next = Some(Next::Quit);
    }
    next
}

/// The sentence under the form for a failed login.
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
