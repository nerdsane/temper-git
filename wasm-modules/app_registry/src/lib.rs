//! app_registry — Genesis App composite result producer.
//!
//! This module is intentionally only a data producer. It reads the
//! spec-triggered App invocation context, resolves any needed registry
//! metadata, and returns a `sub_writes` envelope. The Temper kernel validates
//! the envelope against the Composite contract and applies the writes. WASM
//! does not dispatch Temper actions.

#![forbid(unsafe_code)]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde_json::Value;
use temper_wasm_sdk::prelude::*;

const DEFAULT_TEMPER_API: &str = "http://127.0.0.1:3000";
const CREATED_AT: &str = "1970-01-01T00:00:00Z";

temper_module! {
    fn run(ctx: Context) -> Result<Value> {
        match ctx.trigger_action.as_str() {
            "RegisterNewApp" => run_register_new_app(&ctx),
            "Fork" => run_fork(&ctx),
            "PublishNewVersion" => run_publish_new_version(&ctx),
            "Install" => run_install(&ctx),
            other => Err(format!("app_registry does not handle trigger action '{other}'")),
        }
    }
}

fn run_register_new_app(ctx: &Context) -> Result<Value, String> {
    let params = RegisterParams::from_value(&ctx.trigger_params)?;
    let repository = fetch_repository(ctx, &params.repository_id)?;
    let default_ref = fetch_repository_ref(ctx, &params.repository_id, &repository.default_ref())?;
    let app_id = if ctx.entity_id.is_empty() {
        app_id_for(&repository.owner_account_id, &params.name)
    } else {
        ctx.entity_id.clone()
    };
    let sub_writes = build_register_sub_writes(&app_id, &repository, &default_ref, &params)?;

    Ok(json!({
        "app_id": app_id,
        "repository_id": repository.id,
        "latest_version_hash": default_ref.target_commit_sha,
        "sub_write_count": sub_writes.len(),
        "sub_writes": sub_writes,
    }))
}

fn run_fork(ctx: &Context) -> Result<Value, String> {
    let params = ForkParams::from_value(&ctx.trigger_params)?;
    if !ctx.entity_id.is_empty() && params.parent_app_id != ctx.entity_id {
        return Err(format!(
            "ParentAppId '{}' does not match triggering App '{}'",
            params.parent_app_id, ctx.entity_id
        ));
    }

    let parent = fetch_parent_app(ctx, &params.parent_app_id)?;
    let sub_writes = build_fork_sub_writes(&parent, &params)?;

    Ok(json!({
        "parent_app_id": parent.id,
        "child_repository_id": child_repository_id(&params),
        "child_app_id": child_app_id(&params),
        "sub_write_count": sub_writes.len(),
        "sub_writes": sub_writes,
    }))
}

fn run_publish_new_version(ctx: &Context) -> Result<Value, String> {
    let params = PublishParams::from_value(&ctx.trigger_params)?;
    let app = AppSnapshot::from_entity_state(&ctx.entity_id, &ctx.entity_state)?;
    let sub_writes = build_publish_sub_writes(&app, &params)?;

    Ok(json!({
        "app_id": app.id,
        "repository_id": app.repository_id,
        "ref_name": normalize_ref_name(&params.ref_name),
        "new_hash": params.new_hash,
        "sub_write_count": sub_writes.len(),
        "sub_writes": sub_writes,
    }))
}

fn run_install(ctx: &Context) -> Result<Value, String> {
    let params = InstallParams::from_value(&ctx.trigger_params)?;
    let app = InstallAppSnapshot::from_entity_state(&ctx.entity_id, &ctx.entity_state)?;
    let target_tenant = params
        .target_tenant
        .clone()
        .filter(|tenant| !tenant.is_empty())
        .unwrap_or_else(|| "default".to_string());
    let app_ref = params
        .app_ref
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            format!(
                "{}/{}@{}",
                app.owner_id,
                app.name,
                app.latest_version_hash.trim_start_matches('@')
            )
        });
    let installation_id = installation_id(&app.id, &target_tenant, &app.latest_version_hash);
    let sub_writes = vec![json!({
        "entity_type": "AppInstallation",
        "entity_id": installation_id.clone(),
        "action": "Create",
        "params": {
            "AppId": app.id,
            "AppRef": app_ref,
            "VersionHash": app.latest_version_hash,
            "TargetTenant": target_tenant,
            "ClosureId": "",
            "Installer": params.installer.unwrap_or_else(|| "unknown".to_string()),
            "Message": "install requested",
            "CreatedAt": CREATED_AT
        }
    })];

    Ok(json!({
        "app_id": ctx.entity_id,
        "installation_id": installation_id,
        "sub_write_count": sub_writes.len(),
        "sub_writes": sub_writes,
    }))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RegisterParams {
    name: String,
    repository_id: String,
    description: String,
    exports: String,
    visibility: String,
}

impl RegisterParams {
    fn from_value(value: &Value) -> Result<Self, String> {
        Ok(Self {
            name: read_required_string_for(value, "Name", "App.RegisterNewApp")?,
            repository_id: read_required_string_for(value, "RepositoryId", "App.RegisterNewApp")?,
            description: read_string(value, "Description").unwrap_or_default(),
            exports: read_string(value, "Exports").unwrap_or_else(|| "{}".to_string()),
            visibility: read_string(value, "Visibility").unwrap_or_else(|| "public".to_string()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ForkParams {
    parent_app_id: String,
    parent_version_hash: String,
    child_owner_id: String,
    child_name: String,
    description: String,
}

impl ForkParams {
    fn from_value(value: &Value) -> Result<Self, String> {
        Ok(Self {
            parent_app_id: read_required_string(value, "ParentAppId")?,
            parent_version_hash: read_required_string(value, "ParentVersionHash")?,
            child_owner_id: read_required_string(value, "ChildOwnerId")?,
            child_name: read_required_string(value, "ChildName")?,
            description: read_string(value, "Description").unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PublishParams {
    new_hash: String,
    ref_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallParams {
    target_tenant: Option<String>,
    app_ref: Option<String>,
    installer: Option<String>,
}

impl InstallParams {
    fn from_value(value: &Value) -> Result<Self, String> {
        Ok(Self {
            target_tenant: read_string(value, "TargetTenant")
                .or_else(|| read_string(value, "tenant")),
            app_ref: read_string(value, "AppRef"),
            installer: read_string(value, "Installer"),
        })
    }
}

impl PublishParams {
    fn from_value(value: &Value) -> Result<Self, String> {
        Ok(Self {
            new_hash: read_required_string_for(value, "NewHash", "App.PublishNewVersion")?,
            ref_name: read_required_string_for(value, "RefName", "App.PublishNewVersion")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepositorySnapshot {
    id: String,
    owner_account_id: String,
    default_branch: String,
}

impl RepositorySnapshot {
    fn from_row(repository_id: &str, row: &Value) -> Result<Self, String> {
        Ok(Self {
            id: row_string(row, "Id").unwrap_or_else(|| repository_id.to_string()),
            owner_account_id: row_required_string_with_context(
                row,
                "OwnerAccountId",
                "Repository row",
            )?,
            default_branch: row_string(row, "DefaultBranch").unwrap_or_else(|| "main".to_string()),
        })
    }

    fn default_ref(&self) -> String {
        normalize_ref_name(&self.default_branch)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RefSnapshot {
    target_commit_sha: String,
}

impl RefSnapshot {
    fn from_row(row: &Value) -> Result<Self, String> {
        Ok(Self {
            target_commit_sha: row_required_string_with_context(row, "TargetCommitSha", "Ref row")?,
        })
    }
}

fn fetch_repository(ctx: &Context, repository_id: &str) -> Result<RepositorySnapshot, String> {
    let temper_api = temper_api_base(ctx);
    let url = format!(
        "{temper_api}/tdata/Repositories('{}')",
        odata_key(repository_id)
    );
    let resp = ctx
        .http_call("GET", &url, &[], "")
        .map_err(|e| format!("fetch Repository: {e}"))?;
    if !(200..400).contains(&resp.status) {
        return Err(format!("Repository status {}", resp.status));
    }
    let row: Value =
        serde_json::from_str(&resp.body).map_err(|e| format!("Repository json: {e}"))?;
    RepositorySnapshot::from_row(repository_id, &row)
}

fn fetch_repository_ref(
    ctx: &Context,
    repository_id: &str,
    ref_name: &str,
) -> Result<RefSnapshot, String> {
    let ref_id = ref_id_for(repository_id, ref_name);
    let temper_api = temper_api_base(ctx);
    let url = format!("{temper_api}/tdata/Refs('{}')", odata_key(&ref_id));
    let resp = ctx
        .http_call("GET", &url, &[], "")
        .map_err(|e| format!("fetch Ref: {e}"))?;
    if !(200..400).contains(&resp.status) {
        return Err(format!("Ref {ref_name} status {}", resp.status));
    }
    let row: Value = serde_json::from_str(&resp.body).map_err(|e| format!("Ref json: {e}"))?;
    RefSnapshot::from_row(&row)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParentApp {
    id: String,
    owner_id: String,
    name: String,
    repository_id: String,
    latest_version_hash: String,
    exports: String,
    visibility: String,
}

fn fetch_parent_app(ctx: &Context, parent_app_id: &str) -> Result<ParentApp, String> {
    let temper_api = temper_api_base(ctx);
    let url = format!("{temper_api}/tdata/Apps('{}')", odata_key(parent_app_id));
    let resp = ctx
        .http_call("GET", &url, &[], "")
        .map_err(|e| format!("fetch parent App: {e}"))?;
    if !(200..400).contains(&resp.status) {
        return Err(format!("parent App status {}", resp.status));
    }
    let row: Value =
        serde_json::from_str(&resp.body).map_err(|e| format!("parent App json: {e}"))?;
    ParentApp::from_row(parent_app_id, &row)
}

impl ParentApp {
    fn from_row(parent_app_id: &str, row: &Value) -> Result<Self, String> {
        Ok(Self {
            id: row_string(row, "Id").unwrap_or_else(|| parent_app_id.to_string()),
            owner_id: row_required_string(row, "OwnerId")?,
            name: row_required_string(row, "Name")?,
            repository_id: row_required_string(row, "RepositoryId")?,
            latest_version_hash: row_required_string(row, "LatestVersionHash")?,
            exports: row_required_string(row, "Exports")?,
            visibility: row_string(row, "Visibility").unwrap_or_else(|| "public".to_string()),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AppSnapshot {
    id: String,
    repository_id: String,
    latest_version_hash: String,
}

impl AppSnapshot {
    fn from_entity_state(entity_id: &str, state: &Value) -> Result<Self, String> {
        Ok(Self {
            id: row_string(state, "Id").unwrap_or_else(|| entity_id.to_string()),
            repository_id: row_required_string_with_context(state, "RepositoryId", "App state")?,
            latest_version_hash: row_required_string_with_context(
                state,
                "LatestVersionHash",
                "App state",
            )?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallAppSnapshot {
    id: String,
    owner_id: String,
    name: String,
    latest_version_hash: String,
}

impl InstallAppSnapshot {
    fn from_entity_state(entity_id: &str, state: &Value) -> Result<Self, String> {
        Ok(Self {
            id: row_string(state, "Id").unwrap_or_else(|| entity_id.to_string()),
            owner_id: row_required_string_with_context(state, "OwnerId", "App state")?,
            name: row_required_string_with_context(state, "Name", "App state")?,
            latest_version_hash: row_required_string_with_context(
                state,
                "LatestVersionHash",
                "App state",
            )?,
        })
    }
}

fn build_register_sub_writes(
    app_id: &str,
    repository: &RepositorySnapshot,
    default_ref: &RefSnapshot,
    params: &RegisterParams,
) -> Result<Vec<Value>, String> {
    if default_ref.target_commit_sha.is_empty() {
        return Err("default ref TargetCommitSha must not be empty".to_string());
    }

    Ok(vec![json!({
        "entity_type": "App",
        "entity_id": app_id,
        "action": "Create",
        "params": {
            "OwnerId": repository.owner_account_id,
            "Name": params.name,
            "RepositoryId": repository.id,
            "LatestVersionHash": default_ref.target_commit_sha,
            "Exports": params.exports,
            "Description": params.description,
            "Visibility": params.visibility,
            "CreatedAt": CREATED_AT,
            "UpdatedAt": CREATED_AT
        }
    })])
}

fn build_fork_sub_writes(parent: &ParentApp, params: &ForkParams) -> Result<Vec<Value>, String> {
    if params.parent_version_hash.is_empty() {
        return Err("ParentVersionHash must not be empty".to_string());
    }

    let child_repo_id = child_repository_id(params);
    let child_app_id = child_app_id(params);
    let lineage_id = lineage_id(parent, params);
    let main_ref_id = ref_id_for(&child_repo_id, "refs/heads/main");
    let description = if params.description.is_empty() {
        format!("Fork of {}/{}", parent.owner_id, parent.name)
    } else {
        params.description.clone()
    };

    Ok(vec![
        json!({
            "entity_type": "Repository",
            "entity_id": child_repo_id,
            "action": "Create",
            "params": {
                "OwnerAccountId": params.child_owner_id,
                "Name": params.child_name,
                "Description": description,
                "DefaultBranch": "main",
                "Visibility": parent.visibility,
                "CreatedAt": CREATED_AT,
                "UpdatedAt": CREATED_AT
            }
        }),
        json!({
            "entity_type": "Ref",
            "entity_id": main_ref_id,
            "action": "Create",
            "params": {
                "RepositoryId": child_repository_id(params),
                "Name": "refs/heads/main",
                "TargetCommitSha": params.parent_version_hash,
                "Kind": "branch",
                "UpdatedAt": CREATED_AT
            }
        }),
        json!({
            "entity_type": "App",
            "entity_id": child_app_id,
            "action": "Create",
            "params": {
                "OwnerId": params.child_owner_id,
                "Name": params.child_name,
                "RepositoryId": child_repository_id(params),
                "LatestVersionHash": params.parent_version_hash,
                "Exports": parent.exports,
                "Description": description,
                "Visibility": parent.visibility,
                "CreatedAt": CREATED_AT,
                "UpdatedAt": CREATED_AT
            }
        }),
        json!({
            "entity_type": "Lineage",
            "entity_id": lineage_id,
            "action": "Create",
            "params": {
                "ChildRepositoryId": child_repository_id(params),
                "ParentRepositoryId": parent.repository_id,
                "ParentCommit": params.parent_version_hash,
                "Type": "fork",
                "CreatedBy": params.child_owner_id,
                "Mutations": "[]",
                "CreatedAt": CREATED_AT,
                "UpdatedAt": CREATED_AT
            }
        }),
    ])
}

fn build_publish_sub_writes(
    app: &AppSnapshot,
    params: &PublishParams,
) -> Result<Vec<Value>, String> {
    if params.new_hash == app.latest_version_hash {
        return Err("NewHash must differ from the current LatestVersionHash".to_string());
    }

    let ref_name = normalize_ref_name(&params.ref_name);
    let ref_id = ref_id_for(&app.repository_id, &ref_name);

    Ok(vec![
        json!({
            "entity_type": "Ref",
            "entity_id": ref_id,
            "action": "Update",
            "params": {
                "PreviousCommitSha": app.latest_version_hash,
                "NewCommitSha": params.new_hash,
                "TargetCommitSha": params.new_hash,
                "UpdatedAt": CREATED_AT
            }
        }),
        json!({
            "entity_type": "App",
            "entity_id": app.id,
            "action": "Update",
            "params": {
                "LatestVersionHash": params.new_hash,
                "UpdatedAt": CREATED_AT
            }
        }),
    ])
}

fn normalize_ref_name(input: &str) -> String {
    if input.starts_with("refs/") {
        input.to_string()
    } else {
        format!("refs/heads/{input}")
    }
}

fn child_repository_id(params: &ForkParams) -> String {
    format!(
        "rp-{}-{}",
        sanitize_id_component(&params.child_owner_id),
        sanitize_id_component(&params.child_name)
    )
}

fn child_app_id(params: &ForkParams) -> String {
    format!(
        "app-{}-{}",
        sanitize_id_component(&params.child_owner_id),
        sanitize_id_component(&params.child_name)
    )
}

fn app_id_for(owner_id: &str, name: &str) -> String {
    format!(
        "app-{}-{}",
        sanitize_id_component(owner_id),
        sanitize_id_component(name)
    )
}

fn lineage_id(parent: &ParentApp, params: &ForkParams) -> String {
    format!(
        "ln-{}-from-{}",
        sanitize_id_component(&child_repository_id(params)),
        sanitize_id_component(&parent.id)
    )
}

fn ref_id_for(repository_id: &str, refname: &str) -> String {
    format!("rf-{}-{}", repository_id, refname.replace('/', "-"))
}

fn installation_id(app_id: &str, target_tenant: &str, version_hash: &str) -> String {
    format!(
        "ai-{}-{}-{}",
        sanitize_id_component(app_id),
        sanitize_id_component(target_tenant),
        sanitize_id_component(version_hash)
            .chars()
            .take(16)
            .collect::<String>()
    )
}

fn sanitize_id_component(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "item".to_string()
    } else {
        trimmed
    }
}

fn read_required_string(value: &Value, key: &str) -> Result<String, String> {
    read_required_string_for(value, key, "App.Fork")
}

fn read_required_string_for(value: &Value, key: &str, action: &str) -> Result<String, String> {
    read_string(value, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!("{action} parameter '{key}' is required and must be a non-empty string")
        })
}

fn read_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn row_required_string(row: &Value, key: &str) -> Result<String, String> {
    row_required_string_with_context(row, key, "parent App row")
}

fn row_required_string_with_context(
    row: &Value,
    key: &str,
    context: &str,
) -> Result<String, String> {
    row_string(row, key)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{context} missing {key}"))
}

fn row_string(row: &Value, key: &str) -> Option<String> {
    row.get("fields")
        .and_then(|fields| fields.get(key))
        .or_else(|| row.get(key))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

fn odata_key(input: &str) -> String {
    input.replace('\'', "''")
}

fn temper_api_base(ctx: &Context) -> String {
    ctx.config
        .get("temper_api_url")
        .map(|value| value.trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_TEMPER_API.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};
    use std::thread;

    fn context_with_config(config: BTreeMap<String, String>) -> Context {
        Context {
            config,
            trigger_params: Value::Null,
            entity_state: Value::Null,
            tenant: "test".to_string(),
            entity_type: "App".to_string(),
            entity_id: "app-test".to_string(),
            trigger_action: "RegisterNewApp".to_string(),
            wasm_module: "app_registry".to_string(),
            http_request: None,
        }
    }

    fn parent() -> ParentApp {
        ParentApp {
            id: "app-acme-parent".to_string(),
            owner_id: "acme".to_string(),
            name: "parent".to_string(),
            repository_id: "rp-acme-parent".to_string(),
            latest_version_hash: "1111111111111111111111111111111111111111".to_string(),
            exports: r#"{"entities":["Repository"]}"#.to_string(),
            visibility: "public".to_string(),
        }
    }

    fn fork_params() -> ForkParams {
        ForkParams {
            parent_app_id: "app-acme-parent".to_string(),
            parent_version_hash: "2222222222222222222222222222222222222222".to_string(),
            child_owner_id: "Beta Labs".to_string(),
            child_name: "Child App".to_string(),
            description: "Child fork".to_string(),
        }
    }

    fn register_params() -> RegisterParams {
        RegisterParams {
            name: "registered".to_string(),
            repository_id: "rp-acme-register".to_string(),
            description: "Registered app".to_string(),
            exports: "{}".to_string(),
            visibility: "public".to_string(),
        }
    }

    fn repository_snapshot() -> RepositorySnapshot {
        RepositorySnapshot {
            id: "rp-acme-register".to_string(),
            owner_account_id: "acme".to_string(),
            default_branch: "main".to_string(),
        }
    }

    fn ref_snapshot() -> RefSnapshot {
        RefSnapshot {
            target_commit_sha: "4444444444444444444444444444444444444444".to_string(),
        }
    }

    fn app_snapshot() -> AppSnapshot {
        AppSnapshot {
            id: "app-acme-parent".to_string(),
            repository_id: "rp-acme-parent".to_string(),
            latest_version_hash: "1111111111111111111111111111111111111111".to_string(),
        }
    }

    fn publish_params() -> PublishParams {
        PublishParams {
            new_hash: "3333333333333333333333333333333333333333".to_string(),
            ref_name: "main".to_string(),
        }
    }

    fn indexed_repository_snapshot(idx: usize) -> RepositorySnapshot {
        RepositorySnapshot {
            id: format!("rp-owner-{idx}-registered"),
            owner_account_id: format!("owner-{idx}"),
            default_branch: if idx % 2 == 0 {
                "main".to_string()
            } else {
                "refs/heads/release".to_string()
            },
        }
    }

    fn indexed_ref_snapshot(idx: usize) -> RefSnapshot {
        RefSnapshot {
            target_commit_sha: format!("{idx:040x}"),
        }
    }

    fn indexed_register_params(idx: usize) -> RegisterParams {
        RegisterParams {
            name: format!("registered-{idx}"),
            repository_id: format!("rp-owner-{idx}-registered"),
            description: format!("Registered app {idx}"),
            exports: format!(r#"{{"entities":["Widget{idx}"]}}"#),
            visibility: "public".to_string(),
        }
    }

    fn indexed_parent(idx: usize) -> ParentApp {
        ParentApp {
            id: format!("app-parent-{idx}"),
            owner_id: format!("parent-owner-{idx}"),
            name: format!("parent-{idx}"),
            repository_id: format!("rp-parent-{idx}"),
            latest_version_hash: format!("{:040x}", idx + 1000),
            exports: format!(r#"{{"entities":["Parent{idx}"]}}"#),
            visibility: if idx % 2 == 0 {
                "public".to_string()
            } else {
                "private".to_string()
            },
        }
    }

    fn indexed_fork_params(idx: usize) -> ForkParams {
        ForkParams {
            parent_app_id: format!("app-parent-{idx}"),
            parent_version_hash: format!("{:040x}", idx + 2000),
            child_owner_id: format!("Child Owner {idx}"),
            child_name: format!("Child App {idx}"),
            description: String::new(),
        }
    }

    fn indexed_app_snapshot(idx: usize) -> AppSnapshot {
        AppSnapshot {
            id: format!("app-publish-{idx}"),
            repository_id: format!("rp-publish-{idx}"),
            latest_version_hash: format!("{:040x}", idx + 3000),
        }
    }

    fn indexed_publish_params(idx: usize) -> PublishParams {
        PublishParams {
            new_hash: format!("{:040x}", idx + 4000),
            ref_name: if idx % 2 == 0 {
                "main".to_string()
            } else {
                "refs/heads/release".to_string()
            },
        }
    }

    fn multi_action_envelopes(idx: usize) -> (Vec<Value>, Vec<Value>, Vec<Value>) {
        let repository = indexed_repository_snapshot(idx);
        let default_ref = indexed_ref_snapshot(idx);
        let register_params = indexed_register_params(idx);
        let parent = indexed_parent(idx);
        let fork_params = indexed_fork_params(idx);
        let app = indexed_app_snapshot(idx);
        let publish_params = indexed_publish_params(idx);

        (
            build_register_sub_writes(
                &app_id_for(&repository.owner_account_id, &register_params.name),
                &repository,
                &default_ref,
                &register_params,
            )
            .expect("register sub-writes should build"),
            build_fork_sub_writes(&parent, &fork_params).expect("fork sub-writes should build"),
            build_publish_sub_writes(&app, &publish_params)
                .expect("publish sub-writes should build"),
        )
    }

    #[test]
    fn register_sub_writes_create_app_from_repository_default_ref() {
        let writes = build_register_sub_writes(
            "app-acme-registered",
            &repository_snapshot(),
            &ref_snapshot(),
            &register_params(),
        )
        .unwrap();

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0]["entity_type"], "App");
        assert_eq!(writes[0]["entity_id"], "app-acme-registered");
        assert_eq!(writes[0]["action"], "Create");
        assert_eq!(writes[0]["params"]["OwnerId"], "acme");
        assert_eq!(writes[0]["params"]["Name"], "registered");
        assert_eq!(writes[0]["params"]["RepositoryId"], "rp-acme-register");
        assert_eq!(
            writes[0]["params"]["LatestVersionHash"],
            "4444444444444444444444444444444444444444"
        );
    }

    #[test]
    fn temper_api_base_uses_invocation_config_and_trims_trailing_slash() {
        let mut config = BTreeMap::new();
        config.insert(
            "temper_api_url".to_string(),
            "http://127.0.0.1:3155/".to_string(),
        );
        let ctx = context_with_config(config);

        assert_eq!(temper_api_base(&ctx), "http://127.0.0.1:3155");
    }

    #[test]
    fn temper_api_base_keeps_dev_default_when_config_is_absent() {
        let ctx = context_with_config(BTreeMap::new());

        assert_eq!(temper_api_base(&ctx), DEFAULT_TEMPER_API);
    }

    #[test]
    fn repository_snapshot_default_ref_normalizes_branch_name() {
        let repository = RepositorySnapshot::from_row(
            "rp-acme-register",
            &json!({
                "fields": {
                    "OwnerAccountId": "acme",
                    "DefaultBranch": "release/candidate"
                }
            }),
        )
        .unwrap();

        assert_eq!(repository.default_ref(), "refs/heads/release/candidate");

        let repository = RepositorySnapshot::from_row(
            "rp-acme-register",
            &json!({
                "fields": {
                    "OwnerAccountId": "acme",
                    "DefaultBranch": "refs/heads/main"
                }
            }),
        )
        .unwrap();

        assert_eq!(repository.default_ref(), "refs/heads/main");
    }

    #[test]
    fn register_rejects_empty_default_ref_hash() {
        let err = build_register_sub_writes(
            "app-acme-registered",
            &repository_snapshot(),
            &RefSnapshot {
                target_commit_sha: String::new(),
            },
            &register_params(),
        )
        .unwrap_err();

        assert!(err.contains("TargetCommitSha"));
    }

    #[test]
    fn fork_sub_writes_match_declared_contract() {
        let params = fork_params();
        let writes = build_fork_sub_writes(&parent(), &params).unwrap();
        let pairs: Vec<_> = writes
            .iter()
            .map(|write| {
                (
                    write["entity_type"].as_str().unwrap(),
                    write["action"].as_str().unwrap(),
                )
            })
            .collect();

        assert_eq!(
            pairs,
            vec![
                ("Repository", "Create"),
                ("Ref", "Create"),
                ("App", "Create"),
                ("Lineage", "Create"),
            ]
        );
    }

    #[test]
    fn fork_uses_parent_commit_without_object_copy_writes() {
        let params = fork_params();
        let writes = build_fork_sub_writes(&parent(), &params).unwrap();
        assert_eq!(writes.len(), 4);
        assert!(writes.iter().all(|write| !matches!(
            write["entity_type"].as_str(),
            Some("Blob" | "Tree" | "Commit" | "Tag")
        )));
        assert_eq!(
            writes[1]["params"]["TargetCommitSha"],
            "2222222222222222222222222222222222222222"
        );
    }

    #[test]
    fn child_ids_are_deterministic_and_url_safe() {
        let params = fork_params();
        assert_eq!(child_repository_id(&params), "rp-beta-labs-child-app");
        assert_eq!(child_app_id(&params), "app-beta-labs-child-app");
        assert_eq!(
            lineage_id(&parent(), &params),
            "ln-rp-beta-labs-child-app-from-app-acme-parent"
        );
    }

    #[test]
    fn parent_app_parses_odata_row_shape() {
        let parent = ParentApp::from_row(
            "app-acme-parent",
            &json!({
                "entity_id": "app-acme-parent",
                "fields": {
                    "Id": "app-acme-parent",
                    "OwnerId": "acme",
                    "Name": "parent",
                    "RepositoryId": "rp-acme-parent",
                    "LatestVersionHash": "1111111111111111111111111111111111111111",
                    "Exports": "{}",
                    "Visibility": "public"
                }
            }),
        )
        .unwrap();

        assert_eq!(parent.repository_id, "rp-acme-parent");
        assert_eq!(parent.exports, "{}");
    }

    #[test]
    fn publish_sub_writes_update_ref_and_app() {
        let writes = build_publish_sub_writes(&app_snapshot(), &publish_params()).unwrap();
        let pairs: Vec<_> = writes
            .iter()
            .map(|write| {
                (
                    write["entity_type"].as_str().unwrap(),
                    write["action"].as_str().unwrap(),
                )
            })
            .collect();

        assert_eq!(pairs, vec![("Ref", "Update"), ("App", "Update")]);
        assert_eq!(writes[0]["entity_id"], "rf-rp-acme-parent-refs-heads-main");
        assert_eq!(
            writes[0]["params"]["PreviousCommitSha"],
            "1111111111111111111111111111111111111111"
        );
        assert_eq!(
            writes[0]["params"]["TargetCommitSha"],
            "3333333333333333333333333333333333333333"
        );
        assert_eq!(
            writes[1]["params"]["LatestVersionHash"],
            "3333333333333333333333333333333333333333"
        );
    }

    #[test]
    fn publish_rejects_noop_hash() {
        let err = build_publish_sub_writes(
            &app_snapshot(),
            &PublishParams {
                new_hash: "1111111111111111111111111111111111111111".to_string(),
                ref_name: "refs/heads/main".to_string(),
            },
        )
        .unwrap_err();

        assert!(err.contains("must differ"));
    }

    #[test]
    fn app_snapshot_parses_entity_state_shape() {
        let app = AppSnapshot::from_entity_state(
            "app-acme-parent",
            &json!({
                "fields": {
                    "RepositoryId": "rp-acme-parent",
                    "LatestVersionHash": "1111111111111111111111111111111111111111"
                }
            }),
        )
        .unwrap();

        assert_eq!(app.id, "app-acme-parent");
        assert_eq!(app.repository_id, "rp-acme-parent");
    }

    #[test]
    fn install_sub_write_records_pinned_app_ref() {
        let mut ctx = context_with_config(BTreeMap::new());
        ctx.entity_id = "app-acme-notes".to_string();
        ctx.trigger_action = "Install".to_string();
        ctx.trigger_params = json!({
            "TargetTenant": "tenant-a",
            "AppRef": "acme/notes@1111111111111111111111111111111111111111",
            "Installer": "temperpaw"
        });
        ctx.entity_state = json!({
            "fields": {
                "OwnerId": "acme",
                "Name": "notes",
                "LatestVersionHash": "1111111111111111111111111111111111111111"
            }
        });

        let result = run_install(&ctx).unwrap();
        let writes = result["sub_writes"].as_array().unwrap();

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0]["entity_type"], "AppInstallation");
        assert_eq!(writes[0]["action"], "Create");
        assert_eq!(
            writes[0]["entity_id"],
            "ai-app-acme-notes-tenant-a-1111111111111111"
        );
        assert_eq!(writes[0]["params"]["TargetTenant"], "tenant-a");
        assert_eq!(writes[0]["params"]["Installer"], "temperpaw");
        assert_eq!(
            writes[0]["params"]["AppRef"],
            "acme/notes@1111111111111111111111111111111111111111"
        );
    }

    #[test]
    fn multi_action_sub_write_builders_are_deterministic_under_parallel_load() {
        const ATTEMPTS: usize = 16;

        let handles = (0..ATTEMPTS)
            .map(|idx| thread::spawn(move || (idx, multi_action_envelopes(idx))))
            .collect::<Vec<_>>();

        let mut seen_entity_ids = BTreeSet::new();
        for handle in handles {
            let (idx, (register, fork, publish)) = handle
                .join()
                .expect("parallel app-registry builder should join");
            assert_eq!(
                (register.clone(), fork.clone(), publish.clone()),
                multi_action_envelopes(idx),
                "registry builders must be deterministic for input {idx}"
            );

            assert_eq!(register.len(), 1);
            assert_eq!(register[0]["entity_type"], "App");
            assert_eq!(register[0]["action"], "Create");
            assert_eq!(register[0]["params"]["OwnerId"], format!("owner-{idx}"));

            let fork_contract = fork
                .iter()
                .map(|write| {
                    (
                        write["entity_type"].as_str().unwrap(),
                        write["action"].as_str().unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                fork_contract,
                vec![
                    ("Repository", "Create"),
                    ("Ref", "Create"),
                    ("App", "Create"),
                    ("Lineage", "Create")
                ]
            );
            assert!(fork.iter().all(|write| !matches!(
                write["entity_type"].as_str(),
                Some("Blob" | "Tree" | "Commit" | "Tag")
            )));

            assert_eq!(publish.len(), 2);
            assert_eq!(publish[0]["entity_type"], "Ref");
            assert_eq!(publish[0]["action"], "Update");
            assert_eq!(publish[1]["entity_type"], "App");
            assert_eq!(publish[1]["action"], "Update");

            for write in register.iter().chain(fork.iter()).chain(publish.iter()) {
                let entity_id = write["entity_id"]
                    .as_str()
                    .expect("sub-write entity_id should be a string");
                assert!(
                    seen_entity_ids.insert(entity_id.to_string()),
                    "parallel multi-action builders produced duplicate entity id {entity_id}"
                );
            }
        }
    }
}
