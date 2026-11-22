//! Windows Hello as the device key: a `KeyCredential` named
//! [`CREDENTIAL_NAME`] and the shared secret it derives over the core's
//! challenge.

use crate::bindings::{
    CryptographicBuffer, IKeyCredential2, KeyCredential, KeyCredentialCreationOption,
    KeyCredentialManager, KeyCredentialStatus, WindowId,
};
use chaveiro_core::core::{DeviceKeyError, SecretBytes};
use windows_core::{Array, Interface};
use zeroize::{Zeroize, Zeroizing};

/// The Hello key's name, one per Windows user.
const CREDENTIAL_NAME: &str = "Chaveiro";

/// The text Hello shows in its prompt.
const PROMPT: &str = "Unlock your vault";

/// The device key's secret over `challenge`, behind a Hello prompt owned by
/// the window whose `AppWindow.Id` is `window`, so the prompt opens in front
/// of it. With `create`, a new key replaces any old one first, which asks
/// for Hello once more. Completes when the user answers, which can take
/// minutes.
///
/// A cancelled prompt gives [`DeviceKeyError::Cancelled`], and a machine
/// without Hello, or no window yet, gives [`DeviceKeyError::Unavailable`].
pub async fn secret(
    window: Option<u64>,
    challenge: [u8; 32],
    create: bool,
) -> Result<SecretBytes, DeviceKeyError> {
    let window = WindowId {
        value: window.ok_or(DeviceKeyError::Unavailable)?,
    };
    let credential = credential(window, create).await?;
    let operation = credential
        .cast::<IKeyCredential2>()
        .and_then(|credential| {
            credential.RequestDeriveSharedSecretAsync(
                window,
                PROMPT,
                &CryptographicBuffer::CreateFromByteArray(&challenge)?,
            )
        })
        .map(winui_executor::winrt)
        .map_err(failed)?
        .await
        .map_err(failed)?;
    status(operation.Status().map_err(failed)?)?;
    let buffer = operation.Result().map_err(failed)?;
    let mut bytes = Array::<u8>::new();
    CryptographicBuffer::CopyToByteArray(&buffer, &mut bytes).map_err(failed)?;
    // `Array` frees its block with CoTaskMemFree, which the wiping
    // allocator does not see, so wipe the copy the OS handed over here.
    let secret = Zeroizing::new(bytes.to_vec());
    bytes.zeroize();
    Ok(secret)
}

/// The Hello key: a fresh one with `create`, or else the existing one.
async fn credential(window: WindowId, create: bool) -> Result<KeyCredential, DeviceKeyError> {
    let result = match create {
        true => KeyCredentialManager::RequestCreateForWindowAsync(
            window,
            CREDENTIAL_NAME,
            KeyCredentialCreationOption::ReplaceExisting,
        ),
        false => KeyCredentialManager::OpenAsync(CREDENTIAL_NAME),
    }
    .map(winui_executor::winrt)
    .map_err(failed)?
    .await
    .map_err(failed)?;
    status(result.Status().map_err(failed)?)?;
    result.Credential().map_err(failed)
}

fn status(status: KeyCredentialStatus) -> Result<(), DeviceKeyError> {
    match status {
        KeyCredentialStatus::Success => Ok(()),
        KeyCredentialStatus::UserCanceled | KeyCredentialStatus::UserPrefersPassword => {
            Err(DeviceKeyError::Cancelled)
        }
        KeyCredentialStatus::NotFound => Err(DeviceKeyError::Failed(
            "the Windows Hello key is gone".to_owned(),
        )),
        KeyCredentialStatus::SecurityDeviceLocked => Err(DeviceKeyError::Failed(
            "the security device is locked".to_owned(),
        )),
        KeyCredentialStatus::AlgorithmNotSupported => Err(DeviceKeyError::Unavailable),
        other => Err(DeviceKeyError::Failed(format!(
            "Windows Hello status {}",
            other.0
        ))),
    }
}

fn failed(error: windows_core::Error) -> DeviceKeyError {
    // NTE_NOT_SUPPORTED, or no Hello set up for this user.
    const NOT_SUPPORTED: i32 = 0x8009_0029_u32 as i32;
    match error.code().0 {
        NOT_SUPPORTED => DeviceKeyError::Unavailable,
        _ => DeviceKeyError::Failed(error.to_string()),
    }
}
