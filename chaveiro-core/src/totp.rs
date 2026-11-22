//! Time-based one-time codes (RFC 6238) from the secrets Bitwarden stores.
//! Nothing here allocates, so a view can make the code on every pass.

use hmac::{Hmac, Mac, digest::KeyInit};
use std::fmt;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::crypto::SecretString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Algorithm {
    Sha1,
    Sha256,
    Sha512,
}

/// The longest key read, in bytes: 128, twice SHA-512's block. Real keys
/// are 10 to 64 bytes.
const KEY_LENGTH: usize = 128;

const STEAM: &[u8] = b"23456789BCDFGHJKMNPQRTVWXY";

/// One code, wiped on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct Code {
    /// ASCII digits, or Steam letters, in `text[..length]`.
    text: [u8; 10],
    length: u8,
    /// Seconds from the code's start to the next code.
    #[zeroize(skip)]
    period: u64,
}

impl Code {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.text[..usize::from(self.length)])
            .expect("a code holds only ASCII")
    }

    /// Seconds until the next code, at `now_s`: 1 to the period.
    pub fn seconds_left(&self, now_s: u64) -> u64 {
        self.period - now_s % self.period
    }
}

/// Writes a 6 or 8 digit code as two halves, such as `492 117`, and any
/// other as it is.
impl fmt::Display for Code {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = self.as_str();
        match text.len() {
            6 | 8 if text.bytes().all(|byte| byte.is_ascii_digit()) => {
                let (first, second) = text.split_at(text.len() / 2);
                write!(formatter, "{first} {second}")
            }
            _ => formatter.write_str(text),
        }
    }
}

/// The code for `secret` at `now_s` seconds since the Unix epoch. `secret`
/// is a bare base32 key, an `otpauth://totp/` URI, or a `steam://` key, as
/// Bitwarden accepts them. Gives [`None`] for a secret it cannot read, or a
/// key longer than 128 bytes.
pub fn now(secret: &str, now_s: u64) -> Option<Code> {
    let secret = secret.trim();
    let mut algorithm = Algorithm::Sha1;
    let mut digits = 6u32;
    let mut period = 30u64;
    let mut steam = false;
    let mut key = Key {
        bytes: [0; KEY_LENGTH],
        length: 0,
    };
    if let Some(rest) = strip_prefix_ignore_case(secret, "otpauth://totp/") {
        let mut found = false;
        let query = rest.split_once('?')?.1;
        for pair in query.split('&') {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            if name.eq_ignore_ascii_case("secret") {
                base32(value, &mut key)?;
                found = true;
            } else if name.eq_ignore_ascii_case("digits") {
                digits = value
                    .parse()
                    .ok()
                    .filter(|digits| (1..=10).contains(digits))?;
            } else if name.eq_ignore_ascii_case("period") {
                period = value.parse().ok().filter(|period| *period > 0)?;
            } else if name.eq_ignore_ascii_case("algorithm") {
                algorithm = [
                    ("SHA1", Algorithm::Sha1),
                    ("SHA256", Algorithm::Sha256),
                    ("SHA512", Algorithm::Sha512),
                ]
                .into_iter()
                .find(|(name, _)| value.eq_ignore_ascii_case(name))?
                .1;
            }
        }
        if !found {
            return None;
        }
    } else if let Some(rest) = strip_prefix_ignore_case(secret, "steam://") {
        steam = true;
        base32(rest, &mut key)?;
    } else {
        base32(secret, &mut key)?;
    }
    let key = &key.bytes[..key.length];
    let counter = (now_s / period).to_be_bytes();
    let mut value = match algorithm {
        Algorithm::Sha1 => truncate::<Hmac<sha1::Sha1>>(key, &counter),
        Algorithm::Sha256 => truncate::<Hmac<sha2::Sha256>>(key, &counter),
        Algorithm::Sha512 => truncate::<Hmac<sha2::Sha512>>(key, &counter),
    };
    let mut code = Code {
        text: [0; 10],
        length: 0,
        period,
    };
    if steam {
        for slot in &mut code.text[..5] {
            *slot = STEAM[value as usize % STEAM.len()];
            value /= STEAM.len() as u32;
        }
        code.length = 5;
    } else {
        let value = u64::from(value) % 10u64.pow(digits);
        for (place, slot) in (0..digits).rev().zip(&mut code.text) {
            *slot = b'0' + (value / 10u64.pow(place) % 10) as u8;
        }
        code.length = digits as u8;
    }
    value.zeroize();
    Some(code)
}

/// [`now`] as a string, for the clipboard.
pub fn code(secret: &str, now_s: u64) -> Option<SecretString> {
    now(secret, now_s).map(|code| SecretString::new(code.as_str().to_owned()))
}

/// A decoded key, wiped on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
struct Key {
    bytes: [u8; KEY_LENGTH],
    length: usize,
}

/// The RFC 4226 dynamic truncation of the HMAC of `message` under `key`.
fn truncate<M: Mac + KeyInit>(key: &[u8], message: &[u8]) -> u32 {
    let mut mac = <M as Mac>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(message);
    let mut hash = mac.finalize().into_bytes();
    let offset = usize::from(hash[hash.len() - 1] & 0x0f);
    let value =
        u32::from_be_bytes(hash[offset..offset + 4].try_into().expect("four bytes")) & 0x7fff_ffff;
    hash.as_mut_slice().zeroize();
    value
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

/// Decodes RFC 4648 base32 into `key`, any case, skipping spaces, dashes
/// and padding. Gives [`None`] for a bad letter, an empty key or one longer
/// than [`KEY_LENGTH`].
fn base32(text: &str, key: &mut Key) -> Option<()> {
    let mut buffer = 0u64;
    let mut bits = 0;
    key.length = 0;
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a',
            b'2'..=b'7' => byte - b'2' + 26,
            b' ' | b'-' | b'=' => continue,
            _ => return None,
        };
        buffer = (buffer << 5) | u64::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            *key.bytes.get_mut(key.length)? = (buffer >> bits) as u8;
            key.length += 1;
        }
    }
    buffer.zeroize();
    (key.length > 0).then_some(())
}
#[cfg(test)]
mod tests {
    use super::*;

    // The vectors of RFC 6238 appendix B, with its 20, 32 and 64 byte keys.
    const SHA1_KEY: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";
    const SHA256_KEY: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA";
    const SHA512_KEY: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNA";

    fn uri(key: &str, algorithm: &str) -> String {
        format!("otpauth://totp/Example:me?secret={key}&algorithm={algorithm}&digits=8&period=30")
    }

    #[test]
    fn rfc_6238_vectors() {
        for (time, sha1, sha256, sha512) in [
            (59, "94287082", "46119246", "90693936"),
            (1111111109, "07081804", "68084774", "25091201"),
            (1234567890, "89005924", "91819424", "93441116"),
            (20000000000, "65353130", "77737706", "47863826"),
        ] {
            assert_eq!(
                code(&uri(SHA1_KEY, "SHA1"), time)
                    .as_deref()
                    .map(String::as_str),
                Some(sha1)
            );
            assert_eq!(
                code(&uri(SHA256_KEY, "SHA256"), time)
                    .as_deref()
                    .map(String::as_str),
                Some(sha256)
            );
            assert_eq!(
                code(&uri(SHA512_KEY, "SHA512"), time)
                    .as_deref()
                    .map(String::as_str),
                Some(sha512)
            );
        }
    }

    #[test]
    fn a_bare_key_gives_the_six_last_digits_of_the_uri_code() {
        let code = code("gezd gnbv gy3t qojq gezd gnbv gy3t qojq", 59);
        assert_eq!(code.as_deref().map(String::as_str), Some("287082"));
    }

    #[test]
    fn a_steam_key_gives_five_steam_letters() {
        let code = code(&format!("steam://{SHA1_KEY}"), 59).expect("a valid key");
        assert_eq!(code.len(), 5);
        assert!(
            code.bytes()
                .all(|byte| b"23456789BCDFGHJKMNPQRTVWXY".contains(&byte))
        );
    }

    proptest::proptest! {
        #[test]
        fn a_code_has_the_asked_digits_and_holds_for_its_period(
            digits in 1u32..=10,
            period in 1u64..=120,
            step in 0u64..1 << 40,
            within in 0u64..120,
        ) {
            let uri = format!("otpauth://totp/x?secret={SHA1_KEY}&digits={digits}&period={period}");
            let start = step * period;
            let first = code(&uri, start).expect("a valid key");
            proptest::prop_assert_eq!(first.len(), digits as usize);
            proptest::prop_assert!(first.bytes().all(|byte| byte.is_ascii_digit()));
            proptest::prop_assert_eq!(code(&uri, start + within % period), Some(first));
        }
    }

    #[test]
    fn a_six_digit_code_shows_as_two_halves() {
        let code = now("gezd gnbv gy3t qojq gezd gnbv gy3t qojq", 59).expect("a valid key");
        assert_eq!(code.to_string(), "287 082");
    }

    proptest::proptest! {
        #[test]
        fn the_code_holds_for_the_seconds_left_then_the_next_starts(
            period in 1u64..=120,
            time in 0u64..1 << 40,
        ) {
            let uri = format!("otpauth://totp/x?secret={SHA1_KEY}&period={period}");
            let code = now(&uri, time).expect("a valid key");
            let left = code.seconds_left(time);
            proptest::prop_assert!((1..=period).contains(&left));
            let last = now(&uri, time + left - 1).expect("a valid key");
            proptest::prop_assert_eq!(last.as_str(), code.as_str());
            // The next window starts a new counter, which is all the
            // contract promises; its code may repeat by chance.
            proptest::prop_assert_eq!((time + left) % period, 0);
        }
    }

    #[test]
    fn an_unreadable_secret_gives_none() {
        assert_eq!(code("not base32!", 59), None);
        assert_eq!(code("otpauth://totp/x?digits=6", 59), None);
        assert_eq!(code("otpauth://totp/x?secret=GEZA&algorithm=MD5", 59), None);
        // 129 bytes is one past the longest key read.
        assert_eq!(code(&"A".repeat(129 * 8 / 5 + 1), 59), None);
    }
}
