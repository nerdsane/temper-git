#!/usr/bin/env bash
set -euo pipefail

# Live smoke for Genesis registry tenant partitioning.
#
# Requires a running Temper server with the temper-git specs loaded for each
# tenant in TENANTS. Example:
#
#   cargo run -p temper-cli -- serve --port 3142 --storage turso \
#     --app temper-git \
#     --app beta=/path/to/temper-git/specs \
#     --app gamma=/path/to/temper-git/specs
#
# The smoke writes the same Owner/Repository/App ids into each tenant at the
# same time, then verifies every tenant reads back only its own registry values.

BASE_URL="${TEMPER_URL:-http://127.0.0.1:3000}"
BASE_URL="${BASE_URL%/}"
TENANTS="${TENANTS:-default beta gamma}"
APP_COUNT="${APP_COUNT:-12}"
PRINCIPAL_ID="${TEMPER_PRINCIPAL_ID:-operator}"
RUN_ID="${RUN_ID:-$(date +%s)-$$}"
OWNER_ID="owner-xtenant-${RUN_ID}"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/temper-registry-xtenant.XXXXXX")"

cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

json_escape() {
  node -e 'process.stdout.write(JSON.stringify(process.argv[1]))' "$1"
}

urlencode() {
  node -e 'process.stdout.write(encodeURIComponent(process.argv[1]))' "$1"
}

sha1_hex() {
  node -e '
    const crypto = require("crypto");
    process.stdout.write(crypto.createHash("sha1").update(process.argv[1]).digest("hex"));
  ' "$1"
}

post_json() {
  local tenant="$1"
  local path="$2"
  local body="$3"
  local out="$TMP_DIR/post-${tenant//[^a-zA-Z0-9]/-}.json"
  local status
  local headers=(
    -H "X-Tenant-Id: ${tenant}"
    -H "X-Temper-Principal-Kind: admin"
    -H "X-Temper-Principal-Id: ${PRINCIPAL_ID}"
    -H "X-Temper-Principal-Scopes: admin:repos repo:write pr:write"
    -H "Accept: application/json"
  )
  status="$(
    curl -sS -o "$out" -w "%{http_code}" \
      -X POST "${headers[@]}" -H "Content-Type: application/json" \
      -d "$body" "${BASE_URL}${path}"
  )"
  if [[ "$status" != 2* ]]; then
    printf 'POST %s for tenant %s failed with HTTP %s\n' "$path" "$tenant" "$status" >&2
    sed -n '1,160p' "$out" >&2
    exit 1
  fi
}

get_entity() {
  local tenant="$1"
  local set_name="$2"
  local entity_id="$3"
  local out="$4"
  local headers=(
    -H "X-Tenant-Id: ${tenant}"
    -H "X-Temper-Principal-Kind: admin"
    -H "X-Temper-Principal-Id: ${PRINCIPAL_ID}"
    -H "X-Temper-Principal-Scopes: admin:repos repo:write pr:write"
    -H "Accept: application/json"
  )
  curl -fsS "${headers[@]}" "${BASE_URL}/tdata/${set_name}('${entity_id}')" > "$out"
}

field_from_entity() {
  local tenant="$1"
  local set_name="$2"
  local entity_id="$3"
  local field_name="$4"
  local body="$TMP_DIR/entity-${tenant//[^a-zA-Z0-9]/-}-${set_name}-${field_name}.json"
  get_entity "$tenant" "$set_name" "$entity_id" "$body"
  node -e '
    const fs = require("fs");
    const row = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const field = process.argv[2];
    const value = (row.fields && row.fields[field]) ?? row[field] ?? "";
    process.stdout.write(String(value));
  ' "$body" "$field_name"
}

collection_count_for_owner() {
  local tenant="$1"
  local set_name="$2"
  local field_name="$3"
  local body="$TMP_DIR/${tenant//[^a-zA-Z0-9]/-}-${set_name}.json"
  local filter
  filter="$(urlencode "${field_name} eq '${OWNER_ID}'")"
  local headers=(
    -H "X-Tenant-Id: ${tenant}"
    -H "X-Temper-Principal-Kind: admin"
    -H "X-Temper-Principal-Id: ${PRINCIPAL_ID}"
    -H "X-Temper-Principal-Scopes: admin:repos repo:write pr:write"
    -H "Accept: application/json"
  )
  curl -fsS "${headers[@]}" "${BASE_URL}/tdata/${set_name}?\$filter=${filter}&\$top=5000" > "$body"
  node -e '
    const fs = require("fs");
    const body = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    process.stdout.write(String(Array.isArray(body.value) ? body.value.length : 0));
  ' "$body"
}

create_owner() {
  local tenant="$1"
  post_json "$tenant" "/tdata/Owners" \
    "{\"Id\":$(json_escape "$OWNER_ID"),\"AccountId\":$(json_escape "$OWNER_ID"),\"DisplayName\":$(json_escape "Cross tenant ${tenant}"),\"Contact\":$(json_escape "${tenant}@example.invalid"),\"StorageCapBytes\":104857600,\"RateLimitTier\":\"stress\",\"PublicKey\":\"\",\"VerificationProvider\":\"manual\",\"VerificationSubject\":$(json_escape "$tenant"),\"VerificationRequestedAt\":\"1970-01-01T00:00:00Z\"}"
}

create_registry_pair() {
  local tenant="$1"
  local index="$2"
  local repo_id="rp-xtenant-${RUN_ID}-${index}"
  local app_id="app-xtenant-${RUN_ID}-${index}"
  local app_name="shared-app-${index}"
  local hash
  hash="$(sha1_hex "${tenant}-${RUN_ID}-${index}")"

  post_json "$tenant" "/tdata/Repositories" \
    "{\"Id\":$(json_escape "$repo_id"),\"OwnerAccountId\":$(json_escape "$OWNER_ID"),\"Name\":$(json_escape "$app_name"),\"Description\":$(json_escape "repo ${index} for ${tenant}"),\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}"
  post_json "$tenant" "/tdata/Apps" \
    "{\"Id\":$(json_escape "$app_id"),\"OwnerId\":$(json_escape "$OWNER_ID"),\"Name\":$(json_escape "$app_name"),\"RepositoryId\":$(json_escape "$repo_id"),\"LatestVersionHash\":$(json_escape "$hash"),\"Exports\":$(json_escape "{\"tenant\":\"${tenant}\",\"index\":${index}}"),\"Description\":$(json_escape "app ${index} for ${tenant}"),\"Visibility\":\"public\"}"
}

verify_registry_pair() {
  local tenant="$1"
  local index="$2"
  local repo_id="rp-xtenant-${RUN_ID}-${index}"
  local app_id="app-xtenant-${RUN_ID}-${index}"
  local expected_hash
  expected_hash="$(sha1_hex "${tenant}-${RUN_ID}-${index}")"

  local hash description repo_description
  hash="$(field_from_entity "$tenant" Apps "$app_id" LatestVersionHash)"
  description="$(field_from_entity "$tenant" Apps "$app_id" Description)"
  repo_description="$(field_from_entity "$tenant" Repositories "$repo_id" Description)"

  if [[ "$hash" != "$expected_hash" ]]; then
    printf 'Tenant %s App %s hash mismatch: got %s expected %s\n' "$tenant" "$app_id" "$hash" "$expected_hash" >&2
    exit 1
  fi
  if [[ "$description" != "app ${index} for ${tenant}" ]]; then
    printf 'Tenant %s App %s description leaked or mismatched: %s\n' "$tenant" "$app_id" "$description" >&2
    exit 1
  fi
  if [[ "$repo_description" != "repo ${index} for ${tenant}" ]]; then
    printf 'Tenant %s Repository %s description leaked or mismatched: %s\n' "$tenant" "$repo_id" "$repo_description" >&2
    exit 1
  fi
}

printf 'Creating shared owner %s in tenants: %s\n' "$OWNER_ID" "$TENANTS"
for tenant in $TENANTS; do
  create_owner "$tenant"
done

printf 'Creating %s shared-id Repository/App pairs per tenant through live OData\n' "$APP_COUNT"
pids=()
for tenant in $TENANTS; do
  for index in $(seq 1 "$APP_COUNT"); do
    (create_registry_pair "$tenant" "$index") &
    pids+=("$!")
  done
done
for pid in "${pids[@]}"; do
  wait "$pid"
done

printf 'Verifying tenant-local App and Repository values\n'
checks=0
for tenant in $TENANTS; do
  for index in $(seq 1 "$APP_COUNT"); do
    verify_registry_pair "$tenant" "$index"
    checks=$((checks + 2))
  done
  app_count="$(collection_count_for_owner "$tenant" Apps OwnerId)"
  repo_count="$(collection_count_for_owner "$tenant" Repositories OwnerAccountId)"
  if [[ "$app_count" -ne "$APP_COUNT" ]]; then
    printf 'Tenant %s expected %s App rows for owner, got %s\n' "$tenant" "$APP_COUNT" "$app_count" >&2
    exit 1
  fi
  if [[ "$repo_count" -ne "$APP_COUNT" ]]; then
    printf 'Tenant %s expected %s Repository rows for owner, got %s\n' "$tenant" "$APP_COUNT" "$repo_count" >&2
    exit 1
  fi
  checks=$((checks + 2))
done

tenant_count="$(printf '%s\n' $TENANTS | wc -l | tr -d ' ')"
printf 'PASS Registry cross-tenant smoke\n'
printf '  run: %s\n' "$RUN_ID"
printf '  tenants: %s\n' "$TENANTS"
printf '  apps per tenant: %s\n' "$APP_COUNT"
printf '  shared owner id: %s\n' "$OWNER_ID"
printf '  verification checks: %s\n' "$checks"
printf '  shared ids exercised: %s\n' "$((tenant_count * APP_COUNT))"
