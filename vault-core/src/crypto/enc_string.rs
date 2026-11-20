//! Bitwarden's encrypted string wire format, `<type>.<part>|<part>|...`.

use super::{PrivateKey, SymmetricKey};
use aes::Aes256;
use base64::{Engine, engine::general_purpose::STANDARD};
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
use hmac::{Hmac, Mac};
use rand_core::CryptoRngCore;
use rsa::Oaep;
use sha2::Sha256;
use std::{fmt, str::FromStr};
use zeroize::Zeroizing;

/// An encrypted string as the server stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncString {
    /// Type 0. Decrypt only: it has no MAC.
    Aes256Cbc { iv: [u8; 16], data: Vec<u8> },
    /// Type 2.
    Aes256CbcHmac {
        iv: [u8; 16],
        data: Vec<u8>,
        /// HMAC-SHA256 over `iv || data`.
        mac: [u8; 32],
    },
    /// Type 3 (OAEP-SHA256) and 4 (OAEP-SHA1).
    Rsa2048Oaep { hash: OaepHash, data: Vec<u8> },
    /// Type 7 and anything else we can parse but not open yet: types 1, 5
    /// and 6, and any other number. The `proto` readers also keep a
    /// malformed string here, so it never fails a whole response.
    Unsupported {
        kind: u8,
        /// The whole string as received, which [`Display`](fmt::Display)
        /// writes back.
        raw: String,
    },
}

impl EncString {
    /// Encrypts `plaintext` as type 2 with a fresh IV from `rng`.
    pub fn encrypt(plaintext: &[u8], key: &SymmetricKey, rng: &mut impl CryptoRngCore) -> Self {
        let mut iv = [0u8; 16];
        rng.fill_bytes(&mut iv);
        let data = cbc::Encryptor::<Aes256>::new_from_slices(&key.encryption, &iv)
            .expect("AES-256 takes a 32-byte key and a 16-byte IV")
            .encrypt_padded_vec_mut::<Pkcs7>(plaintext);
        let mac = authenticator(key, &iv, &data)
            .finalize()
            .into_bytes()
            .into();
        Self::Aes256CbcHmac { iv, data, mac }
    }

    /// Decrypts type 2. It checks the MAC in constant time first and fails
    /// with [`DecryptError::Mac`] before it touches the ciphertext.
    ///
    /// Type 0 fails with [`DecryptError::WrongType`] like every other type,
    /// because a key with a MAC half must never accept a string without a
    /// MAC: a hostile server could otherwise strip the MAC from a type 2
    /// string and feed us chosen ciphertext. Bitwarden's SDK has the same
    /// rule. [`decrypt_legacy`](Self::decrypt_legacy) is the only way to
    /// open type 0.
    pub fn decrypt(&self, key: &SymmetricKey) -> Result<Zeroizing<Vec<u8>>, DecryptError> {
        let Self::Aes256CbcHmac { iv, data, mac } = self else {
            return Err(DecryptError::WrongType);
        };
        authenticator(key, iv, data)
            .verify_slice(mac)
            .map_err(|_| DecryptError::Mac)?;
        cbc_decrypt(&key.encryption, iv, data)
    }

    /// Decrypts type 0 with a bare 32-byte AES key, the legacy key shape
    /// that has no MAC half. Other types fail with
    /// [`DecryptError::WrongType`].
    ///
    /// Nothing proves the data is authentic. Call it only for data that
    /// Bitwarden created as type 0 under a legacy key, never as a fallback
    /// when [`decrypt`](Self::decrypt) fails.
    pub fn decrypt_legacy(
        &self,
        encryption_key: &[u8; 32],
    ) -> Result<Zeroizing<Vec<u8>>, DecryptError> {
        let Self::Aes256Cbc { iv, data } = self else {
            return Err(DecryptError::WrongType);
        };
        cbc_decrypt(encryption_key, iv, data)
    }

    /// Decrypts type 3 or 4. Other types fail with
    /// [`DecryptError::WrongType`].
    pub fn decrypt_rsa(&self, key: &PrivateKey) -> Result<Zeroizing<Vec<u8>>, DecryptError> {
        let Self::Rsa2048Oaep { hash, data } = self else {
            return Err(DecryptError::WrongType);
        };
        let padding = match hash {
            OaepHash::Sha256 => Oaep::new::<Sha256>(),
            OaepHash::Sha1 => Oaep::new::<sha1::Sha1>(),
        };
        key.0
            .decrypt(padding, data)
            .map(Zeroizing::new)
            .map_err(|_| DecryptError::Rsa)
    }
}

impl FromStr for EncString {
    type Err = EncStringError;

    /// Parses the wire form. A string with no `<type>.` header is a legacy
    /// one: two parts make type 0 and three make type 1.
    fn from_str(text: &str) -> Result<Self, EncStringError> {
        let (kind, body) = match text.split_once('.') {
            Some((header, body)) => (
                header.parse::<u8>().map_err(|_| EncStringError::Header)?,
                body,
            ),
            None => match text.split('|').count() {
                2 => (0, text),
                3 => (1, text),
                _ => return Err(EncStringError::Header),
            },
        };
        let parts: Vec<&str> = body.split('|').collect();
        match (kind, parts.as_slice()) {
            (0, [iv, data]) => Ok(Self::Aes256Cbc {
                iv: decode_array(iv)?,
                data: decode(data)?,
            }),
            (2, [iv, data, mac]) => Ok(Self::Aes256CbcHmac {
                iv: decode_array(iv)?,
                data: decode(data)?,
                mac: decode_array(mac)?,
            }),
            (3, [data]) => Ok(Self::Rsa2048Oaep {
                hash: OaepHash::Sha256,
                data: decode(data)?,
            }),
            (4, [data]) => Ok(Self::Rsa2048Oaep {
                hash: OaepHash::Sha1,
                data: decode(data)?,
            }),
            (0 | 2 | 3 | 4, _) => Err(EncStringError::PartCount),
            _ => Ok(Self::Unsupported {
                kind,
                raw: text.to_owned(),
            }),
        }
    }
}

impl fmt::Display for EncString {
    /// Writes the wire form with a `<type>.` header.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Aes256Cbc { iv, data } => {
                write!(
                    formatter,
                    "0.{}|{}",
                    STANDARD.encode(iv),
                    STANDARD.encode(data)
                )
            }
            Self::Aes256CbcHmac { iv, data, mac } => write!(
                formatter,
                "2.{}|{}|{}",
                STANDARD.encode(iv),
                STANDARD.encode(data),
                STANDARD.encode(mac)
            ),
            Self::Rsa2048Oaep { hash, data } => {
                let kind = match hash {
                    OaepHash::Sha256 => 3,
                    OaepHash::Sha1 => 4,
                };
                write!(formatter, "{kind}.{}", STANDARD.encode(data))
            }
            Self::Unsupported { raw, .. } => formatter.write_str(raw),
        }
    }
}

/// The OAEP hash of an [`EncString::Rsa2048Oaep`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OaepHash {
    Sha256,
    Sha1,
}

/// Why an [`EncString`] did not parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncStringError {
    /// The `<type>.` header is not a number from 0 to 255, or a headerless
    /// string has neither two nor three parts.
    Header,
    /// A known type has the wrong number of `|` parts.
    PartCount,
    /// A part is not standard base64.
    Base64,
    /// An IV or MAC part has the wrong length.
    Length,
}

impl fmt::Display for EncStringError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Header => "bad encrypted string header",
            Self::PartCount => "wrong number of encrypted string parts",
            Self::Base64 => "encrypted string part is not base64",
            Self::Length => "encrypted string IV or MAC has the wrong length",
        })
    }
}

impl std::error::Error for EncStringError {}

/// Why [`EncString::decrypt`] or [`EncString::decrypt_rsa`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecryptError {
    /// The MAC does not match: wrong key or tampered data.
    Mac,
    /// The plaintext padding is invalid.
    Padding,
    /// The variant does not fit this key kind.
    WrongType,
    /// RSA-OAEP decryption failed.
    Rsa,
}

impl fmt::Display for DecryptError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Mac => "MAC mismatch",
            Self::Padding => "invalid padding",
            Self::WrongType => "encrypted string type does not fit the key",
            Self::Rsa => "RSA decryption failed",
        })
    }
}

impl std::error::Error for DecryptError {}

/// HMAC-SHA256 state over `iv || data`, ready to finalize or verify.
fn authenticator(key: &SymmetricKey, iv: &[u8; 16], data: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(&key.authentication)
        .expect("HMAC takes a key of any length");
    mac.update(iv);
    mac.update(data);
    mac
}

fn cbc_decrypt(
    encryption_key: &[u8; 32],
    iv: &[u8; 16],
    data: &[u8],
) -> Result<Zeroizing<Vec<u8>>, DecryptError> {
    cbc::Decryptor::<Aes256>::new_from_slices(encryption_key, iv)
        .expect("AES-256 takes a 32-byte key and a 16-byte IV")
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map(Zeroizing::new)
        .map_err(|_| DecryptError::Padding)
}

fn decode(part: &str) -> Result<Vec<u8>, EncStringError> {
    STANDARD.decode(part).map_err(|_| EncStringError::Base64)
}

fn decode_array<const N: usize>(part: &str) -> Result<[u8; N], EncStringError> {
    decode(part)?.try_into().map_err(|_| EncStringError::Length)
}

#[cfg(test)]
mod tests {
    // Known-answer vectors come from bitwarden/sdk-internal at commit
    // 2079db5dc425d8c15776608a29d1e6783fd0e070:
    // - vectors/type2_*.txt: crates/bitwarden-core/src/client/test_accounts.rs,
    //   `test_bitwarden_com_account` (test@bitwarden.com, asdfasdfasdf,
    //   PBKDF2 600000).
    // - vectors/type3.txt, type4.txt, type6.txt, rsa_private_key.pem:
    //   crates/bitwarden-crypto/src/enc_string/asymmetric.rs.
    // - vectors/type7.txt: crates/bitwarden-crypto/src/enc_string/symmetric.rs,
    //   `AES256_CBC_HMAC_COSE_ENC_STRING`.
    // - The type 0 strings: symmetric.rs `test_from_str_cbc256` and
    //   keys/master_key.rs `test_decrypt_cbc256`.

    use super::*;
    use crate::crypto::{Kdf, derive_master_key};
    use proptest::prelude::*;
    use rand_chacha::{ChaCha20Rng, rand_core::SeedableRng};
    use sha2::Digest;

    proptest! {
        #[test]
        fn round_trip(
            key_bytes in any::<[u8; 32]>(),
            mac_key_bytes in any::<[u8; 32]>(),
            seed in any::<[u8; 32]>(),
            plaintext in proptest::collection::vec(any::<u8>(), 0..200),
        ) {
            let key = key_from_halves(key_bytes, mac_key_bytes);
            let encrypted = EncString::encrypt(&plaintext, &key, &mut ChaCha20Rng::from_seed(seed));
            prop_assert_eq!(&*encrypted.decrypt(&key).unwrap(), &plaintext);

            let EncString::Aes256CbcHmac { iv, data, mac } = &encrypted else {
                return Err(TestCaseError::fail("encrypt must give type 2"));
            };
            // The type 2 layout, written out from the spec.
            let wire = format!(
                "2.{}|{}|{}",
                STANDARD.encode(iv),
                STANDARD.encode(data),
                STANDARD.encode(mac)
            );
            prop_assert_eq!(encrypted.to_string(), wire.clone());
            prop_assert_eq!(wire.parse::<EncString>().unwrap(), encrypted);
        }

        #[test]
        fn tamper_fails_mac(
            key_bytes in any::<[u8; 32]>(),
            mac_key_bytes in any::<[u8; 32]>(),
            seed in any::<[u8; 32]>(),
            plaintext in proptest::collection::vec(any::<u8>(), 0..100),
            bit in any::<proptest::sample::Index>(),
        ) {
            let key = key_from_halves(key_bytes, mac_key_bytes);
            let EncString::Aes256CbcHmac { iv, data, mac } =
                EncString::encrypt(&plaintext, &key, &mut ChaCha20Rng::from_seed(seed))
            else {
                return Err(TestCaseError::fail("encrypt must give type 2"));
            };
            let mut bytes = [&iv[..], &data, &mac].concat();
            let bit = bit.index(bytes.len() * 8);
            bytes[bit / 8] ^= 1 << (bit % 8);
            let tampered = EncString::Aes256CbcHmac {
                iv: bytes[..16].try_into().unwrap(),
                data: bytes[16..bytes.len() - 32].to_vec(),
                mac: bytes[bytes.len() - 32..].try_into().unwrap(),
            };
            prop_assert_eq!(tampered.decrypt(&key).err(), Some(DecryptError::Mac));
        }
    }

    #[test]
    fn type2_user_key_and_private_key_decrypt() {
        let kdf = Kdf::Pbkdf2 {
            iterations: 600_000,
        };
        let master_key = derive_master_key("asdfasdfasdf", "test@bitwarden.com", kdf).unwrap();
        let wrapped: EncString = include_str!("enc_string/vectors/type2_user_key.txt")
            .parse()
            .unwrap();
        let user_key_bytes = wrapped.decrypt(&master_key.stretch()).unwrap();
        let user_key = SymmetricKey::from_bytes(&user_key_bytes[..].try_into().unwrap());

        let private_key: EncString = include_str!("enc_string/vectors/type2_private_key.txt")
            .parse()
            .unwrap();
        let der = private_key.decrypt(&user_key).unwrap();
        assert!(PrivateKey::from_pkcs8_der(&der).is_ok());
    }

    #[test]
    fn type2_wrong_key_fails_mac() {
        let wrapped: EncString = include_str!("enc_string/vectors/type2_user_key.txt")
            .parse()
            .unwrap();
        let key = SymmetricKey::from_bytes(&[7; 64]);
        assert_eq!(wrapped.decrypt(&key).err(), Some(DecryptError::Mac));
    }

    #[test]
    fn type4_decrypt() {
        let encrypted: EncString = include_str!("enc_string/vectors/type4.txt")
            .parse()
            .unwrap();
        let plaintext = encrypted.decrypt_rsa(&sdk_private_key()).unwrap();
        assert_eq!(&*plaintext, &sdk_seeded_key("test"));
    }

    #[test]
    fn type3_decrypt() {
        let encrypted: EncString = include_str!("enc_string/vectors/type3.txt")
            .parse()
            .unwrap();
        let plaintext = encrypted.decrypt_rsa(&sdk_private_key()).unwrap();
        assert_eq!(&*plaintext, &sdk_seeded_key("test"));
    }

    #[test]
    fn type0_decrypt() {
        let encrypted: EncString = "0.tn/heK4HLbbEe+yEkC+kvw==|8QM94f7aVTtjm/bmvRdVxOxiLiiZtHYYO7+oBdjFCkilncesx0iVrXPl+tMKqW+Jo7+FtZdPNsTrL6RdoG7i5QbCRVwK+9010+xm7MTQY8s=".parse().unwrap();
        let encryption = STANDARD
            .decode("hvBMMb1t79YssFZkpetYsM3deyVuQv4r88Uj9gvYe08=")
            .unwrap()
            .try_into()
            .unwrap();
        let plaintext = encrypted.decrypt_legacy(&encryption).unwrap();
        assert_eq!(
            plaintext[..32],
            [
                116, 170, 187, 43, 80, 212, 193, 202, 234, 181, 57, 66, 151, 249, 59, 47, 70, 16,
                57, 4, 170, 78, 85, 241, 152, 232, 91, 57, 9, 87, 209, 245,
            ]
        );
        assert_eq!(
            plaintext[32..],
            [
                40, 245, 106, 140, 2, 225, 138, 213, 98, 223, 92, 168, 135, 208, 22, 194, 31, 21,
                178, 252, 203, 198, 35, 174, 53, 218, 254, 151, 235, 57, 7, 98,
            ]
        );
    }

    proptest! {
        /// A type 2 string with its MAC stripped, sent as type 0, must not
        /// open under the type 2 key: no MAC downgrade.
        #[test]
        fn type0_rejected_by_mac_key(
            key_bytes in any::<[u8; 32]>(),
            mac_key_bytes in any::<[u8; 32]>(),
            seed in any::<[u8; 32]>(),
            plaintext in proptest::collection::vec(any::<u8>(), 0..100),
        ) {
            let key = key_from_halves(key_bytes, mac_key_bytes);
            let EncString::Aes256CbcHmac { iv, data, .. } =
                EncString::encrypt(&plaintext, &key, &mut ChaCha20Rng::from_seed(seed))
            else {
                return Err(TestCaseError::fail("encrypt must give type 2"));
            };
            let downgraded: EncString =
                format!("0.{}|{}", STANDARD.encode(iv), STANDARD.encode(data)).parse().unwrap();
            prop_assert_eq!(downgraded.decrypt(&key).err(), Some(DecryptError::WrongType));
        }
    }

    #[test]
    fn type0_parses() {
        let parsed: EncString = "0.pMS6/icTQABtulw52pq2lg==|XXbxKxDTh+mWiN1HjH2N1w=="
            .parse()
            .unwrap();
        assert_eq!(
            parsed,
            EncString::Aes256Cbc {
                iv: [
                    164, 196, 186, 254, 39, 19, 64, 0, 109, 186, 92, 57, 218, 154, 182, 150
                ],
                data: vec![
                    93, 118, 241, 43, 16, 211, 135, 233, 150, 136, 221, 71, 140, 125, 141, 215
                ],
            }
        );
    }

    #[test]
    fn type5_parses_unsupported() {
        // Bitwarden's tests have no type 5 sample, so this is the type 3
        // sample with a MAC part added; only the parse is under test.
        let raw = format!(
            "5.{}|AA==",
            &include_str!("enc_string/vectors/type3.txt")[2..]
        );
        assert_unsupported(&raw, 5);
    }

    #[test]
    fn type6_parses_unsupported() {
        assert_unsupported(include_str!("enc_string/vectors/type6.txt"), 6);
    }

    #[test]
    fn type7_parses_unsupported() {
        assert_unsupported(include_str!("enc_string/vectors/type7.txt"), 7);
    }

    fn assert_unsupported(raw: &str, kind: u8) {
        let parsed: EncString = raw.parse().unwrap();
        assert_eq!(
            parsed,
            EncString::Unsupported {
                kind,
                raw: raw.to_owned()
            }
        );
        assert_eq!(parsed.to_string(), raw);
    }

    fn key_from_halves(encryption: [u8; 32], authentication: [u8; 32]) -> SymmetricKey {
        SymmetricKey {
            encryption,
            authentication,
        }
    }

    /// The PEM from the SDK test, decoded by hand to PKCS#8 DER.
    fn sdk_private_key() -> PrivateKey {
        let body: String = include_str!("enc_string/vectors/rsa_private_key.pem")
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        PrivateKey::from_pkcs8_der(&STANDARD.decode(body).unwrap()).unwrap()
    }

    /// The SDK's `generate_seeded_for_unit_tests`: 64 bytes of ChaCha20
    /// seeded with SHA-256 of `seed`.
    fn sdk_seeded_key(seed: &str) -> [u8; 64] {
        let mut rng = ChaCha20Rng::from_seed(Sha256::digest(seed.as_bytes()).into());
        let mut bytes = [0u8; 64];
        rand_chacha::rand_core::RngCore::fill_bytes(&mut rng, &mut bytes);
        bytes
    }
}
