#!/usr/bin/env bash
set -Eeuo pipefail

# Live smoke for Genesis-owned app installation.
#
# Requires a running Temper server with only the temper-git/Genesis app
# bootstrapped. The smoke creates a tiny app bundle, pushes it through smart
# HTTP into Genesis, registers it as an App, installs the pinned commit into
# three target tenants through the supported surfaces, and then proves the
# installed Note entity is usable in each tenant.

BASE_URL="${TEMPER_URL:-http://127.0.0.1:3188}"
BASE_URL="${BASE_URL%/}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
TEMPER_CARGO_MANIFEST="${TEMPER_CARGO_MANIFEST:-${REPO_ROOT}/temper/Cargo.toml}"
TEMPER_BIN="${TEMPER_BIN:-$(dirname "$TEMPER_CARGO_MANIFEST")/target/debug/temper}"
TENANT="${TEMPER_TENANT:-default}"
RUN_ID="${RUN_ID:-$(date +%H%M%S)}"
OWNER="${OWNER:-genesis-e2e}"
REPO="${REPO:-tiny-notes-${RUN_ID}}"
REPO_ID="rp-${OWNER}-${REPO}"
REF_ID="rf-${REPO_ID}-refs-heads-main"
APP_ID="app-${OWNER}-${REPO}"
REMOTE="${BASE_URL}/${OWNER}/${REPO}.git"
KEEP_TMP="${KEEP_TMP:-1}"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/genesis-install-e2e.XXXXXX")"
if [[ "$KEEP_TMP" != "1" ]]; then
  trap 'rm -rf "$TMP_DIR"' EXIT
fi
trap 'printf "FAIL line %s status %s\n" "$LINENO" "$?" >&2' ERR

json_headers=(
  -H "Content-Type: application/json"
  -H "Accept: application/json"
  -H "X-Temper-Principal-Kind: admin"
  -H "X-Temper-Principal-Id: operator"
  -H "X-Temper-Principal-Scopes: admin:repos admin:owners repo:write pr:write"
)
system_headers=(-H "X-Temper-Agent-Type: system")

json_escape() {
  node -e 'process.stdout.write(JSON.stringify(process.argv[1]))' "$1"
}

run_temper_cli() {
  if [[ -x "$TEMPER_BIN" ]]; then
    "$TEMPER_BIN" "$@"
  else
    cargo run -q --manifest-path "$TEMPER_CARGO_MANIFEST" -p temper-cli -- "$@"
  fi
}

post_json() {
  local tenant="$1"
  local path="$2"
  local body="$3"
  local out="$4"
  local status
  status="$(
    curl -sS -o "$out" -w "%{http_code}" -X POST \
      "${json_headers[@]}" \
      -H "X-Tenant-Id: ${tenant}" \
      -d "$body" \
      "${BASE_URL}${path}"
  )"
  if [[ "$status" != 2* ]]; then
    printf 'POST %s failed with HTTP %s\n' "$path" "$status" >&2
    sed -n '1,200p' "$out" >&2
    exit 1
  fi
}

post_json_system() {
  local tenant="$1"
  local path="$2"
  local body="$3"
  local out="$4"
  local status
  status="$(
    curl -sS -o "$out" -w "%{http_code}" -X POST \
      "${json_headers[@]}" \
      "${system_headers[@]}" \
      -H "X-Tenant-Id: ${tenant}" \
      -d "$body" \
      "${BASE_URL}${path}"
  )"
  if [[ "$status" != 2* ]]; then
    printf 'POST %s failed with HTTP %s\n' "$path" "$status" >&2
    sed -n '1,200p' "$out" >&2
    exit 1
  fi
}

get_json() {
  local tenant="$1"
  local path="$2"
  local out="$3"
  local status
  status="$(
    curl -sS -o "$out" -w "%{http_code}" \
      -H "Accept: application/json" \
      -H "X-Tenant-Id: ${tenant}" \
      -H "X-Temper-Principal-Kind: admin" \
      -H "X-Temper-Principal-Id: operator" \
      -H "X-Temper-Principal-Scopes: admin:repos admin:owners repo:write pr:write" \
      "${BASE_URL}${path}"
  )"
  if [[ "$status" != 2* ]]; then
    printf 'GET %s failed with HTTP %s\n' "$path" "$status" >&2
    sed -n '1,200p' "$out" >&2
    exit 1
  fi
}

ensure_endpoint() {
  local endpoint_id="$1"
  local body="$2"
  local out="${TMP_DIR}/${endpoint_id}.json"
  local status
  status="$(curl -sS -o "$out" -w "%{http_code}" -H "X-Tenant-Id: ${TENANT}" "${BASE_URL}/tdata/HttpEndpoints('${endpoint_id}')")"
  if [[ "$status" == "200" ]]; then
    return
  fi
  post_json "$TENANT" "/tdata/HttpEndpoints" "$body" "$out"
}

write_tiny_app_bundle() {
  local app_dir="$1"
  mkdir -p \
    "$app_dir/specs" \
    "$app_dir/policies" \
    "$app_dir/adrs" \
    "$app_dir/content" \
    "$app_dir/agents" \
    "$app_dir/agent-skills/note-helper" \
    "$app_dir/seed-data"

  cat > "$app_dir/app.toml" <<EOF
name = "${REPO}"
description = "Tiny Genesis install E2E app"
version = "0.1.0"
startup_install = "manual"
EOF

  cat > "$app_dir/specs/note.ioa.toml" <<'EOF'
[automaton]
name = "Note"
states = ["Active"]
initial = "Active"
allow_indefinite_states = ["Active"]

[[state]]
name = "Title"
type = "string"
initial = ""
query_indexed = true

[[state]]
name = "Body"
type = "string"
initial = ""
query_indexed = false

[[action]]
name = "Create"
kind = "input"
from = ["Active"]
to = "Active"
params = ["Title", "Body"]
hint = "Create a note."
EOF
  printf '\n# e2e-run = "%s"\n' "$RUN_ID" >> "$app_dir/specs/note.ioa.toml"

  cat > "$app_dir/specs/model.csdl.xml" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<!-- e2e run ${RUN_ID} -->
<edmx:Edmx Version="4.0" xmlns:edmx="http://docs.oasis-open.org/odata/ns/edmx">
  <edmx:DataServices>
    <Schema Namespace="Tiny.Notes" xmlns="http://docs.oasis-open.org/odata/ns/edm">
      <EntityType Name="Note">
        <Key><PropertyRef Name="Id"/></Key>
        <Property Name="Id" Type="Edm.String" Nullable="false"/>
        <Property Name="Title" Type="Edm.String" Nullable="false"/>
        <Property Name="Body" Type="Edm.String" Nullable="false"/>
        <Property Name="Status" Type="Edm.String" Nullable="false"/>
      </EntityType>
      <EntityContainer Name="Container">
        <EntitySet Name="Notes" EntityType="Tiny.Notes.Note"/>
      </EntityContainer>
    </Schema>
  </edmx:DataServices>
</edmx:Edmx>
EOF

  cat > "$app_dir/policies/app.cedar" <<EOF
// e2e run ${RUN_ID}
permit(principal, action, resource is Note);
EOF

  cat > "$app_dir/APP.md" <<EOF
# Tiny Notes

Tiny app ${REPO} used to prove Genesis install from pinned repository bytes.
EOF

  cat > "$app_dir/adrs/0001-genesis-install.md" <<EOF
# ADR 0001: Genesis install proof

This app is intentionally tiny and installed only by pinned Genesis ref for ${REPO}.
EOF

  cat > "$app_dir/content/example.md" <<EOF
This content file proves arbitrary app content is preserved in Genesis objects for ${REPO}.
EOF

  cat > "$app_dir/agents/note-agent.toml" <<EOF
name = "note-agent"
description = "Example app-contained agent definition for ${REPO}."
EOF

  cat > "$app_dir/agent-skills/note-helper/SKILL.md" <<EOF
---
name: note-helper
description: Helper skill shipped inside ${REPO}.
---
Use installed Note entities.
EOF

  printf '[{"run":"%s"}]\n' "$RUN_ID" > "$app_dir/seed-data/notes.json"
}

create_note_in_tenant() {
  local tenant="$1"
  local title="$2"
  local note_id="note-${tenant}"
  post_json "$tenant" "/tdata/Notes" \
    "{\"Id\":$(json_escape "$note_id"),\"Title\":$(json_escape "$title"),\"Body\":\"Genesis installed app usable\"}" \
    "${TMP_DIR}/${note_id}.json"
  get_json "$tenant" "/tdata/Notes('${note_id}')" "${TMP_DIR}/read-${note_id}.json"
}

printf 'Seeding smart HTTP endpoints at %s\n' "$BASE_URL"
ensure_endpoint "he-info-refs" \
  '{"Id":"he-info-refs","PathPrefix":"/{owner}/{repo}.git/info/refs","Methods":"GET","IntegrationModule":"git_upload_pack","RequiresAuth":false,"TimeoutSecs":60}'
ensure_endpoint "he-upload-pack" \
  '{"Id":"he-upload-pack","PathPrefix":"/{owner}/{repo}.git/git-upload-pack","Methods":"POST","IntegrationModule":"git_upload_pack","RequiresAuth":false,"TimeoutSecs":300}'
ensure_endpoint "he-receive-pack" \
  '{"Id":"he-receive-pack","PathPrefix":"/{owner}/{repo}.git/git-receive-pack","Methods":"POST","IntegrationModule":"git_receive_pack","RequiresAuth":false,"TimeoutSecs":300,"ActionBridgeEntityType":"Repository","ActionBridgeEntityId":"rp-{owner}-{repo}","ActionBridgeAction":"IngestPack","ActionBridgeResponse":"git-receive-pack"}'

printf 'Creating Genesis repository %s\n' "$REPO_ID"
post_json "$TENANT" "/tdata/Repositories" \
  "{\"Id\":$(json_escape "$REPO_ID"),\"OwnerAccountId\":$(json_escape "$OWNER"),\"Name\":$(json_escape "$REPO"),\"Description\":\"Genesis install live E2E app repository\",\"DefaultBranch\":\"main\",\"Visibility\":\"public\"}" \
  "$TMP_DIR/repository.json"
post_json_system "$TENANT" "/tdata/Repositories('${REPO_ID}')/Temper.Git.MarkProvisioned" \
  "{\"LibsqlDbName\":$(json_escape "${REPO_ID}.db")}" \
  "$TMP_DIR/provision.json"

APP_DIR="$TMP_DIR/app"
write_tiny_app_bundle "$APP_DIR"
git -C "$APP_DIR" init -b main >/dev/null
git -C "$APP_DIR" config user.email "rita.mirai@gmail.com"
git -C "$APP_DIR" config user.name "rita-aga"
git -C "$APP_DIR" add .
git -C "$APP_DIR" commit -m "Add tiny notes genesis app" >/dev/null
COMMIT_SHA="$(git -C "$APP_DIR" rev-parse HEAD)"

printf 'Pushing app commit %s to %s\n' "$COMMIT_SHA" "$REMOTE"
git -C "$APP_DIR" push "$REMOTE" main > "$TMP_DIR/push.log" 2>&1

get_json "$TENANT" "/tdata/Refs('${REF_ID}')" "$TMP_DIR/ref.json"
TARGET_SHA="$(node -e 'const fs=require("fs");const j=JSON.parse(fs.readFileSync(process.argv[1],"utf8"));process.stdout.write(j.fields?.TargetCommitSha||j.TargetCommitSha||"")' "$TMP_DIR/ref.json")"
if [[ "$TARGET_SHA" != "$COMMIT_SHA" ]]; then
  printf 'Ref target mismatch: got %s expected %s\n' "$TARGET_SHA" "$COMMIT_SHA" >&2
  sed -n '1,200p' "$TMP_DIR/push.log" >&2
  exit 1
fi

printf 'Registering App %s\n' "$APP_ID"
post_json "$TENANT" "/tdata/Apps('${APP_ID}')/Temper.Git.RegisterNewApp?await_integration=true" \
  "{\"Name\":$(json_escape "$REPO"),\"RepositoryId\":$(json_escape "$REPO_ID"),\"Description\":\"Tiny Genesis install E2E app\",\"Exports\":\"{}\",\"Visibility\":\"public\"}" \
  "$TMP_DIR/register.json"
get_json "$TENANT" "/tdata/Apps('${APP_ID}')" "$TMP_DIR/app-row.json"

APP_REF="${OWNER}/${REPO}@${COMMIT_SHA}"
printf 'Installing %s through OData and TemperPaw-shaped OData call\n' "$APP_REF"
ODATA_TENANT="paw-odata-${RUN_ID}"
TOOL_TENANT="paw-tool-${RUN_ID}"
CLI_TENANT="paw-cli-${RUN_ID}"
for target in "$ODATA_TENANT" "$TOOL_TENANT"; do
  post_json "$TENANT" "/tdata/Apps('${APP_ID}')/App.Install?await_integration=true" \
    "{\"TargetTenant\":$(json_escape "$target"),\"AppRef\":$(json_escape "$APP_REF"),\"Installer\":$(json_escape "$target")}" \
    "$TMP_DIR/install-${target}.json"
  create_note_in_tenant "$target" "installed via ${target}"
done

printf 'Installing %s through temper install\n' "$APP_REF"
run_temper_cli install "$APP_REF" --tenant "$CLI_TENANT" --url "$BASE_URL" --installer cli-e2e > "$TMP_DIR/cli-install.log" 2>&1
create_note_in_tenant "$CLI_TENANT" "installed via cli"

get_json "$TENANT" "/tdata/AppInstallations" "$TMP_DIR/installations.json"
node -e '
const fs = require("fs");
const rows = JSON.parse(fs.readFileSync(process.argv[1], "utf8")).value || [];
const installed = rows.filter((r) => (r.fields?.Status || r.status) === "Installed");
if (installed.length < 3) {
  console.error(`expected at least 3 installed AppInstallation rows, got ${installed.length}`);
  process.exit(1);
}
' "$TMP_DIR/installations.json"

cat > "$TMP_DIR/proof.env" <<EOF
BASE_URL=${BASE_URL}
TENANT=${TENANT}
OWNER=${OWNER}
REPO=${REPO}
REPO_ID=${REPO_ID}
APP_ID=${APP_ID}
APP_REF=${APP_REF}
COMMIT_SHA=${COMMIT_SHA}
REF_ID=${REF_ID}
ODATA_TENANT=${ODATA_TENANT}
TOOL_TENANT=${TOOL_TENANT}
CLI_TENANT=${CLI_TENANT}
EOF

printf 'PASS local genesis install e2e\n'
printf '  tmp: %s\n' "$TMP_DIR"
printf '  app_ref: %s\n' "$APP_REF"
printf '  commit: %s\n' "$COMMIT_SHA"
printf '  remote: %s\n' "$REMOTE"
