"""Creates a test user on a Vaultwarden server and turns on TOTP and email 2FA.

Usage: vaultwarden_seed.py <server url> <mailpit url> <email> <password> <totp secret>

The TOTP secret is base32 and must decode to 20 bytes. Email codes are read
from Mailpit, so the server must send mail there. Exits nonzero on any failure.
"""

import base64
import hashlib
import hmac
import json
import os
import re
import struct
import sys
import time
import urllib.parse
import urllib.request
import uuid

from cryptography.hazmat.primitives import padding
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes

KDF_ITERATIONS = 600_000


def request(method, url, body=None, token=None, form=False):
    headers = {"Accept": "application/json"}
    data = None
    if body is not None:
        if form:
            data = urllib.parse.urlencode(body).encode()
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        else:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
    if token:
        headers["Authorization"] = f"Bearer {token}"
    req = urllib.request.Request(url, data=data, method=method, headers=headers)
    try:
        with urllib.request.urlopen(req) as response:
            text = response.read().decode()
    except urllib.error.HTTPError as error:
        sys.exit(f"{method} {url} failed: {error.code} {error.read().decode()}")
    return json.loads(text) if text else None


def hkdf_expand(key, info):
    # One HMAC block is enough for a 32-byte output.
    return hmac.new(key, info + b"\x01", hashlib.sha256).digest()


def encrypt(plain, enc_key, mac_key):
    """Returns a type 2 EncString (AES-256-CBC with HMAC-SHA256)."""
    iv = os.urandom(16)
    padder = padding.PKCS7(128).padder()
    padded = padder.update(plain) + padder.finalize()
    encryptor = Cipher(algorithms.AES(enc_key), modes.CBC(iv)).encryptor()
    cipher = encryptor.update(padded) + encryptor.finalize()
    mac = hmac.new(mac_key, iv + cipher, hashlib.sha256).digest()
    b64 = lambda raw: base64.b64encode(raw).decode()
    return f"2.{b64(iv)}|{b64(cipher)}|{b64(mac)}"


def totp(secret):
    key = base64.b32decode(secret)
    counter = struct.pack(">Q", int(time.time()) // 30)
    digest = hmac.new(key, counter, hashlib.sha1).digest()
    offset = digest[-1] & 0x0F
    code = struct.unpack(">I", digest[offset : offset + 4])[0] & 0x7FFFFFFF
    return f"{code % 1_000_000:06d}"


def read_email_code(mailpit, email, after):
    """Waits for a mail to `email` newer than message id set `after`."""
    for _ in range(50):
        messages = request("GET", f"{mailpit}/api/v1/messages")["messages"]
        for message in messages:
            to = [address["Address"] for address in message["To"]]
            if message["ID"] in after or email not in to:
                continue
            text = request("GET", f"{mailpit}/api/v1/message/{message['ID']}")["Text"]
            match = re.search(r"\b(\d{6})\b", text)
            if match:
                return match.group(1)
        time.sleep(0.2)
    sys.exit(f"no 2FA email reached {email}")


def main():
    server, mailpit, email, password, totp_secret = sys.argv[1:]
    email = email.lower()

    master_key = hashlib.pbkdf2_hmac(
        "sha256", password.encode(), email.encode(), KDF_ITERATIONS, 32
    )
    password_hash = base64.b64encode(
        hashlib.pbkdf2_hmac("sha256", master_key, password.encode(), 1, 32)
    ).decode()
    user_key = encrypt(
        os.urandom(64), hkdf_expand(master_key, b"enc"), hkdf_expand(master_key, b"mac")
    )

    # Signup is two steps: get a verification token, then finish with it.
    # With SIGNUPS_VERIFY off the server returns the token instead of mailing it.
    verification_token = request(
        "POST",
        f"{server}/identity/accounts/register/send-verification-email",
        {"email": email, "name": "Test User"},
    )
    request(
        "POST",
        f"{server}/identity/accounts/register/finish",
        {
            "email": email,
            "emailVerificationToken": verification_token,
            "masterPasswordHash": password_hash,
            "masterPasswordHint": None,
            "key": user_key,
            "kdf": 0,
            "kdfIterations": KDF_ITERATIONS,
        },
    )

    token = request(
        "POST",
        f"{server}/identity/connect/token",
        {
            "grant_type": "password",
            "username": email,
            "password": password_hash,
            "scope": "api offline_access",
            "client_id": "web",
            "deviceType": "9",
            "deviceIdentifier": str(uuid.uuid4()),
            "deviceName": "seed",
        },
        form=True,
    )["access_token"]

    request(
        "POST",
        f"{server}/api/two-factor/authenticator",
        {"key": totp_secret, "token": totp(totp_secret), "masterPasswordHash": password_hash},
        token,
    )

    seen = {m["ID"] for m in request("GET", f"{mailpit}/api/v1/messages")["messages"]}
    request(
        "POST",
        f"{server}/api/two-factor/send-email",
        {"email": email, "masterPasswordHash": password_hash},
        token,
    )
    request(
        "PUT",
        f"{server}/api/two-factor/email",
        {
            "email": email,
            "token": read_email_code(mailpit, email, seen),
            "masterPasswordHash": password_hash,
        },
        token,
    )


main()
