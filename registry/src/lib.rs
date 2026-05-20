//! Genesis registry helpers shared by operator tooling and tests.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use sha2::{Digest, Sha256};

/// Resolver algorithm version used by the v1 deterministic Closure ID format.
pub const DEFAULT_RESOLVER_VERSION: &str = "1.0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedClosure {
    pub id: String,
    pub root: String,
    pub resolved: BTreeMap<String, String>,
    pub resolver_version: String,
}

impl ResolvedClosure {
    pub fn bootstrap_manifest(&self) -> String {
        format!("closure = \"{}\"\n", self.id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosureResolveError {
    InvalidRootRef(String),
    ReadDir {
        path: PathBuf,
        error: String,
    },
    ReadManifest {
        path: PathBuf,
        error: String,
    },
    ParseManifest {
        path: PathBuf,
        error: String,
    },
    InvalidManifest {
        path: PathBuf,
        reason: String,
    },
    DuplicateApp {
        name: String,
        first: PathBuf,
        second: PathBuf,
    },
    MissingApp {
        name: String,
    },
    ConflictingHash {
        name: String,
        first: String,
        second: String,
    },
    InvalidRegistryResponse(String),
    InvalidRegistryRow {
        row: String,
        reason: String,
    },
    InvalidRegistryMetadata {
        app: String,
        reason: String,
    },
    AmbiguousRegistryApp {
        key: String,
        matches: Vec<String>,
    },
    RegistryHashMismatch {
        app: String,
        requested: String,
        current: String,
    },
    MissingRepositoryObject {
        kind: String,
        repository_id: String,
        id: String,
    },
    DuplicateRepositoryObject {
        kind: String,
        repository_id: String,
        id: String,
    },
    MissingAppManifest {
        app: String,
        repository_id: String,
        commit: String,
    },
    InvalidRepositoryObject {
        kind: String,
        id: String,
        reason: String,
    },
}

impl fmt::Display for ClosureResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRootRef(value) => {
                write!(f, "invalid root ref '{value}', expected app-name@hash")
            }
            Self::ReadDir { path, error } => {
                write!(
                    f,
                    "failed to read app directory '{}': {error}",
                    path.display()
                )
            }
            Self::ReadManifest { path, error } => {
                write!(
                    f,
                    "failed to read app manifest '{}': {error}",
                    path.display()
                )
            }
            Self::ParseManifest { path, error } => {
                write!(
                    f,
                    "failed to parse app manifest '{}': {error}",
                    path.display()
                )
            }
            Self::InvalidManifest { path, reason } => {
                write!(f, "invalid app manifest '{}': {reason}", path.display())
            }
            Self::DuplicateApp {
                name,
                first,
                second,
            } => write!(
                f,
                "duplicate app '{name}' in '{}' and '{}'",
                first.display(),
                second.display()
            ),
            Self::MissingApp { name } => {
                write!(f, "missing local app manifest for dependency '{name}'")
            }
            Self::ConflictingHash {
                name,
                first,
                second,
            } => write!(
                f,
                "conflicting hashes for app '{name}': '{first}' vs '{second}'"
            ),
            Self::InvalidRegistryResponse(reason) => {
                write!(f, "invalid registry response: {reason}")
            }
            Self::InvalidRegistryRow { row, reason } => {
                write!(f, "invalid registry App row '{row}': {reason}")
            }
            Self::InvalidRegistryMetadata { app, reason } => {
                write!(f, "invalid registry metadata for app '{app}': {reason}")
            }
            Self::AmbiguousRegistryApp { key, matches } => write!(
                f,
                "ambiguous registry app '{key}', matched {}",
                matches.join(", ")
            ),
            Self::RegistryHashMismatch {
                app,
                requested,
                current,
            } => write!(
                f,
                "registry app '{app}' is at '{current}', not requested pin '{requested}'"
            ),
            Self::MissingRepositoryObject {
                kind,
                repository_id,
                id,
            } => write!(f, "missing {kind} '{id}' in repository '{repository_id}'"),
            Self::DuplicateRepositoryObject {
                kind,
                repository_id,
                id,
            } => write!(f, "duplicate {kind} '{id}' in repository '{repository_id}'"),
            Self::MissingAppManifest {
                app,
                repository_id,
                commit,
            } => write!(
                f,
                "missing app.toml for app '{app}' at commit '{commit}' in repository '{repository_id}'"
            ),
            Self::InvalidRepositoryObject { kind, id, reason } => {
                write!(f, "invalid {kind} '{id}': {reason}")
            }
        }
    }
}

impl std::error::Error for ClosureResolveError {}

/// Derive the content-addressed Closure entity ID.
///
/// The canonical byte format is deliberately length-prefixed and sorted so the
/// ID is stable across JSON map ordering, host platforms, and process runs.
pub fn closure_id(
    root: &str,
    resolved: &BTreeMap<String, String>,
    resolver_version: &str,
) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"genesis-closure:v1\0");
    push_field(&mut bytes, "root", root);
    push_resolved(&mut bytes, resolved);
    push_field(&mut bytes, "resolver_version", resolver_version);

    let digest = Sha256::digest(&bytes);
    format!("cl-{}", hex_lower(&digest))
}

/// Build the stable JSON object expected by the Closure entity's `Resolved`.
pub fn resolved_json(resolved: &BTreeMap<String, String>) -> String {
    serde_json::to_string(resolved).expect("BTreeMap<String, String> serializes")
}

/// Resolve a content-addressed Closure from local app manifests.
///
/// `root_ref` is an exact app reference such as `paw-heal@7a3f8e2c`.
/// Each `app_dirs` entry may point at an app bundle directory containing
/// `app.toml` or at a parent directory containing app bundle directories.
/// Only locked `[deps]` string entries are followed; `[deps.hints]` and other
/// non-string helper tables are ignored because they are not runtime pins.
pub fn resolve_local_closure(
    root_ref: &str,
    app_dirs: &[PathBuf],
) -> Result<ResolvedClosure, ClosureResolveError> {
    let (root_name, root_hash) = parse_root_ref(root_ref)?;
    let catalog = discover_local_apps(app_dirs)?;
    let mut resolved = BTreeMap::new();
    resolve_manifest_deps(&root_name, &root_hash, &catalog, &mut resolved)?;
    let id = closure_id(root_ref, &resolved, DEFAULT_RESOLVER_VERSION);

    Ok(ResolvedClosure {
        id,
        root: root_ref.to_string(),
        resolved,
        resolver_version: DEFAULT_RESOLVER_VERSION.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryApp {
    pub id: String,
    pub name: String,
    pub repository_id: String,
    pub latest_version_hash: String,
    pub exports: String,
}

impl RegistryApp {
    pub fn from_odata_row(row: &serde_json::Value) -> Result<Self, ClosureResolveError> {
        let id = registry_row_string(row, "Id").ok_or_else(|| {
            ClosureResolveError::InvalidRegistryRow {
                row: registry_row_label(row),
                reason: "missing Id".to_string(),
            }
        })?;
        let name = registry_row_string(row, "Name").ok_or_else(|| {
            ClosureResolveError::InvalidRegistryRow {
                row: id.clone(),
                reason: "missing Name".to_string(),
            }
        })?;
        let repository_id = registry_row_string(row, "RepositoryId").ok_or_else(|| {
            ClosureResolveError::InvalidRegistryRow {
                row: id.clone(),
                reason: "missing RepositoryId".to_string(),
            }
        })?;
        let latest_version_hash =
            registry_row_string(row, "LatestVersionHash").ok_or_else(|| {
                ClosureResolveError::InvalidRegistryRow {
                    row: id.clone(),
                    reason: "missing LatestVersionHash".to_string(),
                }
            })?;
        let exports = registry_row_string(row, "Exports").unwrap_or_else(|| "{}".to_string());

        Ok(Self {
            id,
            name,
            repository_id,
            latest_version_hash,
            exports,
        })
    }
}

/// Parse a Temper OData `/tdata/Apps` response into registry app snapshots.
pub fn registry_apps_from_odata_json(body: &str) -> Result<Vec<RegistryApp>, ClosureResolveError> {
    let parsed = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|e| ClosureResolveError::InvalidRegistryResponse(e.to_string()))?;
    let rows = parsed
        .get("value")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            ClosureResolveError::InvalidRegistryResponse(
                "expected top-level JSON object with array field 'value'".to_string(),
            )
        })?;
    let mut apps = Vec::new();
    for row in rows {
        let has_registry_shape = registry_row_string(row, "Name").is_some()
            || registry_row_string(row, "LatestVersionHash").is_some()
            || registry_row_string(row, "Exports").is_some();
        if !has_registry_shape {
            continue;
        }
        apps.push(RegistryApp::from_odata_row(row)?);
    }
    Ok(apps)
}

/// Resolve a Closure from registry App rows fetched from a Temper OData API.
///
/// The v1 registry row does not have a dedicated dependency column yet, so this
/// resolver reads locked dependencies from `Exports` JSON. It accepts either
/// `{ "deps": { "app": "@hash" } }` or `{ "dependencies": { ... } }` and
/// treats missing dependency metadata as an empty dependency set.
pub fn resolve_registry_closure(
    root_ref: &str,
    apps: &[RegistryApp],
) -> Result<ResolvedClosure, ClosureResolveError> {
    let (root_name, root_hash) = parse_root_ref(root_ref)?;
    let catalog = RegistryCatalog::new(apps)?;
    let mut resolved = BTreeMap::new();
    resolve_registry_deps(&root_name, &root_hash, &catalog, &mut resolved)?;
    let id = closure_id(root_ref, &resolved, DEFAULT_RESOLVER_VERSION);

    Ok(ResolvedClosure {
        id,
        root: root_ref.to_string(),
        resolved,
        resolver_version: DEFAULT_RESOLVER_VERSION.to_string(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryCommit {
    pub id: String,
    pub repository_id: String,
    pub tree_sha: String,
}

impl RegistryCommit {
    pub fn from_odata_row(row: &serde_json::Value) -> Result<Self, ClosureResolveError> {
        let id = registry_row_string(row, "Id").ok_or_else(|| {
            ClosureResolveError::InvalidRepositoryObject {
                kind: "Commit".to_string(),
                id: registry_row_label(row),
                reason: "missing Id".to_string(),
            }
        })?;
        Ok(Self {
            repository_id: registry_row_string_any(row, &["RepositoryId", "repository_id"])
                .ok_or_else(|| ClosureResolveError::InvalidRepositoryObject {
                    kind: "Commit".to_string(),
                    id: id.clone(),
                    reason: "missing RepositoryId".to_string(),
                })?,
            tree_sha: registry_row_string_any(row, &["TreeSha", "tree_sha"]).ok_or_else(|| {
                ClosureResolveError::InvalidRepositoryObject {
                    kind: "Commit".to_string(),
                    id: id.clone(),
                    reason: "missing TreeSha".to_string(),
                }
            })?,
            id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryTree {
    pub id: String,
    pub repository_id: String,
    body: Vec<u8>,
}

impl RegistryTree {
    pub fn from_odata_row(row: &serde_json::Value) -> Result<Self, ClosureResolveError> {
        let id = registry_row_string(row, "Id").ok_or_else(|| {
            ClosureResolveError::InvalidRepositoryObject {
                kind: "Tree".to_string(),
                id: registry_row_label(row),
                reason: "missing Id".to_string(),
            }
        })?;
        let canonical = registry_row_string_any(row, &["CanonicalBytes", "canonical_bytes"])
            .ok_or_else(|| ClosureResolveError::InvalidRepositoryObject {
                kind: "Tree".to_string(),
                id: id.clone(),
                reason: "missing CanonicalBytes".to_string(),
            })?;

        Ok(Self {
            repository_id: registry_row_string_any(row, &["RepositoryId", "repository_id"])
                .ok_or_else(|| ClosureResolveError::InvalidRepositoryObject {
                    kind: "Tree".to_string(),
                    id: id.clone(),
                    reason: "missing RepositoryId".to_string(),
                })?,
            body: decode_canonical_body(&canonical, "Tree", &id)?,
            id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryTreeEntry {
    pub tree_id: String,
    pub repository_id: String,
    pub path: String,
    pub mode: String,
    pub object_sha: String,
    pub kind: String,
}

impl RegistryTreeEntry {
    pub fn from_odata_row(row: &serde_json::Value) -> Result<Self, ClosureResolveError> {
        let id = registry_row_string(row, "Id").unwrap_or_else(|| registry_row_label(row));
        Ok(Self {
            tree_id: registry_row_string_any(row, &["TreeId", "tree_id"]).ok_or_else(|| {
                ClosureResolveError::InvalidRepositoryObject {
                    kind: "TreeEntry".to_string(),
                    id: id.clone(),
                    reason: "missing TreeId".to_string(),
                }
            })?,
            repository_id: registry_row_string_any(row, &["RepositoryId", "repository_id"])
                .ok_or_else(|| ClosureResolveError::InvalidRepositoryObject {
                    kind: "TreeEntry".to_string(),
                    id: id.clone(),
                    reason: "missing RepositoryId".to_string(),
                })?,
            path: registry_row_string_any(row, &["Path", "path"]).ok_or_else(|| {
                ClosureResolveError::InvalidRepositoryObject {
                    kind: "TreeEntry".to_string(),
                    id: id.clone(),
                    reason: "missing Path".to_string(),
                }
            })?,
            mode: registry_row_string_any(row, &["Mode", "mode"]).unwrap_or_default(),
            object_sha: registry_row_string_any(row, &["ObjectSha", "object_sha"]).ok_or_else(
                || ClosureResolveError::InvalidRepositoryObject {
                    kind: "TreeEntry".to_string(),
                    id: id.clone(),
                    reason: "missing ObjectSha".to_string(),
                },
            )?,
            kind: registry_row_string_any(row, &["Kind", "kind"]).unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryBlob {
    pub id: String,
    pub repository_id: String,
    pub content: Vec<u8>,
}

impl RegistryBlob {
    pub fn from_odata_row(row: &serde_json::Value) -> Result<Self, ClosureResolveError> {
        let id = registry_row_string(row, "Id").ok_or_else(|| {
            ClosureResolveError::InvalidRepositoryObject {
                kind: "Blob".to_string(),
                id: registry_row_label(row),
                reason: "missing Id".to_string(),
            }
        })?;
        let content = registry_row_string_any(row, &["Content", "content"]).ok_or_else(|| {
            ClosureResolveError::InvalidRepositoryObject {
                kind: "Blob".to_string(),
                id: id.clone(),
                reason: "missing Content".to_string(),
            }
        })?;
        Ok(Self {
            repository_id: registry_row_string_any(row, &["RepositoryId", "repository_id"])
                .ok_or_else(|| ClosureResolveError::InvalidRepositoryObject {
                    kind: "Blob".to_string(),
                    id: id.clone(),
                    reason: "missing RepositoryId".to_string(),
                })?,
            content: decode_blob_content(&content),
            id,
        })
    }
}

pub fn registry_commits_from_odata_json(
    body: &str,
) -> Result<Vec<RegistryCommit>, ClosureResolveError> {
    parse_odata_collection(body, RegistryCommit::from_odata_row)
}

pub fn registry_trees_from_odata_json(
    body: &str,
) -> Result<Vec<RegistryTree>, ClosureResolveError> {
    parse_odata_collection(body, RegistryTree::from_odata_row)
}

pub fn registry_tree_entries_from_odata_json(
    body: &str,
) -> Result<Vec<RegistryTreeEntry>, ClosureResolveError> {
    parse_odata_collection(body, RegistryTreeEntry::from_odata_row)
}

pub fn registry_blobs_from_odata_json(
    body: &str,
) -> Result<Vec<RegistryBlob>, ClosureResolveError> {
    parse_odata_collection(body, RegistryBlob::from_odata_row)
}

/// Resolve a Closure from the `app.toml` files stored at pinned app commits.
///
/// Unlike `resolve_registry_closure`, this follows the actual repository bytes
/// for each requested app version. That keeps old Closure IDs reproducible even
/// after an App row's `LatestVersionHash` or metadata moves forward.
pub fn resolve_registry_app_toml_closure(
    root_ref: &str,
    apps: &[RegistryApp],
    commits: &[RegistryCommit],
    tree_entries: &[RegistryTreeEntry],
    blobs: &[RegistryBlob],
) -> Result<ResolvedClosure, ClosureResolveError> {
    resolve_registry_app_toml_closure_with_trees(root_ref, apps, commits, &[], tree_entries, blobs)
}

/// Resolve a Closure from pinned registry versions using either explicit
/// TreeEntry rows or canonical Tree rows produced by real git pack ingest.
pub fn resolve_registry_app_toml_closure_with_trees(
    root_ref: &str,
    apps: &[RegistryApp],
    commits: &[RegistryCommit],
    trees: &[RegistryTree],
    tree_entries: &[RegistryTreeEntry],
    blobs: &[RegistryBlob],
) -> Result<ResolvedClosure, ClosureResolveError> {
    let (root_name, root_hash) = parse_root_ref(root_ref)?;
    let app_catalog = RegistryCatalog::new(apps)?;
    let object_graph = RepositoryObjectGraph::new(commits, trees, tree_entries, blobs)?;
    let mut resolved = BTreeMap::new();
    resolve_registry_app_toml_deps(
        &root_name,
        &root_hash,
        &app_catalog,
        &object_graph,
        &mut resolved,
    )?;
    let id = closure_id(root_ref, &resolved, DEFAULT_RESOLVER_VERSION);

    Ok(ResolvedClosure {
        id,
        root: root_ref.to_string(),
        resolved,
        resolver_version: DEFAULT_RESOLVER_VERSION.to_string(),
    })
}

#[derive(Debug, Clone)]
struct LocalAppManifest {
    name: String,
    deps: BTreeMap<String, String>,
    path: PathBuf,
}

fn parse_root_ref(root_ref: &str) -> Result<(String, String), ClosureResolveError> {
    let Some((name, hash)) = root_ref.split_once('@') else {
        return Err(ClosureResolveError::InvalidRootRef(root_ref.to_string()));
    };
    if name.trim().is_empty() || hash.trim().is_empty() {
        return Err(ClosureResolveError::InvalidRootRef(root_ref.to_string()));
    }
    Ok((name.to_string(), normalize_hash_pin(hash)))
}

fn discover_local_apps(
    app_dirs: &[PathBuf],
) -> Result<BTreeMap<String, LocalAppManifest>, ClosureResolveError> {
    let mut catalog = BTreeMap::new();
    for path in app_dirs {
        if path.join("app.toml").is_file() {
            insert_manifest(&mut catalog, read_local_manifest(path)?)?;
            continue;
        }

        let entries = std::fs::read_dir(path).map_err(|e| ClosureResolveError::ReadDir {
            path: path.clone(),
            error: e.to_string(),
        })?;
        let mut children = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|child| child.join("app.toml").is_file())
            .collect::<Vec<_>>();
        children.sort();
        for child in children {
            insert_manifest(&mut catalog, read_local_manifest(&child)?)?;
        }
    }
    Ok(catalog)
}

fn insert_manifest(
    catalog: &mut BTreeMap<String, LocalAppManifest>,
    manifest: LocalAppManifest,
) -> Result<(), ClosureResolveError> {
    if let Some(existing) = catalog.get(&manifest.name) {
        return Err(ClosureResolveError::DuplicateApp {
            name: manifest.name.clone(),
            first: existing.path.clone(),
            second: manifest.path,
        });
    }
    catalog.insert(manifest.name.clone(), manifest);
    Ok(())
}

fn read_local_manifest(app_dir: &Path) -> Result<LocalAppManifest, ClosureResolveError> {
    let manifest_path = app_dir.join("app.toml");
    let source =
        std::fs::read_to_string(&manifest_path).map_err(|e| ClosureResolveError::ReadManifest {
            path: manifest_path.clone(),
            error: e.to_string(),
        })?;
    read_manifest_source(manifest_path, &source)
}

fn read_manifest_source(
    manifest_path: PathBuf,
    source: &str,
) -> Result<LocalAppManifest, ClosureResolveError> {
    let parsed = source
        .parse::<toml::Value>()
        .map_err(|e| ClosureResolveError::ParseManifest {
            path: manifest_path.clone(),
            error: e.to_string(),
        })?;
    let table = parsed
        .as_table()
        .ok_or_else(|| ClosureResolveError::InvalidManifest {
            path: manifest_path.clone(),
            reason: "top-level manifest must be a TOML table".to_string(),
        })?;
    let name = table
        .get("name")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| ClosureResolveError::InvalidManifest {
            path: manifest_path.clone(),
            reason: "missing non-empty string field 'name'".to_string(),
        })?
        .to_string();

    let mut deps = BTreeMap::new();
    if let Some(deps_table) = table.get("deps").and_then(toml::Value::as_table) {
        for (dep_name, dep_value) in deps_table {
            let Some(dep_hash) = dep_value.as_str() else {
                continue;
            };
            let dep_hash = dep_hash.trim();
            if dep_name.trim().is_empty() || dep_hash.is_empty() {
                return Err(ClosureResolveError::InvalidManifest {
                    path: manifest_path.clone(),
                    reason: "locked [deps] entries must have non-empty names and hashes"
                        .to_string(),
                });
            }
            deps.insert(dep_name.to_string(), dep_hash.to_string());
        }
    }

    Ok(LocalAppManifest {
        name,
        deps,
        path: manifest_path,
    })
}

fn resolve_manifest_deps(
    app_name: &str,
    app_hash: &str,
    catalog: &BTreeMap<String, LocalAppManifest>,
    resolved: &mut BTreeMap<String, String>,
) -> Result<(), ClosureResolveError> {
    if let Some(existing) = resolved.get(app_name) {
        if existing == app_hash {
            return Ok(());
        }
        return Err(ClosureResolveError::ConflictingHash {
            name: app_name.to_string(),
            first: existing.clone(),
            second: app_hash.to_string(),
        });
    }

    let manifest = catalog
        .get(app_name)
        .ok_or_else(|| ClosureResolveError::MissingApp {
            name: app_name.to_string(),
        })?;
    resolved.insert(app_name.to_string(), app_hash.to_string());
    for (dep_name, dep_hash) in &manifest.deps {
        resolve_manifest_deps(dep_name, dep_hash, catalog, resolved)?;
    }
    Ok(())
}

struct RegistryCatalog<'a> {
    by_id: BTreeMap<&'a str, &'a RegistryApp>,
    by_name: BTreeMap<&'a str, Vec<&'a RegistryApp>>,
}

impl<'a> RegistryCatalog<'a> {
    fn new(apps: &'a [RegistryApp]) -> Result<Self, ClosureResolveError> {
        let mut by_id = BTreeMap::new();
        let mut by_name = BTreeMap::new();
        for app in apps {
            if app.id.trim().is_empty() {
                return Err(ClosureResolveError::InvalidRegistryRow {
                    row: "<empty>".to_string(),
                    reason: "Id must not be empty".to_string(),
                });
            }
            if let Some(existing) = by_id.insert(app.id.as_str(), app) {
                return Err(ClosureResolveError::DuplicateApp {
                    name: app.id.clone(),
                    first: PathBuf::from(&existing.id),
                    second: PathBuf::from(&app.id),
                });
            }
            by_name
                .entry(app.name.as_str())
                .or_insert_with(Vec::new)
                .push(app);
        }
        Ok(Self { by_id, by_name })
    }

    fn get(&self, key: &str) -> Result<&'a RegistryApp, ClosureResolveError> {
        if let Some(app) = self.by_id.get(key) {
            return Ok(app);
        }
        let Some(matches) = self.by_name.get(key) else {
            return Err(ClosureResolveError::MissingApp {
                name: key.to_string(),
            });
        };
        if matches.len() != 1 {
            return Err(ClosureResolveError::AmbiguousRegistryApp {
                key: key.to_string(),
                matches: matches.iter().map(|app| app.id.clone()).collect(),
            });
        }
        Ok(matches[0])
    }
}

fn resolve_registry_deps(
    app_key: &str,
    app_hash: &str,
    catalog: &RegistryCatalog<'_>,
    resolved: &mut BTreeMap<String, String>,
) -> Result<(), ClosureResolveError> {
    let app = catalog.get(app_key)?;
    let canonical_key = app.name.clone();
    let latest = normalize_hash_pin(&app.latest_version_hash);
    if latest != app_hash {
        return Err(ClosureResolveError::RegistryHashMismatch {
            app: app.id.clone(),
            requested: app_hash.to_string(),
            current: latest,
        });
    }

    if let Some(existing) = resolved.get(&canonical_key) {
        if existing == app_hash {
            return Ok(());
        }
        return Err(ClosureResolveError::ConflictingHash {
            name: canonical_key,
            first: existing.clone(),
            second: app_hash.to_string(),
        });
    }

    resolved.insert(canonical_key, app_hash.to_string());
    for (dep_name, dep_hash) in registry_deps_from_exports(app)? {
        resolve_registry_deps(&dep_name, &dep_hash, catalog, resolved)?;
    }
    Ok(())
}

struct RepositoryObjectGraph<'a> {
    commits: BTreeMap<(String, String), &'a RegistryCommit>,
    trees: BTreeMap<(String, String), &'a RegistryTree>,
    tree_entries: BTreeMap<(String, String), Vec<RegistryTreeEntry>>,
    blobs: BTreeMap<(String, String), &'a RegistryBlob>,
}

impl<'a> RepositoryObjectGraph<'a> {
    fn new(
        commits: &'a [RegistryCommit],
        trees: &'a [RegistryTree],
        tree_entries: &'a [RegistryTreeEntry],
        blobs: &'a [RegistryBlob],
    ) -> Result<Self, ClosureResolveError> {
        let mut commit_map = BTreeMap::new();
        for commit in commits {
            insert_unique_object(
                &mut commit_map,
                "Commit",
                &commit.repository_id,
                &commit.id,
                commit,
            )?;
        }

        let mut tree_map = BTreeMap::new();
        for tree in trees {
            insert_unique_object(&mut tree_map, "Tree", &tree.repository_id, &tree.id, tree)?;
        }

        let mut tree_entry_map: BTreeMap<(String, String), Vec<RegistryTreeEntry>> =
            BTreeMap::new();
        for entry in tree_entries {
            tree_entry_map
                .entry((entry.repository_id.clone(), entry.tree_id.clone()))
                .or_default()
                .push(entry.clone());
        }
        for entries in tree_entry_map.values_mut() {
            entries.sort_by(|a, b| a.path.cmp(&b.path));
        }

        let mut blob_map = BTreeMap::new();
        for blob in blobs {
            insert_unique_object(&mut blob_map, "Blob", &blob.repository_id, &blob.id, blob)?;
        }

        Ok(Self {
            commits: commit_map,
            trees: tree_map,
            tree_entries: tree_entry_map,
            blobs: blob_map,
        })
    }

    fn commit(
        &self,
        repository_id: &str,
        commit_id: &str,
    ) -> Result<&'a RegistryCommit, ClosureResolveError> {
        self.commits
            .get(&(repository_id.to_string(), commit_id.to_string()))
            .copied()
            .ok_or_else(|| ClosureResolveError::MissingRepositoryObject {
                kind: "Commit".to_string(),
                repository_id: repository_id.to_string(),
                id: commit_id.to_string(),
            })
    }

    fn read_file(
        &self,
        repository_id: &str,
        root_tree: &str,
        path: &str,
    ) -> Result<Vec<u8>, ClosureResolveError> {
        let mut tree_id = root_tree.to_string();
        let parts = path
            .split('/')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        for (index, part) in parts.iter().enumerate() {
            let entries = self.entries_for_tree(repository_id, &tree_id)?;
            let entry = entries
                .iter()
                .find(|entry| entry.path == *part)
                .ok_or_else(|| ClosureResolveError::MissingRepositoryObject {
                    kind: "TreeEntry".to_string(),
                    repository_id: repository_id.to_string(),
                    id: format!("{tree_id}/{part}"),
                })?;

            let is_last = index + 1 == parts.len();
            if is_last {
                if entry_is_tree(entry) {
                    return Err(ClosureResolveError::InvalidRepositoryObject {
                        kind: "TreeEntry".to_string(),
                        id: format!("{tree_id}/{part}"),
                        reason: "expected blob entry, found tree".to_string(),
                    });
                }
                let blob = self
                    .blobs
                    .get(&(repository_id.to_string(), entry.object_sha.clone()))
                    .copied()
                    .ok_or_else(|| ClosureResolveError::MissingRepositoryObject {
                        kind: "Blob".to_string(),
                        repository_id: repository_id.to_string(),
                        id: entry.object_sha.clone(),
                    })?;
                return Ok(blob.content.clone());
            }

            if !entry_is_tree(entry) {
                return Err(ClosureResolveError::InvalidRepositoryObject {
                    kind: "TreeEntry".to_string(),
                    id: format!("{tree_id}/{part}"),
                    reason: "expected tree entry".to_string(),
                });
            }
            tree_id = entry.object_sha.clone();
        }

        Err(ClosureResolveError::InvalidRepositoryObject {
            kind: "TreeEntry".to_string(),
            id: root_tree.to_string(),
            reason: "empty path".to_string(),
        })
    }

    fn entries_for_tree(
        &self,
        repository_id: &str,
        tree_id: &str,
    ) -> Result<Vec<RegistryTreeEntry>, ClosureResolveError> {
        let key = (repository_id.to_string(), tree_id.to_string());
        if let Some(entries) = self.tree_entries.get(&key) {
            return Ok(entries.clone());
        }

        let tree = self.trees.get(&key).copied().ok_or_else(|| {
            ClosureResolveError::MissingRepositoryObject {
                kind: "Tree".to_string(),
                repository_id: repository_id.to_string(),
                id: tree_id.to_string(),
            }
        })?;
        let parsed = tg_canonical::parse_tree(&tree.body).map_err(|e| {
            ClosureResolveError::InvalidRepositoryObject {
                kind: "Tree".to_string(),
                id: tree_id.to_string(),
                reason: format!("parse tree body: {e}"),
            }
        })?;
        let mut entries = parsed
            .into_iter()
            .map(|entry| RegistryTreeEntry {
                tree_id: tree_id.to_string(),
                repository_id: repository_id.to_string(),
                path: entry.name,
                mode: entry.mode,
                object_sha: entry.sha,
                kind: if entry.is_tree { "Tree" } else { "Blob" }.to_string(),
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(entries)
    }
}

fn insert_unique_object<'a, T>(
    map: &mut BTreeMap<(String, String), &'a T>,
    kind: &str,
    repository_id: &str,
    id: &str,
    value: &'a T,
) -> Result<(), ClosureResolveError> {
    let key = (repository_id.to_string(), id.to_string());
    if map.insert(key, value).is_some() {
        return Err(ClosureResolveError::DuplicateRepositoryObject {
            kind: kind.to_string(),
            repository_id: repository_id.to_string(),
            id: id.to_string(),
        });
    }
    Ok(())
}

fn resolve_registry_app_toml_deps(
    app_key: &str,
    app_hash: &str,
    app_catalog: &RegistryCatalog<'_>,
    object_graph: &RepositoryObjectGraph<'_>,
    resolved: &mut BTreeMap<String, String>,
) -> Result<(), ClosureResolveError> {
    let app = app_catalog.get(app_key)?;
    let canonical_key = app.name.clone();
    if let Some(existing) = resolved.get(&canonical_key) {
        if existing == app_hash {
            return Ok(());
        }
        return Err(ClosureResolveError::ConflictingHash {
            name: canonical_key,
            first: existing.clone(),
            second: app_hash.to_string(),
        });
    }

    let commit_id = unpin_hash(app_hash);
    let commit = object_graph.commit(&app.repository_id, &commit_id)?;
    let manifest_bytes = object_graph
        .read_file(&app.repository_id, &commit.tree_sha, "app.toml")
        .map_err(|error| match error {
            ClosureResolveError::MissingRepositoryObject { .. } => {
                ClosureResolveError::MissingAppManifest {
                    app: app.id.clone(),
                    repository_id: app.repository_id.clone(),
                    commit: commit_id.clone(),
                }
            }
            other => other,
        })?;
    let manifest = app_manifest_from_bytes(&app.id, &manifest_bytes)?;
    if manifest.name != app.name {
        return Err(ClosureResolveError::InvalidRegistryMetadata {
            app: app.id.clone(),
            reason: format!(
                "app.toml name '{}' does not match registry App name '{}'",
                manifest.name, app.name
            ),
        });
    }

    resolved.insert(canonical_key, app_hash.to_string());
    for (dep_name, dep_hash) in manifest.deps {
        resolve_registry_app_toml_deps(&dep_name, &dep_hash, app_catalog, object_graph, resolved)?;
    }
    Ok(())
}

fn app_manifest_from_bytes(
    app: &str,
    bytes: &[u8],
) -> Result<LocalAppManifest, ClosureResolveError> {
    let source =
        std::str::from_utf8(bytes).map_err(|e| ClosureResolveError::InvalidRegistryMetadata {
            app: app.to_string(),
            reason: format!("app.toml is not utf-8: {e}"),
        })?;
    read_manifest_source(PathBuf::from(format!("{app}/app.toml")), source)
}

fn registry_deps_from_exports(
    app: &RegistryApp,
) -> Result<BTreeMap<String, String>, ClosureResolveError> {
    let exports = app.exports.trim();
    if exports.is_empty() {
        return Ok(BTreeMap::new());
    }
    let parsed = serde_json::from_str::<serde_json::Value>(exports).map_err(|e| {
        ClosureResolveError::InvalidRegistryMetadata {
            app: app.id.clone(),
            reason: format!("Exports must be JSON: {e}"),
        }
    })?;
    let Some(object) = parsed.as_object() else {
        return Err(ClosureResolveError::InvalidRegistryMetadata {
            app: app.id.clone(),
            reason: "Exports must be a JSON object".to_string(),
        });
    };
    let Some(deps) = object.get("deps").or_else(|| object.get("dependencies")) else {
        return Ok(BTreeMap::new());
    };
    let Some(deps_object) = deps.as_object() else {
        return Err(ClosureResolveError::InvalidRegistryMetadata {
            app: app.id.clone(),
            reason: "Exports deps/dependencies must be a JSON object".to_string(),
        });
    };

    let mut resolved = BTreeMap::new();
    for (name, value) in deps_object {
        let Some(hash) = value.as_str() else {
            return Err(ClosureResolveError::InvalidRegistryMetadata {
                app: app.id.clone(),
                reason: format!("dependency '{name}' must be a string pin"),
            });
        };
        if name.trim().is_empty() || hash.trim().is_empty() {
            return Err(ClosureResolveError::InvalidRegistryMetadata {
                app: app.id.clone(),
                reason: "dependency names and pins must be non-empty".to_string(),
            });
        }
        resolved.insert(name.clone(), normalize_hash_pin(hash));
    }
    Ok(resolved)
}

fn entry_is_tree(entry: &RegistryTreeEntry) -> bool {
    entry.kind.eq_ignore_ascii_case("Tree")
        || entry.kind.eq_ignore_ascii_case("tree")
        || entry.mode == "40000"
        || entry.mode == "040000"
}

fn parse_odata_collection<T>(
    body: &str,
    parse_row: fn(&serde_json::Value) -> Result<T, ClosureResolveError>,
) -> Result<Vec<T>, ClosureResolveError> {
    let parsed = serde_json::from_str::<serde_json::Value>(body)
        .map_err(|e| ClosureResolveError::InvalidRegistryResponse(e.to_string()))?;
    let rows = parsed
        .get("value")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            ClosureResolveError::InvalidRegistryResponse(
                "expected top-level JSON object with array field 'value'".to_string(),
            )
        })?;
    rows.iter().map(parse_row).collect()
}

fn decode_blob_content(value: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .unwrap_or_else(|_| value.as_bytes().to_vec())
}

fn decode_canonical_body(
    value: &str,
    kind: &str,
    id: &str,
) -> Result<Vec<u8>, ClosureResolveError> {
    let canonical = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|e| ClosureResolveError::InvalidRepositoryObject {
            kind: kind.to_string(),
            id: id.to_string(),
            reason: format!("CanonicalBytes must be base64: {e}"),
        })?;
    let nul = canonical
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(|| ClosureResolveError::InvalidRepositoryObject {
            kind: kind.to_string(),
            id: id.to_string(),
            reason: "CanonicalBytes missing git object header terminator".to_string(),
        })?;
    let expected_prefix = format!("{} ", kind.to_ascii_lowercase());
    let header = std::str::from_utf8(&canonical[..nul]).map_err(|e| {
        ClosureResolveError::InvalidRepositoryObject {
            kind: kind.to_string(),
            id: id.to_string(),
            reason: format!("CanonicalBytes header is not UTF-8: {e}"),
        }
    })?;
    if !header.starts_with(&expected_prefix) {
        return Err(ClosureResolveError::InvalidRepositoryObject {
            kind: kind.to_string(),
            id: id.to_string(),
            reason: format!("CanonicalBytes header must start with '{expected_prefix}'"),
        });
    }
    Ok(canonical[nul + 1..].to_vec())
}

fn normalize_hash_pin(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('@') {
        value.to_string()
    } else {
        format!("@{value}")
    }
}

fn unpin_hash(value: &str) -> String {
    value
        .trim()
        .strip_prefix('@')
        .unwrap_or(value.trim())
        .to_string()
}

fn registry_row_string(row: &serde_json::Value, key: &str) -> Option<String> {
    registry_row_string_any(row, &[key])
}

fn registry_row_string_any(row: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let fields = row.get("fields");
    for key in keys {
        if *key == "Id" {
            if let Some(value) = row.get("entity_id").and_then(serde_json::Value::as_str) {
                return Some(value.to_string());
            }
        }
        for source in [Some(row), fields] {
            if let Some(value) = source.and_then(|source| source.get(*key)) {
                if let Some(value) = value.as_str() {
                    return Some(value.to_string());
                }
                if value.is_number() || value.is_boolean() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn registry_row_label(row: &serde_json::Value) -> String {
    registry_row_string(row, "Id").unwrap_or_else(|| "<unknown>".to_string())
}

fn push_field(bytes: &mut Vec<u8>, name: &str, value: &str) {
    bytes.extend_from_slice(name.as_bytes());
    bytes.push(0);
    push_len_prefixed(bytes, value);
}

fn push_resolved(bytes: &mut Vec<u8>, resolved: &BTreeMap<String, String>) {
    bytes.extend_from_slice(b"resolved\0");
    bytes.extend_from_slice(resolved.len().to_string().as_bytes());
    bytes.push(0);
    for (name, hash) in resolved {
        push_len_prefixed(bytes, name);
        push_len_prefixed(bytes, hash);
    }
}

fn push_len_prefixed(bytes: &mut Vec<u8>, value: &str) {
    bytes.extend_from_slice(value.len().to_string().as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(name, hash)| ((*name).to_string(), (*hash).to_string()))
            .collect()
    }

    #[test]
    fn closure_id_is_stable_for_sorted_inputs() {
        let a = resolved(&[
            ("paw-heal", "@7a3f8e2c"),
            ("temper-git", "@b21c4f8a"),
            ("user-app", "@e44a7c2b"),
        ]);
        let b = resolved(&[
            ("user-app", "@e44a7c2b"),
            ("paw-heal", "@7a3f8e2c"),
            ("temper-git", "@b21c4f8a"),
        ]);

        assert_eq!(
            closure_id("paw-heal@7a3f8e2c", &a, DEFAULT_RESOLVER_VERSION),
            closure_id("paw-heal@7a3f8e2c", &b, DEFAULT_RESOLVER_VERSION)
        );
    }

    #[test]
    fn closure_id_changes_when_semantic_inputs_change() {
        let base = resolved(&[("temper-git", "@b21c4f8a")]);
        let different_dep = resolved(&[("temper-git", "@e44a7c2b")]);

        let base_id = closure_id("temper-git@b21c4f8a", &base, DEFAULT_RESOLVER_VERSION);
        assert_ne!(
            base_id,
            closure_id("temper-git@e44a7c2b", &base, DEFAULT_RESOLVER_VERSION)
        );
        assert_ne!(
            base_id,
            closure_id(
                "temper-git@b21c4f8a",
                &different_dep,
                DEFAULT_RESOLVER_VERSION
            )
        );
        assert_ne!(base_id, closure_id("temper-git@b21c4f8a", &base, "2.0"));
    }

    #[test]
    fn resolved_json_is_canonical_for_closure_rows() {
        let resolved = resolved(&[("zeta", "@z"), ("alpha", "@a"), ("temper-git", "@b21c4f8a")]);

        assert_eq!(
            resolved_json(&resolved),
            r#"{"alpha":"@a","temper-git":"@b21c4f8a","zeta":"@z"}"#
        );
    }

    #[test]
    fn local_closure_resolves_transitive_locked_deps() {
        let root = temp_app_root("transitive");
        write_app(
            &root,
            "paw-ui",
            r#"
name = "paw-ui"

[deps]
paw-heal = "@heal"
temper-git = "@git"

[deps.hints]
temper-git = "^1.0"
"#,
        );
        write_app(
            &root,
            "paw-heal",
            r#"
name = "paw-heal"

[deps]
temper-git = "@git"
"#,
        );
        write_app(&root, "temper-git", "name = \"temper-git\"\n");

        let closure = resolve_local_closure("paw-ui@ui", &[root.clone()])
            .expect("local closure should resolve");

        assert_eq!(closure.root, "paw-ui@ui");
        assert_eq!(closure.resolver_version, DEFAULT_RESOLVER_VERSION);
        assert_eq!(
            closure.resolved,
            resolved(&[
                ("paw-ui", "@ui"),
                ("paw-heal", "@heal"),
                ("temper-git", "@git"),
            ])
        );
        assert_eq!(
            closure.id,
            closure_id(&closure.root, &closure.resolved, DEFAULT_RESOLVER_VERSION)
        );
        assert_eq!(
            closure.bootstrap_manifest(),
            format!("closure = \"{}\"\n", closure.id)
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_closure_fails_when_a_dep_manifest_is_missing() {
        let root = temp_app_root("missing");
        write_app(
            &root,
            "paw-ui",
            r#"
name = "paw-ui"

[deps]
temper-git = "@git"
"#,
        );

        let error = resolve_local_closure("paw-ui@ui", &[root.clone()])
            .expect_err("missing dependency manifest should fail");
        assert!(matches!(
            error,
            ClosureResolveError::MissingApp { ref name } if name == "temper-git"
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn local_closure_rejects_conflicting_dep_hashes() {
        let root = temp_app_root("conflict");
        write_app(
            &root,
            "root-app",
            r#"
name = "root-app"

[deps]
left = "@left"
right = "@right"
"#,
        );
        write_app(
            &root,
            "left",
            r#"
name = "left"

[deps]
temper-git = "@git-a"
"#,
        );
        write_app(
            &root,
            "right",
            r#"
name = "right"

[deps]
temper-git = "@git-b"
"#,
        );
        write_app(&root, "temper-git", "name = \"temper-git\"\n");

        let error = resolve_local_closure("root-app@root", &[root.clone()])
            .expect_err("conflicting dependency hashes should fail");
        assert!(matches!(
            error,
            ClosureResolveError::ConflictingHash { ref name, .. } if name == "temper-git"
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn registry_closure_resolves_exports_deps() {
        let apps = vec![
            RegistryApp {
                id: "app-paw-ui".to_string(),
                name: "paw-ui".to_string(),
                repository_id: "repo-paw-ui".to_string(),
                latest_version_hash: "ui".to_string(),
                exports: r#"{"deps":{"paw-heal":"heal","temper-git":"@git"}}"#.to_string(),
            },
            RegistryApp {
                id: "app-paw-heal".to_string(),
                name: "paw-heal".to_string(),
                repository_id: "repo-paw-heal".to_string(),
                latest_version_hash: "@heal".to_string(),
                exports: r#"{"dependencies":{"temper-git":"git"}}"#.to_string(),
            },
            RegistryApp {
                id: "app-temper-git".to_string(),
                name: "temper-git".to_string(),
                repository_id: "repo-temper-git".to_string(),
                latest_version_hash: "@git".to_string(),
                exports: r#"{"entities":["Repository"]}"#.to_string(),
            },
        ];

        let closure =
            resolve_registry_closure("paw-ui@ui", &apps).expect("registry closure should resolve");

        assert_eq!(closure.root, "paw-ui@ui");
        assert_eq!(
            closure.resolved,
            resolved(&[
                ("paw-ui", "@ui"),
                ("paw-heal", "@heal"),
                ("temper-git", "@git"),
            ])
        );
        assert_eq!(
            closure.id,
            closure_id(&closure.root, &closure.resolved, DEFAULT_RESOLVER_VERSION)
        );
    }

    #[test]
    fn registry_closure_fails_when_requested_hash_is_not_current() {
        let apps = vec![RegistryApp {
            id: "app-paw-ui".to_string(),
            name: "paw-ui".to_string(),
            repository_id: "repo-paw-ui".to_string(),
            latest_version_hash: "@new".to_string(),
            exports: "{}".to_string(),
        }];

        let error = resolve_registry_closure("paw-ui@old", &apps)
            .expect_err("stale registry pin should fail closed");

        assert!(matches!(
            error,
            ClosureResolveError::RegistryHashMismatch { ref app, .. }
                if app == "app-paw-ui"
        ));
    }

    #[test]
    fn registry_closure_fails_on_ambiguous_names() {
        let apps = vec![
            RegistryApp {
                id: "app-alice-notes".to_string(),
                name: "notes".to_string(),
                repository_id: "repo-alice-notes".to_string(),
                latest_version_hash: "@a".to_string(),
                exports: "{}".to_string(),
            },
            RegistryApp {
                id: "app-bob-notes".to_string(),
                name: "notes".to_string(),
                repository_id: "repo-bob-notes".to_string(),
                latest_version_hash: "@b".to_string(),
                exports: "{}".to_string(),
            },
        ];

        let error = resolve_registry_closure("notes@a", &apps)
            .expect_err("ambiguous registry app name should fail closed");

        assert!(matches!(
            error,
            ClosureResolveError::AmbiguousRegistryApp { ref key, .. } if key == "notes"
        ));
    }

    #[test]
    fn registry_app_toml_closure_reads_historical_commit_deps() {
        let apps = vec![
            RegistryApp {
                id: "app-paw-ui".to_string(),
                name: "paw-ui".to_string(),
                repository_id: "repo-paw-ui".to_string(),
                latest_version_hash: "@newer-ui".to_string(),
                exports: "{}".to_string(),
            },
            RegistryApp {
                id: "app-paw-heal".to_string(),
                name: "paw-heal".to_string(),
                repository_id: "repo-paw-heal".to_string(),
                latest_version_hash: "@newer-heal".to_string(),
                exports: "{}".to_string(),
            },
            RegistryApp {
                id: "app-temper-git".to_string(),
                name: "temper-git".to_string(),
                repository_id: "repo-temper-git".to_string(),
                latest_version_hash: "@newer-git".to_string(),
                exports: "{}".to_string(),
            },
        ];
        let commits = vec![
            commit("repo-paw-ui", "old-ui", "tree-ui-old"),
            commit("repo-paw-heal", "old-heal", "tree-heal-old"),
            commit("repo-temper-git", "old-git", "tree-git-old"),
        ];
        let tree_entries = vec![
            app_toml_entry("repo-paw-ui", "tree-ui-old", "blob-ui-old"),
            app_toml_entry("repo-paw-heal", "tree-heal-old", "blob-heal-old"),
            app_toml_entry("repo-temper-git", "tree-git-old", "blob-git-old"),
        ];
        let blobs = vec![
            blob(
                "repo-paw-ui",
                "blob-ui-old",
                r#"
name = "paw-ui"

[deps]
paw-heal = "@old-heal"
temper-git = "@old-git"
"#,
            ),
            blob(
                "repo-paw-heal",
                "blob-heal-old",
                r#"
name = "paw-heal"

[deps]
temper-git = "@old-git"
"#,
            ),
            blob("repo-temper-git", "blob-git-old", "name = \"temper-git\"\n"),
        ];

        let closure = resolve_registry_app_toml_closure(
            "paw-ui@old-ui",
            &apps,
            &commits,
            &tree_entries,
            &blobs,
        )
        .expect("historical app.toml closure should resolve");

        assert_eq!(
            closure.resolved,
            resolved(&[
                ("paw-ui", "@old-ui"),
                ("paw-heal", "@old-heal"),
                ("temper-git", "@old-git"),
            ])
        );
    }

    #[test]
    fn registry_app_toml_closure_resolves_large_historical_graph_deterministically() {
        const APP_COUNT: usize = 128;

        let mut apps = Vec::new();
        let mut commits = Vec::new();
        let mut tree_entries = Vec::new();
        let mut blobs = Vec::new();
        for index in 0..APP_COUNT {
            let name = numbered_app(index);
            let repository_id = numbered_repo(index);
            let hash = numbered_hash(index);
            let tree_id = numbered_tree(index);
            let blob_id = numbered_blob(index);

            apps.push(RegistryApp {
                id: format!("registry-{name}"),
                name: name.clone(),
                repository_id: repository_id.clone(),
                latest_version_hash: format!("@newer-{index:03}"),
                exports: "{}".to_string(),
            });
            commits.push(commit(&repository_id, &hash, &tree_id));
            tree_entries.push(app_toml_entry(&repository_id, &tree_id, &blob_id));
            blobs.push(blob(
                &repository_id,
                &blob_id,
                &numbered_manifest(index, APP_COUNT),
            ));
        }

        let root_ref = format!("{}@{}", numbered_app(0), numbered_hash(0));
        let closure =
            resolve_registry_app_toml_closure(&root_ref, &apps, &commits, &tree_entries, &blobs)
                .expect("large historical registry closure should resolve");

        let mut reversed_apps = apps.clone();
        let mut reversed_commits = commits.clone();
        let mut reversed_tree_entries = tree_entries.clone();
        let mut reversed_blobs = blobs.clone();
        reversed_apps.reverse();
        reversed_commits.reverse();
        reversed_tree_entries.reverse();
        reversed_blobs.reverse();
        let reversed = resolve_registry_app_toml_closure(
            &root_ref,
            &reversed_apps,
            &reversed_commits,
            &reversed_tree_entries,
            &reversed_blobs,
        )
        .expect("row order should not affect historical registry closure resolution");

        assert_eq!(closure.id, reversed.id);
        assert_eq!(closure.resolved, reversed.resolved);
        assert_eq!(closure.resolved.len(), APP_COUNT);
        assert_eq!(
            closure.resolved.get(&numbered_app(0)),
            Some(&"@hash-000".to_string())
        );
        assert_eq!(
            closure.resolved.get(&numbered_app(APP_COUNT - 1)),
            Some(&format!("@{}", numbered_hash(APP_COUNT - 1)))
        );
        assert_eq!(
            closure.id,
            closure_id(&closure.root, &closure.resolved, DEFAULT_RESOLVER_VERSION)
        );
    }

    #[test]
    fn registry_app_toml_closure_reads_real_tree_canonical_bytes() {
        let repository_id = "repo-paw-ui";
        let blob_id = "1111111111111111111111111111111111111111";
        let tree = tree_with_app_toml(repository_id, blob_id);
        let apps = vec![RegistryApp {
            id: "app-paw-ui".to_string(),
            name: "paw-ui".to_string(),
            repository_id: repository_id.to_string(),
            latest_version_hash: "@old-ui".to_string(),
            exports: "{}".to_string(),
        }];
        let commits = vec![commit(repository_id, "old-ui", &tree.id)];
        let blobs = vec![blob(repository_id, blob_id, "name = \"paw-ui\"\n")];

        let closure = resolve_registry_app_toml_closure_with_trees(
            "paw-ui@old-ui",
            &apps,
            &commits,
            &[tree],
            &[],
            &blobs,
        )
        .expect("canonical Tree rows from real pack ingest should resolve");

        assert_eq!(closure.resolved, resolved(&[("paw-ui", "@old-ui")]));
    }

    #[test]
    fn registry_app_toml_closure_fails_when_manifest_missing() {
        let apps = vec![RegistryApp {
            id: "app-paw-ui".to_string(),
            name: "paw-ui".to_string(),
            repository_id: "repo-paw-ui".to_string(),
            latest_version_hash: "@old-ui".to_string(),
            exports: "{}".to_string(),
        }];
        let commits = vec![commit("repo-paw-ui", "old-ui", "tree-ui-old")];

        let error = resolve_registry_app_toml_closure("paw-ui@old-ui", &apps, &commits, &[], &[])
            .expect_err("missing app.toml should fail closed");

        assert!(matches!(
            error,
            ClosureResolveError::MissingAppManifest { ref app, .. }
                if app == "app-paw-ui"
        ));
    }

    #[test]
    fn registry_apps_parse_odata_rows() {
        let apps = registry_apps_from_odata_json(
            r#"{
              "value": [
                {
                  "entity_id": "ap-platform-install",
                  "fields": {
                    "Status": "Active"
                  }
                },
                {
                  "entity_id": "app-paw-ui",
                  "fields": {
                    "Name": "paw-ui",
                    "RepositoryId": "repo-paw-ui",
                    "LatestVersionHash": "ui",
                    "Exports": "{\"deps\":{\"temper-git\":\"git\"}}"
                  }
                }
              ]
            }"#,
        )
        .expect("odata apps should parse");

        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].id, "app-paw-ui");
        assert_eq!(apps[0].name, "paw-ui");
        assert_eq!(apps[0].repository_id, "repo-paw-ui");
        assert_eq!(apps[0].latest_version_hash, "ui");
    }

    fn temp_app_root(label: &str) -> PathBuf {
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "genesis-registry-{label}-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("temp app root should be created");
        root
    }

    fn write_app(root: &Path, dir_name: &str, manifest: &str) {
        let app_dir = root.join(dir_name);
        std::fs::create_dir_all(&app_dir).expect("app dir should be created");
        std::fs::write(app_dir.join("app.toml"), manifest).expect("manifest should be written");
    }

    fn commit(repository_id: &str, id: &str, tree_sha: &str) -> RegistryCommit {
        RegistryCommit {
            id: id.to_string(),
            repository_id: repository_id.to_string(),
            tree_sha: tree_sha.to_string(),
        }
    }

    fn app_toml_entry(repository_id: &str, tree_id: &str, object_sha: &str) -> RegistryTreeEntry {
        RegistryTreeEntry {
            tree_id: tree_id.to_string(),
            repository_id: repository_id.to_string(),
            path: "app.toml".to_string(),
            mode: "100644".to_string(),
            object_sha: object_sha.to_string(),
            kind: "Blob".to_string(),
        }
    }

    fn tree_with_app_toml(repository_id: &str, object_sha: &str) -> RegistryTree {
        let entries = vec![tg_canonical::TreeEntry {
            mode: tg_canonical::Mode::RegularFile,
            name: b"app.toml".to_vec(),
            object_sha: object_sha.to_string(),
        }];
        let id = tg_canonical::tree_hash(entries.clone());
        let canonical = tg_canonical::tree_canonical_bytes(entries);
        let body_start = canonical
            .iter()
            .position(|&byte| byte == 0)
            .expect("canonical tree has a git object header")
            + 1;
        RegistryTree {
            id,
            repository_id: repository_id.to_string(),
            body: canonical[body_start..].to_vec(),
        }
    }

    fn blob(repository_id: &str, id: &str, content: &str) -> RegistryBlob {
        RegistryBlob {
            id: id.to_string(),
            repository_id: repository_id.to_string(),
            content: content.as_bytes().to_vec(),
        }
    }

    fn numbered_app(index: usize) -> String {
        format!("app-{index:03}")
    }

    fn numbered_repo(index: usize) -> String {
        format!("repo-{index:03}")
    }

    fn numbered_hash(index: usize) -> String {
        format!("hash-{index:03}")
    }

    fn numbered_tree(index: usize) -> String {
        format!("tree-{index:03}")
    }

    fn numbered_blob(index: usize) -> String {
        format!("blob-{index:03}")
    }

    fn numbered_manifest(index: usize, app_count: usize) -> String {
        let mut manifest = format!("name = \"{}\"\n", numbered_app(index));
        let deps = [index + 1, index + 2]
            .into_iter()
            .filter(|dep| *dep < app_count)
            .collect::<Vec<_>>();
        if !deps.is_empty() {
            manifest.push_str("\n[deps]\n");
            for dep in deps {
                manifest.push_str(&format!(
                    "{} = \"@{}\"\n",
                    numbered_app(dep),
                    numbered_hash(dep)
                ));
            }
        }
        manifest
    }
}
