#!/usr/bin/env bash
# Mints the display's own 1-year Claude token (OAuth + PKCE with Claude Code's public client ID),
# saves it to .device-auth/claude.json and checks it against the usage endpoint.
# Recipe from github.com/Pharkie/claude-usage-epaper: the usage endpoint needs `user:profile`, a
# 1-year `expires_in` is only granted for narrow scopes, and the OAuth endpoints answer a fake-looking
# User-Agent with a permanent 429. The device never refreshes the token: a refresh would swap it for
# an 8-hour one. Re-run this once a year, or when the screen says "Sign in again".
# Usage: scripts/claude-login.sh        (SCOPE="user:profile user:inference" if user:profile alone is refused)
set -euo pipefail
cd "$(dirname "$0")/.."
umask 077
mkdir -p .device-auth
CLIENT_ID=9d1c250a-e61b-44d9-88ed-5944d1962f5e
CALLBACK=https://platform.claude.com/oauth/code/callback
VERSION=$(claude --version 2>/dev/null | cut -d' ' -f1)
VERSION=${VERSION:-2.1.330}
SCOPE=${SCOPE:-user:profile}
uri() { jq -rn --arg v "$1" '$v|@uri'; }
b64url() { openssl base64 -A | tr '+/' '-_' | tr -d '='; }

verifier=$(openssl rand 32 | b64url)
state=$(openssl rand 32 | b64url)
challenge=$(printf %s "$verifier" | openssl dgst -sha256 -binary | b64url)
echo "1) Open this URL, sign in with the account to display, click Authorize:"
echo
echo "https://claude.ai/oauth/authorize?code=true&client_id=$CLIENT_ID&response_type=code&redirect_uri=$(uri "$CALLBACK")&scope=$(uri "$SCOPE")&code_challenge=$challenge&code_challenge_method=S256&state=$state"
echo
read -rsp "2) Paste the code shown (code#state, hidden): " pasted; echo
IFS='#' read -r code got_state <<< "$pasted"
[[ -z "$got_state" || "$got_state" == "$state" ]] || { echo "State mismatch: use the URL above." >&2; exit 1; }

jq -n --arg code "$code" --arg state "$state" --arg c "$CLIENT_ID" --arg r "$CALLBACK" --arg v "$verifier" \
    '{grant_type:"authorization_code",code:$code,state:$state,client_id:$c,redirect_uri:$r,code_verifier:$v,expires_in:31536000}' |
    curl -sS --fail-with-body -H 'Content-Type: application/json' -H 'anthropic-beta: oauth-2025-04-20' \
        -A "claude-cli/$VERSION (external, cli)" --data-binary @- \
        https://platform.claude.com/v1/oauth/token > .device-auth/claude.json ||
    { cat .device-auth/claude.json >&2; rm -f .device-auth/claude.json; exit 1; }
jq -r '"Token valid \(.expires_in / 86400 | floor) days, scope: \(.scope)"' .device-auth/claude.json

# Check it: the header goes through a file so the token stays out of the process list.
jq -r '"Authorization: Bearer \(.access_token)"' .device-auth/claude.json > .device-auth/claude.hdr
curl -sS --fail-with-body -H @.device-auth/claude.hdr -H 'anthropic-beta: oauth-2025-04-20' \
    -A "claude-code/$VERSION" https://api.anthropic.com/api/oauth/usage |
    jq '{five_hour, seven_day}'
rm .device-auth/claude.hdr
