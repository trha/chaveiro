//! The login screen: the server, the email, the master password, one accent
//! Log in button, and an InfoBar when the server refuses.
//!
//! An error about one field shows on the line under it. A field shows its
//! error after Log in, or after the user leaves it with a value in it, and
//! the error clears as soon as the value is valid. Only errors that name no
//! field, such as a refusal from the server, go in the InfoBar.

use chaveiro_core::{
    core::LoginError,
    login_form::{
        CLOUDS, EmailError, ServerError, email_error, server_text, server_url, with_scheme,
    },
};
use url::Url;
use windows_core::Result;
use winui_immediate::{Align, Severity, Ui};
use zeroize::Zeroizing;

pub struct LoginForm {
    /// The Server box's text: a cloud name from [`CLOUDS`], or an address.
    pub server: String,
    pub email: String,
    /// Which fields show their errors.
    shown: Shown,
    /// "Other server…" was picked, so the box takes focus once its list
    /// closes.
    focus_server: bool,
}

/// Which fields show their errors. A field that is not shown is still
/// checked on Log in.
#[derive(Default)]
struct Shown {
    server: bool,
    email: bool,
    password: bool,
}

/// What the screen shows besides the form.
pub struct LoginState<'a> {
    /// A request is running: every field is disabled and the button shows
    /// a ring.
    pub busy: bool,
    /// Why the server refused the last attempt, or why it could not be
    /// reached.
    pub error: Option<&'a str>,
    /// Why the settings did not load.
    pub warning: Option<&'a str>,
}

/// A login the user asked for.
pub struct Submit {
    pub server: Url,
    pub email: String,
    pub password: Zeroizing<String>,
}

/// The last item of the Server box, which clears it for a typed address.
const OTHER: &str = "Other server…";

/// The caption under the Server box while it has no error, because an
/// editable combo box looks like a plain drop-down.
const SERVER_HINT: &str = "Pick one, or type your server’s address";

const PASSWORD_ERROR: &str = "Enter your master password";

/// The line under the Server box for `error`.
fn server_message(error: ServerError) -> &'static str {
    match error {
        ServerError::Empty => "Enter your server’s address",
        ServerError::NotHttps => "Use an address that starts with https://",
        ServerError::NoHost => "Enter an address like https://vault.example.com",
    }
}

/// The line under the Email box for `error`.
fn email_message(error: EmailError) -> &'static str {
    match error {
        EmailError::Empty => "Enter your email",
        EmailError::Malformed => "Enter an email address like name@example.com",
    }
}

impl LoginForm {
    pub fn new() -> Self {
        LoginForm {
            server: CLOUDS[0].0.to_owned(),
            email: String::new(),
            shown: Shown::default(),
            focus_server: false,
        }
    }

    /// Shows `server` in the form: a cloud by its name, or else the address.
    pub fn set_server(&mut self, server: &Url) {
        self.server = server_text(server);
    }
}

/// Draws the screen and returns the login the user asked for with Log in
/// or Enter. While a field is wrong, Log in sends nothing: every wrong field
/// shows its error and focus moves to the first one.
///
/// The password box is cleared on every login sent, so after a failure the
/// user types it again while the email stays.
pub fn view(ui: &mut Ui, form: &mut LoginForm, state: LoginState<'_>) -> Result<Option<Submit>> {
    let mut submitted = None;
    let enabled = !state.busy;
    crate::screen::form(ui, "login", |ui| {
        crate::screen::title(ui, "Log in")?;
        if let Some(warning) = state.warning {
            ui.info_bar("warning", Severity::Warning, "Settings not loaded", warning)?;
        }

        // Server: a cloud from the list, or a typed address.
        let mut enter = false;
        let items = [CLOUDS[0].0, CLOUDS[1].0, OTHER];
        let before = form.server.clone();
        let mut server = ui.editable_combo_box("server", &items, &mut form.server)?;
        let other = server.picked() == Some(items.len() - 1);
        let closed = server.closed();
        let left = server.left()?;
        if other {
            // Picking "Other server…" clears a cloud name for an address but
            // keeps a typed one, and starts the box fresh: opening the list
            // moved focus away, which must not count as leaving the box.
            form.server = match CLOUDS.iter().any(|(name, _)| *name == before.trim()) {
                true => String::new(),
                false => before,
            };
            form.shown.server = false;
            form.focus_server = true;
        } else if left {
            if let Some(full) = with_scheme(&form.server) {
                form.server = full;
            }
            form.shown.server |= !form.server.trim().is_empty();
        }
        let mut server_error = server_url(&form.server).err().map(server_message);

        server
            .header("Server")?
            .required(true)?
            .hint(SERVER_HINT)?
            .error(server_error.filter(|_| form.shown.server))?
            .enabled(enabled)?
            .access_key("S")?;
        let server_field = server.field();
        if form.focus_server && closed {
            form.focus_server = false;
            ui.focus(server_field)?;
        }

        // Email.
        let mut email = ui.text_box("email", "name@example.com", &mut form.email)?;
        enter |= email.enter_pressed()?;
        form.shown.email |= email.left()? && !form.email.trim().is_empty();
        let email_error = email_error(&form.email).map(email_message);

        email
            .header("Email")?
            .required(true)?
            .error(email_error.filter(|_| form.shown.email))?
            .enabled(enabled)?
            .access_key("E")?;
        let email_field = email.field();

        // Master password.
        let mut password = ui.password_box("password", "")?;
        password
            .header("Master password")?
            .required(true)?
            .enabled(enabled)?
            .access_key("P")?;
        enter |= password.enter_pressed()?;
        let changed = password.changed()?;
        let left = password.left()?;

        // Read only when it can change what shows, since each read copies
        // the password.
        let empty = (form.shown.password || left || changed) && password.is_empty()?;
        form.shown.password |= left && !empty;
        let password_field = password.handle();
        let mut password_error = (form.shown.password && empty).then_some(PASSWORD_ERROR);

        // Log in.
        let label = match state.busy {
            true => "Logging in…",
            false => "Log in",
        };
        let mut button = ui.button("submit", label)?;
        let clicked = button.clicked()?;
        button
            .accent(true)?
            .busy(state.busy)?
            .align(Align::Stretch)?
            .margin(crate::screen::GROUP_GAP_ABOVE)?
            .access_key("L")?;

        // On submit, every field shows its error, and the login goes out
        // only when none has one.
        if (clicked || enter) && !state.busy {
            form.shown = Shown {
                server: true,
                email: true,
                password: true,
            };
            if let Some(full) = with_scheme(&form.server) {
                form.server = full;
            }
            let empty = ui.password_is_empty(password_field)?;
            password_error = empty.then_some(PASSWORD_ERROR);

            match server_url(&form.server) {
                Ok(server) if email_error.is_none() && !empty => {
                    // Sending clears the box, which must not read as a
                    // missing password when the server answers.
                    form.shown.password = false;
                    password_error = None;
                    submitted = Some(Submit {
                        server,
                        email: form.email.trim().to_owned(),
                        password: ui.take_password(password_field)?,
                    });
                }
                server => {
                    server_error = server.err().map(server_message);
                    // Focus goes to the first wrong field, top to bottom.
                    let first = [
                        server_error.map(|_| server_field),
                        email_error.map(|_| email_field),
                        password_error.map(|_| password_field.field()),
                    ]
                    .into_iter()
                    .flatten()
                    .next();
                    if let Some(first) = first {
                        ui.focus(first)?;
                    }
                }
            }

            ui.set_error(email_field, email_error)?;
            ui.set_error(server_field, server_error)?;
        }
        ui.set_error(password_field.field(), password_error)?;

        // Errors from the server, which name no field.
        if let Some(error) = state.error {
            ui.info_bar("error", Severity::Error, "Login failed", error)?;
        }
        Ok(())
    })?;
    Ok(submitted)
}

/// The InfoBar sentence for a failed login.
pub fn login_error(error: &LoginError) -> String {
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
