#!/usr/bin/env bash
set -euo pipefail

# Managed live smoke for Genesis commons guardrails plus tenant-scoped
# registry Composite actions.
#
# The script owns the server lifecycle:
#   1. start a fresh operator-mode server and seed verified owners plus
#      Repository/Ref/App rows that represent already-admitted registry data;
#   2. restart the same DB in commons mode;
#   3. prove direct App/Repository mutation, including spoofed action_context
#      headers, is denied;
#   4. run mixed App.RegisterNewApp, App.PublishNewVersion, and App.Fork
#      actions as customer principals and verify the spec-declared Composite
#      sub-writes.
#
# WASM is intentionally only a Composite result producer. The customer request
# invokes spec-declared App actions; the kernel validates and applies sub-writes.

KERNEL_DIR="${TEMPER_KERNEL_DIR:-/Users/seshendranalla/Development/temper-worktrees/genesis-kernel-primitives}"
APP_WORKTREES_DIR="${TEMPER_APP_WORKTREES_DIR:-/Users/seshendranalla/Development/temper-git-worktrees}"
APP_DIR="${TEMPER_APP_DIR:-/Users/seshendranalla/Development/temper-git-worktrees/genesis}"
PORT="${PORT:-3155}"
BASE_URL="http://127.0.0.1:${PORT}"
PRIMARY_TENANT="${PRIMARY_TENANT:-default}"
SECONDARY_TENANT="${SECONDARY_TENANT:-beta}"
TENANTS="${TENANTS:-${PRIMARY_TENANT} ${SECONDARY_TENANT}}"
REGISTER_COUNT="${REGISTER_COUNT:-8}"
PUBLISH_COUNT="${PUBLISH_COUNT:-8}"
FORK_COUNT="${FORK_COUNT:-8}"
PARALLELISM="${PARALLELISM:-6}"
RUN_ID="${RUN_ID:-$(date +%s)-$$}"
TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/temper-commons-mixed.XXXXXX")"
TMP_DIR="${TMP_ROOT}/work"
HOME_DIR="${TMP_ROOT}/home"
DB_PATH="${TMP_ROOT}/agents.db"
SERVER_PID=""
REAL_HOME="${REAL_HOME:-$HOME}"
REAL_CARGO_HOME="${CARGO_HOME:-${REAL_HOME}/.cargo}"
REAL_RUSTUP_HOME="${RUSTUP_HOME:-${REAL_HOME}/.rustup}"

mkdir -p "$TMP_DIR" "$HOME_DIR"

cleanup() {
  local status=$?
  stop_server >/dev/null 2>&1 || true
  if [[ "${KEEP_TMP:-}" == "1" || "$status" -ne 0 ]]; then
    printf 'Preserved temp directory for inspection: %s\n' "$TMP_ROOT" >&2
  else
    rm -rf "$TMP_ROOT"
  fi
}
trap cleanup EXIT

json_escape() {
  node -e 'process.stdout.write(JSON.stringify(process.argv[1]))' "$1"
}

sha1_hex() {
  node -e '
    const crypto = require("crypto");
    process.stdout.write(crypto.createHash("sha1").update(process.argv[1]).digest("hex"));
  ' "$1"
}

sanitize_component() {
  node -e '
    const raw = process.argv[1].toLowerCase();
    const out = raw
      .split("")
      .map(ch => /[a-z0-9]/.test(ch) ? ch : "-")
      .join("")
      .replace(/-+/g, "-")
      .replace(/^-|-$/g, "");
    process.stdout.write(out);
  ' "$1"
}

wait_for_server() {
  local tenant="$1"
  local deadline=$((SECONDS + 300))
  local out="$TMP_DIR/readiness-${tenant}.json"
  while [[ "$SECONDS" -lt "$deadline" ]]; do
    if curl -fsS -H "X-Tenant-Id: ${tenant}" "${BASE_URL}/tdata/Apps?\$top=1" > "$out" 2>/dev/null; then
      return 0
    fi
    if [[ -n "$SERVER_PID" ]] && ! kill -0 "$SERVER_PID" >/dev/null 2>&1; then
      printf 'Server exited before tenant %s was ready. Log follows:\n' "$tenant" >&2
      sed -n '1,220p' "$TMP_DIR/server.log" >&2 || true
      exit 1
    fi
    sleep 1
  done
  printf 'Timed out waiting for tenant %s. Log follows:\n' "$tenant" >&2
  sed -n '1,220p' "$TMP_DIR/server.log" >&2 || true
  exit 1
}

start_server() {
  local mode="$1"
  local auto_tenant="$2"
  if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    printf 'Port %s is already in use\n' "$PORT" >&2
    lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >&2 || true
    exit 1
  fi

  : > "$TMP_DIR/server.log"
  (
    cd "$KERNEL_DIR"
    HOME="$HOME_DIR" \
    CARGO_HOME="$REAL_CARGO_HOME" \
    RUSTUP_HOME="$REAL_RUSTUP_HOME" \
    TURSO_URL="file:${DB_PATH}" \
    TEMPER_ACTION_TIMEOUT_SECS=120 \
    TEMPER_OS_APP_TEMPER_GIT_MODE="$mode" \
    TEMPER_OS_APPS_DIR="$APP_WORKTREES_DIR" \
    TEMPER_GENESIS_WEB_DIR="${APP_DIR}/web/build" \
    TEMPER_TENANT="$auto_tenant" \
    cargo run -p temper-cli -- serve --port "$PORT" --storage turso --no-observe --app temper-git
  ) > "$TMP_DIR/server.log" 2>&1 &
  SERVER_PID="$!"

  for tenant in $TENANTS; do
    wait_for_server "$tenant"
  done
}

stop_server() {
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
  fi
  for _ in $(seq 1 20); do
    if ! lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
      break
    fi
    sleep 0.5
  done
  if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    while read -r pid; do
      [[ -n "$pid" ]] || continue
      kill "$pid" >/dev/null 2>&1 || true
    done < <(lsof -nP -t -iTCP:"$PORT" -sTCP:LISTEN)
    sleep 1
  fi
  if lsof -nP -iTCP:"$PORT" -sTCP:LISTEN >/dev/null 2>&1; then
    while read -r pid; do
      [[ -n "$pid" ]] || continue
      kill -9 "$pid" >/dev/null 2>&1 || true
    done < <(lsof -nP -t -iTCP:"$PORT" -sTCP:LISTEN)
  fi
  if [[ -n "$SERVER_PID" ]]; then
    wait "$SERVER_PID" >/dev/null 2>&1 || true
  fi
  SERVER_PID=""
}

headers_for() {
  local tenant="$1"
  local kind="$2"
  local principal="$3"
  local scopes="$4"
  printf '%s\n' \
    "-H" "X-Tenant-Id: ${tenant}" \
    "-H" "X-Temper-Principal-Kind: ${kind}" \
    "-H" "X-Temper-Principal-Id: ${principal}" \
    "-H" "X-Temper-Principal-Scopes: ${scopes}" \
    "-H" "Accept: application/json"
}

curl_headers_array() {
  local -n target="$1"
  shift
  target=("$@")
}

admin_post_json() {
  local tenant="$1"
  local path="$2"
  local body="$3"
  local out="$TMP_DIR/admin-post-${tenant//[^a-zA-Z0-9]/-}-${RANDOM}.json"
  local status
  local headers=(
    -H "X-Tenant-Id: ${tenant}"
    -H "X-Temper-Principal-Kind: admin"
    -H "X-Temper-Principal-Id: operator"
    -H "X-Temper-Principal-Scopes: admin:repos admin:owners repo:write pr:write"
    -H "Accept: application/json"
    -H "Content-Type: application/json"
  )
  status="$(curl -sS -o "$out" -w "%{http_code}" -X POST "${headers[@]}" -d "$body" "${BASE_URL}${path}")"
  if [[ "$status" != 2* ]]; then
    printf 'Admin POST %s tenant %s failed with HTTP %s\n' "$path" "$tenant" "$status" >&2
    sed -n '1,180p' "$out" >&2
    exit 1
  fi
}

customer_post_json_retry() {
  local tenant="$1"
  local owner="$2"
  local path="$3"
  local body="$4"
  local idempotency_key="$5"
  local retry_slot="${6:-0}"
  local max_attempts="${7:-18}"
  local attempt=1
  local out
  local status
  local headers=(
    -H "X-Tenant-Id: ${tenant}"
    -H "X-Temper-Principal-Kind: customer"
    -H "X-Temper-Principal-Id: ${owner}"
    -H "X-Temper-Principal-Scopes: repo:write pr:write"
    -H "Accept: application/json"
    -H "Content-Type: application/json"
  )

  while [[ "$attempt" -le "$max_attempts" ]]; do
    out="$TMP_DIR/customer-post-${tenant//[^a-zA-Z0-9]/-}-${idempotency_key}-${attempt}.json"
    status="$(
      curl -sS -o "$out" -w "%{http_code}" \
        -X POST "${headers[@]}" -H "Idempotency-Key: ${idempotency_key}" \
        -d "$body" "${BASE_URL}${path}"
    )"
    if [[ "$status" == 2* ]]; then
      printf '%s' "$attempt"
      return 0
    fi
    if { [[ "$status" == "409" ]] && grep -q "optimistic concurrency\\|composite batch persistence conflict" "$out"; } || [[ "$status" == "503" ]]; then
      local jitter_digit=$(( (attempt + retry_slot) % 7 + 2 ))
      sleep "0.${jitter_digit}"
      attempt=$((attempt + 1))
      continue
    fi
    printf 'Customer POST %s tenant %s failed with HTTP %s on attempt %s\n' "$path" "$tenant" "$status" "$attempt" >&2
    sed -n '1,180p' "$out" >&2
    exit 1
  done

  printf 'Customer POST %s tenant %s still failed after %s attempts\n' "$path" "$tenant" "$max_attempts" >&2
  sed -n '1,180p' "$out" >&2
  exit 1
}

expect_customer_post_denied() {
  local tenant="$1"
  local owner="$2"
  local path="$3"
  local body="$4"
  local label="$5"
  shift 5
  local out="$TMP_DIR/denied-${tenant//[^a-zA-Z0-9]/-}-${label}.json"
  local status
  local headers=(
    -H "X-Tenant-Id: ${tenant}"
    -H "X-Temper-Principal-Kind: customer"
    -H "X-Temper-Principal-Id: ${owner}"
    -H "X-Temper-Principal-Scopes: repo:write pr:write"
    -H "Accept: application/json"
    -H "Content-Type: application/json"
    "$@"
  )
  status="$(curl -sS -o "$out" -w "%{http_code}" -X POST "${headers[@]}" -d "$body" "${BASE_URL}${path}")"
  if [[ "$status" != "403" ]]; then
    printf 'Expected denied %s tenant %s to return HTTP 403, got %s\n' "$label" "$tenant" "$status" >&2
    sed -n '1,180p' "$out" >&2
    exit 1
  fi
  if ! grep -q "AuthorizationDenied" "$out"; then
    printf 'Expected denied %s tenant %s to be AuthorizationDenied\n' "$label" "$tenant" >&2
    sed -n '1,180p' "$out" >&2
    exit 1
  fi
}

entity_exists() {
  local tenant="$1"
  local set_name="$2"
  local entity_id="$3"
  curl -fsS \
    -H "X-Tenant-Id: ${tenant}" \
    -H "X-Temper-Principal-Kind: admin" \
    -H "X-Temper-Principal-Id: operator" \
    -H "X-Temper-Principal-Scopes: admin:repos admin:owners repo:write pr:write" \
    -H "Accept: application/json" \
    "${BASE_URL}/tdata/${set_name}('${entity_id}')" >/dev/null 2>&1
}

field_from_entity() {
  local tenant="$1"
  local set_name="$2"
  local entity_id="$3"
  local field_name="$4"
  local body="$TMP_DIR/entity-${tenant//[^a-zA-Z0-9]/-}-${set_name}-${field_name}-${RANDOM}.json"
  curl -fsS \
    -H "X-Tenant-Id: ${tenant}" \
    -H "X-Temper-Principal-Kind: admin" \
    -H "X-Temper-Principal-Id: operator" \
    -H "X-Temper-Principal-Scopes: admin:repos admin:owners repo:write pr:write" \
    -H "Accept: application/json" \
    "${BASE_URL}/tdata/${set_name}('${entity_id}')" > "$body"
  node -e '
    const fs = require("fs");
    const row = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const field = process.argv[2];
    const value = (row.fields && row.fields[field]) ?? row[field] ?? "";
    process.stdout.write(String(value));
  ' "$body" "$field_name"
}

collection_count_all() {
  local tenant="$1"
  local set_name="$2"
  local body="$TMP_DIR/count-${tenant//[^a-zA-Z0-9]/-}-${set_name}-${RANDOM}.json"
  curl -fsS \
    -H "X-Tenant-Id: ${tenant}" \
    -H "X-Temper-Principal-Kind: admin" \
    -H "X-Temper-Principal-Id: operator" \
    -H "X-Temper-Principal-Scopes: admin:repos admin:owners repo:write pr:write" \
    -H "Accept: application/json" \
    "${BASE_URL}/tdata/${set_name}?\$top=5000" > "$body"
  node -e '
    const fs = require("fs");
    const body = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    process.stdout.write(String(Array.isArray(body.value) ? body.value.length : 0));
  ' "$body"
}

owner_id() {
  local tenant="$1"
  printf 'commons-owner-%s-%s' "$(sanitize_component "$tenant")" "$RUN_ID"
}

rate_limit_id() {
  local owner="$1"
  printf 'rl-%s-write' "$(sanitize_component "$owner")"
}

register_name() {
  printf 'commons-register-%03d' "$1"
}

register_repo_id() {
  local tenant="$1"
  printf 'rp-cmreg-%s-%s-%03d' "$(sanitize_component "$tenant")" "$RUN_ID" "$2"
}

register_app_id() {
  local owner="$1"
  local index="$2"
  printf 'app-%s-%s' "$(sanitize_component "$owner")" "$(sanitize_component "$(register_name "$index")")"
}

publish_name() {
  printf 'commons-publish-%03d' "$1"
}

publish_repo_id() {
  local tenant="$1"
  printf 'rp-cmpub-%s-%s-%03d' "$(sanitize_component "$tenant")" "$RUN_ID" "$2"
}

publish_app_id() {
  local tenant="$1"
  printf 'app-cmpub-%s-%s-%03d' "$(sanitize_component "$tenant")" "$RUN_ID" "$2"
}

parent_app_id() {
  local tenant="$1"
  printf 'app-cmfork-parent-%s-%s' "$(sanitize_component "$tenant")" "$RUN_ID"
}

parent_repo_id() {
  local tenant="$1"
  printf 'rp-cmfork-parent-%s-%s' "$(sanitize_component "$tenant")" "$RUN_ID"
}

fork_child_name() {
  printf 'commons-fork-%03d' "$1"
}

fork_child_repo_id() {
  local owner="$1"
  local index="$2"
  printf 'rp-%s-%s' "$(sanitize_component "$owner")" "$(sanitize_component "$(fork_child_name "$index")")"
}

fork_child_app_id() {
  local owner="$1"
  local index="$2"
  printf 'app-%s-%s' "$(sanitize_component "$owner")" "$(sanitize_component "$(fork_child_name "$index")")"
}

ref_id_for_repo() {
  local repo_id="$1"
  printf 'rf-%s-refs-heads-main' "$repo_id"
}

initial_hash() {
  sha1_hex "commons-initial-${RUN_ID}-$1"
}

published_hash() {
  sha1_hex "commons-published-${RUN_ID}-$1"
}

fork_hash() {
  sha1_hex "commons-fork-parent-${RUN_ID}-$1"
}

create_and_verify_owner() {
  local tenant="$1"
  local owner="$2"
  admin_post_json "$tenant" "/tdata/Owners" \
    "{\"Id\":$(json_escape "$owner"),\"AccountId\":$(json_escape "$owner"),\"DisplayName\":$(json_escape "Commons ${tenant} owner"),\"Contact\":$(json_escape "${owner}@example.invalid"),\"StorageCapBytes\":1073741824,\"RateLimitTier\":\"stress\",\"PublicKey\":\"\",\"VerificationProvider\":\"manual\",\"VerificationSubject\":$(json_escape "$tenant"),\"VerificationRequestedAt\":\"2026-05-19T00:00:00Z\"}"
  admin_post_json "$tenant" "/tdata/Owners('${owner}')/Temper.Git.MarkVerified" \
    "{\"VerificationProvider\":\"manual\",\"VerificationSubject\":$(json_escape "$tenant"),\"VerifiedAt\":\"2026-05-19T00:00:00Z\"}"
  admin_post_json "$tenant" "/tdata/RateLimits" \
    "{\"Id\":$(json_escape "$(rate_limit_id "$owner")"),\"OwnerId\":$(json_escape "$owner"),\"ActionClass\":\"write\",\"Tokens\":10000,\"Capacity\":10000,\"RefillPerSecond\":0,\"LastRefillAt\":\"2026-05-19T00:00:00Z\"}"
}

seed_register_target() {
  local tenant="$1"
  local owner="$2"
  local index="$3"
  local repo_id
  local ref_id
  local hash
  repo_id="$(register_repo_id "$tenant" "$index")"
  ref_id="$(ref_id_for_repo "$repo_id")"
  hash="$(initial_hash "register-${tenant}-${index}")"

  admin_post_json "$tenant" "/tdata/Repositories" \
    "{\"Id\":$(json_escape "$repo_id"),\"OwnerAccountId\":$(json_escape "$owner"),\"Name\":$(json_escape "$(register_name "$index")"),\"Description\":$(json_escape "commons register ${tenant} ${index}"),\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}"
  admin_post_json "$tenant" "/tdata/Refs" \
    "{\"Id\":$(json_escape "$ref_id"),\"RepositoryId\":$(json_escape "$repo_id"),\"Name\":\"refs/heads/main\",\"TargetCommitSha\":$(json_escape "$hash"),\"Kind\":\"branch\"}"
}

seed_publish_target() {
  local tenant="$1"
  local owner="$2"
  local index="$3"
  local repo_id
  local app_id
  local ref_id
  local hash
  repo_id="$(publish_repo_id "$tenant" "$index")"
  app_id="$(publish_app_id "$tenant" "$index")"
  ref_id="$(ref_id_for_repo "$repo_id")"
  hash="$(initial_hash "publish-${tenant}-${index}")"

  admin_post_json "$tenant" "/tdata/Repositories" \
    "{\"Id\":$(json_escape "$repo_id"),\"OwnerAccountId\":$(json_escape "$owner"),\"Name\":$(json_escape "$(publish_name "$index")"),\"Description\":$(json_escape "commons publish ${tenant} ${index}"),\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}"
  admin_post_json "$tenant" "/tdata/Refs" \
    "{\"Id\":$(json_escape "$ref_id"),\"RepositoryId\":$(json_escape "$repo_id"),\"Name\":\"refs/heads/main\",\"TargetCommitSha\":$(json_escape "$hash"),\"Kind\":\"branch\"}"
  admin_post_json "$tenant" "/tdata/Apps" \
    "{\"Id\":$(json_escape "$app_id"),\"OwnerId\":$(json_escape "$owner"),\"Name\":$(json_escape "$(publish_name "$index")"),\"RepositoryId\":$(json_escape "$repo_id"),\"LatestVersionHash\":$(json_escape "$hash"),\"Exports\":\"{}\",\"Description\":$(json_escape "commons publish app ${tenant} ${index}"),\"Visibility\":\"public\"}"
}

seed_fork_parent() {
  local tenant="$1"
  local owner="$2"
  local hash
  hash="$(fork_hash "$tenant")"
  admin_post_json "$tenant" "/tdata/Apps" \
    "{\"Id\":$(json_escape "$(parent_app_id "$tenant")"),\"OwnerId\":$(json_escape "$owner"),\"Name\":$(json_escape "commons-parent-${tenant}"),\"RepositoryId\":$(json_escape "$(parent_repo_id "$tenant")"),\"LatestVersionHash\":$(json_escape "$hash"),\"Exports\":\"{}\",\"Description\":$(json_escape "commons fork parent ${tenant}"),\"Visibility\":\"public\"}"
}

run_register() {
  local tenant="$1"
  local owner="$2"
  local index="$3"
  local app_id
  local repo_id
  local attempts
  app_id="$(register_app_id "$owner" "$index")"
  repo_id="$(register_repo_id "$tenant" "$index")"
  attempts="$(
    customer_post_json_retry "$tenant" "$owner" \
      "/tdata/Apps('${app_id}')/Temper.Git.RegisterNewApp?await_integration=true" \
      "{\"Name\":$(json_escape "$(register_name "$index")"),\"RepositoryId\":$(json_escape "$repo_id"),\"Description\":$(json_escape "commons registered ${tenant} ${index}"),\"Exports\":\"{}\",\"Visibility\":\"public\"}" \
      "commons-register-${tenant}-${RUN_ID}-${index}" \
      "$index"
  )"
  printf '%s\n' "$attempts" > "$TMP_DIR/register-${tenant//[^a-zA-Z0-9]/-}-${index}.attempts"
}

run_publish() {
  local tenant="$1"
  local owner="$2"
  local index="$3"
  local app_id
  local new_hash
  local attempts
  app_id="$(publish_app_id "$tenant" "$index")"
  new_hash="$(published_hash "${tenant}-${index}")"
  attempts="$(
    customer_post_json_retry "$tenant" "$owner" \
      "/tdata/Apps('${app_id}')/Temper.Git.PublishNewVersion?await_integration=true" \
      "{\"NewHash\":$(json_escape "$new_hash"),\"RefName\":\"main\"}" \
      "commons-publish-${tenant}-${RUN_ID}-${index}" \
      "$((REGISTER_COUNT + index))"
  )"
  printf '%s\n' "$attempts" > "$TMP_DIR/publish-${tenant//[^a-zA-Z0-9]/-}-${index}.attempts"
}

run_fork() {
  local tenant="$1"
  local owner="$2"
  local index="$3"
  local parent_app
  local parent_hash
  local attempts
  parent_app="$(parent_app_id "$tenant")"
  parent_hash="$(fork_hash "$tenant")"
  attempts="$(
    customer_post_json_retry "$tenant" "$owner" \
      "/tdata/Apps('${parent_app}')/Temper.Git.Fork?await_integration=true" \
      "{\"ParentAppId\":$(json_escape "$parent_app"),\"ParentVersionHash\":$(json_escape "$parent_hash"),\"ChildOwnerId\":$(json_escape "$owner"),\"ChildName\":$(json_escape "$(fork_child_name "$index")"),\"Description\":$(json_escape "commons fork ${tenant} ${index}")}" \
      "commons-fork-${tenant}-${RUN_ID}-${index}" \
      "$((REGISTER_COUNT + PUBLISH_COUNT + index))" \
      24
  )"
  printf '%s\n' "$attempts" > "$TMP_DIR/fork-${tenant//[^a-zA-Z0-9]/-}-${index}.attempts"
}

verify_tenant() {
  local tenant="$1"
  local owner="$2"
  local checks=0

  for index in $(seq 1 "$REGISTER_COUNT"); do
    local app_id
    local repo_id
    local expected_hash
    app_id="$(register_app_id "$owner" "$index")"
    repo_id="$(register_repo_id "$tenant" "$index")"
    expected_hash="$(initial_hash "register-${tenant}-${index}")"
    if ! entity_exists "$tenant" Apps "$app_id"; then
      printf 'Missing registered App %s in tenant %s\n' "$app_id" "$tenant" >&2
      exit 1
    fi
    local owner_field
    local repo_field
    local hash_field
    owner_field="$(field_from_entity "$tenant" Apps "$app_id" OwnerId)"
    repo_field="$(field_from_entity "$tenant" Apps "$app_id" RepositoryId)"
    hash_field="$(field_from_entity "$tenant" Apps "$app_id" LatestVersionHash)"
    if [[ "$owner_field" != "$owner" || "$repo_field" != "$repo_id" || "$hash_field" != "$expected_hash" ]]; then
      printf 'Registered App mismatch in tenant %s id %s owner=%s repo=%s hash=%s\n' "$tenant" "$app_id" "$owner_field" "$repo_field" "$hash_field" >&2
      exit 1
    fi
    checks=$((checks + 4))
  done

  for index in $(seq 1 "$PUBLISH_COUNT"); do
    local app_id
    local repo_id
    local ref_id
    local expected_hash
    app_id="$(publish_app_id "$tenant" "$index")"
    repo_id="$(publish_repo_id "$tenant" "$index")"
    ref_id="$(ref_id_for_repo "$repo_id")"
    expected_hash="$(published_hash "${tenant}-${index}")"
    local app_hash
    local ref_hash
    app_hash="$(field_from_entity "$tenant" Apps "$app_id" LatestVersionHash)"
    ref_hash="$(field_from_entity "$tenant" Refs "$ref_id" TargetCommitSha)"
    if [[ "$app_hash" != "$expected_hash" || "$ref_hash" != "$expected_hash" ]]; then
      printf 'Publish mismatch in tenant %s id %s app=%s ref=%s expected=%s\n' "$tenant" "$app_id" "$app_hash" "$ref_hash" "$expected_hash" >&2
      exit 1
    fi
    checks=$((checks + 2))
  done

  for index in $(seq 1 "$FORK_COUNT"); do
    local repo_id
    local app_id
    local ref_id
    local lineage_id
    local expected_hash
    repo_id="$(fork_child_repo_id "$owner" "$index")"
    app_id="$(fork_child_app_id "$owner" "$index")"
    ref_id="$(ref_id_for_repo "$repo_id")"
    lineage_id="ln-${repo_id}-from-$(parent_app_id "$tenant")"
    expected_hash="$(fork_hash "$tenant")"
    for set_and_id in "Repositories:${repo_id}" "Refs:${ref_id}" "Apps:${app_id}" "Lineages:${lineage_id}"; do
      local set_name="${set_and_id%%:*}"
      local entity_id="${set_and_id#*:}"
      if ! entity_exists "$tenant" "$set_name" "$entity_id"; then
        printf 'Missing %s %s in tenant %s\n' "$set_name" "$entity_id" "$tenant" >&2
        exit 1
      fi
    done
    local repo_owner
    local app_owner
    local ref_hash
    local lineage_parent
    repo_owner="$(field_from_entity "$tenant" Repositories "$repo_id" OwnerAccountId)"
    app_owner="$(field_from_entity "$tenant" Apps "$app_id" OwnerId)"
    ref_hash="$(field_from_entity "$tenant" Refs "$ref_id" TargetCommitSha)"
    lineage_parent="$(field_from_entity "$tenant" Lineages "$lineage_id" ParentRepositoryId)"
    if [[ "$repo_owner" != "$owner" || "$app_owner" != "$owner" || "$ref_hash" != "$expected_hash" || "$lineage_parent" != "$(parent_repo_id "$tenant")" ]]; then
      printf 'Fork mismatch in tenant %s index %s repo_owner=%s app_owner=%s ref=%s lineage_parent=%s\n' "$tenant" "$index" "$repo_owner" "$app_owner" "$ref_hash" "$lineage_parent" >&2
      exit 1
    fi
    checks=$((checks + 8))
  done

  printf '%s' "$checks" > "$TMP_DIR/checks-${tenant//[^a-zA-Z0-9]/-}.count"
}

printf 'Starting operator-mode seed server on %s\n' "$BASE_URL"
start_server operator "$SECONDARY_TENANT"

printf 'Seeding verified owners and registry fixtures in tenants: %s\n' "$TENANTS"
for tenant in $TENANTS; do
  owner="$(owner_id "$tenant")"
  create_and_verify_owner "$tenant" "$owner"
  for index in $(seq 1 "$REGISTER_COUNT"); do
    seed_register_target "$tenant" "$owner" "$index"
  done
  for index in $(seq 1 "$PUBLISH_COUNT"); do
    seed_publish_target "$tenant" "$owner" "$index"
  done
  seed_fork_parent "$tenant" "$owner"
done

stop_server

printf 'Restarting same DB in commons mode\n'
start_server commons "$SECONDARY_TENANT"

printf 'Checking commons denies direct registry mutation and spoofed action_context headers\n'
denials=0
for tenant in $TENANTS; do
  owner="$(owner_id "$tenant")"
  expect_customer_post_denied "$tenant" "$owner" "/tdata/Repositories" \
    "{\"Id\":$(json_escape "rp-denied-${tenant}-${RUN_ID}"),\"OwnerAccountId\":$(json_escape "$owner"),\"Name\":\"denied-repo\",\"Description\":\"direct repo denied\",\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}" \
    "direct-repository"
  denials=$((denials + 1))
  expect_customer_post_denied "$tenant" "$owner" "/tdata/Apps" \
    "{\"Id\":$(json_escape "app-denied-${tenant}-${RUN_ID}"),\"OwnerId\":$(json_escape "$owner"),\"Name\":\"denied-app\",\"RepositoryId\":$(json_escape "rp-denied-${tenant}-${RUN_ID}"),\"LatestVersionHash\":\"0000000000000000000000000000000000000000\",\"Exports\":\"{}\",\"Description\":\"direct app denied\",\"Visibility\":\"public\"}" \
    "direct-app"
  denials=$((denials + 1))
  expect_customer_post_denied "$tenant" "$owner" "/tdata/Repositories" \
    "{\"Id\":$(json_escape "rp-spoof-${tenant}-${RUN_ID}"),\"OwnerAccountId\":$(json_escape "$owner"),\"Name\":\"spoof-repo\",\"Description\":\"spoof repo denied\",\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}" \
    "spoofed-repository" \
    -H "X-Temper-Action-Context: composite:App.Fork"
  denials=$((denials + 1))
done

for tenant in $TENANTS; do
  tenant_slug="${tenant//[^a-zA-Z0-9]/-}"
  collection_count_all "$tenant" Blobs > "$TMP_DIR/before-${tenant_slug}-blobs.count"
  collection_count_all "$tenant" Trees > "$TMP_DIR/before-${tenant_slug}-trees.count"
  collection_count_all "$tenant" Commits > "$TMP_DIR/before-${tenant_slug}-commits.count"
  collection_count_all "$tenant" Tags > "$TMP_DIR/before-${tenant_slug}-tags.count"
done

printf 'Running mixed commons Composite actions with parallelism %s\n' "$PARALLELISM"
pids=()
for tenant in $TENANTS; do
  owner="$(owner_id "$tenant")"
  for index in $(seq 1 "$REGISTER_COUNT"); do
    (run_register "$tenant" "$owner" "$index") &
    pids+=("$!")
    if [[ "${#pids[@]}" -ge "$PARALLELISM" ]]; then
      for pid in "${pids[@]}"; do
        wait "$pid"
      done
      pids=()
    fi
  done
  for index in $(seq 1 "$PUBLISH_COUNT"); do
    (run_publish "$tenant" "$owner" "$index") &
    pids+=("$!")
    if [[ "${#pids[@]}" -ge "$PARALLELISM" ]]; then
      for pid in "${pids[@]}"; do
        wait "$pid"
      done
      pids=()
    fi
  done
  for index in $(seq 1 "$FORK_COUNT"); do
    (run_fork "$tenant" "$owner" "$index") &
    pids+=("$!")
    if [[ "${#pids[@]}" -ge "$PARALLELISM" ]]; then
      for pid in "${pids[@]}"; do
        wait "$pid"
      done
      pids=()
    fi
  done
done
if [[ "${#pids[@]}" -gt 0 ]]; then
  for pid in "${pids[@]}"; do
    wait "$pid"
  done
fi

printf 'Verifying tenant-local Composite projections and object invariants\n'
checks=0
for tenant in $TENANTS; do
  tenant_slug="${tenant//[^a-zA-Z0-9]/-}"
  owner="$(owner_id "$tenant")"
  verify_tenant "$tenant" "$owner"
  tenant_checks="$(cat "$TMP_DIR/checks-${tenant_slug}.count")"
  checks=$((checks + tenant_checks))

  before_blobs="$(cat "$TMP_DIR/before-${tenant_slug}-blobs.count")"
  before_trees="$(cat "$TMP_DIR/before-${tenant_slug}-trees.count")"
  before_commits="$(cat "$TMP_DIR/before-${tenant_slug}-commits.count")"
  before_tags="$(cat "$TMP_DIR/before-${tenant_slug}-tags.count")"
  after_blobs="$(collection_count_all "$tenant" Blobs)"
  after_trees="$(collection_count_all "$tenant" Trees)"
  after_commits="$(collection_count_all "$tenant" Commits)"
  after_tags="$(collection_count_all "$tenant" Tags)"
  if [[ "$before_blobs" != "$after_blobs" || "$before_trees" != "$after_trees" || "$before_commits" != "$after_commits" || "$before_tags" != "$after_tags" ]]; then
    printf 'Tenant %s object counts changed: before %s/%s/%s/%s after %s/%s/%s/%s\n' \
      "$tenant" "$before_blobs" "$before_trees" "$before_commits" "$before_tags" \
      "$after_blobs" "$after_trees" "$after_commits" "$after_tags" >&2
    exit 1
  fi
  checks=$((checks + 4))
done

register_attempts=0
publish_attempts=0
fork_attempts=0
max_attempts=0
for tenant in $TENANTS; do
  tenant_slug="${tenant//[^a-zA-Z0-9]/-}"
  for index in $(seq 1 "$REGISTER_COUNT"); do
    attempts="$(cat "$TMP_DIR/register-${tenant_slug}-${index}.attempts")"
    register_attempts=$((register_attempts + attempts))
    if [[ "$attempts" -gt "$max_attempts" ]]; then max_attempts="$attempts"; fi
  done
  for index in $(seq 1 "$PUBLISH_COUNT"); do
    attempts="$(cat "$TMP_DIR/publish-${tenant_slug}-${index}.attempts")"
    publish_attempts=$((publish_attempts + attempts))
    if [[ "$attempts" -gt "$max_attempts" ]]; then max_attempts="$attempts"; fi
  done
  for index in $(seq 1 "$FORK_COUNT"); do
    attempts="$(cat "$TMP_DIR/fork-${tenant_slug}-${index}.attempts")"
    fork_attempts=$((fork_attempts + attempts))
    if [[ "$attempts" -gt "$max_attempts" ]]; then max_attempts="$attempts"; fi
  done
done

tenant_count="$(printf '%s\n' $TENANTS | wc -l | tr -d ' ')"
printf 'PASS Commons tenant mixed action stress smoke\n'
printf '  run: %s\n' "$RUN_ID"
printf '  tenants: %s\n' "$TENANTS"
printf '  register actions: %s\n' "$((tenant_count * REGISTER_COUNT))"
printf '  publish actions: %s\n' "$((tenant_count * PUBLISH_COUNT))"
printf '  fork actions: %s\n' "$((tenant_count * FORK_COUNT))"
printf '  denial checks: %s\n' "$denials"
printf '  verification checks: %s\n' "$checks"
printf '  register POST attempts: %s\n' "$register_attempts"
printf '  publish POST attempts: %s\n' "$publish_attempts"
printf '  fork POST attempts: %s\n' "$fork_attempts"
printf '  max attempts for one action: %s\n' "$max_attempts"
