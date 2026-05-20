#!/usr/bin/env bash
set -euo pipefail

# Live smoke for high-volume Genesis registry Composite actions.
#
# Requires a running Temper server with the temper-git app installed:
#
#   TEMPER_URL=http://127.0.0.1:3143 \
#     scripts/live-registry-composite-stress-smoke.sh
#
# The smoke seeds one parent App, then runs many concurrent App.Fork bound
# actions. Each fork must create exactly Repository, Ref, App, and Lineage rows
# without creating Blob/Tree/Commit/Tag rows.

BASE_URL="${TEMPER_URL:-http://127.0.0.1:3000}"
BASE_URL="${BASE_URL%/}"
TENANT="${TEMPER_TENANT:-default}"
PRINCIPAL_ID="${TEMPER_PRINCIPAL_ID:-operator}"
FORK_COUNT="${FORK_COUNT:-40}"
PARALLELISM="${PARALLELISM:-6}"
RUN_ID="${RUN_ID:-$(date +%s)-$$}"
PARENT_OWNER="stress-parent-${RUN_ID}"
PARENT_APP="app-stress-parent-${RUN_ID}"
PARENT_REPO="rp-stress-parent-${RUN_ID}"
PARENT_VERSION="1111111111111111111111111111111111111111"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/temper-registry-composite-stress.XXXXXX")"

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

post_json_retry_conflict() {
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
    if [[ "$status" == "409" ]] && grep -q "optimistic concurrency\\|composite batch persistence conflict" "$out"; then
      jitter_digit=$(( (attempt + retry_slot) % 7 + 2 ))
      sleep "0.${jitter_digit}"
      attempt=$((attempt + 1))
      continue
    fi
    printf 'POST %s failed with HTTP %s on attempt %s\n' "$path" "$status" "$attempt" >&2
    sed -n '1,180p' "$out" >&2
    exit 1
  done
  printf 'POST %s still conflicted after %s attempts\n' "$path" "$max_attempts" >&2
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
  local body="$TMP_DIR/entity-${set_name}-${field_name}.json"
  curl -fsS "${api_headers[@]}" "${BASE_URL}/tdata/${set_name}('${entity_id}')" > "$body"
  node -e '
    const fs = require("fs");
    const row = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    const field = process.argv[2];
    const value = (row.fields && row.fields[field]) ?? row[field] ?? "";
    process.stdout.write(String(value));
  ' "$body" "$field_name"
}

collection_count_by_filter() {
  local set_name="$1"
  local filter="$2"
  local body="$TMP_DIR/${set_name}-count.json"
  local encoded
  encoded="$(urlencode "$filter")"
  curl -fsS "${api_headers[@]}" "${BASE_URL}/tdata/${set_name}?\$filter=${encoded}&\$top=5000" > "$body"
  node -e '
    const fs = require("fs");
    const body = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    process.stdout.write(String(Array.isArray(body.value) ? body.value.length : 0));
  ' "$body"
}

collection_count_all() {
  local set_name="$1"
  local body="$TMP_DIR/${set_name}-all.json"
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
  local body="$TMP_DIR/${set_name}-prefix.json"
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

child_owner() {
  printf 'fork-owner-%s-%03d' "$RUN_ID" "$1"
}

child_name() {
  printf 'forked-app-%03d' "$1"
}

child_repo_id() {
  printf 'rp-fork-owner-%s-%03d-forked-app-%03d' "$RUN_ID" "$1" "$1"
}

child_app_id() {
  printf 'app-fork-owner-%s-%03d-forked-app-%03d' "$RUN_ID" "$1" "$1"
}

child_ref_id() {
  printf 'rf-%s-refs-heads-main' "$(child_repo_id "$1")"
}

child_lineage_id() {
  printf 'ln-%s-from-%s' "$(child_repo_id "$1")" "$PARENT_APP"
}

run_fork() {
  local index="$1"
  local owner
  local name
  owner="$(child_owner "$index")"
  name="$(child_name "$index")"
  attempts="$(
    post_json_retry_conflict \
      "/tdata/Apps('${PARENT_APP}')/Temper.Git.Fork?await_integration=true" \
      "{\"ParentAppId\":$(json_escape "$PARENT_APP"),\"ParentVersionHash\":$(json_escape "$PARENT_VERSION"),\"ChildOwnerId\":$(json_escape "$owner"),\"ChildName\":$(json_escape "$name"),\"Description\":$(json_escape "Stress fork ${index}")}" \
      "registry-composite-stress-${RUN_ID}-${index}" \
      20 \
      "$index"
  )"
  printf '%s\n' "$attempts" > "$TMP_DIR/fork-${index}.attempts"
}

printf 'Seeding parent App %s for %s forks\n' "$PARENT_APP" "$FORK_COUNT"
post_json "/tdata/Apps" \
  "{\"Id\":$(json_escape "$PARENT_APP"),\"OwnerId\":$(json_escape "$PARENT_OWNER"),\"Name\":\"stress-parent\",\"RepositoryId\":$(json_escape "$PARENT_REPO"),\"LatestVersionHash\":$(json_escape "$PARENT_VERSION"),\"Exports\":\"{}\",\"Description\":\"Composite stress parent\",\"Visibility\":\"public\"}"

before_blobs="$(collection_count_all Blobs)"
before_trees="$(collection_count_all Trees)"
before_commits="$(collection_count_all Commits)"
before_tags="$(collection_count_all Tags)"

printf 'Running %s App.Fork bound actions with parallelism %s\n' "$FORK_COUNT" "$PARALLELISM"
pids=()
for index in $(seq 1 "$FORK_COUNT"); do
  (run_fork "$index") &
  pids+=("$!")
  if [[ "${#pids[@]}" -ge "$PARALLELISM" ]]; then
    for pid in "${pids[@]}"; do
      wait "$pid"
    done
    pids=()
  fi
done
for pid in "${pids[@]}"; do
  wait "$pid"
done
total_attempts=0
max_attempts=0
for index in $(seq 1 "$FORK_COUNT"); do
  attempts="$(cat "$TMP_DIR/fork-${index}.attempts")"
  total_attempts=$((total_attempts + attempts))
  if [[ "$attempts" -gt "$max_attempts" ]]; then
    max_attempts="$attempts"
  fi
done

printf 'Verifying Composite sub-write rows and metadata-only invariant\n'
checks=0
for index in $(seq 1 "$FORK_COUNT"); do
  repo_id="$(child_repo_id "$index")"
  app_id="$(child_app_id "$index")"
  ref_id="$(child_ref_id "$index")"
  lineage_id="$(child_lineage_id "$index")"
  for pair in "Repositories:$repo_id" "Apps:$app_id" "Refs:$ref_id" "Lineages:$lineage_id"; do
    set_name="${pair%%:*}"
    entity_id="${pair#*:}"
    if ! entity_exists "$set_name" "$entity_id"; then
      printf 'Missing %s %s after App.Fork stress\n' "$set_name" "$entity_id" >&2
      exit 1
    fi
    checks=$((checks + 1))
  done
  ref_target="$(field_from_entity Refs "$ref_id" TargetCommitSha)"
  app_hash="$(field_from_entity Apps "$app_id" LatestVersionHash)"
  if [[ "$ref_target" != "$PARENT_VERSION" || "$app_hash" != "$PARENT_VERSION" ]]; then
    printf 'Fork %s version mismatch: Ref=%s App=%s expected=%s\n' "$index" "$ref_target" "$app_hash" "$PARENT_VERSION" >&2
    exit 1
  fi
  checks=$((checks + 2))
done

repo_count="$(collection_count_id_prefix Repositories "rp-fork-owner-${RUN_ID}-")"
app_count="$(collection_count_id_prefix Apps "app-fork-owner-${RUN_ID}-")"
lineage_count="$(collection_count_id_prefix Lineages "ln-rp-fork-owner-${RUN_ID}-")"
if [[ "$repo_count" -ne "$FORK_COUNT" || "$app_count" -ne "$FORK_COUNT" || "$lineage_count" -ne "$FORK_COUNT" ]]; then
  printf 'Expected %s child rows, got Repositories=%s Apps=%s Lineages=%s\n' "$FORK_COUNT" "$repo_count" "$app_count" "$lineage_count" >&2
  exit 1
fi
checks=$((checks + 3))

after_blobs="$(collection_count_all Blobs)"
after_trees="$(collection_count_all Trees)"
after_commits="$(collection_count_all Commits)"
after_tags="$(collection_count_all Tags)"
if [[ "$before_blobs" != "$after_blobs" || "$before_trees" != "$after_trees" || "$before_commits" != "$after_commits" || "$before_tags" != "$after_tags" ]]; then
  printf 'Metadata-only fork copied objects: before %s/%s/%s/%s after %s/%s/%s/%s\n' \
    "$before_blobs" "$before_trees" "$before_commits" "$before_tags" \
    "$after_blobs" "$after_trees" "$after_commits" "$after_tags" >&2
  exit 1
fi
checks=$((checks + 4))

printf 'PASS Registry composite stress smoke\n'
printf '  run: %s\n' "$RUN_ID"
printf '  parent app: %s\n' "$PARENT_APP"
printf '  forks: %s\n' "$FORK_COUNT"
printf '  parallelism: %s\n' "$PARALLELISM"
printf '  sub-write rows verified: %s\n' "$((FORK_COUNT * 4))"
printf '  verification checks: %s\n' "$checks"
printf '  total fork POST attempts: %s\n' "$total_attempts"
printf '  max attempts for one fork: %s\n' "$max_attempts"
printf '  object counts before/after: %s/%s/%s/%s\n' "$before_blobs" "$before_trees" "$before_commits" "$before_tags"
