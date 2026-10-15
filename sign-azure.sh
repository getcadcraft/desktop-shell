#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# sign-azure.sh — sign one Windows file with Azure Artifact Signing through jsign (W.6; SIGNING.md).
#
#   sign-azure.sh <file.exe|file.msi>     sign it in place (RFC 3161 timestamp from Microsoft)
#   sign-azure.sh --check                 only fetch an access token: proves the app registration and secret work
#
# Used as Tauri's signCommand:  CC_SIGN_COMMAND='/path/to/sign-azure.sh %1' node build.mjs studio …
# Everything comes from the environment (nothing about any server is written here):
#   AZURE_TENANT_ID, AZURE_CLIENT_ID, AZURE_CLIENT_SECRET   the app registration with the
#                                                           "Artifact Signing Certificate Profile Signer" role
#   AZURE_SIGNING_ENDPOINT   e.g. eus.codesigning.azure.net
#   AZURE_SIGNING_ACCOUNT    the Artifact Signing account
#   AZURE_SIGNING_PROFILE    the certificate profile
#   JSIGN_JAR                jsign's jar (7.5 or later)
# The access token is fetched per call (it lasts about an hour) and passed to jsign only; it is never printed.
set -euo pipefail

need() { [ -n "${!1:-}" ] || { echo "sign-azure: $1 is not set" >&2; exit 2; }; }
for v in AZURE_TENANT_ID AZURE_CLIENT_ID AZURE_CLIENT_SECRET; do need "$v"; done

token() {
  local body
  body=$(curl -sS --fail-with-body -X POST "https://login.microsoftonline.com/${AZURE_TENANT_ID}/oauth2/v2.0/token" \
    --data-urlencode "grant_type=client_credentials" \
    --data-urlencode "client_id=${AZURE_CLIENT_ID}" \
    --data-urlencode "client_secret=${AZURE_CLIENT_SECRET}" \
    --data-urlencode "scope=https://codesigning.azure.net/.default") || { echo "sign-azure: Microsoft refused the sign-in:" >&2; echo "$body" | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("error"), "-", d.get("error_description","").splitlines()[0])' >&2 || true; exit 3; }
  python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])' <<<"$body"
}

if [ "${1:-}" = "--check" ]; then
  t=$(token)
  echo "sign-azure: signed in as the app registration (token of ${#t} characters, not shown)"
  exit 0
fi

[ $# -eq 1 ] && [ -f "$1" ] || { echo "usage: sign-azure.sh <file> | --check" >&2; exit 2; }
for v in AZURE_SIGNING_ENDPOINT AZURE_SIGNING_ACCOUNT AZURE_SIGNING_PROFILE JSIGN_JAR; do need "$v"; done

SIGN_AZURE_TOKEN=$(token)
export SIGN_AZURE_TOKEN   # jsign reads it from the environment: never on a command line (ps would show it)
java -jar "$JSIGN_JAR" \
  --storetype TRUSTEDSIGNING \
  --keystore "$AZURE_SIGNING_ENDPOINT" \
  --storepass env:SIGN_AZURE_TOKEN \
  --alias "${AZURE_SIGNING_ACCOUNT}/${AZURE_SIGNING_PROFILE}" \
  --tsaurl http://timestamp.acs.microsoft.com --tsmode RFC3161 \
  "$1"
