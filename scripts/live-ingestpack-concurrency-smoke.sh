#!/usr/bin/env bash
set -euo pipefail

# Live smoke for the Repository.IngestPack smart-HTTP path.
#
# Requires a running Temper server with the temper-git app installed:
#
#   TEMPER_URL=http://127.0.0.1:3137 \
#     scripts/live-ingestpack-concurrency-smoke.sh
#
# The smoke seeds the smart-HTTP endpoints and one repository, races two real
# git clients pushing unrelated root commits to the same empty branch, verifies
# that exactly one push wins, and proves the rejected push leaves no unique
# object rows behind.

BASE_URL="${TEMPER_URL:-http://127.0.0.1:3000}"
BASE_URL="${BASE_URL%/}"
TENANT="${TEMPER_TENANT:-default}"
PRINCIPAL_ID="${TEMPER_PRINCIPAL_ID:-operator}"
RUN_ID="${RUN_ID:-$(date +%s)-$$}"
OWNER="race-${RUN_ID}"
REPO="ingestpack-${RUN_ID}"
REPO_ID="rp-${OWNER}-${REPO}"
REF_ID="rf-${REPO_ID}-refs-heads-main"
REMOTE="${BASE_URL}/${OWNER}/${REPO}.git"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/temper-ingestpack-race.XXXXXX")"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

api_headers=(
  -H "X-Tenant-Id: ${TENANT}"
  -H "X-Temper-Principal-Kind: admin"
  -H "X-Temper-Principal-Id: ${PRINCIPAL_ID}"
  -H "X-Temper-Principal-Scopes: admin:repos repo:write pr:write"
  -H "Accept: application/json"
)
json_headers=("${api_headers[@]}" -H "Content-Type: application/json")
system_headers=(
  -H "X-Tenant-Id: ${TENANT}"
  -H "X-Temper-Principal-Kind: admin"
  -H "X-Temper-Principal-Id: ${PRINCIPAL_ID}"
  -H "X-Temper-Agent-Type: system"
  -H "X-Temper-Principal-Scopes: admin:repos repo:write pr:write"
  -H "Accept: application/json"
  -H "Content-Type: application/json"
)

json_escape() {
  node -e 'process.stdout.write(JSON.stringify(process.argv[1]))' "$1"
}

post_json() {
  local path="$1"
  local body="$2"
  local out="$TMP_DIR/post-response.json"
  local status
  status="$(curl -sS -o "$out" -w "%{http_code}" -X POST "${json_headers[@]}" -d "$body" "${BASE_URL}${path}")"
  if [[ "$status" != 2* ]]; then
    printf 'POST %s failed with HTTP %s\n' "$path" "$status" >&2
    sed -n '1,120p' "$out" >&2
    exit 1
  fi
}

post_json_system() {
  local path="$1"
  local body="$2"
  local out="$TMP_DIR/post-system-response.json"
  local status
  status="$(curl -sS -o "$out" -w "%{http_code}" -X POST "${system_headers[@]}" -d "$body" "${BASE_URL}${path}")"
  if [[ "$status" != 2* ]]; then
    printf 'POST %s failed with HTTP %s\n' "$path" "$status" >&2
    sed -n '1,120p' "$out" >&2
    exit 1
  fi
}

entity_exists() {
  local set_name="$1"
  local entity_id="$2"
  curl -fsS "${api_headers[@]}" "${BASE_URL}/tdata/${set_name}('${entity_id}')" >/dev/null 2>&1
}

ensure_endpoint() {
  local id="$1"
  local body="$2"
  if entity_exists "HttpEndpoints" "$id"; then
    return
  fi
  post_json "/tdata/HttpEndpoints" "$body"
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

collection_for_git_type() {
  case "$1" in
    blob) printf 'Blobs' ;;
    commit) printf 'Commits' ;;
    tree) printf 'Trees' ;;
    tag) printf 'Tags' ;;
    *) return 1 ;;
  esac
}

write_object_inventory() {
  local client="$1"
  local out="$2"
  git -C "$TMP_DIR/$client" rev-list --objects --all \
    | awk '{print $1}' \
    | git -C "$TMP_DIR/$client" cat-file --batch-check='%(objectname) %(objecttype)' \
    | sort -u > "$out"
}

assert_objects_present() {
  local inventory="$1"
  local checked=0
  local sha kind collection
  while read -r sha kind; do
    collection="$(collection_for_git_type "$kind")"
    if ! entity_exists "$collection" "$sha"; then
      printf 'Expected winner %s object %s in %s, but it was absent\n' "$kind" "$sha" "$collection" >&2
      exit 1
    fi
    checked=$((checked + 1))
  done < "$inventory"
  if [[ "$checked" -lt 3 ]]; then
    printf 'Expected at least commit/tree/blob winner objects, checked only %s\n' "$checked" >&2
    exit 1
  fi
  printf '%s' "$checked"
}

assert_objects_absent() {
  local inventory="$1"
  local checked=0
  local sha kind collection
  while read -r sha kind; do
    collection="$(collection_for_git_type "$kind")"
    if entity_exists "$collection" "$sha"; then
      printf 'Rejected push leaked %s object %s into %s\n' "$kind" "$sha" "$collection" >&2
      exit 1
    fi
    checked=$((checked + 1))
  done < "$inventory"
  if [[ "$checked" -lt 3 ]]; then
    printf 'Expected at least commit/tree/blob loser rollback checks, checked only %s\n' "$checked" >&2
    exit 1
  fi
  printf '%s' "$checked"
}

printf 'Seeding smart-HTTP endpoints for %s\n' "$BASE_URL"
ensure_endpoint "he-info-refs" \
  '{"Id":"he-info-refs","PathPrefix":"/{owner}/{repo}.git/info/refs","Methods":"GET","IntegrationModule":"git_upload_pack","RequiresAuth":false,"TimeoutSecs":60}'
ensure_endpoint "he-upload-pack" \
  '{"Id":"he-upload-pack","PathPrefix":"/{owner}/{repo}.git/git-upload-pack","Methods":"POST","IntegrationModule":"git_upload_pack","RequiresAuth":false,"TimeoutSecs":300}'
ensure_endpoint "he-receive-pack" \
  '{"Id":"he-receive-pack","PathPrefix":"/{owner}/{repo}.git/git-receive-pack","Methods":"POST","IntegrationModule":"git_receive_pack","RequiresAuth":false,"TimeoutSecs":300,"ActionBridgeEntityType":"Repository","ActionBridgeEntityId":"rp-{owner}-{repo}","ActionBridgeAction":"IngestPack","ActionBridgeResponse":"git-receive-pack"}'

printf 'Creating Repository %s\n' "$REPO_ID"
post_json "/tdata/Repositories" \
  "{\"Id\":$(json_escape "$REPO_ID"),\"OwnerAccountId\":$(json_escape "$OWNER"),\"Name\":$(json_escape "$REPO"),\"Description\":\"IngestPack concurrency smoke\",\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}"

post_json_system "/tdata/Repositories('${REPO_ID}')/Temper.Git.MarkProvisioned" \
  "{\"LibsqlDbName\":$(json_escape "${REPO_ID}.db")}"

create_client() {
  local name="$1"
  local path="$TMP_DIR/$name"
  mkdir -p "$path"
  git -C "$path" init -b main >/dev/null
  git -C "$path" config user.email "${name}@example.invalid"
  git -C "$path" config user.name "$name"
  printf '%s\n' "$name pushed at $(date -u +%Y-%m-%dT%H:%M:%SZ)" > "$path/README.md"
  git -C "$path" add README.md
  git -C "$path" commit -m "race ${name}" >/dev/null
  git -C "$path" rev-parse HEAD
}

LEFT_SHA="$(create_client left)"
RIGHT_SHA="$(create_client right)"

printf 'Racing two git pushes to %s\n' "$REMOTE"
(
  cd "$TMP_DIR/left"
  set +e
  git push "$REMOTE" main > "$TMP_DIR/left.push.log" 2>&1
  status=$?
  set -e
  echo "$status" > "$TMP_DIR/left.status"
) &
left_pid=$!
(
  cd "$TMP_DIR/right"
  set +e
  git push "$REMOTE" main > "$TMP_DIR/right.push.log" 2>&1
  status=$?
  set -e
  echo "$status" > "$TMP_DIR/right.status"
) &
right_pid=$!
wait "$left_pid" || true
wait "$right_pid" || true

LEFT_STATUS="$(cat "$TMP_DIR/left.status")"
RIGHT_STATUS="$(cat "$TMP_DIR/right.status")"
if [[ "$LEFT_STATUS" == "0" && "$RIGHT_STATUS" != "0" ]]; then
  WINNER="left"
  LOSER="right"
  WINNER_SHA="$LEFT_SHA"
elif [[ "$RIGHT_STATUS" == "0" && "$LEFT_STATUS" != "0" ]]; then
  WINNER="right"
  LOSER="left"
  WINNER_SHA="$RIGHT_SHA"
else
  printf 'Expected exactly one successful push; left=%s right=%s\n' "$LEFT_STATUS" "$RIGHT_STATUS" >&2
  printf '\n--- left push log ---\n' >&2
  sed -n '1,160p' "$TMP_DIR/left.push.log" >&2
  printf '\n--- right push log ---\n' >&2
  sed -n '1,160p' "$TMP_DIR/right.push.log" >&2
  exit 1
fi

write_object_inventory left "$TMP_DIR/left.objects"
write_object_inventory right "$TMP_DIR/right.objects"
WINNER_OBJECTS="$TMP_DIR/${WINNER}.objects"
LOSER_OBJECTS="$TMP_DIR/${LOSER}.objects"
LOSER_UNIQUE_OBJECTS="$TMP_DIR/loser.unique.objects"
comm -23 "$LOSER_OBJECTS" "$WINNER_OBJECTS" > "$LOSER_UNIQUE_OBJECTS"

WINNER_OBJECT_CHECKS="$(assert_objects_present "$WINNER_OBJECTS")"
LOSER_ROLLBACK_CHECKS="$(assert_objects_absent "$LOSER_UNIQUE_OBJECTS")"

TARGET_SHA="$(field_from_entity Refs "$REF_ID" TargetCommitSha)"
if [[ "$TARGET_SHA" != "$WINNER_SHA" ]]; then
  printf 'Stored Ref target mismatch: got %s, expected winner %s (%s)\n' "$TARGET_SHA" "$WINNER_SHA" "$WINNER" >&2
  exit 1
fi

git clone "$REMOTE" "$TMP_DIR/clone" > "$TMP_DIR/clone.log" 2>&1
CLONED_SHA="$(git -C "$TMP_DIR/clone" rev-parse HEAD)"
if [[ "$CLONED_SHA" != "$WINNER_SHA" ]]; then
  printf 'Clone HEAD mismatch: got %s, expected %s\n' "$CLONED_SHA" "$WINNER_SHA" >&2
  sed -n '1,160p' "$TMP_DIR/clone.log" >&2
  exit 1
fi

printf 'PASS IngestPack concurrent push smoke\n'
printf '  run: %s\n' "$RUN_ID"
printf '  repository: %s\n' "$REPO_ID"
printf '  winner: %s %s\n' "$WINNER" "$WINNER_SHA"
printf '  loser status: left=%s right=%s\n' "$LEFT_STATUS" "$RIGHT_STATUS"
printf '  winner objects verified: %s\n' "$WINNER_OBJECT_CHECKS"
printf '  loser unique objects absent: %s\n' "$LOSER_ROLLBACK_CHECKS"
printf '  ref: %s -> %s\n' "$REF_ID" "$TARGET_SHA"
