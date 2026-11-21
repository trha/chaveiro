//! The few fixed CBOR shapes a passkey reply needs, written in CTAP2
//! canonical form: map keys sorted by encoded length, then by bytes.

/// The COSE_Key of an ES256 public key:
/// `{1: 2, 3: -7, -1: 1, -2: x, -3: y}` (EC2, ES256, P-256).
pub(super) fn cose_es256_key(x: &[u8; 32], y: &[u8; 32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(77);
    head(&mut out, MAP, 5);
    integer(&mut out, 1);
    integer(&mut out, 2);
    integer(&mut out, 3);
    integer(&mut out, -7);
    integer(&mut out, -1);
    integer(&mut out, 1);
    integer(&mut out, -2);
    bytes(&mut out, x);
    integer(&mut out, -3);
    bytes(&mut out, y);
    out
}

/// The attestation object with no attestation:
/// `{"fmt": "none", "attStmt": {}, "authData": authenticator_data}`.
pub(super) fn attestation_none(authenticator_data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(authenticator_data.len() + 32);
    head(&mut out, MAP, 3);
    text(&mut out, "fmt");
    text(&mut out, "none");
    text(&mut out, "attStmt");
    head(&mut out, MAP, 0);
    text(&mut out, "authData");
    bytes(&mut out, authenticator_data);
    out
}

const UNSIGNED: u8 = 0;
const NEGATIVE: u8 = 1;
const BYTES: u8 = 2;
const TEXT: u8 = 3;
const MAP: u8 = 5;

/// A major type with its argument in the shortest form.
fn head(out: &mut Vec<u8>, major: u8, argument: u64) {
    let major = major << 5;
    match argument {
        0..24 => out.push(major | argument as u8),
        24..0x100 => out.extend([major | 24, argument as u8]),
        0x100..0x1_0000 => {
            out.push(major | 25);
            out.extend((argument as u16).to_be_bytes());
        }
        0x1_0000..0x1_0000_0000 => {
            out.push(major | 26);
            out.extend((argument as u32).to_be_bytes());
        }
        _ => {
            out.push(major | 27);
            out.extend(argument.to_be_bytes());
        }
    }
}

fn integer(out: &mut Vec<u8>, value: i64) {
    match u64::try_from(value) {
        Ok(value) => head(out, UNSIGNED, value),
        // CBOR stores a negative n as -1 - n.
        Err(_) => head(out, NEGATIVE, value.unsigned_abs() - 1),
    }
}

fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    head(out, BYTES, value.len() as u64);
    out.extend_from_slice(value);
}

fn text(out: &mut Vec<u8>, value: &str) {
    head(out, TEXT, value.len() as u64);
    out.extend_from_slice(value.as_bytes());
}
