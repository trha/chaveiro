//! Symmetric, master and private keys.

use base64::{Engine, engine::general_purpose::STANDARD};
use hkdf::Hkdf;
use p256::{
    ecdsa::{Signature, signature::Signer},
    pkcs8::EncodePrivateKey,
};
use rand_core::CryptoRngCore;
use rsa::pkcs8::DecodePrivateKey;
use sha2::Sha256;
use std::array;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

/// 64-byte AES-256-CBC + HMAC-SHA256 key pair: the shape of the user key,
/// org keys, cipher keys and the stretched master key.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SymmetricKey {
    pub(super) encryption: [u8; 32],
    pub(super) authentication: [u8; 32],
}

impl SymmetricKey {
    /// Takes the encryption key from the first 32 bytes and the MAC key from
    /// the last 32, the layout of a decrypted user, org or cipher key.
    pub fn from_bytes(bytes: &[u8; 64]) -> Self {
        Self {
            encryption: array::from_fn(|index| bytes[index]),
            authentication: array::from_fn(|index| bytes[32 + index]),
        }
    }

    /// Draws a fresh key from `rng`.
    pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
        let mut bytes = Zeroizing::new([0u8; 64]);
        rng.fill_bytes(&mut *bytes);
        Self::from_bytes(&bytes)
    }

    /// The layout [`from_bytes`](Self::from_bytes) reads: encryption key,
    /// then MAC key.
    pub fn to_bytes(&self) -> Zeroizing<[u8; 64]> {
        Zeroizing::new(array::from_fn(|index| match index {
            0..32 => self.encryption[index],
            _ => self.authentication[index - 32],
        }))
    }
}

/// The key that seals the user key for device unlock: HKDF-SHA256 of
/// `secret` with salt `chaveiro-core device v1` and empty info, 64 bytes split
/// as in [`SymmetricKey::from_bytes`]. The same secret always gives the same
/// key.
pub fn device_key(secret: &[u8]) -> SymmetricKey {
    let hkdf = Hkdf::<Sha256>::new(Some(b"chaveiro-core device v1"), secret);
    let mut bytes = Zeroizing::new([0u8; 64]);
    hkdf.expand(&[], &mut *bytes)
        .expect("64 bytes is below the HKDF-SHA256 output limit");
    SymmetricKey::from_bytes(&bytes)
}

/// The master key before stretching. Never leaves `crypto`.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct MasterKey(pub(super) [u8; 32]);

impl MasterKey {
    /// The key that wraps the user key: HKDF-Expand-SHA256 of the master key
    /// with info `enc` for the encryption half and `mac` for the MAC half.
    pub fn stretch(&self) -> SymmetricKey {
        // Bitwarden uses the master key as the PRK directly and skips HKDF-Extract.
        let hkdf = Hkdf::<Sha256>::from_prk(&self.0)
            .expect("a 32-byte PRK is as long as the SHA-256 output");
        let mut key = SymmetricKey {
            encryption: [0; 32],
            authentication: [0; 32],
        };
        hkdf.expand(b"enc", &mut key.encryption)
            .expect("32 bytes is below the HKDF-SHA256 output limit");
        hkdf.expand(b"mac", &mut key.authentication)
            .expect("32 bytes is below the HKDF-SHA256 output limit");
        key
    }

    /// The master password hash the server checks at login: base64 of one
    /// round of PBKDF2-SHA256 with the master key as password and `password`
    /// as salt.
    pub fn password_hash(&self, password: &str) -> String {
        let mut hash = Zeroizing::new([0u8; 32]);
        pbkdf2::pbkdf2_hmac::<Sha256>(&self.0, password.as_bytes(), 1, &mut *hash);
        STANDARD.encode(*hash)
    }
}

/// PKCS#8 RSA private key, used only to open org keys.
pub struct PrivateKey(pub(super) rsa::RsaPrivateKey); // rsa zeroizes on drop

impl PrivateKey {
    /// Parses a PKCS#8 DER private key, the plaintext of the account's
    /// encrypted `privateKey`.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self, rsa::pkcs8::Error> {
        rsa::RsaPrivateKey::from_pkcs8_der(der).map(Self)
    }
}

/// The ECDSA P-256 private key of a passkey. Each copy is zeroized on drop.
#[derive(Clone)]
pub struct P256SigningKey(p256::ecdsa::SigningKey);

impl P256SigningKey {
    /// Draws a fresh key from `rng`.
    pub fn generate(rng: &mut impl CryptoRngCore) -> Self {
        Self(p256::ecdsa::SigningKey::random(rng))
    }

    /// Parses a PKCS#8 DER private key, the decoded `keyValue` of a stored
    /// passkey.
    pub fn from_pkcs8_der(der: &[u8]) -> Result<Self, p256::pkcs8::Error> {
        p256::ecdsa::SigningKey::from_pkcs8_der(der).map(Self)
    }

    /// The key as PKCS#8 DER, with the public key inside, as Bitwarden
    /// stores it.
    pub fn to_pkcs8_der(&self) -> Zeroizing<Vec<u8>> {
        let document = p256::SecretKey::from(&self.0)
            .to_pkcs8_der()
            .expect("a valid P-256 scalar always encodes");
        Zeroizing::new(document.as_bytes().to_vec())
    }

    /// The affine `x` and `y` of the public key, big-endian.
    pub fn public_xy(&self) -> ([u8; 32], [u8; 32]) {
        let point = self.0.verifying_key().to_encoded_point(false);
        let x = point.x().expect("an uncompressed point has x");
        let y = point.y().expect("an uncompressed point has y");
        (
            array::from_fn(|index| x[index]),
            array::from_fn(|index| y[index]),
        )
    }

    /// The DER ECDSA signature over SHA-256 of `message` (WebAuthn ES256).
    pub fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature: Signature = self.0.sign(message);
        signature.to_der().as_bytes().to_vec()
    }
}

/// A secret string from the UI or a decrypted field.
pub type SecretString = Zeroizing<String>;
