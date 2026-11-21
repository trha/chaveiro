//! Prelogin, token and the error bodies of the identity and API servers.

use super::enc_option;
use crate::crypto::{EncString, Kdf};
use serde::Deserialize;
use serde_json::{Map, Value};

/// The body of `POST {identity}/accounts/prelogin`.
pub fn prelogin_body(email: &str) -> Value {
    serde_json::json!({ "email": email })
}

/// The answer to prelogin.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct PreloginResponse {
    /// 0 PBKDF2-SHA256, 1 Argon2id.
    pub kdf: u8,
    #[serde(rename = "kdfiterations")]
    pub kdf_iterations: u32,
    /// MiB.
    #[serde(rename = "kdfmemory", default)]
    pub kdf_memory: Option<u32>,
    #[serde(rename = "kdfparallelism", default)]
    pub kdf_parallelism: Option<u32>,
}

impl PreloginResponse {
    /// The KDF settings. An unknown `kdf`, or Argon2id without memory or
    /// parallelism, gives [`None`].
    pub fn kdf(&self) -> Option<Kdf> {
        match self.kdf {
            0 => Some(Kdf::Pbkdf2 {
                iterations: self.kdf_iterations,
            }),
            1 => Some(Kdf::Argon2id {
                iterations: self.kdf_iterations,
                memory_mebibytes: self.kdf_memory?,
                parallelism: self.kdf_parallelism?,
            }),
            _ => None,
        }
    }
}

/// A successful `POST {identity}/connect/token`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub struct TokenResponse {
    pub access_token: String,
    /// Missing for `grant_type=client_credentials` on some servers.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Seconds.
    pub expires_in: u64,
    /// The user key under the stretched master key. Missing for an API key
    /// login.
    #[serde(default, deserialize_with = "enc_option")]
    pub key: Option<EncString>,
    /// The PKCS#8 private key under the user key.
    #[serde(rename = "privatekey", default, deserialize_with = "enc_option")]
    pub private_key: Option<EncString>,
    /// Present only after a second factor sent with `twoFactorRemember=1`.
    #[serde(rename = "twofactortoken", default)]
    pub two_factor_token: Option<String>,
}

/// The body of a failed token or API request. Bitwarden and Vaultwarden
/// fill different fields, so read it through [`message`](Self::message) and
/// [`token_failure`](Self::token_failure).
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase", default)]
pub struct ErrorResponse {
    pub error: Option<String>,
    pub error_description: Option<String>,
    pub message: Option<String>,
    #[serde(rename = "errormodel")]
    pub error_model: Option<ErrorModel>,
    /// Provider number (as text) to provider details. Present when a
    /// second factor is required.
    #[serde(rename = "twofactorproviders2")]
    pub two_factor_providers: Option<Map<String, Value>>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase", default)]
pub struct ErrorModel {
    pub message: Option<String>,
}

impl ErrorResponse {
    /// The most specific non-empty text: `ErrorModel.Message`, then
    /// `message`, then `error_description`, then `error`.
    pub fn message(&self) -> Option<&str> {
        [
            self.error_model
                .as_ref()
                .and_then(|model| model.message.as_deref()),
            self.message.as_deref(),
            self.error_description.as_deref(),
            self.error.as_deref(),
        ]
        .into_iter()
        .flatten()
        .find(|text| !text.is_empty())
    }

    /// What a 400 from the token endpoint asks the client to do.
    pub fn token_failure(&self) -> TokenFailure {
        if let Some(providers) = &self.two_factor_providers {
            let mut sorted: Vec<u8> = providers
                .keys()
                .filter_map(|key| key.parse().ok())
                .collect();
            sorted.sort_unstable();
            // Email's entry names the address, masked, such as
            // `{"1": {"Email": "f***@example.com"}}`, with keys lowercased
            // by `from_slice`.
            let masked_email = providers
                .get("1")
                .and_then(|email| email.get("email"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            return TokenFailure::SecondFactor {
                providers: sorted,
                masked_email,
            };
        }
        let new_device = self.error.as_deref() == Some("device_error")
            || self.message().is_some_and(|message| {
                message
                    .to_lowercase()
                    .contains("new device verification required")
            });
        if new_device {
            return TokenFailure::NewDeviceOtp;
        }
        TokenFailure::Rejected {
            message: self.message().unwrap_or_default().to_owned(),
        }
    }
}

/// See [`ErrorResponse::token_failure`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TokenFailure {
    /// Resend with `twoFactorToken` and `twoFactorProvider`, one of
    /// `providers` (0 authenticator, 1 email, ...), sorted.
    SecondFactor {
        providers: Vec<u8>,
        /// The address codes go to, masked by the server.
        masked_email: Option<String>,
    },
    /// Resend with the `NewDeviceOtp` form field from the email.
    NewDeviceOtp,
    /// Wrong password or another refusal; `message` is for the user.
    Rejected { message: String },
}
