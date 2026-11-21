#!/usr/bin/env nix-shell
#!nix-shell -i bash -p vaultwarden mailpit oath-toolkit curl "python3.withPackages (ps: [ ps.cryptography ])"
# Runs a throwaway Vaultwarden server for testing, with Mailpit to catch mail.
#
#   Vault:   http://$HOST:$PORT        (web vault, sign up here)
#   Admin:   http://$HOST:$PORT/admin  (token: $ADMIN_TOKEN)
#   Mail:    http://$HOST:$MAIL_UI_PORT (email 2FA codes and verify links)
#
# TOTP is on by default in Vaultwarden. To get a code for a test account,
# copy the secret from the 2FA setup screen and run:
#   oathtool --totp -b <secret>
#
# Env overrides: HOST, PORT, MAIL_UI_PORT, SMTP_PORT, ADMIN_TOKEN, DATA_DIR.
# Set KEEP=1 to keep the data folder after exit.
set -euo pipefail

HOST="${HOST:-127.0.0.1}"
PORT="${PORT:-8000}"
MAIL_UI_PORT="${MAIL_UI_PORT:-8025}"
SMTP_PORT="${SMTP_PORT:-1025}"
ADMIN_TOKEN="${ADMIN_TOKEN:-admin}"
DATA_DIR="${DATA_DIR:-$(mktemp -d -t vaultwarden-test.XXXXXX)}"
TEST_EMAIL="${TEST_EMAIL:-test@test.local}"
TEST_PASSWORD="${TEST_PASSWORD:-test}"
# Fixed so tests can compute TOTP codes. Base32, 20 bytes.
TEST_TOTP_SECRET="${TEST_TOTP_SECRET:-JBSWY3DPEHPK3PXPJBSWY3DPEHPK3PXP}"

# The web vault is a separate package, so ask nix for its store path.
WEB_VAULT="$(nix-build '<nixpkgs>' -A vaultwarden.webvault --no-out-link)/share/vaultwarden/vault"

cleanup() {
  kill "${VAULTWARDEN_PID:-}" "${MAILPIT_PID:-}" 2>/dev/null || true
  if [[ "${KEEP:-0}" != 1 ]]; then
    rm -rf "$DATA_DIR"
  else
    echo "Data kept in $DATA_DIR"
  fi
}
trap cleanup EXIT

mailpit \
  --smtp "$HOST:$SMTP_PORT" \
  --listen "$HOST:$MAIL_UI_PORT" \
  --smtp-auth-accept-any \
  --smtp-auth-allow-insecure \
  --database "$DATA_DIR/mailpit.db" \
  >"$DATA_DIR/mailpit.log" 2>&1 &
MAILPIT_PID=$!

echo "Vault:  http://$HOST:$PORT"
echo "Admin:  http://$HOST:$PORT/admin (token: $ADMIN_TOKEN)"
echo "Mail:   http://$HOST:$MAIL_UI_PORT"
echo "Data:   $DATA_DIR"

export DATA_FOLDER="$DATA_DIR"
export WEB_VAULT_FOLDER="$WEB_VAULT"
export WEB_VAULT_ENABLED=true
export ROCKET_ADDRESS="$HOST"
export ROCKET_PORT="$PORT"
export DOMAIN="http://$HOST:$PORT"
export ADMIN_TOKEN
export SIGNUPS_ALLOWED=true
export SIGNUPS_VERIFY=false
export LOG_LEVEL=info

export SMTP_HOST="$HOST"
export SMTP_PORT
export SMTP_SECURITY=off
export SMTP_FROM=vaultwarden@test.local
export SMTP_FROM_NAME=Vaultwarden
export _ENABLE_EMAIL_2FA=true

vaultwarden >"$DATA_DIR/vaultwarden.log" 2>&1 &
VAULTWARDEN_PID=$!

until curl -sf "http://$HOST:$PORT/alive" >/dev/null; do
  kill -0 "$VAULTWARDEN_PID" 2>/dev/null || { cat "$DATA_DIR/vaultwarden.log"; exit 1; }
  sleep 0.2
done

python3 "$(dirname "$0")/vaultwarden_seed.py" \
  "http://$HOST:$PORT" "http://$HOST:$MAIL_UI_PORT" \
  "$TEST_EMAIL" "$TEST_PASSWORD" "$TEST_TOTP_SECRET"

echo
echo "Test user: $TEST_EMAIL / $TEST_PASSWORD"
echo "2FA:       TOTP and email"
echo "TOTP code: oathtool --totp -b $TEST_TOTP_SECRET"
echo "Logs:      $DATA_DIR/vaultwarden.log"

wait "$VAULTWARDEN_PID"
