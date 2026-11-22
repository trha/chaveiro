//! Time-based one-time codes (RFC 6238) from the secrets Bitwarden stores.

use hmac::{Hmac, Mac};
use zeroize::Zeroizing;

use crate::crypto::SecretString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Algorithm {
    Sha1,
    Sha256,
    Sha512,
}

/// The code for `secret` at `now_s` seconds since the Unix epoch. `secret`
/// is a bare base32 key, an `otpauth://totp/` URI, or a `steam://` key, as
/// Bitwarden accepts them. Gives [`None`] for a secret it cannot read.
pub fn code(secret: &str, now_s: u64) -> Option<SecretString> {
    let secret = secret.trim();
    let mut algorithm = Algorithm::Sha1;
    let mut digits = 6u32;
    let mut period = 30u64;
    let mut steam = false;
    let key = if let Some(rest) = strip_prefix_ignore_case(secret, "otpauth://totp/") {
        let mut key = None;
        let query = rest.split_once('?')?.1;
        for pair in query.split('&') {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            match name.to_ascii_lowercase().as_str() {
                "secret" => key = Some(base32(value)?),
                "digits" => {
                    digits = value
                        .parse()
                        .ok()
                        .filter(|digits| (1..=10).contains(digits))?
                }
                "period" => period = value.parse().ok().filter(|period| *period > 0)?,
                "algorithm" => {
                    algorithm = match value.to_ascii_uppercase().as_str() {
                        "SHA1" => Algorithm::Sha1,
                        "SHA256" => Algorithm::Sha256,
                        "SHA512" => Algorithm::Sha512,
                        _ => return None,
                    }
                }
                _ => {}
            }
        }
        key?
    } else if let Some(rest) = strip_prefix_ignore_case(secret, "steam://") {
        steam = true;
        base32(rest)?
    } else {
        base32(secret)?
    };
    let counter = (now_s / period).to_be_bytes();
    let hash: Zeroizing<Vec<u8>> = Zeroizing::new(match algorithm {
        Algorithm::Sha1 => mac::<Hmac<sha1::Sha1>>(&key, &counter),
        Algorithm::Sha256 => mac::<Hmac<sha2::Sha256>>(&key, &counter),
        Algorithm::Sha512 => mac::<Hmac<sha2::Sha512>>(&key, &counter),
    });
    let offset = usize::from(hash[hash.len() - 1] & 0x0f);
    let mut value =
        u32::from_be_bytes(hash[offset..offset + 4].try_into().expect("four bytes")) & 0x7fff_ffff;
    let mut code = Zeroizing::new(String::new());
    if steam {
        const STEAM: &[u8] = b"23456789BCDFGHJKMNPQRTVWXY";
        for _ in 0..5 {
            code.push(char::from(STEAM[value as usize % STEAM.len()]));
            value /= STEAM.len() as u32;
        }
    } else {
        let value = u64::from(value) % 10u64.pow(digits);
        for place in (0..digits).rev() {
            let digit = (value / 10u64.pow(place) % 10) as u8;
            code.push(char::from(b'0' + digit));
        }
    }
    Some(code)
}

fn mac<M: Mac + hmac::digest::KeyInit>(key: &[u8], message: &[u8]) -> Vec<u8> {
    let mut mac = <M as Mac>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(message);
    mac.finalize().into_bytes().to_vec()
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    text.get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &text[prefix.len()..])
}

/// Decodes RFC 4648 base32, any case, skipping spaces, dashes and padding.
fn base32(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(text.len() * 5 / 8));
    let mut buffer = 0u64;
    let mut bits = 0;
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
            bytes.push((buffer >> bits) as u8);
        }
    }
    (!bytes.is_empty()).then_some(bytes)
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
    fn an_unreadable_secret_gives_none() {
        assert_eq!(code("not base32!", 59), None);
        assert_eq!(code("otpauth://totp/x?digits=6", 59), None);
        assert_eq!(code("otpauth://totp/x?secret=GEZA&algorithm=MD5", 59), None);
    }
}
