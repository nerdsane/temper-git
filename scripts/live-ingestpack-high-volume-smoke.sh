#!/usr/bin/env bash
set -euo pipefail

# High-volume live smoke for Repository.IngestPack.
#
# Requires a running Temper server with the temper-git app installed:
#
#   TEMPER_URL=http://127.0.0.1:3137 \
#     scripts/live-ingestpack-high-volume-smoke.sh
#
# The smoke creates one git commit containing FILE_COUNT unique files, pushes it
# through smart HTTP, verifies object projections by repository, verifies the
# stored Ref target, then clones and diffs the working tree.

BASE_URL="${TEMPER_URL:-http://127.0.0.1:3000}"
BASE_URL="${BASE_URL%/}"
TENANT="${TEMPER_TENANT:-default}"
PRINCIPAL_ID="${TEMPER_PRINCIPAL_ID:-operator}"
FILE_COUNT="${FILE_COUNT:-1000}"
RUN_ID="${RUN_ID:-$(date +%s)-$$}"
OWNER="stress-${RUN_ID}"
REPO="ingestpack-${RUN_ID}"
REPO_ID="rp-${OWNER}-${REPO}"
REF_ID="rf-${REPO_ID}-refs-heads-main"
REMOTE="${BASE_URL}/${OWNER}/${REPO}.git"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/temper-ingestpack-stress.XXXXXX")"
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

urlencode() {
  node -e 'process.stdout.write(encodeURIComponent(process.argv[1]))' "$1"
}

post_json() {
  local path="$1"
  local body="$2"
  local out="$TMP_DIR/post-response.json"
  local status
  status="$(curl -sS -o "$out" -w "%{http_code}" -X POST "${json_headers[@]}" -d "$body" "${BASE_URL}${path}")"
  if [[ "$status" != 2* ]]; then
    printf 'POST %s failed with HTTP %s\n' "$path" "$status" >&2
    sed -n '1,160p' "$out" >&2
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
    sed -n '1,160p' "$out" >&2
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

collection_count_for_repo() {
  local set_name="$1"
  local filter
  local body="$TMP_DIR/${set_name}.json"
  filter="$(urlencode "RepositoryId eq '${REPO_ID}'")"
  curl -fsS "${api_headers[@]}" "${BASE_URL}/tdata/${set_name}?\$filter=${filter}&\$top=5000" > "$body"
  node -e '
    const fs = require("fs");
    const body = JSON.parse(fs.readFileSync(process.argv[1], "utf8"));
    process.stdout.write(String(Array.isArray(body.value) ? body.value.length : 0));
  ' "$body"
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
  "{\"Id\":$(json_escape "$REPO_ID"),\"OwnerAccountId\":$(json_escape "$OWNER"),\"Name\":$(json_escape "$REPO"),\"Description\":\"IngestPack high-volume smoke\",\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}"

post_json_system "/tdata/Repositories('${REPO_ID}')/Temper.Git.MarkProvisioned" \
  "{\"LibsqlDbName\":$(json_escape "${REPO_ID}.db")}"

WORK="$TMP_DIR/work"
mkdir -p "$WORK/files"
git -C "$WORK" init -b main >/dev/null
git -C "$WORK" config user.email "stress@example.invalid"
git -C "$WORK" config user.name "IngestPack Stress"

printf 'Creating %s unique files\n' "$FILE_COUNT"
for i in $(seq 1 "$FILE_COUNT"); do
  printf 'stress file %04d for %s\n' "$i" "$RUN_ID" > "$WORK/files/file-$(printf '%04d' "$i").txt"
done
git -C "$WORK" add files
git -C "$WORK" commit -m "stress ${FILE_COUNT} files" >/dev/null
COMMIT_SHA="$(git -C "$WORK" rev-parse HEAD)"
PACK_OBJECTS="$(git -C "$WORK" rev-list --objects --all | wc -l | tr -d ' ')"

printf 'Pushing %s files (%s git objects) to %s\n' "$FILE_COUNT" "$PACK_OBJECTS" "$REMOTE"
start_ms="$(node -e 'process.stdout.write(String(Date.now()))')"
git -C "$WORK" push "$REMOTE" main > "$TMP_DIR/push.log" 2>&1
end_ms="$(node -e 'process.stdout.write(String(Date.now()))')"
push_ms="$(( end_ms - start_ms ))"

TARGET_SHA="$(field_from_entity Refs "$REF_ID" TargetCommitSha)"
if [[ "$TARGET_SHA" != "$COMMIT_SHA" ]]; then
  printf 'Stored Ref target mismatch: got %s, expected %s\n' "$TARGET_SHA" "$COMMIT_SHA" >&2
  sed -n '1,160p' "$TMP_DIR/push.log" >&2
  exit 1
fi

BLOB_COUNT="$(collection_count_for_repo Blobs)"
COMMIT_COUNT="$(collection_count_for_repo Commits)"
TREE_COUNT="$(collection_count_for_repo Trees)"
if [[ "$BLOB_COUNT" -ne "$FILE_COUNT" ]]; then
  printf 'Expected %s Blob rows, got %s\n' "$FILE_COUNT" "$BLOB_COUNT" >&2
  exit 1
fi
if [[ "$COMMIT_COUNT" -lt 1 ]]; then
  printf 'Expected at least one Commit row, got %s\n' "$COMMIT_COUNT" >&2
  exit 1
fi
if [[ "$TREE_COUNT" -lt 1 ]]; then
  printf 'Expected at least one Tree row, got %s\n' "$TREE_COUNT" >&2
  exit 1
fi

git clone "$REMOTE" "$TMP_DIR/clone" > "$TMP_DIR/clone.log" 2>&1
CLONED_SHA="$(git -C "$TMP_DIR/clone" rev-parse HEAD)"
if [[ "$CLONED_SHA" != "$COMMIT_SHA" ]]; then
  printf 'Clone HEAD mismatch: got %s, expected %s\n' "$CLONED_SHA" "$COMMIT_SHA" >&2
  sed -n '1,160p' "$TMP_DIR/clone.log" >&2
  exit 1
fi
diff -qr "$WORK/files" "$TMP_DIR/clone/files" > "$TMP_DIR/diff.log"

printf 'PASS IngestPack high-volume smoke\n'
printf '  run: %s\n' "$RUN_ID"
printf '  repository: %s\n' "$REPO_ID"
printf '  files: %s\n' "$FILE_COUNT"
printf '  git objects: %s\n' "$PACK_OBJECTS"
printf '  push_ms: %s\n' "$push_ms"
printf '  blobs/commits/trees: %s/%s/%s\n' "$BLOB_COUNT" "$COMMIT_COUNT" "$TREE_COUNT"
printf '  ref: %s -> %s\n' "$REF_ID" "$TARGET_SHA"
