# Fixture sources

Each fixture is `{"status": <HTTP status>, "body": <response JSON>}`.

Recorded fixtures come from Vaultwarden 1.37.4 (nixpkgs, run in a throwaway
data folder in WSL) driven by `record.py` in this folder. The account is a
throwaway: email `fixture-user@example.com`, password
`fixture-test-password-1`, PBKDF2 600000. Every key and EncString in them is
real and consistent, so tests can derive the master key from that password and
decrypt the whole vault. Tokens in them are for a server that no longer exists.

| Fixture | Source |
|---|---|
| `prelogin.json` | recorded from Vaultwarden 1.37.4 |
| `token_success.json` | recorded from Vaultwarden 1.37.4 |
| `token_two_factor_required.json` | recorded from Vaultwarden 1.37.4 (TOTP enabled) |
| `token_wrong_password_vaultwarden.json` | recorded from Vaultwarden 1.37.4 |
| `token_wrong_password_bitwarden.json` | hand-written from bitwarden/server@8c88e007893b73d7b83593e12ad97b25129ffcf7 `src/Identity/IdentityServer/RequestValidators/BaseRequestValidator.cs` L255, L478-496 and `ResourceOwnerPasswordValidator.cs` L156-161; `ErrorModel` shape from `src/Libraries/ExceptionHandling/ErrorResponseModel.cs` |
| `token_new_device_otp_required.json` | hand-written from bitwarden/server@8c88e007893b73d7b83593e12ad97b25129ffcf7 `src/Identity/IdentityServer/RequestValidators/DeviceValidator.cs` L273-310; casing `ErrorModel.Message` from bitwarden/clients@68452233110217069ac0755517f2f3af899acb1f `libs/common/src/services/api.service.ts` (`NEW_DEVICE_VERIFICATION_REQUIRED_MESSAGE`). Vaultwarden 1.37.4 does not issue it. |
| `sync.json` | recorded from Vaultwarden 1.37.4: one folder, one org with one collection, ciphers of type 1-5, an org login, a login with its own `key`, a login with `fido2Credentials` |
| `revision_date.json` | recorded from Vaultwarden 1.37.4 |
| `cipher_post_login.json` | recorded from Vaultwarden 1.37.4 (`POST /api/ciphers`) |
| `cipher_put_success.json` | recorded from Vaultwarden 1.37.4 (`PUT /api/ciphers/{id}`) |
| `cipher_put_out_of_date.json` | recorded from Vaultwarden 1.37.4 (stale `lastKnownRevisionDate`) |

The SshKey cipher holds placeholder key text, so only its EncStrings are real.
The passkey's `keyValue` in `sync.json` was replaced after recording with a real
P-256 PKCS#8 key (base64url), encrypted under that cipher's key, so tests can
sign with it.
