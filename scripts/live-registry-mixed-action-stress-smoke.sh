#!/usr/bin/env bash
set -euo pipefail

# Live smoke for mixed Genesis registry Composite actions.
#
# Requires a running Temper server with the temper-git app installed:
#
#   TEMPER_URL=http://127.0.0.1:3143 \
#     scripts/live-registry-mixed-action-stress-smoke.sh
#
# The smoke seeds Repository/Ref/App metadata, then runs many live
# App.RegisterNewApp and App.PublishNewVersion bound actions concurrently.
# Register must create App rows from existing Repository/default Ref rows.
# Publish must advance both the backing Ref and App.LatestVersionHash.
# Neither path should create Blob/Tree/Commit/Tag rows.

BASE_URL="${TEMPER_URL:-http://127.0.0.1:3000}"
BASE_URL="${BASE_URL%/}"
TENANT="${TEMPER_TENANT:-default}"
PRINCIPAL_ID="${TEMPER_PRINCIPAL_ID:-operator}"
REGISTER_COUNT="${REGISTER_COUNT:-40}"
PUBLISH_COUNT="${PUBLISH_COUNT:-40}"
PARALLELISM="${PARALLELISM:-8}"
RUN_ID="${RUN_ID:-$(date +%s)-$$}"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/temper-registry-mixed-stress.XXXXXX")"

cleanup() {
  rm -rf "$TMP_DIR"
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

api_headers=(
  -H "X-Tenant-Id: ${TENANT}"
  -H "X-Temper-Principal-Kind: admin"
  -H "X-Temper-Principal-Id: ${PRINCIPAL_ID}"
  -H "X-Temper-Principal-Scopes: admin:repos repo:write pr:write"
  -H "Accept: application/json"
)
json_headers=("${api_headers[@]}" -H "Content-Type: application/json")

post_json() {
  local path="$1"
  local body="$2"
  local out="$TMP_DIR/post-${$}-${RANDOM}.json"
  local status
  status="$(curl -sS -o "$out" -w "%{http_code}" -X POST "${json_headers[@]}" -d "$body" "${BASE_URL}${path}")"
  if [[ "$status" != 2* ]]; then
    printf 'POST %s failed with HTTP %s\n' "$path" "$status" >&2
    sed -n '1,180p' "$out" >&2
    exit 1
  fi
}

post_json_retry() {
  local path="$1"
  local body="$2"
  local idempotency_key="$3"
  local max_attempts="${4:-12}"
  local retry_slot="${5:-0}"
  local attempt=1
  local out
  local status
  while [[ "$attempt" -le "$max_attempts" ]]; do
    out="$TMP_DIR/post-retry-${idempotency_key}-${attempt}.json"
    status="$(
      curl -sS -o "$out" -w "%{http_code}" \
        -X POST "${json_headers[@]}" -H "Idempotency-Key: ${idempotency_key}" \
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
    printf 'POST %s failed with HTTP %s on attempt %s\n' "$path" "$status" "$attempt" >&2
    sed -n '1,180p' "$out" >&2
    exit 1
  done
  printf 'POST %s still failed after %s attempts\n' "$path" "$max_attempts" >&2
  sed -n '1,180p' "$out" >&2
  exit 1
}

entity_exists() {
  local set_name="$1"
  local entity_id="$2"
  curl -fsS "${api_headers[@]}" "${BASE_URL}/tdata/${set_name}('${entity_id}')" >/dev/null 2>&1
}

field_from_entity() {
  local set_name="$1"
  local entity_id="$2"
  local field_name="$3"
  local body="$TMP_DIR/entity-${set_name}-${field_name}-${RANDOM}.json"
  curl -fsS "${api_headers[@]}" "${BASE_URL}/tdata/${set_name}('${entity_id}')" > "$body"
  node -e '
    const fs = require("fs");
    const row = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const field = process.argv[2];
    const value = (row.fields && row.fields[field]) ?? row[field] ?? "";
    process.stdout.write(String(value));
  ' "$body" "$field_name"
}

collection_count_all() {
  local set_name="$1"
  local body="$TMP_DIR/${set_name}-all-${RANDOM}.json"
  curl -fsS "${api_headers[@]}" "${BASE_URL}/tdata/${set_name}?\$top=5000" > "$body"
  node -e '
    const fs = require("fs");
    const body = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    process.stdout.write(String(Array.isArray(body.value) ? body.value.length : 0));
  ' "$body"
}

collection_count_id_prefix() {
  local set_name="$1"
  local prefix="$2"
  local body="$TMP_DIR/${set_name}-prefix-${RANDOM}.json"
  curl -fsS "${api_headers[@]}" "${BASE_URL}/tdata/${set_name}?\$top=5000" > "$body"
  node -e '
    const fs = require("fs");
    const body = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const prefix = process.argv[2];
    const rows = Array.isArray(body.value) ? body.value : [];
    const count = rows.filter(row => {
      const id = (row.fields && row.fields.Id) ?? row.Id ?? "";
      return String(id).startsWith(prefix);
    }).length;
    process.stdout.write(String(count));
  ' "$body" "$prefix"
}

register_owner() {
  printf 'reg-owner-%s-%03d' "$RUN_ID" "$1"
}

register_name() {
  printf 'registered-app-%03d' "$1"
}

register_repo_id() {
  printf 'rp-reg-%s-%03d' "$RUN_ID" "$1"
}

register_app_id() {
  printf 'app-reg-owner-%s-%03d-registered-app-%03d' "$RUN_ID" "$1" "$1"
}

publish_owner() {
  printf 'pub-owner-%s-%03d' "$RUN_ID" "$1"
}

publish_name() {
  printf 'published-app-%03d' "$1"
}

publish_repo_id() {
  printf 'rp-pub-%s-%03d' "$RUN_ID" "$1"
}

publish_app_id() {
  printf 'app-pub-%s-%03d' "$RUN_ID" "$1"
}

ref_id_for_repo() {
  printf 'rf-%s-refs-heads-main' "$1"
}

initial_hash() {
  sha1_hex "initial-${RUN_ID}-$1"
}

published_hash() {
  sha1_hex "published-${RUN_ID}-$1"
}

seed_register_target() {
  local index="$1"
  local repo_id
  local owner
  local app_name
  local ref_id
  local hash
  repo_id="$(register_repo_id "$index")"
  owner="$(register_owner "$index")"
  app_name="$(register_name "$index")"
  ref_id="$(ref_id_for_repo "$repo_id")"
  hash="$(initial_hash "register-${index}")"

  post_json "/tdata/Repositories" \
    "{\"Id\":$(json_escape "$repo_id"),\"OwnerAccountId\":$(json_escape "$owner"),\"Name\":$(json_escape "$app_name"),\"Description\":$(json_escape "register stress repository ${index}"),\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}"
  post_json "/tdata/Refs" \
    "{\"Id\":$(json_escape "$ref_id"),\"RepositoryId\":$(json_escape "$repo_id"),\"Name\":\"refs/heads/main\",\"TargetCommitSha\":$(json_escape "$hash"),\"Kind\":\"branch\"}"
}

seed_publish_target() {
  local index="$1"
  local repo_id
  local owner
  local app_name
  local app_id
  local ref_id
  local hash
  repo_id="$(publish_repo_id "$index")"
  owner="$(publish_owner "$index")"
  app_name="$(publish_name "$index")"
  app_id="$(publish_app_id "$index")"
  ref_id="$(ref_id_for_repo "$repo_id")"
  hash="$(initial_hash "publish-${index}")"

  post_json "/tdata/Repositories" \
    "{\"Id\":$(json_escape "$repo_id"),\"OwnerAccountId\":$(json_escape "$owner"),\"Name\":$(json_escape "$app_name"),\"Description\":$(json_escape "publish stress repository ${index}"),\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}"
  post_json "/tdata/Refs" \
    "{\"Id\":$(json_escape "$ref_id"),\"RepositoryId\":$(json_escape "$repo_id"),\"Name\":\"refs/heads/main\",\"TargetCommitSha\":$(json_escape "$hash"),\"Kind\":\"branch\"}"
  post_json "/tdata/Apps" \
    "{\"Id\":$(json_escape "$app_id"),\"OwnerId\":$(json_escape "$owner"),\"Name\":$(json_escape "$app_name"),\"RepositoryId\":$(json_escape "$repo_id"),\"LatestVersionHash\":$(json_escape "$hash"),\"Exports\":\"{}\",\"Description\":$(json_escape "publish stress app ${index}"),\"Visibility\":\"public\"}"
}

run_register() {
  local index="$1"
  local app_id
  local repo_id
  local app_name
  local attempts
  app_id="$(register_app_id "$index")"
  repo_id="$(register_repo_id "$index")"
  app_name="$(register_name "$index")"
  attempts="$(
    post_json_retry \
      "/tdata/Apps('${app_id}')/Temper.Git.RegisterNewApp?await_integration=true" \
      "{\"Name\":$(json_escape "$app_name"),\"RepositoryId\":$(json_escape "$repo_id"),\"Description\":$(json_escape "registered stress app ${index}"),\"Exports\":\"{}\",\"Visibility\":\"public\"}" \
      "registry-mixed-register-${RUN_ID}-${index}" \
      16 \
      "$index"
  )"
  printf '%s\n' "$attempts" > "$TMP_DIR/register-${index}.attempts"
}

run_publish() {
  local index="$1"
  local app_id
  local new_hash
  local attempts
  app_id="$(publish_app_id "$index")"
  new_hash="$(published_hash "$index")"
  attempts="$(
    post_json_retry \
      "/tdata/Apps('${app_id}')/Temper.Git.PublishNewVersion?await_integration=true" \
      "{\"NewHash\":$(json_escape "$new_hash"),\"RefName\":\"main\"}" \
      "registry-mixed-publish-${RUN_ID}-${index}" \
      16 \
      "$((index + REGISTER_COUNT))"
  )"
  printf '%s\n' "$attempts" > "$TMP_DIR/publish-${index}.attempts"
}

printf 'Seeding %s RegisterNewApp targets and %s PublishNewVersion targets\n' "$REGISTER_COUNT" "$PUBLISH_COUNT"
for index in $(seq 1 "$REGISTER_COUNT"); do
  seed_register_target "$index"
done
for index in $(seq 1 "$PUBLISH_COUNT"); do
  seed_publish_target "$index"
done

before_blobs="$(collection_count_all Blobs)"
before_trees="$(collection_count_all Trees)"
before_commits="$(collection_count_all Commits)"
before_tags="$(collection_count_all Tags)"

printf 'Running mixed registry actions with parallelism %s\n' "$PARALLELISM"
pids=()
for index in $(seq 1 "$REGISTER_COUNT"); do
  (run_register "$index") &
  pids+=("$!")
  if [[ "${#pids[@]}" -ge "$PARALLELISM" ]]; then
    for pid in "${pids[@]}"; do
      wait "$pid"
    done
    pids=()
  fi
done
for index in $(seq 1 "$PUBLISH_COUNT"); do
  (run_publish "$index") &
  pids+=("$!")
  if [[ "${#pids[@]}" -ge "$PARALLELISM" ]]; then
    for pid in "${pids[@]}"; do
      wait "$pid"
    done
    pids=()
  fi
done
if [[ "${#pids[@]}" -gt 0 ]]; then
  for pid in "${pids[@]}"; do
    wait "$pid"
  done
fi

register_attempts=0
publish_attempts=0
max_attempts=0
for index in $(seq 1 "$REGISTER_COUNT"); do
  attempts="$(cat "$TMP_DIR/register-${index}.attempts")"
  register_attempts=$((register_attempts + attempts))
  if [[ "$attempts" -gt "$max_attempts" ]]; then
    max_attempts="$attempts"
  fi
done
for index in $(seq 1 "$PUBLISH_COUNT"); do
  attempts="$(cat "$TMP_DIR/publish-${index}.attempts")"
  publish_attempts=$((publish_attempts + attempts))
  if [[ "$attempts" -gt "$max_attempts" ]]; then
    max_attempts="$attempts"
  fi
done

printf 'Verifying RegisterNewApp and PublishNewVersion projections\n'
checks=0
for index in $(seq 1 "$REGISTER_COUNT"); do
  app_id="$(register_app_id "$index")"
  repo_id="$(register_repo_id "$index")"
  expected_owner="$(register_owner "$index")"
  expected_name="$(register_name "$index")"
  expected_hash="$(initial_hash "register-${index}")"
  if ! entity_exists Apps "$app_id"; then
    printf 'Missing registered App %s\n' "$app_id" >&2
    exit 1
  fi
  owner="$(field_from_entity Apps "$app_id" OwnerId)"
  name="$(field_from_entity Apps "$app_id" Name)"
  repository_id="$(field_from_entity Apps "$app_id" RepositoryId)"
  latest_hash="$(field_from_entity Apps "$app_id" LatestVersionHash)"
  if [[ "$owner" != "$expected_owner" || "$name" != "$expected_name" || "$repository_id" != "$repo_id" || "$latest_hash" != "$expected_hash" ]]; then
    printf 'Registered App %s mismatch: owner=%s name=%s repo=%s hash=%s\n' "$app_id" "$owner" "$name" "$repository_id" "$latest_hash" >&2
    exit 1
  fi
  checks=$((checks + 5))
done

for index in $(seq 1 "$PUBLISH_COUNT"); do
  app_id="$(publish_app_id "$index")"
  repo_id="$(publish_repo_id "$index")"
  ref_id="$(ref_id_for_repo "$repo_id")"
  expected_hash="$(published_hash "$index")"
  app_hash="$(field_from_entity Apps "$app_id" LatestVersionHash)"
  ref_hash="$(field_from_entity Refs "$ref_id" TargetCommitSha)"
  if [[ "$app_hash" != "$expected_hash" || "$ref_hash" != "$expected_hash" ]]; then
    printf 'Published App %s mismatch: app=%s ref=%s expected=%s\n' "$app_id" "$app_hash" "$ref_hash" "$expected_hash" >&2
    exit 1
  fi
  checks=$((checks + 2))
done

registered_app_count="$(collection_count_id_prefix Apps "app-reg-owner-${RUN_ID}-")"
published_app_count="$(collection_count_id_prefix Apps "app-pub-${RUN_ID}-")"
register_repo_count="$(collection_count_id_prefix Repositories "rp-reg-${RUN_ID}-")"
publish_repo_count="$(collection_count_id_prefix Repositories "rp-pub-${RUN_ID}-")"
if [[ "$registered_app_count" -ne "$REGISTER_COUNT" || "$published_app_count" -ne "$PUBLISH_COUNT" || "$register_repo_count" -ne "$REGISTER_COUNT" || "$publish_repo_count" -ne "$PUBLISH_COUNT" ]]; then
  printf 'Unexpected row counts: registered apps=%s published apps=%s register repos=%s publish repos=%s\n' \
    "$registered_app_count" "$published_app_count" "$register_repo_count" "$publish_repo_count" >&2
  exit 1
fi
checks=$((checks + 4))

after_blobs="$(collection_count_all Blobs)"
after_trees="$(collection_count_all Trees)"
after_commits="$(collection_count_all Commits)"
after_tags="$(collection_count_all Tags)"
if [[ "$before_blobs" != "$after_blobs" || "$before_trees" != "$after_trees" || "$before_commits" != "$after_commits" || "$before_tags" != "$after_tags" ]]; then
  printf 'Registry metadata actions touched objects: before %s/%s/%s/%s after %s/%s/%s/%s\n' \
    "$before_blobs" "$before_trees" "$before_commits" "$before_tags" \
    "$after_blobs" "$after_trees" "$after_commits" "$after_tags" >&2
  exit 1
fi
checks=$((checks + 4))

printf 'PASS Registry mixed action stress smoke\n'
printf '  run: %s\n' "$RUN_ID"
printf '  register actions: %s\n' "$REGISTER_COUNT"
printf '  publish actions: %s\n' "$PUBLISH_COUNT"
printf '  parallelism: %s\n' "$PARALLELISM"
printf '  verification checks: %s\n' "$checks"
printf '  register POST attempts: %s\n' "$register_attempts"
printf '  publish POST attempts: %s\n' "$publish_attempts"
printf '  max attempts for one action: %s\n' "$max_attempts"
printf '  object counts before/after: %s/%s/%s/%s\n' "$before_blobs" "$before_trees" "$before_commits" "$before_tags"
