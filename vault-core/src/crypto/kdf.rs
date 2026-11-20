//! Master key derivation from the master password.

use super::MasterKey;
use sha2::{Digest, Sha256};
use std::fmt;

/// The KDF settings the server sends with prelogin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kdf {
    Pbkdf2 {
        iterations: u32,
    },
    Argon2id {
        iterations: u32,
        /// `memory_mebibytes` as sent by the server.
        memory_mebibytes: u32,
        parallelism: u32,
    },
}

/// Derives the master key from `password`, salted with `email` trimmed and
/// lower-cased.
///
/// CPU-heavy: the shell runs it off the UI thread. Settings below
/// Bitwarden's minimums (PBKDF2 5000 iterations; Argon2id 2 iterations,
/// 16 MiB, parallelism 1) fail with [`KdfError::TooWeak`], so a hostile
/// server cannot make the password cheap to guess.
pub fn derive_master_key(password: &str, email: &str, kdf: Kdf) -> Result<MasterKey, KdfError> {
    let salt = email.trim().to_lowercase();
    let mut key = MasterKey([0; 32]);
    match kdf {
        Kdf::Pbkdf2 { iterations } => {
            if iterations < 5000 {
                return Err(KdfError::TooWeak);
            }
            pbkdf2::pbkdf2_hmac::<Sha256>(
                password.as_bytes(),
                salt.as_bytes(),
                iterations,
                &mut key.0,
            );
        }
        Kdf::Argon2id {
            iterations,
            memory_mebibytes,
            parallelism,
        } => {
            if iterations < 2 || memory_mebibytes < 16 || parallelism < 1 {
                return Err(KdfError::TooWeak);
            }
            let params = argon2::Params::new(
                memory_mebibytes.saturating_mul(1024),
                iterations,
                parallelism,
                Some(32),
            )
            .map_err(KdfError::Argon2)?;
            // Bitwarden salts Argon2id with SHA-256 of the email, not the email itself.
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
                .hash_password_into(
                    password.as_bytes(),
                    &Sha256::digest(salt.as_bytes()),
                    &mut key.0,
                )
                .map_err(KdfError::Argon2)?;
        }
    }
    Ok(key)
}

/// Why [`derive_master_key`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KdfError {
    /// The settings are below Bitwarden's minimums.
    TooWeak,
    /// Argon2 rejected the settings, for example memory above its limit.
    Argon2(argon2::Error),
}

impl fmt::Display for KdfError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooWeak => formatter.write_str("KDF settings are below the minimum"),
            Self::Argon2(error) => write!(formatter, "Argon2: {error}"),
        }
    }
}

impl std::error::Error for KdfError {}

#[cfg(test)]
mod tests {
    // Vectors from bitwarden/sdk-internal at commit
    // 2079db5dc425d8c15776608a29d1e6783fd0e070, crates/bitwarden-crypto/src:
    // keys/kdf.rs (`test_master_key_derive_*`), keys/master_key.rs
    // (`test_password_hash_*`) and keys/utils.rs (`test_stretch_kdf_key`).

    use super::*;

    #[test]
    fn pbkdf2_master_key() {
        let kdf = Kdf::Pbkdf2 { iterations: 10000 };
        let key = derive_master_key("67t9b5g67$%Dh89n", "test_key", kdf).unwrap();
        assert_eq!(key.0, SDK_MASTER_KEY);
    }

    #[test]
    fn argon2id_master_key() {
        let kdf = Kdf::Argon2id {
            iterations: 4,
            memory_mebibytes: 32,
            parallelism: 2,
        };
        let key = derive_master_key("67t9b5g67$%Dh89n", "test_key", kdf).unwrap();
        assert_eq!(
            key.0,
            [
                207, 240, 225, 177, 162, 19, 163, 76, 98, 106, 179, 175, 224, 9, 17, 240, 20, 147,
                237, 47, 246, 150, 141, 184, 62, 225, 131, 242, 51, 53, 225, 242
            ]
        );
    }

    #[test]
    fn pbkdf2_password_hash_ignores_email_case_and_space() {
        let kdf = Kdf::Pbkdf2 {
            iterations: 100_000,
        };
        for email in [
            "test@bitwarden.com",
            "TEST@bitwarden.com",
            " test@bitwarden.com",
        ] {
            let key = derive_master_key("asdfasdf", email, kdf).unwrap();
            assert_eq!(
                key.password_hash("asdfasdf"),
                "wmyadRMyBZOH7P/a/ucTCbSghKgdzDpPqUnu/DAVtSw="
            );
        }
    }

    #[test]
    fn argon2id_password_hash() {
        let kdf = Kdf::Argon2id {
            iterations: 4,
            memory_mebibytes: 32,
            parallelism: 2,
        };
        let key = derive_master_key("asdfasdf", "test_salt", kdf).unwrap();
        assert_eq!(
            key.password_hash("asdfasdf"),
            "PR6UjYmjmppTYcdyTiNbAhPJuQQOmynKbdEl1oyi/iQ="
        );
    }

    #[test]
    fn stretched_key() {
        let stretched = MasterKey(SDK_MASTER_KEY).stretch();
        assert_eq!(
            stretched.encryption,
            [
                111, 31, 178, 45, 238, 152, 37, 114, 143, 215, 124, 83, 135, 173, 195, 23, 142,
                134, 120, 249, 61, 132, 163, 182, 113, 197, 189, 204, 188, 21, 237, 96
            ]
        );
        assert_eq!(
            stretched.authentication,
            [
                221, 127, 206, 234, 101, 27, 202, 38, 86, 52, 34, 28, 78, 28, 185, 16, 48, 61, 127,
                166, 209, 247, 194, 87, 232, 26, 48, 85, 193, 249, 179, 155
            ]
        );
    }

    #[test]
    fn weak_settings_fail() {
        for kdf in [
            Kdf::Pbkdf2 { iterations: 4999 },
            Kdf::Argon2id {
                iterations: 1,
                memory_mebibytes: 32,
                parallelism: 2,
            },
            Kdf::Argon2id {
                iterations: 4,
                memory_mebibytes: 15,
                parallelism: 2,
            },
            Kdf::Argon2id {
                iterations: 4,
                memory_mebibytes: 32,
                parallelism: 0,
            },
        ] {
            assert_eq!(
                derive_master_key("a", "b", kdf).err(),
                Some(KdfError::TooWeak)
            );
        }
    }

    /// PBKDF2 of `67t9b5g67$%Dh89n` salted `test_key`, 10000 iterations.
    const SDK_MASTER_KEY: [u8; 32] = [
        31, 79, 104, 226, 150, 71, 177, 90, 194, 80, 172, 209, 17, 129, 132, 81, 138, 167, 69, 167,
        254, 149, 2, 27, 39, 197, 64, 42, 22, 195, 86, 75,
    ];
}
