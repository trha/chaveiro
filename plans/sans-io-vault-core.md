# Sans-io vault core

Status: Plan

## Goal

A Rust library crate, `vault-core`, that holds the whole Bitwarden client
logic as a pure state machine. It takes inputs (user actions, HTTP responses,
storage reads, finished key derivations, the time) and returns effects
(HTTP requests, storage writes and reads, key derivation jobs). It does no
I/O, has no async, and owns no threads. The WinUI app (`bitwarden-winui`)
becomes a thin shell: its `winui-immediate` `App` owns the core, runs each
effect on the thread pool, and feeds the result back as an input through
`Handle::send`.

The first delivery covers login (password with 2FA by authenticator or email,
new-device OTP, API key), unlock and lock, sync, an encrypted offline cache,
decrypting and browsing every cipher type, and creating, editing, deleting
and restoring ciphers and folders with revision conflict handling. It talks
to official Bitwarden servers (cloud and self-hosted) and to Vaultwarden.

It also covers Windows Hello unlock, with a key derived from a Hello signature
so that nothing is held in memory while locked. And it covers acting as a
Windows 11 passkey provider: browsers and apps create and use passkeys stored
in our vault as `login.fido2Credentials`.

The core must also serve a later macOS app (SwiftUI and AppKit) without
changes to its logic. So it names no platform: device unlock, protected
storage, HTTP and passkey transport are all effects that each shell carries
out its own way. A Swift shell reaches the core through a thin FFI crate that
copies the borrowed screen into owned values. The macOS app and that crate
are not part of this plan.

## Out of scope

Organization admin (collections management, member keys), Sends,
attachments upload and download, push notifications over SignalR, PIN
unlock, logging in to Bitwarden with a passkey (WebAuthn PRF login),
emergency access, the import and export of vaults, and the UI screens
themselves. Push sync is the next plan.

## Facts this plan rests on

**Licensing.** `bitwarden-crypto` and `bitwarden-vault` are under
`GPL-3.0-only OR LicenseRef-Bitwarden-SDK` (their `Cargo.toml` in
github.com/bitwarden/sdk-internal). The SDK License limits use to official
Bitwarden servers and forbids offering the app to third parties (sections
1.3, 3.1, 3.3). `bitwarden-vault` depends on `bitwarden-core`, `reqwest` and
`tokio`, so it can't sit in a sans-io core. `bitwarden-crypto` is free of
network code but had four major versions in about two years and pins several
pre-release crypto crates. Source: research agent report of 2026-11-20.

**Protocol.** Only the symmetric EncString types were checked against source
(`crates/bitwarden-crypto/src/enc_string/symmetric.rs` in sdk-internal):
type 0 `0.iv|data` (no MAC, decrypt only), type 2 `2.iv|data|mac` (current),
type 7 `7.<COSE Encrypt0>`, and type 1 is removed. The rest of this list comes
from a research report written from memory, and step 1 checks it against
`bitwarden/clients` `libs/common/src/vault/models/response/*.ts` and
`vaultwarden src/api/core/ciphers.rs` before the proto types freeze:

- Prelogin `POST {identity}/accounts/prelogin {email}` returns `kdf`
  (0 PBKDF2-SHA256, 1 Argon2id), `kdfIterations`, `kdfMemory` (MiB),
  `kdfParallelism`.
- Master key: PBKDF2(password, salt = trimmed lowercase email) or
  Argon2id(password, salt = SHA-256(email)), 32 bytes. Master password hash:
  base64(PBKDF2-SHA256(master key, salt = password, 1 round)).
- Stretched key: HKDF-Expand-SHA256(master key, "enc", 32) and
  (master key, "mac", 32). It decrypts the user key (64 bytes) from the token
  response `Key` or `profile.key`.
- Token: `POST {identity}/connect/token`, form-encoded, `grant_type=password`
  with `username`, `password` (the hash), `scope=api offline_access`,
  `client_id`, `deviceType`, `deviceIdentifier`, `deviceName`. 2FA comes back
  as HTTP 400 `invalid_grant` with `TwoFactorProviders2`. The client resends
  with `twoFactorToken` and `twoFactorProvider`. New-device verification is a
  400 whose description names the OTP, answered with `newDeviceOtp`.
  `grant_type=refresh_token` refreshes. `grant_type=client_credentials` logs in
  by API key and returns no usable key.
- Headers: `Device-Type`, `Bitwarden-Client-Name`, `Bitwarden-Client-Version`.
- Sync: `GET /api/sync?excludeDomains=true` returns `profile` (with `key`,
  `privateKey`, `organizations[].key`), `folders`, `collections`, `ciphers`,
  `policies`, `sends`. Cipher `type` is 1 Login, 2 SecureNote, 3 Card,
  4 Identity, 5 SshKey. Every user string is an EncString. Ids, numbers,
  dates, `match`, field `type` and `linkedId` are plain.
- Org keys are type 4 (RSA-2048 OAEP-SHA1) under the user's private key.
  The private key is PKCS#8 DER, type 2 under the user key.
- Cipher key order: org key or user key, then the cipher's own `key` if set.
- Writes: `POST /api/ciphers`, `POST /api/ciphers/create` (org, with
  `collectionIds`), `PUT /api/ciphers/{id}`, `PUT /api/ciphers/{id}/delete`
  (trash), `PUT /api/ciphers/{id}/restore`, `DELETE /api/ciphers/{id}`,
  `POST/PUT/DELETE /api/folders[/{id}]`. A stale `lastKnownRevisionDate`
  (beyond 1 s) gets HTTP 400 "out of date".
- `GET /api/accounts/revision-date` returns epoch milliseconds.
- Vaultwarden: JSON key case varies (camelCase and PascalCase), arrays may be
  missing, `archivedDate`, `permissions` and SshKey may be absent, type 7 is
  not issued.

**The shell.** From `plans/winui-immediate.md` (WP-00 to WP-07 done): the
app implements `winui_immediate::App`, whose `update(message)` and
`view(&mut self, ui)` run on the UI thread. Work leaves the UI thread however
the app chooses, and its result comes back as an `A::Message` through
`Handle::send`, which coalesces a burst into one wake and one pass. `view`
borrows the app mutably and reads its state with no copies. A `PasswordBox`
keeps its text; `Ui::take_password` reads it once and clears it. Shared
clients go in the app as `&'static` (via `Box::leak`) or owned state, never
in a message.

**Windows Hello in Bitwarden desktop**
(`bitwarden/clients` `apps/desktop/desktop_native/biometric/src/windows.rs`,
read 2026-11-20). A KeyCredential named "BitwardenBiometricsV2" comes from
`KeyCredentialManager::RequestCreateAsync`, falling back to `OpenAsync`.
`RequestSignAsync` signs a random challenge made at enrollment, and
`WindowsHelloPrf::derive_from_signature` hashes the signature into the key
that seals the user key. The challenge is not secret: security rests on
needing a real Hello approval. The challenge and the sealed key are stored
in Windows Credential Manager. Bitwarden also keeps the user key in DPAPI
memory while locked and then asks only a yes/no `UserConsentVerifier`
prompt. We drop that path, because it breaks our threat model. The signing
call has no window-handle variant, so Bitwarden polls every 500 ms to bring
the Hello prompt to the front (`windows_focus.rs`). That the signature is
deterministic (RSA PKCS#1 v1.5) is an assumption in their code, unverified
by us.

**Window-aware Hello calls** (learn.microsoft.com `KeyCredential` and
`KeyCredentialManager`, winrt-26100, read 2026-11-20).
`KeyCredentialManager.RequestCreateForWindowAsync(WindowId, String,
KeyCredentialCreationOption)`, `KeyCredential.RequestSignForWindowAsync(
WindowId, IBuffer)` and `KeyCredential.RequestDeriveSharedSecretAsync(
WindowId, String, IBuffer)` are documented only for SDK 26100 and later.
The exact build that introduced each is not stated. That the derived secret
is the same for the same input is our reading of its purpose, unverified.
Step 8 checks it on hardware, and the enroll check guards it.

**Windows passkey provider** (`microsoft/webauthn` `webauthnplugin.h`, and
learn.microsoft.com/windows/apps/develop/security/third-party, read
2026-11-20). The header exports `WebAuthNPluginAddAuthenticator`,
`WebAuthNPluginAuthenticatorAddCredentials`, `GetAllCredentials`,
`PerformUserVerification`, `GetOperationSigningPublicKey`, and
decode/encode helpers for the MakeCredential and GetAssertion requests and
responses. The plugin registers a COM CLSID, and the user turns it on under
Settings > Accounts > Passkeys. Bitwarden ships one
(`desktop_native/windows_plugin_authenticator`, AAGUID d548826e-…,
algorithm -7), which returns `NotSupported` when it doesn't run from an Appx
package. So the provider needs package identity: MSIX, or a sparse package
for our unpackaged exe, with an out-of-process COM server in the manifest.
From memory, unverified: the COM interface is `IPluginAuthenticator`
(MakeCredential, GetAssertion, CancelOperation), and the API needs Windows 11
24H2 with the late-2025 update.

**Stored passkeys** (from memory, step 1 verifies them):
`login.fido2Credentials[]` holds `credentialId`, `keyType`, `keyAlgorithm`,
`keyCurve`, `keyValue` (base64url PKCS#8 P-256), `rpId`, `rpName`,
`userHandle`, `userName`, `userDisplayName`, `counter`, `discoverable`, all
as EncStrings, and `creationDate` as a plain date.

**macOS counterparts** (from memory, unverified; check before the macOS plan):
- Touch ID keys live in the Secure Enclave and are P-256 only. ECDSA
  signatures there are randomized, so a key derived from a signature can't
  work. ECDH (`SecKeyCopyKeyExchangeResult`) with a stored peer public key
  gives the same shared secret each time, behind a Touch ID prompt when the
  key's access control says `.biometryCurrentSet`.
- A passkey provider is an `ASCredentialProviderExtension`
  (AuthenticationServices, passkeys from macOS 14). It runs in its own
  process with its own memory and reaches the app's data through an App
  Group container.
- `ASPasskeyRegistrationCredential` takes a ready attestation object (CBOR),
  and `ASPasskeyAssertionCredential` takes the authenticator data, signature,
  credential id and user handle.
- The Keychain stores protected values, and `URLSession` does HTTP.
- UniFFI generates Swift bindings for a Rust crate whose API uses owned
  values, records, enums and callback-free functions.

**The threat model** (`.claude/memory/threat-model.md`, option C): after lock
or logout our process holds no master password, keys or decrypted vault
data. The app uses a wiping global allocator (measured cost: none within
noise). Secrets travel in zeroizing types.

## Architecture

```text
 winui-immediate App (UI thread)               thread pool
 ┌──────────────────────────────────┐   pool job           ┌──────────────┐
 │ update(msg)                      │ ───────────────────▶ │ shell::run   │
 │   core.handle(now, rng, input)   │   Effect             │  HTTP (ureq) │
 │     -> Vec<Effect>               │                      │  files       │
 │   for each effect: submit        │ ◀─────────────────── │  KDF         │
 │ view(ui) reads core.screen()     │   Handle::send(      └──────────────┘
 └──────────────────────────────────┘     Msg::Core(Input))
```

`vault-core` has three layers. `crypto` holds pure functions and secret key
types. `proto` holds the serde types for the JSON the server sends and
accepts. `core` holds the state machine. The crate depends on no HTTP, file,
thread, async or platform crate. Randomness and the clock come in as
call-time arguments, so tests are deterministic.

## Platform boundary

The core's API serves every shell: WinUI now and macOS later. It keeps
Rust borrows (`Screen<'_>`, `&mut impl CryptoRngCore`), so the Rust shell
reads the screen without copies. It follows two rules:

- **Platform words stay in the shell.** The core speaks of a "device key"
  (a hardware-backed secret behind a biometric prompt), "protected storage"
  and "passkey requests". Windows Hello, DPAPI, `webauthn.dll`, Touch ID,
  Keychain and AuthenticationServices appear only in shell code.
- **No assumption of one process.** A macOS passkey extension runs its own
  `Core` over the same encrypted cache and storage. It shares state only
  through storage, never through memory.

| Effect or input | WinUI shell | macOS shell (later) |
|---|---|---|
| `Http` | `ureq` on the thread pool | `URLSession` |
| `Store`, `Load`, `Clear` with `protected: true` | files + DPAPI | Keychain (App Group) |
| `Derive` | thread pool | background `Task` |
| `DeviceSecret` | Hello `RequestDeriveSharedSecretAsync` | Secure Enclave ECDH |
| `PasskeyReply` | `webauthn.dll` plugin COM | `ASCredentialProviderExtension` |

A Swift shell (SwiftUI needs Swift, because it has no Objective-C API) goes
through a later crate, `crates/vault-core-ffi`. It owns a `Core` and a
`rand_core::OsRng`, calls `handle` and `screen`, and copies the borrowed
screen into owned UniFFI records. Only the Swift path pays for those copies,
and secrets cross as zeroizing bytes. Each shell decides how long its own
copy lives: Swift can't wipe a `String`, so the macOS shell keeps secrets in
`Data` and zeroes it. Inputs and effects are already plain enums of owned
data, so they need no change for the FFI. This plan builds no Swift and no
FFI crate.

Key derivation is CPU-heavy: about 0.3 s for PBKDF2 at 600,000 rounds, and
64 MiB for Argon2id. The core emits it as an effect (`Effect::Derive`), and
the shell runs `vault_core::crypto::derive_master_key` on the thread pool.
The work stays pure. Only where it runs moves. Everything else, including
decrypting a few thousand ciphers with AES-CBC-HMAC, runs inside
`handle`.

## Crate layout

The repo is already a virtual workspace (`spikes`, `winui-immediate`). This
plan adds two members:

```text
Cargo.toml                    [workspace] members += ["app", "crates/vault-core"]
crates/vault-core/src/lib.rs
crates/vault-core/src/crypto.rs        pub use of the children
crates/vault-core/src/crypto/kdf.rs
crates/vault-core/src/crypto/key.rs
crates/vault-core/src/crypto/enc_string.rs
crates/vault-core/src/proto.rs          case-insensitive JSON entry points
crates/vault-core/src/proto/identity.rs prelogin, token, errors
crates/vault-core/src/proto/sync.rs     sync response
crates/vault-core/src/proto/cipher.rs   cipher and folder request/response
crates/vault-core/src/vault.rs         decrypted domain types
crates/vault-core/src/core.rs          Core, Input, Effect, State
crates/vault-core/src/core/request.rs  RequestId, pending-request table
app/src/main.rs                        the App: owns Core, view, update
app/src/shell.rs                       effect runner
```

Dependencies of `vault-core`: `zeroize`, `aes`, `cbc`, `hmac`, `sha2`,
`sha1`, `hkdf`, `pbkdf2`, `argon2`, `rsa`, `base64`, `serde`, `serde_json`,
`uuid`, `rand_core`, `p256` (ECDSA for passkeys). Dev: `proptest`. The app
crate adds `winui-immediate`, `vault-core`, `ureq`, `getrandom`,
`windows-sys` (DPAPI, `webauthn.dll`) and `windows` (KeyCredentialManager,
COM).

## Crypto

```rust
// crypto/key.rs
/// 64-byte AES-256-CBC + HMAC-SHA256 key pair, the shape of the user key,
/// org keys, cipher keys and the stretched master key.
#[derive(Clone, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct SymmetricKey {
    encryption: [u8; 32],
    authentication: [u8; 32],
}

/// The master key before stretching. Never leaves `crypto`.
#[derive(zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
pub struct MasterKey([u8; 32]);

/// PKCS#8 RSA private key, used only to open org keys.
pub struct PrivateKey(rsa::RsaPrivateKey); // rsa zeroizes on drop

/// A secret string from the UI or a decrypted field.
pub type SecretString = zeroize::Zeroizing<String>;

impl SymmetricKey {
    pub fn from_bytes(bytes: &[u8; 64]) -> Self;
    pub fn generate(rng: &mut impl rand_core::CryptoRngCore) -> Self;
}
impl MasterKey {
    pub fn stretch(&self) -> SymmetricKey;        // HKDF-Expand "enc"/"mac"
    pub fn password_hash(&self, password: &str) -> String; // base64, 1 round
}

// crypto/kdf.rs
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kdf {
    Pbkdf2 { iterations: u32 },
    /// `memory_mebibytes` as sent by the server.
    Argon2id { iterations: u32, memory_mebibytes: u32, parallelism: u32 },
}
/// CPU-heavy: the shell runs it off the UI thread.
pub fn derive_master_key(password: &str, email: &str, kdf: Kdf)
    -> Result<MasterKey, KdfError>;

// crypto/enc_string.rs
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncString {
    /// Type 0. Decrypt only: it has no MAC.
    Aes256Cbc { iv: [u8; 16], data: Vec<u8> },
    /// Type 2.
    Aes256CbcHmac { iv: [u8; 16], data: Vec<u8>, mac: [u8; 32] },
    /// Type 3 (OAEP-SHA256) and 4 (OAEP-SHA1).
    Rsa2048Oaep { hash: OaepHash, data: Vec<u8> },
    /// Type 7 and anything else we can parse but not open yet.
    Unsupported { kind: u8, raw: String },
}
impl std::str::FromStr for EncString { type Err = EncStringError; }
impl std::fmt::Display for EncString {} // writes the wire form
impl EncString {
    pub fn encrypt(plaintext: &[u8], key: &SymmetricKey,
                   rng: &mut impl rand_core::CryptoRngCore) -> Self; // type 2
    pub fn decrypt(&self, key: &SymmetricKey)
        -> Result<zeroize::Zeroizing<Vec<u8>>, DecryptError>;
    pub fn decrypt_rsa(&self, key: &PrivateKey)
        -> Result<zeroize::Zeroizing<Vec<u8>>, DecryptError>;
}
```

`decrypt` checks the MAC in constant time before it decrypts. Types 5 and 6
(the RSA variants with a MAC) and type 1 parse to `Unsupported`, so a vault
with one odd item still loads. The UI shows that item as unreadable.

## Proto layer

The server sends JSON keys in either case. `proto::from_slice<T>` parses to
`serde_json::Value`, lowercases every object key, and then deserializes `T`,
whose fields all use `#[serde(rename_all = "lowercase")]` plus
`#[serde(default)]` on every list and optional field. Unknown keys are
ignored.

Editing must not drop data the client doesn't understand, such as
`fido2Credentials`, `archivedDate` or a new cipher type field. So each proto
cipher keeps its original JSON object:

```rust
// proto/cipher.rs
pub struct CipherResponse {
    pub id: uuid::Uuid,
    pub organization_id: Option<uuid::Uuid>,
    pub folder_id: Option<uuid::Uuid>,
    pub kind: u8, // `type`; unknown values are kept, not rejected
    pub revision_date: String,
    pub deleted_date: Option<String>,
    pub key: Option<EncString>,
    pub collection_ids: Vec<uuid::Uuid>,
    pub edit: bool,
    pub view_password: bool,
    /// The whole object as received (original key case), for
    /// write-back of fields this client does not model.
    pub raw: serde_json::Map<String, serde_json::Value>,
}
```

A cipher update builds the request from `raw` and overwrites only the keys
the client edits. The invariant: for any cipher, decode then encode with no
edits sends back every key the server sent.

## Vault model

Decrypted types live in `vault.rs`. Following the threat model, the core
decrypts only what the list needs (name, the login username, the first URI,
the folder and the type) when it unlocks. A full item decrypts when the user
opens it and drops when they close it.

```rust
pub struct VaultIndex {
    pub folders: Vec<FolderSummary>,      // sorted by name
    pub ciphers: Vec<CipherSummary>,      // sorted by name, then id
}
pub struct CipherSummary {
    pub id: uuid::Uuid,
    pub kind: CipherKind,
    pub name: SecretString,
    pub subtitle: SecretString,           // username, card brand, ...
    pub folder_id: Option<uuid::Uuid>,
    pub organization_id: Option<uuid::Uuid>,
    pub favorite: bool,
    pub deleted: bool,
    pub readable: bool,                   // false when a key or field failed
}
pub enum CipherKind { Login, SecureNote, Card, Identity, SshKey, Unknown(u8) }

/// One opened item. Dropped (and wiped) when the item closes.
pub struct CipherView {
    pub id: Option<uuid::Uuid>,           // None while creating
    pub kind: CipherKind,
    pub name: SecretString,
    pub notes: Option<SecretString>,
    pub folder_id: Option<uuid::Uuid>,
    pub favorite: bool,
    pub reprompt: bool,
    pub content: CipherContent,
    pub fields: Vec<CustomField>,
    pub password_history: Vec<PasswordHistoryEntry>,
}
pub enum CipherContent {
    Login(LoginView),
    SecureNote,
    Card(CardView),
    Identity(IdentityView),
    SshKey(SshKeyView),
    Unknown,
}
```

Each `*View` holds `Option<SecretString>` for every string field listed under
"Facts". `LoginView` also holds `uris: Vec<UriView>` with `match` as
`Option<u8>`.

## Cache

The offline cache is one storage slot, `VaultCache`, holding one document:

```rust
// proto/cache.rs, serialized as JSON into StoreSlot::VaultCache
pub struct CacheDocument {
    pub format: u32,                 // 1; a newer format is ignored, not parsed
    pub user_id: uuid::Uuid,
    /// Account revision (epoch ms) this sync reflects: the value of
    /// `GET /api/accounts/revision-date` read just before the sync, or the
    /// server time of our own last write.
    pub revision: u64,
    pub synced_at: u64,              // our clock, for "last synced" in the UI
    /// The `GET /api/sync` body as received, patched by our writes.
    pub sync: serde_json::Value,
}
```

**Writes.** The core emits `Store(VaultCache)` after each successful sync,
with `revision` set to the revision date it fetched first. It also emits one
after each successful cipher or folder write, with the changed object patched
into `sync` and `revision` set to the write response's `revisionDate`. Inside
the core each cipher is its own record (`Vec<CipherResponse>` keyed by id),
and the document is rebuilt from them when it is stored. So a later shell
can store one entry per cipher without changing core logic.

**Freshness.** While unlocked, every `Tick` after five minutes of no check
emits `Http(GET /api/accounts/revision-date)`. A value newer than
`revision` starts a sync. One that isn't newer does nothing. A sync also runs
right after unlock and after a conflict. Push sync, in the next plan, will
make the poll rarer.

**Other writers.** `StorageChanged(VaultCache)` and `Started` compare
`revision`. The higher one wins, and an equal one keeps the in-memory copy. A
document for another `user_id`, or with an unknown `format`, is ignored.

**Shell duties** (stated in this plan so both shells keep them): write each
slot to a temporary file in the same directory, flush it, then rename it over
the old file, so a crash leaves the old or the new file but never half of
one. `VaultCache` is protected storage (see "Decisions to confirm").

## State machine

```rust
// core.rs
pub struct Core {
    config: Config,             // server URLs, device id/name, client name
    state: State,
    requests: request::Table,   // pending RequestId -> what it was for
}

pub enum Input {
    Started { stored: StoredAccount },        // from Effect::Load at boot
    User(UserAction),
    Http { id: RequestId, response: HttpResponse },
    Derived { id: RequestId, result: Result<MasterKey, KdfError> },
    Stored { id: RequestId, result: Result<(), StorageError> },
    Tick,                                     // timer; drives lock timeout
    /// The device key's secret for a challenge (Windows: Hello signature;
    /// macOS: Secure Enclave ECDH), or why it failed.
    DeviceSecret { id: RequestId, result: Result<SecretBytes, DeviceKeyError> },
    /// A decoded MakeCredential or GetAssertion from the passkey plugin.
    Passkey { id: RequestId, request: PasskeyRequest },
    /// Another process wrote a shared slot (`None`: it was cleared).
    StorageChanged { slot: StoreSlot, bytes: Option<Vec<u8>> },
}

pub enum Effect {
    Http { id: RequestId, request: HttpRequest },
    Derive { id: RequestId, password: SecretString, email: String, kdf: Kdf },
    Store { id: RequestId, slot: StoreSlot, bytes: Vec<u8> },
    Clear { slot: StoreSlot },
    ScreenChanged,                            // the shell re-renders
    /// Ask the device key for its secret over `challenge`, behind a
    /// biometric prompt. `create` makes the key first if it is missing.
    DeviceSecret { id: RequestId, challenge: [u8; 32], create: bool },
    /// Answer a passkey request through the platform's provider API.
    PasskeyReply { id: RequestId, reply: Result<PasskeyReply, PasskeyError> },
    /// Tell the platform which passkeys we hold (ids, rp ids, user names only).
    PasskeyCatalog { entries: Vec<PasskeyCatalogEntry> },
}

pub struct HttpRequest {
    pub method: Method,
    pub url: String,
    pub headers: Vec<(&'static str, String)>,
    pub body: Body,                           // None | Json | Form
}
pub struct HttpResponse { pub status: u16, pub body: Vec<u8> }

pub enum StoreSlot {
    Account,       // email, server URLs, KDF, device id: plain JSON
    RefreshToken,  // protected: DPAPI on Windows, Keychain on macOS
    VaultCache,    // a CacheDocument (see "Cache"), protected
    DeviceWrap,    // challenge + user key sealed under the device-key secret
}
impl StoreSlot {
    /// The shell must store this slot with OS protection.
    pub fn protected(self) -> bool; // RefreshToken, DeviceWrap, VaultCache
}

impl Core {
    pub fn new(config: Config) -> Self;
    /// One step. `now` is Unix milliseconds.
    pub fn handle(&mut self, now: u64,
                  rng: &mut impl rand_core::CryptoRngCore,
                  input: Input) -> Vec<Effect>;
    /// What the UI shows. Borrowed, so nothing secret is copied.
    pub fn screen(&self) -> Screen<'_>;
}
```

`State` is one enum, and `handle` is one `match` on it, with each arm doing
its work inline:

```rust
enum State {
    LoggedOut { form: LoginForm, error: Option<LoginError> },
    Prelogin { email: String, password: SecretString, request: RequestId },
    Deriving { email: String, password: SecretString, kdf: Kdf,
               purpose: DerivePurpose, request: RequestId },
    TokenRequest { email: String, master: MasterKey, request: RequestId,
                   second_factor: Option<SecondFactor> },
    SecondFactor { email: String, master: MasterKey,
                   providers: Vec<TwoFactorProvider>, error: Option<String> },
    Locked { account: Account, error: Option<UnlockError> },
    Unlocked(Box<Unlocked>),
}

struct Unlocked {
    account: Account,           // tokens, profile, the cache as received
    user_key: SymmetricKey,
    org_keys: Vec<(uuid::Uuid, SymmetricKey)>,
    index: VaultIndex,
    open: Option<OpenItem>,     // the CipherView being viewed or edited
    sync: SyncState,            // Idle | Running(RequestId) | Failed
    writes: Vec<PendingWrite>,  // at most one per cipher id
    last_activity: u64,
}
```

Field owners and lifetimes:

| Field | Written by | Read by | Lifetime |
|---|---|---|---|
| `password` | the login arm | `Effect::Derive` | until `Derived` arrives; then dropped and wiped |
| `master` | the `Derived` arm | token request, stretch | until the user key decrypts |
| `user_key`, `org_keys` | unlock | decrypt, encrypt | until lock or logout |
| `index` | unlock, each sync | `screen()` | until lock |
| `open` | open, edit | `screen()`, save | until the item closes or the vault locks |
| `account.tokens` | token, refresh | every API request | until logout |

**Stale responses.** Every request gets a fresh `RequestId` from a counter.
A state that waits names the id it waits for. A response whose id is not in
`requests`, or that the current state does not name, is dropped. So a slow
sync that returns after a lock can't write decrypted data into `Locked`, and a
second login attempt ignores the first one's token response.

**Lock** replaces `Unlocked` with `Locked`. Dropping `Unlocked` drops
(and wipes) every key, the index and the open item. Pending responses for the
unlocked session are then dropped by the stale rule. Lock happens on a user
action, on `Tick` after the timeout, and on a `401` that refresh can't fix.

**Token refresh.** Before an API request the core checks `expires_at`. If it
is within 60 s, the core sends the refresh first and queues the API request
in `Account::waiting`. A `401` response triggers one refresh and one retry.
A failed refresh moves to `LoggedOut`, keeping the email.

## Transactions

**Login with 2FA.** The user submits the email and password. The core emits
`Http(prelogin)` and moves to `Prelogin`. The response gives the KDF, so the
core emits `Derive` and moves to `Deriving`. `Derived` gives the master key,
and the core emits `Http(token)` with the password hash. The 400 with
`TwoFactorProviders2` moves to `SecondFactor` with the providers. The user
enters the code, the core resends the token request with `twoFactorToken`,
and the 200 gives the tokens and `Key`. The core decrypts the user key with
`master.stretch()`, drops `master`, emits `Store(Account)`,
`Store(RefreshToken)` and `Http(sync)`, and moves to `Unlocked` with an empty
index and `sync: Running`.

**Unlock offline.** At boot the shell loads the three slots and sends
`Started`. With a cache and a refresh token present, the core moves to
`Locked`. The user types the password, the core emits `Derive` with the
stored KDF, and `Derived` gives the master key. The core decrypts
`profile.key` from the cache. A MAC failure means a wrong password, so it
returns to `Locked` with `UnlockError::WrongPassword`. Success builds the index
from the cache and emits `Http(sync)` in the background. An offline sync
failure leaves the cached index in place and shows a banner.

**Edit with a conflict.** The user saves an edited login. The core encrypts
the `CipherView` with the cipher's key, builds the body from `raw` with
`lastKnownRevisionDate`, emits `Http(PUT)`, and records a `PendingWrite`.
A 400 "out of date" makes the core keep the user's `CipherView`, run a sync,
and then show a conflict screen with both versions. The user picks one, and
saving again uses the new revision date. A 200 replaces the cipher in the
cache and the index and emits `Store(VaultCache)`.

**Sync while editing.** A sync result that changes the cipher open for
editing doesn't replace `open`. The core marks it `stale`, and the save path
then fails as a conflict, as above.

## Device unlock (Windows Hello now, Touch ID later)

Device unlock replaces typing the master password, but it keeps nothing in
memory while locked. Each unlock asks the device key for a fresh secret over a
stored challenge. The core needs only one property from the shell: the same
challenge always gives the same secret, and only after a biometric or PIN
check. On Windows the secret is a Hello KeyCredential signature (RSA
PKCS#1 v1.5). On macOS it will be a Secure Enclave ECDH result, because
Secure Enclave signatures are randomized.

```rust
// crypto/key.rs
/// HKDF-SHA256(salt = "vault-core device v1", ikm = secret) -> 64 bytes.
pub fn device_key(secret: &[u8]) -> SymmetricKey;

// core.rs, stored as JSON in StoreSlot::DeviceWrap (protected storage)
struct DeviceWrap {
    user_id: uuid::Uuid,
    challenge: [u8; 32],
    /// Type 2 EncString of the 64-byte user key under `device_key(secret)`.
    sealed_user_key: EncString,
}
```

**Enroll** (only while unlocked). The core draws a 32-byte challenge from
`rng` and emits `DeviceSecret { create: true }`. The shell creates or opens
the device key and returns the secret. The core then asks for a second
secret over the same challenge. If the two differ, the platform's secret
isn't deterministic, so enrollment fails with
`DeviceKeyError::NotDeterministic` and stores nothing. Otherwise the core
seals the user key and emits `Store(DeviceWrap)`.

**Unlock.** `Locked` with a `DeviceWrap` present shows a device-unlock button
(the shell labels it "Windows Hello" or "Touch ID"). The core emits
`DeviceSecret { challenge, create: false }`. The secret gives `device_key`,
which decrypts `sealed_user_key`, and the core then runs the same path as a
password unlock. A MAC failure (a biometric reset, a TPM clear, a changed
key) emits `Clear(DeviceWrap)` and asks for the master password. The secret
and the derived key are `Zeroizing` and drop at the end of the arm.

**Invalidation.** Logout, a changed master password or KDF (the profile key
changes), and the user turning device unlock off all emit
`Clear(DeviceWrap)`.

**Windows shell** (`app/src/shell/device_key.rs`). The app requires Windows 11
24H2 (build 26100) or later, so it uses only the window-aware calls:
`KeyCredentialManager::RequestCreateForWindowAsync(WindowId, name, option)`
and then `KeyCredential::RequestDeriveSharedSecretAsync(WindowId, message,
challenge)` as the secret. A derived shared secret is meant to be the same
for the same input, unlike a signature that only happens to be. The Hello
prompt is owned by our window, so it opens in front with focus and needs no
polling. The shell gets the `WindowId` from the main window's
`WindowResponse` (`AppWindow.Id`), so it needs no window search.
Credential name: "BitwardenWinUI". The core's check that two secrets match
at enroll stays, because it costs one extra prompt and guards the property
the design rests on.

**macOS shell (later).** A Secure Enclave P-256 key with
`.biometryCurrentSet`. The challenge seeds a fixed peer key, and
`SecKeyCopyKeyExchangeResult` gives the secret. The core is unchanged.

## Passkey provider (Windows plugin now, macOS extension later)

The app registers as the platform's passkey provider, so browsers and apps
can create and use the passkeys in our vault. On Windows the shell owns the
plugin COM server and all of `webauthn.dll`. The core owns everything else:
picking the credential, the user check, P-256 signing, the CBOR of
authenticator data and attestation, and saving new passkeys as cipher edits.
That way a macOS shell maps the same `PasskeyRequest` and `PasskeyReply` to
AuthenticationServices without new crypto.

```rust
// vault.rs
pub struct Fido2CredentialView {
    pub credential_id: SecretString,       // GUID text, as Bitwarden stores it
    pub rp_id: SecretString,
    pub rp_name: Option<SecretString>,
    pub user_handle: Option<SecretString>, // base64url
    pub user_name: Option<SecretString>,
    pub user_display_name: Option<SecretString>,
    pub counter: u32,
    pub discoverable: bool,
    pub key: crypto::P256SigningKey,       // from keyValue, zeroized on drop
    pub creation_date: String,
}

// core/passkey.rs
pub enum PasskeyRequest {
    MakeCredential {
        rp_id: String, rp_name: Option<String>,
        user_handle: Vec<u8>, user_name: String, user_display_name: String,
        client_data_hash: [u8; 32], require_resident: bool,
        exclude: Vec<Vec<u8>>,               // credential ids
    },
    GetAssertion {
        rp_id: String, client_data_hash: [u8; 32],
        allow: Vec<Vec<u8>>,                 // empty: discoverable flow
    },
}
pub enum PasskeyReply {
    Created { credential_id: Vec<u8>, authenticator_data: Vec<u8>,
              public_key_cose: Vec<u8>,
              /// CBOR {fmt: "none", attStmt: {}, authData}; macOS needs it,
              /// Windows may re-encode from the parts.
              attestation_object: Vec<u8> },
    Asserted { credential_id: Vec<u8>, authenticator_data: Vec<u8>,
               signature: Vec<u8>, user_handle: Option<Vec<u8>> },
}
pub struct PasskeyCatalogEntry {
    pub credential_id: Vec<u8>, pub rp_id: String,
    pub user_name: String, pub user_display_name: String,
}
```

`authenticator_data` is built in the core: SHA-256 of `rp_id`, the flags
UP | UV (| AT with attested credential data on create), and the counter.
The signature is ECDSA P-256 over `authenticator_data || client_data_hash`,
using the `p256` crate (a pure dependency). The core also writes the small
fixed CBOR it needs: the COSE public key and the "none" attestation object,
in `core/passkey/cbor.rs`, with no CBOR crate. On Windows the shell's
`WebAuthNEncode*Response` helpers wrap the parts for the plugin API.

**Locked or logged out.** A request that arrives while `Locked` moves to a
`PasskeyPending` sub-state that keeps the request and shows the unlock screen
(device unlock or password). After unlock, the core answers it. Cancel, or
no unlock within 120 s, replies `PasskeyError::NotAllowed`.

**User verification.** Every request asks for a biometric check before it
signs, even when unlocked, so the flag UV is true. That needs
`Effect::PasskeyVerify` and `Input::PasskeyVerified`, with the same
`RequestId` rules. Windows runs `WebAuthNPluginPerformUserVerification`.
macOS will use the extension's own Touch ID prompt.

**Writes.** MakeCredential adds a `fido2Credentials` entry to an existing
login whose URI matches `rp_id` (the user picks one), or to a new login. It
saves through the normal cipher write path, and replies only after the
server returns 200. GetAssertion bumps `counter`, replies at once, and
queues the counter write. A conflict on that write is resolved by keeping
the higher counter.

**Catalog.** After each sync or passkey change, the core emits
`PasskeyCatalog` with ids, rp ids and user names only, never keys. The shell
calls `WebAuthNPluginAuthenticatorAddCredentials` so Windows can list our
passkeys in its picker. Lock keeps the catalog, because it holds no secrets,
and logout clears it.

**Packaging.** The plugin needs package identity. Plan for a sparse package
(an `AppxManifest.xml` with `com:ExeServer`, the plugin extension and our
exe), signed with a test certificate for development. The same exe runs as
the COM server. A running app registers its class object with
`CoRegisterClassObject(REGCLS_MULTIPLEUSE)`, so COM sends requests into the
process that already holds the single `Core`. A cold start with `-Embedding`
launches the app hidden until it needs the unlock screen. So on Windows
there is one process and one unlock state.

**Second process (macOS).** An `ASCredentialProviderExtension` runs its own
`Core` in its own process. The core supports this with no special mode: it
starts from `Input::Started` with the shared storage slots, unlocks by
device key or password, answers the request, and queues the write as usual.
A write from the extension and an edit in the app can collide. The normal
revision conflict path handles that, and for counter writes the higher
counter wins. The core never assumes it is the only writer of the cache. The
shell watches the shared slots (a file watcher on Windows, a Darwin
notification on macOS) and sends `Input::StorageChanged { slot, bytes }`. The
core takes the `VaultCache` with the higher `CacheDocument::revision`, and a
changed
`DeviceWrap` or a cleared `RefreshToken` locks or logs out. Windows needs this
too, because a second app instance can't be ruled out.

## Fit with winui-immediate and the threat model

The shell is the `Vault` type, which implements `winui_immediate::App`. It
owns `Core` as a field, following "one owner per resource", and keeps the
`Handle<Msg>` it gets from `run`'s start callback. `update(Msg::Core(input))`
calls `core.handle(now, &mut OsRng, input)` and passes each effect to
`shell::run(effect, handle)`. That submits a thread-pool job, so each HTTP
call, file write or KDF runs off the UI thread, and the job returns its
result with `handle.send(Msg::Core(Input::...))`. The HTTP agent
(`ureq::Agent`) is created once and leaked as `&'static`. A message that holds
a secret wipes itself on drop, because the mailbox drops queued messages when
the last window closes.

`view` reads `core.screen()` directly, with no `Rc<str>` copies: list rows
read their entry from `&mut Vault` by index (`RowFn`). Decrypted names still
reach the WinUI `TextBlock`s of realized rows, which the unlocked-state
best-effort rule accepts, and lock drops them because the locked screen
declares no list. The login password stays in the `PasswordBox` (threat
model option A): on submit, the view calls `ui.take_password`, which reads it
once and clears the box, and sends it as `UserAction::Submit { password:
SecretString }` into `State::Prelogin`. `LoginForm` holds only the email.

## Tests

1. `crypto::kdf` known answers: PBKDF2 and Argon2id master key, master
   password hash and stretched key against vectors copied from Bitwarden's
   test suites (`bitwarden/clients` and `sdk-internal`). Each vector is its
   own named test.
2. `enc_string` round trip (proptest): for any key, IV and plaintext,
   `decrypt(encrypt(p)) == p`, and `parse(format(e)) == e`. The oracle is the
   type 2 layout written out by hand in the test, not the formatter.
3. `enc_string` tamper (proptest): flipping any bit of IV, data or MAC makes
   `decrypt` fail with `DecryptError::Mac`.
4. `enc_string` known answers: decrypt a type 2 and a type 4 sample from
   Bitwarden's tests. Parse type 0, 5, 6 and 7 samples to the right variant.
5. `proto` case tolerance (proptest): for a sync fixture, any random mix of key
   case deserializes to the same value.
6. `proto` raw round trip (proptest): for a cipher with random extra keys, the
   update body built with no edits contains every key and value of `raw`.
7. `core` login paths, each a named test that drives `handle` and asserts
   the effects: plain login, 2FA by authenticator, 2FA by email, new-device
   OTP, wrong password (400 `invalid_grant`), and API key then unlock.
8. `core` stale responses (proptest): for random interleavings of login,
   lock, sync and their responses, no response for an old `RequestId`
   changes the state, and after `Lock` the state holds no key.
9. `core` refresh: an expired token queues the request, and a `401` refreshes
   once and retries once. A second `401` logs out.
10. `core` edit: save, a 200, cache updated; save, a 400 out of date, then
    sync, then a conflict screen with both versions.
11. `core` device unlock: enroll seals the user key, and a matching secret
    unlocks. Different secrets at enroll store nothing. A secret that fails
    the MAC clears `DeviceWrap` and asks for the password. After lock, the
    state holds no key (shared with test 8).
12. `core` passkey create (proptest over rp ids and user handles): the reply's
    public key verifies the next assertion. The authenticator data decodes
    to SHA-256(rp_id), flags UP|UV|AT and counter 0. The oracle parses the
    bytes with its own layout code. The new credential is saved through a
    cipher write before the reply.
13. `core` passkey assert: picks by `allow` list, or offers discoverable
    credentials for the rp id. The signature verifies with `p256` over
    `authenticator_data || client_data_hash`. The counter increases. While
    locked, the request waits for unlock and times out with `NotAllowed`.
14. `fido2Credentials` round trip: decrypt, then re-encrypt with no edits,
    gives the same plaintexts. Fixture from step 1.
15. Two cores, one storage (proptest over interleavings): core A writes a
    cipher, the harness feeds B `StorageChanged`, and B's index shows the
    new revision. Concurrent counter writes end at the higher counter.
    Neither core ever applies an older cache over a newer one.
18. Cache freshness: with `revision` = R, a `Tick` past five minutes emits
    one revision-date request. A reply ≤ R emits nothing, and a reply > R
    emits a sync. Two `Tick`s inside five minutes emit one request.
19. Cache revision rule (proptest over sequences of `StorageChanged` and
    sync results): the kept document's `revision` never decreases, and
    documents with another `user_id` or an unknown `format` never replace it.
20. Cache document round trip: a cipher write patches `sync` so that
    re-parsing the stored document gives the same `CipherResponse` set as
    the core holds in memory, and the write response's `revisionDate`
    becomes `revision`.
16. Platform-neutral API: a compile check (`cargo check -p vault-core`)
    plus `cargo tree -p vault-core` showing no `windows*`, `objc2*` or HTTP
    crate.
17. Integration (`#[ignore]`, run by hand): a recorded session against a
    Vaultwarden test server, replayed through `handle` from fixtures.

## Steps

1. Check the protocol facts against `bitwarden/clients` and
   `vaultwarden` source, and save the sample JSON for sync, token and errors
   as fixtures in `crates/vault-core/tests/fixtures/`. Record the source
   commit of each fixture. Fix "Facts" where they differ.
2. Workspace and `crypto`: tests 1–4 and 16. Test 16 then runs in every
   later step.
3. `proto`: tests 5–6, on the step 1 fixtures.
4. `vault` and the decrypt path from proto to `VaultIndex` and `CipherView`,
   with fixture tests for each cipher type.
5. `core` login, unlock, lock and sync: tests 7–9.
6. `core` writes, conflicts, the cache and `StorageChanged`: tests 10, 15,
   18, 19 and 20.
7. The `app` crate: `app/src/shell.rs` and the `App` wired to the core, a
   login screen and the list from `winui-immediate/examples/vault_list.rs`. Manual check against a test server, then test 17.
8. `core` device-key enroll and unlock: test 11. `app/src/shell/device_key.rs` with
   KeyCredentialManager. Manual check: enroll, lock, unlock with Hello, the
   prompt opens in front with focus, and the derived secret is the same
   across two unlocks and an app restart on this machine.
9. `core` passkeys: tests 12–14.
10. Sparse package, COM server and `app/src/shell/passkey.rs` with
    `webauthn.dll`. Manual check: register a passkey on webauthn.io in Edge,
    sign in with it, and see the item in the vault after sync.

## Work breakdown

### WP-01: protocol fixtures

Status: open  
Owner: subagent  
Depends on: none

Scope: `crates/vault-core/tests/fixtures/*.json`, "Facts" in this plan.

Deliverable: fixtures for prelogin, token (success, 2FA, new device, wrong
password), sync (every cipher type, an org cipher, a cipher with `key`),
and the cipher write responses, each with its source URL and commit.

Acceptance: each fixture parses as JSON. Each fact in "Facts" cites a source
line or is marked unverified.

Handoff: fixture paths and a list of facts changed.

### WP-02: crypto

Status: open  
Owner: subagent  
Depends on: none

Scope: workspace `Cargo.toml`, `crates/vault-core/src/crypto*`.

Deliverable: the crypto module of this plan.

Acceptance: `cargo test -p vault-core crypto` passes tests 1–4, and
`cargo tree -p vault-core` lists no HTTP, async or file crate.

Handoff: changed paths, test output.

### WP-03: proto and vault

Status: open  
Owner: subagent  
Depends on: WP-01, WP-02

Scope: `crates/vault-core/src/proto*`, `vault.rs`.

Deliverable: proto types and decrypt to `VaultIndex` and `CipherView`.

Acceptance: tests 5–6 and the step 4 fixture tests pass.

Handoff: changed paths, test output.

### WP-04: core state machine

Status: open  
Owner: main agent  
Depends on: WP-03

Scope: `crates/vault-core/src/core*`.

Deliverable: `Core`, `Input`, `Effect`, `State` and `handle`.

Acceptance: tests 7–10, 15, 16 and 18–20 pass.

Handoff: changed paths, test output.

### WP-05: shell

Status: open  
Owner: main agent  
Depends on: WP-04

Scope: the new `app` crate (`app/Cargo.toml`, `app/src/main.rs`,
`app/src/shell.rs`), a workspace member that depends on `winui-immediate`
and `vault-core`.

Deliverable: the app logs in, syncs, lists and edits against a test server.

Acceptance: a manual run against the test server, then test 17.

Handoff: the run log and screenshots.

### WP-06: device unlock (Windows Hello)

Status: open  
Owner: main agent  
Depends on: WP-05

Scope: device-key arms in `crates/vault-core/src/core.rs`, `device_key` in
`crypto/key.rs`, `app/src/shell/device_key.rs`.

Deliverable: enroll and unlock with Hello, with nothing kept while locked.

Acceptance: test 11 passes. Manual run: enroll, lock, unlock with Hello. A
memory scan after lock (the probe from `spikes/src/bin/password_probe.rs`) finds
no user key bytes.

Handoff: changed paths, test output, the scan result.

### WP-07: passkey core

Status: open  
Owner: subagent  
Depends on: WP-04

Scope: `crates/vault-core/src/core/passkey.rs`, `Fido2CredentialView` in
`vault.rs`, P-256 helpers in `crypto`.

Deliverable: MakeCredential, GetAssertion, the catalog and the locked wait.

Acceptance: tests 12–14 pass.

Handoff: changed paths, test output.

### WP-08: passkey provider shell and packaging

Status: open  
Owner: main agent  
Depends on: WP-05, WP-06, WP-07

Scope: `app/src/shell/passkey.rs`, `packaging/AppxManifest.xml`, the
registration script.

Deliverable: the app shows up as a passkey provider in Windows Settings and
serves create and sign-in on webauthn.io.

Acceptance: the manual check from step 10.

Handoff: the run log, screenshots, the Windows build number.

Waves: WP-01 and WP-02 together, then WP-03, then WP-04, then WP-05 and
WP-07 together, then WP-06, then WP-08.

## Decisions to confirm

- **Protect the cache file.** Recommended: yes, so `VaultCache` is protected
  storage (DPAPI on Windows, Keychain-backed key on macOS). Its secrets are
  already EncStrings, but ids, types, folder and org membership, dates,
  `favorite`, counts, the email and the KDF are readable. Protection costs
  one DPAPI call per read and write and hides them from other users and disk
  images, though not from our own user. If not, `protected()` drops
  `VaultCache`.

- **Test server.** Run Vaultwarden on one of the NixOS hosts, configured in
  `/share/infra/machines/<host>`, for WP-05 and test 11. Also test against a
  free Bitwarden cloud account by hand before release. Otherwise the
  integration test runs only against Vaultwarden.
- **Type 7 (COSE).** Parse it as `Unsupported` for now. Bitwarden cloud may
  start issuing it, and adding decrypt then is a crypto change only.
- **New ciphers get their own `key`.** Recommended: no, encrypt with the user
  or org key, but keep an existing cipher `key` on edit. Bitwarden's newer
  clients create item keys when the server allows it, and this can come later.
- **Eager versus lazy decryption.** Recommended: decrypt the index on unlock
  and full items on open, as above. The other choice decrypts everything at
  unlock, which is simpler but keeps every secret in memory while unlocked.

- **Signing certificate.** The sparse package needs a signed manifest.
  Recommended: a self-signed test certificate for development, and a
  decision on a real code-signing certificate before release.
- **Where new passkeys go.** Recommended: offer the logins whose URI host
  matches `rp_id`, plus "new login". Bitwarden does the same. The other
  choice always creates a new login, which is simpler but clutters the vault.

## Settled

- **Write our own sans-io core and crypto on RustCrypto** (2026-11-20).
  `bitwarden-vault` and `bitwarden-core` do I/O and are async.
  `bitwarden-crypto` is pure but would make the app GPL-3.0, churns, and is
  labelled internal. Bitwarden's code stays a reference and a source of test
  vectors.
- **Support Bitwarden and Vaultwarden servers** (2026-11-20). This means
  case-insensitive JSON and missing-field tolerance from the start.
- **Keep an encrypted vault cache on disk** (2026-11-20). It stores the sync
  response as received, so offline unlock works.
- **First delivery covers reading and editing** (2026-11-20).
- **Windows Hello unlock uses a key derived from a signature** (2026-11-20).
  Every unlock signs the stored challenge, and nothing stays in memory while
  locked. Bitwarden's yes/no prompt over a key kept in memory breaks the
  threat model. The core sees only a generic device secret, so macOS can use
  Secure Enclave ECDH, because Secure Enclave signatures are randomized.
- **One file per storage slot, no SQLite for now** (2026-11-20). Every field
  we'd query is encrypted, the data is a few MB read in one go at unlock,
  and a database handle doesn't fit a sans-io core. Atomic temp-then-rename
  writes and a revision stamp cover the needs. Revisit when the macOS passkey
  extension and the app both write the cache: if file coordination gets
  painful, a SQLite store in the shell, with one entry per cipher, can
  replace the files without changing the core.
- **Windows 11 24H2 (build 26100) or later only** (2026-11-20). No support
  for older builds. Hello uses only the window-aware calls and needs no
  polling to bring the prompt to the front. The passkey provider needs no
  runtime feature detection. If the plugin API turns out to need a later
  update than 24H2, that update becomes the minimum.
- **The core stays open to a macOS app** (2026-11-20). It names no platform,
  never assumes one process, and builds the passkey CBOR itself. It keeps
  Rust borrows (`Screen<'_>`, `&mut impl CryptoRngCore`). A later
  `vault-core-ffi` crate copies the screen into owned values for a SwiftUI
  shell. Writing the Mac UI in Rust through `objc2` was rejected, because it
  rules out SwiftUI and needs hand-built bundles and `define_class!`
  boilerplate.
- **Passkeys: the Windows passkey provider is in scope** (2026-11-20). Login
  to Bitwarden by passkey is out: Bitwarden enables it only for web and the
  extension, and Vaultwarden likely lacks it. Decoding `fido2Credentials` comes
  with the provider.
