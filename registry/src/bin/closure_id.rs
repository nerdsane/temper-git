use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use genesis_registry::{
    DEFAULT_RESOLVER_VERSION, closure_id, registry_apps_from_odata_json,
    registry_blobs_from_odata_json, registry_commits_from_odata_json,
    registry_tree_entries_from_odata_json, registry_trees_from_odata_json, resolve_local_closure,
    resolve_registry_app_toml_closure_with_trees, resolve_registry_closure, resolved_json,
};

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg| arg == "resolve-local") {
        run_resolve_local(&args[1..]);
        return;
    }
    if args.first().is_some_and(|arg| arg == "resolve-registry") {
        run_resolve_registry(&args[1..]);
        return;
    }
    if args
        .first()
        .is_some_and(|arg| arg == "resolve-registry-version")
    {
        run_resolve_registry_version(&args[1..]);
        return;
    }

    let Some(root) = args.first() else {
        usage_and_exit();
    };
    let remaining = &args[1..];
    let (resolver_version, dependencies) = match remaining.first() {
        Some(first) if first.contains('=') => (DEFAULT_RESOLVER_VERSION.to_string(), remaining),
        Some(first) => (first.clone(), &remaining[1..]),
        None => {
            eprintln!("at least one resolved dependency is required");
            std::process::exit(2);
        }
    };

    let mut resolved = BTreeMap::new();
    for arg in dependencies {
        let Some((name, hash)) = arg.split_once('=') else {
            eprintln!("invalid dependency '{arg}', expected name=hash");
            std::process::exit(2);
        };
        if name.is_empty() || hash.is_empty() {
            eprintln!("invalid dependency '{arg}', expected non-empty name and hash");
            std::process::exit(2);
        }
        resolved.insert(name.to_string(), hash.to_string());
    }
    if resolved.is_empty() {
        eprintln!("at least one resolved dependency is required");
        std::process::exit(2);
    }

    println!("{}", closure_id(&root, &resolved, &resolver_version));
    println!("{}", resolved_json(&resolved));
}

fn run_resolve_local(args: &[String]) {
    if args.len() < 2 {
        usage_and_exit();
    }
    let root = &args[0];
    let app_dirs = args[1..].iter().map(PathBuf::from).collect::<Vec<_>>();
    let closure = match resolve_local_closure(root, &app_dirs) {
        Ok(closure) => closure,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };

    println!("{}", closure.id);
    println!("{}", resolved_json(&closure.resolved));
    print!("{}", closure.bootstrap_manifest());
}

fn run_resolve_registry(args: &[String]) {
    if args.len() < 2 {
        usage_and_exit();
    }
    let root = &args[0];
    let registry_base = &args[1];
    let options = match RegistryOptions::parse(&args[2..]) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            usage_and_exit();
        }
    };

    let body = match http_get(&apps_url(registry_base), &options.headers()) {
        Ok(body) => body,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let apps = match registry_apps_from_odata_json(&body) {
        Ok(apps) => apps,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let closure = match resolve_registry_closure(root, &apps) {
        Ok(closure) => closure,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };

    println!("{}", closure.id);
    println!("{}", resolved_json(&closure.resolved));
    print!("{}", closure.bootstrap_manifest());
}

fn run_resolve_registry_version(args: &[String]) {
    if args.len() < 2 {
        usage_and_exit();
    }
    let root = &args[0];
    let registry_base = &args[1];
    let options = match RegistryOptions::parse(&args[2..]) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("{error}");
            usage_and_exit();
        }
    };
    let headers = options.headers();

    let apps = match fetch_collection(
        registry_base,
        "Apps",
        &headers,
        registry_apps_from_odata_json,
    ) {
        Ok(apps) => apps,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let commits = match fetch_collection(
        registry_base,
        "Commits",
        &headers,
        registry_commits_from_odata_json,
    ) {
        Ok(commits) => commits,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let trees = match fetch_collection(
        registry_base,
        "Trees",
        &headers,
        registry_trees_from_odata_json,
    ) {
        Ok(trees) => trees,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let tree_entries = match fetch_collection(
        registry_base,
        "TreeEntries",
        &headers,
        registry_tree_entries_from_odata_json,
    ) {
        Ok(tree_entries) => tree_entries,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    let blobs = match fetch_collection(
        registry_base,
        "Blobs",
        &headers,
        registry_blobs_from_odata_json,
    ) {
        Ok(blobs) => blobs,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };

    let closure = match resolve_registry_app_toml_closure_with_trees(
        root,
        &apps,
        &commits,
        &trees,
        &tree_entries,
        &blobs,
    ) {
        Ok(closure) => closure,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };

    println!("{}", closure.id);
    println!("{}", resolved_json(&closure.resolved));
    print!("{}", closure.bootstrap_manifest());
}

fn fetch_collection<T>(
    registry_base: &str,
    collection: &str,
    headers: &[(String, String)],
    parse: fn(&str) -> Result<Vec<T>, genesis_registry::ClosureResolveError>,
) -> Result<Vec<T>, String> {
    let body = http_get(&collection_url(registry_base, collection), headers)?;
    parse(&body).map_err(|error| error.to_string())
}

#[derive(Default)]
struct RegistryOptions {
    tenant: Option<String>,
    principal_id: Option<String>,
    principal_kind: Option<String>,
}

impl RegistryOptions {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = RegistryOptions::default();
        let mut index = 0;
        while index < args.len() {
            let flag = &args[index];
            let Some(value) = args.get(index + 1) else {
                return Err(format!("{flag} requires a value"));
            };
            match flag.as_str() {
                "--tenant" => options.tenant = Some(value.clone()),
                "--principal-id" => options.principal_id = Some(value.clone()),
                "--principal-kind" => options.principal_kind = Some(value.clone()),
                other => return Err(format!("unknown option '{other}'")),
            }
            index += 2;
        }
        Ok(options)
    }

    fn headers(&self) -> Vec<(String, String)> {
        let mut headers = vec![("Accept".to_string(), "application/json".to_string())];
        if let Some(tenant) = &self.tenant {
            headers.push(("X-Tenant-Id".to_string(), tenant.clone()));
        }
        if let Some(principal_id) = &self.principal_id {
            headers.push(("X-Temper-Principal-Id".to_string(), principal_id.clone()));
        }
        if let Some(principal_kind) = &self.principal_kind {
            headers.push((
                "X-Temper-Principal-Kind".to_string(),
                principal_kind.clone(),
            ));
        }
        headers
    }
}

fn apps_url(registry_base: &str) -> String {
    collection_url(registry_base, "Apps")
}

fn collection_url(registry_base: &str, collection: &str) -> String {
    let base = registry_base.trim_end_matches('/');
    if base.ends_with("/tdata") {
        format!("{base}/{collection}")
    } else {
        format!("{base}/tdata/{collection}")
    }
}

fn http_get(url: &str, headers: &[(String, String)]) -> Result<String, String> {
    let parsed = ParsedHttpUrl::parse(url)?;
    let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port))
        .map_err(|e| format!("connect {}:{}: {e}", parsed.host, parsed.port))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("set read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| format!("set write timeout: {e}"))?;

    let mut request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        parsed.path, parsed.host_header
    );
    for (name, value) in headers {
        request.push_str(name);
        request.push_str(": ");
        request.push_str(value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("write request: {e}"))?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| format!("read response: {e}"))?;
    parse_http_response(&response)
}

struct ParsedHttpUrl {
    host: String,
    host_header: String,
    port: u16,
    path: String,
}

impl ParsedHttpUrl {
    fn parse(url: &str) -> Result<Self, String> {
        let Some(rest) = url.strip_prefix("http://") else {
            return Err("resolve-registry currently supports http:// registry URLs".to_string());
        };
        let (authority, path) = match rest.split_once('/') {
            Some((authority, path)) => (authority, format!("/{path}")),
            None => (rest, "/".to_string()),
        };
        if authority.is_empty() {
            return Err("http URL is missing a host".to_string());
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() => {
                let port = port
                    .parse::<u16>()
                    .map_err(|e| format!("invalid URL port '{port}': {e}"))?;
                (host.to_string(), port)
            }
            _ => (authority.to_string(), 80),
        };

        Ok(Self {
            host,
            host_header: authority.to_string(),
            port,
            path,
        })
    }
}

fn parse_http_response(response: &[u8]) -> Result<String, String> {
    let raw = String::from_utf8_lossy(response);
    let Some((head, body)) = raw.split_once("\r\n\r\n") else {
        return Err("HTTP response missing header/body separator".to_string());
    };
    let status_line = head
        .lines()
        .next()
        .ok_or_else(|| "HTTP response missing status line".to_string())?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("invalid HTTP status line '{status_line}'"))?
        .parse::<u16>()
        .map_err(|e| format!("invalid HTTP status code in '{status_line}': {e}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("registry GET failed with HTTP {status}: {body}"));
    }
    Ok(body.to_string())
}

fn usage_and_exit() -> ! {
    eprintln!("usage:");
    eprintln!("  closure-id <root> [resolver_version] <name=hash>...");
    eprintln!("  closure-id resolve-local <root@hash> <app-dir-or-parent-dir>...");
    eprintln!(
        "  closure-id resolve-registry <root@hash> <registry-base-url> [--tenant TENANT] [--principal-id ID] [--principal-kind KIND]"
    );
    eprintln!(
        "  closure-id resolve-registry-version <root@hash> <registry-base-url> [--tenant TENANT] [--principal-id ID] [--principal-kind KIND]"
    );
    std::process::exit(2);
}
