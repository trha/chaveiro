//! The login screen, from the "Login" boards of the Chaveiro macOS design:
//! a title, the server, email and master password in a form with headers
//! on the left, the server's refusal under the form, then Quit and Log In.
//!
//! An error about one field shows on the line under it. A field shows its
//! error after Log In, or after the user leaves it with a value in it, and
//! the error clears as soon as the value is valid.

use appkit_immediate::{Align, Orientation, Style, TextColor, Ui};
use url::Url;
use vault_core::login_form::{
    CLOUDS, EmailError, ServerError, email_error, server_text, server_url, with_scheme,
};
use zeroize::Zeroizing;

pub struct LoginForm {
    /// The Server box's text: a cloud name from [`CLOUDS`], or an address.
    pub server: String,
    pub email: String,
    /// Which fields show their errors.
    shown: Shown,
}

/// Which fields show their errors. A field that is not shown is still
/// checked on Log In.
#[derive(Default)]
struct Shown {
    server: bool,
    email: bool,
    password: bool,
}

/// What the screen shows besides the form.
pub struct LoginState<'a> {
    /// A request is running: the fields are disabled, Quit becomes Cancel,
    /// and Log In shows a spinner.
    pub busy: bool,
    /// Why the server refused the last attempt, or why it could not be
    /// reached.
    pub error: Option<&'a str>,
    /// Why the settings did not load.
    pub warning: Option<&'a str>,
}

/// What the user asked for in a pass.
pub enum LoginAction {
    Submit(Submit),
    /// Stop the login in flight.
    Cancel,
    Quit,
}

pub struct Submit {
    pub server: Url,
    pub email: String,
    pub password: Zeroizing<String>,
}

/// The last item of the Server box, which clears it for a typed address.
const OTHER: &str = "Other Server…";

/// The caption under the Server box while it has no error, because an
/// editable combo box looks like a plain drop-down.
const SERVER_HINT: &str = "Pick one, or type your server’s address";

const PASSWORD_ERROR: &str = "Enter your master password";

fn server_message(error: ServerError) -> &'static str {
    match error {
        ServerError::Empty => "Enter your server’s address",
        ServerError::NotHttps => "Use an address that starts with https://",
        ServerError::NoHost => "Enter an address like https://vault.example.com",
    }
}

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
        }
    }

    /// Shows `server` in the form: a cloud by its name, or else the address.
    pub fn set_server(&mut self, server: &Url) {
        self.server = server_text(server);
    }
}

/// Draws the window and returns what the user asked for. While a field is
/// wrong, Log In sends nothing: every wrong field shows its error and focus
/// moves to the first one.
///
/// The password field is cleared on every login sent, so after a failure
/// the user types it again while the email stays.
pub fn view(ui: &mut Ui, form: &mut LoginForm, state: LoginState<'_>) -> Option<LoginAction> {
    let mut action = None;
    let enabled = !state.busy;
    let mut window = ui.window("main", "Chaveiro", |ui| {
        // The spacing between groups, from the design.
        ui.stack_view("login", Orientation::Vertical, 16.0, |ui| {
            ui.stack_view("heading", Orientation::Vertical, 4.0, |ui| {
                ui.label("title", format_args!("Log in to Chaveiro"))
                    .style(Style::Heading);
                ui.label(
                    "subtitle",
                    format_args!("Choose your server, then enter your account details."),
                )
                .text_color(TextColor::Secondary);
            })
            .padding([0.0, 0.0, 0.0, 8.0]);
            if let Some(warning) = state.warning {
                ui.alert_line("warning", Some(warning));
            }

            let mut enter = false;
            let mut fields = None;
            ui.form("fields", HEADER_WIDTH, |ui| {
                // Server: a cloud from the list, or a typed address.
                let before = form.server.clone();
                let items = [CLOUDS[0].0, CLOUDS[1].0, OTHER];
                let mut server = ui.combo_box("server", &items, &mut form.server);
                let picked_other = server.changed() && form.server == OTHER;
                let left = server.did_end_editing();
                if picked_other {
                    // Clears a cloud name for an address, but keeps a typed
                    // one.
                    form.server = match CLOUDS.iter().any(|(name, _)| *name == before.trim()) {
                        true => String::new(),
                        false => before,
                    };
                    form.shown.server = false;
                } else if left {
                    if let Some(full) = with_scheme(&form.server) {
                        form.server = full;
                    }
                    form.shown.server |= !form.server.trim().is_empty();
                }
                let server_error = server_url(&form.server).err().map(server_message);
                server
                    .header("Server")
                    .hint(SERVER_HINT)
                    .error(server_error.filter(|_| form.shown.server))
                    .enabled(enabled);
                let server_field = server.field();
                if picked_other {
                    ui.make_first_responder(server_field);
                }

                // Email.
                let mut email = ui.text_field("email", "name@example.com", &mut form.email);
                enter |= email.return_pressed();
                form.shown.email |= email.did_end_editing() && !form.email.trim().is_empty();
                let email_error = email_error(&form.email).map(email_message);
                email
                    .header("Email")
                    .error(email_error.filter(|_| form.shown.email))
                    .enabled(enabled);
                let email_field = email.field();

                // Master password.
                let mut password = ui.secure_text_field("password", "");
                enter |= password.return_pressed();
                let changed = password.changed();
                let left = password.did_end_editing();
                // Read only when it can change what shows.
                let empty = (form.shown.password || left || changed) && password.is_empty();
                form.shown.password |= left && !empty;
                password
                    .header("Master password")
                    .error((form.shown.password && empty).then_some(PASSWORD_ERROR))
                    .enabled(enabled);
                fields = Some((server_field, email_field, password.handle(), email_error));
            });
            let (server_field, email_field, password_field, email_error) =
                fields.expect("the form body ran");

            // The server's refusal sits left of the buttons, in a row that is
            // there anyway, so it takes no space of its own.
            let mut clicked = false;
            ui.stack_view("buttons", Orientation::Horizontal, 10.0, |ui| {
                ui.alert_line("error", state.error).align(Align::Center);
                let label = if state.busy { "Cancel" } else { "Quit" };
                let mut quit = ui.button("quit", label);
                quit.cancel_button(state.busy);
                if quit.clicked() {
                    action = Some(match state.busy {
                        true => LoginAction::Cancel,
                        false => LoginAction::Quit,
                    });
                }
                let label = if state.busy {
                    "Logging In…"
                } else {
                    "Log In"
                };
                let mut log_in = ui.button("log in", label);
                log_in
                    .default_button(true)
                    .busy(state.busy)
                    .enabled(enabled);
                clicked = log_in.clicked();
            })
            .padding([0.0; 4]);

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
                let empty = ui.password_is_empty(password_field);
                let server = server_url(&form.server);
                ui.set_error(
                    server_field,
                    server.as_ref().err().copied().map(server_message),
                );
                ui.set_error(email_field, email_error);
                ui.set_error(password_field.field(), empty.then_some(PASSWORD_ERROR));
                match server {
                    Ok(server) if email_error.is_none() && !empty => {
                        // Sending clears the field, which must not read as a
                        // missing password when the server answers.
                        form.shown.password = false;
                        ui.set_error(password_field.field(), None);
                        action = Some(LoginAction::Submit(Submit {
                            server,
                            email: form.email.trim().to_owned(),
                            password: ui.take_password(password_field),
                        }));
                    }
                    server => {
                        // Focus goes to the first wrong field, top to bottom.
                        let first = [
                            server.is_err().then_some(server_field),
                            email_error.map(|_| email_field),
                            empty.then_some(password_field.field()),
                        ]
                        .into_iter()
                        .flatten()
                        .next();
                        if let Some(first) = first {
                            ui.make_first_responder(first);
                        }
                    }
                }
            }
        })
        .padding([20.0, 36.0, 20.0, 20.0])
        .width(WIDTH);
    });
    // The plain window background, which the system tints with the
    // wallpaper, as the design's card does.
    window.compact().size_to_fit();
    if window.should_close() {
        action = Some(LoginAction::Quit);
    }
    action
}

/// The window's width, from the design.
const WIDTH: f64 = 480.0;

/// The width of the form's header column, from the design.
const HEADER_WIDTH: f64 = 116.0;
