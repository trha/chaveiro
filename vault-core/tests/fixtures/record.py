"""Record Vaultwarden responses as fixtures. Throwaway account and password only."""
import base64, hashlib, hmac, json, os, struct, sys, time, uuid
import requests
from cryptography.hazmat.primitives import hashes, padding as sympad
from cryptography.hazmat.primitives.asymmetric import rsa, padding, ec
from cryptography.hazmat.primitives.ciphers import Cipher, algorithms, modes
from cryptography.hazmat.primitives.kdf.hkdf import HKDFExpand
from cryptography.hazmat.primitives import serialization

BASE = sys.argv[1]
OUT = sys.argv[2]
EMAIL = "fixture-user@example.com"
PASSWORD = "fixture-test-password-1"
ITER = 600000
DEVICE = str(uuid.UUID(int=1))
b64 = lambda b: base64.b64encode(b).decode()
HDR = {"Device-Type": "6", "Bitwarden-Client-Name": "desktop", "Bitwarden-Client-Version": "2025.1.0"}


def save(name, resp, note=""):
    try:
        body = resp.json()
    except Exception:
        body = resp.text
    with open(os.path.join(OUT, name), "w") as f:
        json.dump({"status": resp.status_code, "body": body}, f, indent=2)
    print(name, resp.status_code, note)


def enc(data: bytes, key: bytes) -> str:
    iv = os.urandom(16)
    p = sympad.PKCS7(128).padder()
    pt = p.update(data) + p.finalize()
    e = Cipher(algorithms.AES(key[:32]), modes.CBC(iv)).encryptor()
    ct = e.update(pt) + e.finalize()
    mac = hmac.new(key[32:], iv + ct, hashlib.sha256).digest()
    return f"2.{b64(iv)}|{b64(ct)}|{b64(mac)}"


def s(text, key):
    return enc(text.encode(), key)


master = hashlib.pbkdf2_hmac("sha256", PASSWORD.encode(), EMAIL.strip().lower().encode(), ITER, 32)
mp_hash = b64(hashlib.pbkdf2_hmac("sha256", master, PASSWORD.encode(), 1, 32))
stretched = HKDFExpand(hashes.SHA256(), 32, b"enc").derive(master) + HKDFExpand(hashes.SHA256(), 32, b"mac").derive(master)
user_key = os.urandom(64)
priv = rsa.generate_private_key(public_exponent=65537, key_size=2048)
pub_der = priv.public_key().public_bytes(serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo)
priv_der = priv.private_bytes(serialization.Encoding.DER, serialization.PrivateFormat.PKCS8, serialization.NoEncryption())

S = requests.Session()
S.headers.update(HDR)

reg = {"email": EMAIL, "name": "Fixture User", "masterPasswordHash": mp_hash, "masterPasswordHint": None,
       "key": enc(user_key, stretched), "kdf": 0, "kdfIterations": ITER,
       "keys": {"publicKey": b64(pub_der), "encryptedPrivateKey": enc(priv_der, user_key)}}
vt = S.post(BASE + "/identity/accounts/register/send-verification-email", json={"email": EMAIL, "name": "Fixture User"},
            headers={"Accept": "application/json"}).json()
reg["emailVerificationToken"] = vt
r = S.post(BASE + "/identity/accounts/register/finish", json=reg)
print("register", r.status_code, r.text[:200])

save("prelogin.json", S.post(BASE + "/identity/accounts/prelogin", json={"email": EMAIL}))


def token(pw, extra=None):
    form = {"grant_type": "password", "username": EMAIL, "password": pw, "scope": "api offline_access",
            "client_id": "desktop", "deviceType": "6", "deviceIdentifier": DEVICE, "deviceName": "windows"}
    form.update(extra or {})
    return S.post(BASE + "/identity/connect/token", data=form)


wrong = b64(hashlib.pbkdf2_hmac("sha256", master, b"wrong", 1, 32))
save("token_invalid_grant.json", token(wrong))
r = token(mp_hash)
save("token_success.json", r)
A = {"Authorization": "Bearer " + r.json()["access_token"]}
_p = r.json()["access_token"].split(".")[1]
USER_ID = json.loads(base64.urlsafe_b64decode(_p + "=" * (-len(_p) % 4)))["sub"]

# Folder
folder = S.post(BASE + "/api/folders", json={"name": s("Work", user_key)}, headers=A).json()


def login(name, key, **extra):
    return {"type": 1, "name": s(name, key), "notes": s("a note", key), "favorite": False, "reprompt": 0,
            "folderId": None, "organizationId": None,
            "fields": [{"type": 0, "name": s("pin", key), "value": s("1234", key), "linkedId": None},
                       {"type": 1, "name": s("secret", key), "value": s("hidden", key), "linkedId": None},
                       {"type": 3, "name": s("linked", key), "value": None, "linkedId": 100}],
            "login": {"username": s("alice", key), "password": s("hunter2", key), "totp": s("JBSWY3DPEHPK3PXP", key),
                      "uris": [{"uri": s("https://example.com", key), "match": None},
                               {"uri": s("https://example.org/login", key), "match": 3}],
                      "passwordRevisionDate": None, **extra}}


def post(body, path="/api/ciphers"):
    (body.get("cipher") or body)["encryptedFor"] = USER_ID
    r = S.post(BASE + path, json=body, headers=A)
    print(path, r.status_code, r.text[:300])
    return r


c_login = login("Example login", user_key)
c_login["folderId"] = folder["id"]
login_resp = post(c_login)
save("cipher_post_login.json", login_resp)
post({"type": 2, "name": s("Secure note", user_key), "notes": s("note body", user_key), "favorite": True,
      "reprompt": 1, "secureNote": {"type": 0}})
post({"type": 3, "name": s("Card", user_key), "reprompt": 0,
      "card": {k: s(v, user_key) for k, v in {"cardholderName": "Alice A", "brand": "Visa", "number": "4111111111111111",
                                               "expMonth": "12", "expYear": "2030", "code": "123"}.items()}})
post({"type": 4, "name": s("Identity", user_key), "reprompt": 0,
      "identity": {k: s(v, user_key) for k, v in {"title": "Ms", "firstName": "Alice", "lastName": "Doe",
                                                   "email": "alice@example.com", "city": "Berlin", "country": "DE"}.items()}})
ssh = ec.generate_private_key(ec.SECP256R1())  # stands in for an ed25519 key; only the strings matter
post({"type": 5, "name": s("SSH key", user_key), "reprompt": 0,
      "sshKey": {"privateKey": s("-----BEGIN OPENSSH PRIVATE KEY-----\nfixture\n-----END OPENSSH PRIVATE KEY-----", user_key),
                 "publicKey": s("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFixture fixture", user_key),
                 "keyFingerprint": s("SHA256:fixturefingerprint", user_key)}})
# Cipher with its own key
ck = os.urandom(64)
own = login("Login with own key", ck)
own["key"] = enc(ck, user_key)
post(own)
# Login with fido2 credential
fido = {k: s(v, user_key) for k, v in {"credentialId": str(uuid.uuid4()), "keyType": "public-key", "keyAlgorithm": "ECDSA",
                                        "keyCurve": "P-256", "keyValue": "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgfixture",
                                        "rpId": "example.com", "userHandle": "dXNlcg", "userName": "alice",
                                        "counter": "0", "rpName": "Example", "userDisplayName": "Alice",
                                        "discoverable": "true"}.items()}
fido["creationDate"] = "2026-11-20T00:00:00.000Z"
post(login("Passkey login", user_key, fido2Credentials=[fido]))

# Organization
org_key = os.urandom(64)
org_priv = rsa.generate_private_key(public_exponent=65537, key_size=2048)
wrapped = b64(priv.public_key().encrypt(org_key, padding.OAEP(padding.MGF1(hashes.SHA1()), hashes.SHA1(), None)))
org = S.post(BASE + "/api/organizations", headers=A, json={
    "name": "Fixture Org", "billingEmail": EMAIL, "collectionName": s("Default collection", org_key),
    "key": "4." + wrapped, "planType": 0,
    "keys": {"publicKey": b64(org_priv.public_key().public_bytes(serialization.Encoding.DER, serialization.PublicFormat.SubjectPublicKeyInfo)),
             "encryptedPrivateKey": enc(org_priv.private_bytes(serialization.Encoding.DER, serialization.PrivateFormat.PKCS8, serialization.NoEncryption()), org_key)}})
print("org", org.status_code, org.text[:300])
org_id = org.json()["id"]
cols = S.get(BASE + f"/api/organizations/{org_id}/collections", headers=A).json()
col_id = cols["data"][0]["id"]
oc = login("Org login", org_key)
oc["organizationId"] = org_id
post({"cipher": oc, "collectionIds": [col_id]}, "/api/ciphers/create")

save("sync.json", S.get(BASE + "/api/sync?excludeDomains=true", headers=A))
save("revision_date.json", S.get(BASE + "/api/accounts/revision-date", headers=A))

# Writes
lc = login_resp.json()
time.sleep(2)
upd = login("Example login (edited)", user_key)
upd["folderId"] = folder["id"]
upd["lastKnownRevisionDate"] = lc["revisionDate"]
upd["encryptedFor"] = USER_ID
r = S.put(BASE + f"/api/ciphers/{lc['id']}", json=upd, headers=A)
save("cipher_put_success.json", r)
upd["lastKnownRevisionDate"] = lc["revisionDate"]  # now stale
time.sleep(2)
save("cipher_put_out_of_date.json", S.put(BASE + f"/api/ciphers/{lc['id']}", json=upd, headers=A))

# Enable TOTP 2FA, then record the 2FA challenge
secret = "JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP"


def totp(sec):
    k = base64.b32decode(sec)
    h = hmac.new(k, struct.pack(">Q", int(time.time()) // 30), hashlib.sha1).digest()
    o = h[-1] & 15
    return "%06d" % ((struct.unpack(">I", h[o:o + 4])[0] & 0x7fffffff) % 1000000)


r = S.post(BASE + "/api/two-factor/authenticator", headers=A,
           json={"key": secret, "token": totp(secret), "masterPasswordHash": mp_hash})
print("2fa", r.status_code, r.text[:200])
save("token_two_factor_required.json", token(mp_hash))


