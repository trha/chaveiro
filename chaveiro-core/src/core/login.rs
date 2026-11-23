use super::{
    config::{Config, Server, endpoint},
    effect::{Body, HttpRequest, Method},
    input::{HttpError, HttpResponse},
    record::REMEMBER_PROVIDER,
    screen::LoginError,
    state::Answer,
};
use crate::{crypto::SecretString, proto, proto::identity::ErrorResponse};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

/// The request that asks the server to email a second-factor code.
pub(super) fn send_email_code(
    server: &Server,
    config: &Config,
    email: &str,
    password_hash: &SecretString,
) -> HttpRequest {
    HttpRequest {
        method: Method::Post,
        url: endpoint(&server.urls.api, &["two-factor", "send-email-login"], &[]),
        headers: client_headers(config),
        body: Body::Json(serde_json::json!({
            "email": email,
            "masterPasswordHash": password_hash.as_str(),
        })),
    }
}

/// The `grant_type=password` token request, with the second factor or
/// new-device code when the user gave one.
/// A token request with the password hash, to the `identity` base.
pub(super) fn password_token(
    identity: &Url,
    config: &Config,
    email: &str,
    password_hash: &SecretString,
    answer: Option<&Answer>,
) -> HttpRequest {
    let mut form = device_form(config);
    form.extend([
        ("grant_type", secret("password")),
        ("scope", secret("api offline_access")),
        ("client_id", secret(&config.client_id)),
        ("username", secret(email)),
        ("password", password_hash.clone()),
    ]);
    match answer {
        None => {}
        Some(Answer::TwoFactor {
            provider,
            code,
            remember,
        }) => form.extend([
            ("twoFactorToken", code.clone()),
            ("twoFactorProvider", secret(&provider.to_string())),
            (
                "twoFactorRemember",
                secret(if *remember { "1" } else { "0" }),
            ),
        ]),
        Some(Answer::Remembered { token }) => form.extend([
            ("twoFactorToken", token.clone()),
            ("twoFactorProvider", secret(&REMEMBER_PROVIDER.to_string())),
            ("twoFactorRemember", secret("0")),
        ]),
        Some(Answer::NewDevice { code }) => form.push(("newDeviceOtp", code.clone())),
    }
    token_request(identity, config, form)
}

/// A request to the token endpoint under the `identity` base.
pub(super) fn token_request(
    identity: &Url,
    config: &Config,
    form: Vec<(&'static str, SecretString)>,
) -> HttpRequest {
    HttpRequest {
        method: Method::Post,
        url: endpoint(identity, &["connect", "token"], &[]),
        headers: client_headers(config),
        body: Body::Form(form),
    }
}

pub(super) fn device_form(config: &Config) -> Vec<(&'static str, SecretString)> {
    vec![
        ("deviceType", secret(&config.device_type.to_string())),
        ("deviceIdentifier", secret(&config.device_id.to_string())),
        ("deviceName", secret(&config.device_name)),
    ]
}

pub(super) fn client_headers(config: &Config) -> Vec<(&'static str, String)> {
    vec![
        ("Device-Type", config.device_type.to_string()),
        ("Bitwarden-Client-Name", config.client_name.clone()),
        ("Bitwarden-Client-Version", config.client_version.clone()),
    ]
}

pub(super) fn secret(text: &str) -> SecretString {
    Zeroizing::new(text.to_owned())
}

/// The error to show for a failed identity request.
pub(super) fn login_error(response: &Result<HttpResponse, HttpError>) -> LoginError {
    match response {
        Err(error) => LoginError::Network(error.message.clone()),
        Ok(answer) => match proto::from_slice::<ErrorResponse>(&answer.body)
            .ok()
            .as_ref()
            .and_then(ErrorResponse::message)
        {
            Some(message) if (400..500).contains(&answer.status) => {
                LoginError::Rejected(message.to_owned())
            }
            _ => LoginError::Server {
                status: answer.status,
            },
        },
    }
}

/// The user id in the `sub` claim of a JWT access token. The signature is
/// not checked: the server that issued the token is the only one that
/// reads it.
pub(super) fn jwt_subject(token: &str) -> Option<Uuid> {
    #[derive(Deserialize)]
    struct Claims {
        sub: Uuid,
    }
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice::<Claims>(&bytes)
        .ok()
        .map(|claims| claims.sub)
}
