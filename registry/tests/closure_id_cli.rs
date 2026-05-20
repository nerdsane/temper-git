use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use base64::Engine as _;

fn closure_id(args: &[&str]) -> Vec<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_closure-id"))
        .args(args)
        .output()
        .expect("closure-id exits");
    assert!(
        output.status.success(),
        "closure-id failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("stdout is utf-8")
        .lines()
        .map(str::to_string)
        .collect()
}

fn closure_id_failure(args: &[&str]) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_closure-id"))
        .args(args)
        .output()
        .expect("closure-id exits");
    assert!(
        !output.status.success(),
        "closure-id should have failed, stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).expect("stderr is utf-8")
}

#[test]
fn defaults_resolver_version_when_dependencies_start_immediately() {
    let defaulted = closure_id(&["temper-git@b21c4f8a", "temper-git=@b21c4f8a"]);
    let explicit = closure_id(&["temper-git@b21c4f8a", "1.0", "temper-git=@b21c4f8a"]);

    assert_eq!(defaulted, explicit);
    assert!(defaulted[0].starts_with("cl-"));
    assert_eq!(defaulted[1], r#"{"temper-git":"@b21c4f8a"}"#);
}

#[test]
fn dependency_order_does_not_change_output() {
    let first = closure_id(&[
        "temper-git@b21c4f8a",
        "temper-git=@b21c4f8a",
        "temper-genesis=@7a3f8e2c",
    ]);
    let second = closure_id(&[
        "temper-git@b21c4f8a",
        "temper-genesis=@7a3f8e2c",
        "temper-git=@b21c4f8a",
    ]);

    assert_eq!(first, second);
    assert_eq!(
        first[1],
        r#"{"temper-genesis":"@7a3f8e2c","temper-git":"@b21c4f8a"}"#
    );
}

#[test]
fn resolve_local_walks_manifest_deps_and_prints_bootstrap_manifest() {
    let root = temp_app_root("cli-resolve");
    write_app(
        &root,
        "paw-ui",
        r#"
name = "paw-ui"

[deps]
paw-heal = "@heal"
temper-git = "@git"
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

    let output = closure_id(&["resolve-local", "paw-ui@ui", root.to_str().unwrap()]);

    assert_eq!(output.len(), 3);
    assert!(output[0].starts_with("cl-"));
    assert_eq!(
        output[1],
        r#"{"paw-heal":"@heal","paw-ui":"@ui","temper-git":"@git"}"#
    );
    assert_eq!(output[2], format!("closure = \"{}\"", output[0]));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn resolve_local_fails_closed_on_missing_transitive_manifest() {
    let root = temp_app_root("cli-missing");
    write_app(
        &root,
        "paw-ui",
        r#"
name = "paw-ui"

[deps]
temper-git = "@git"
"#,
    );

    let error = closure_id_failure(&["resolve-local", "paw-ui@ui", root.to_str().unwrap()]);

    assert!(error.contains("missing local app manifest"));
    assert!(error.contains("temper-git"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn resolve_registry_fetches_apps_and_prints_bootstrap_manifest() {
    let server = single_response_server(
        r#"{
          "value": [
            {
              "entity_id": "app-paw-ui",
              "fields": {
                "Name": "paw-ui",
                "RepositoryId": "repo-paw-ui",
                "LatestVersionHash": "ui",
                "Exports": "{\"deps\":{\"paw-heal\":\"heal\",\"temper-git\":\"git\"}}"
              }
            },
            {
              "entity_id": "app-paw-heal",
              "fields": {
                "Name": "paw-heal",
                "RepositoryId": "repo-paw-heal",
                "LatestVersionHash": "heal",
                "Exports": "{\"dependencies\":{\"temper-git\":\"git\"}}"
              }
            },
            {
              "entity_id": "app-temper-git",
              "fields": {
                "Name": "temper-git",
                "RepositoryId": "repo-temper-git",
                "LatestVersionHash": "git",
                "Exports": "{}"
              }
            }
          ]
        }"#,
    );

    let output = closure_id(&[
        "resolve-registry",
        "paw-ui@ui",
        &format!("http://{}", server.addr),
        "--tenant",
        "test-tenant",
    ]);
    server.join();

    assert_eq!(output.len(), 3);
    assert!(output[0].starts_with("cl-"));
    assert_eq!(
        output[1],
        r#"{"paw-heal":"@heal","paw-ui":"@ui","temper-git":"@git"}"#
    );
    assert_eq!(output[2], format!("closure = \"{}\"", output[0]));
}

#[test]
fn resolve_registry_version_fetches_app_toml_objects_and_prints_bootstrap_manifest() {
    let apps = r#"{
      "value": [
        {
          "entity_id": "app-paw-ui",
          "fields": {
            "Name": "paw-ui",
            "RepositoryId": "repo-paw-ui",
            "LatestVersionHash": "new-ui",
            "Exports": "{}"
          }
        },
        {
          "entity_id": "app-paw-heal",
          "fields": {
            "Name": "paw-heal",
            "RepositoryId": "repo-paw-heal",
            "LatestVersionHash": "new-heal",
            "Exports": "{}"
          }
        },
        {
          "entity_id": "app-temper-git",
          "fields": {
            "Name": "temper-git",
            "RepositoryId": "repo-temper-git",
            "LatestVersionHash": "new-git",
            "Exports": "{}"
          }
        }
      ]
    }"#;
    let commits = r#"{
      "value": [
        {"entity_id":"old-ui","fields":{"repository_id":"repo-paw-ui","tree_sha":"tree-ui"}},
        {"entity_id":"old-heal","fields":{"repository_id":"repo-paw-heal","tree_sha":"tree-heal"}},
        {"entity_id":"old-git","fields":{"repository_id":"repo-temper-git","tree_sha":"tree-git"}}
      ]
    }"#;
    let tree_entries = r#"{
      "value": [
        {"entity_id":"te-ui","fields":{"tree_id":"tree-ui","repository_id":"repo-paw-ui","path":"app.toml","mode":"100644","object_sha":"blob-ui","kind":"Blob"}},
        {"entity_id":"te-heal","fields":{"tree_id":"tree-heal","repository_id":"repo-paw-heal","path":"app.toml","mode":"100644","object_sha":"blob-heal","kind":"Blob"}},
        {"entity_id":"te-git","fields":{"tree_id":"tree-git","repository_id":"repo-temper-git","path":"app.toml","mode":"100644","object_sha":"blob-git","kind":"Blob"}}
      ]
    }"#;
    let blobs = format!(
        r#"{{
          "value": [
            {{"entity_id":"blob-ui","fields":{{"repository_id":"repo-paw-ui","content":"{}"}}}},
            {{"entity_id":"blob-heal","fields":{{"repository_id":"repo-paw-heal","content":"{}"}}}},
            {{"entity_id":"blob-git","fields":{{"repository_id":"repo-temper-git","content":"{}"}}}}
          ]
        }}"#,
        b64(r#"
name = "paw-ui"

[deps]
paw-heal = "@old-heal"
temper-git = "@old-git"
"#),
        b64(r#"
name = "paw-heal"

[deps]
temper-git = "@old-git"
"#),
        b64("name = \"temper-git\"\n")
    );
    let server = response_server(vec![
        ("/tdata/Apps", apps.to_string()),
        ("/tdata/Commits", commits.to_string()),
        ("/tdata/Trees", r#"{"value":[]}"#.to_string()),
        ("/tdata/TreeEntries", tree_entries.to_string()),
        ("/tdata/Blobs", blobs),
    ]);

    let output = closure_id(&[
        "resolve-registry-version",
        "paw-ui@old-ui",
        &format!("http://{}", server.addr),
        "--tenant",
        "test-tenant",
    ]);
    server.join();

    assert_eq!(output.len(), 3);
    assert!(output[0].starts_with("cl-"));
    assert_eq!(
        output[1],
        r#"{"paw-heal":"@old-heal","paw-ui":"@old-ui","temper-git":"@old-git"}"#
    );
    assert_eq!(output[2], format!("closure = \"{}\"", output[0]));
}

fn temp_app_root(label: &str) -> PathBuf {
    let id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock should be after epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("closure-id-{label}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&root).expect("temp app root should be created");
    root
}

fn write_app(root: &Path, dir_name: &str, manifest: &str) {
    let app_dir = root.join(dir_name);
    std::fs::create_dir_all(&app_dir).expect("app dir should be created");
    std::fs::write(app_dir.join("app.toml"), manifest).expect("manifest should be written");
}

struct TestServer {
    addr: String,
    handle: thread::JoinHandle<()>,
}

impl TestServer {
    fn join(self) {
        self.handle.join().expect("test server should join");
    }
}

fn single_response_server(body: &'static str) -> TestServer {
    response_server(vec![("/tdata/Apps", body.to_string())])
}

fn response_server(responses: Vec<(&str, String)>) -> TestServer {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test listener binds");
    let addr = listener
        .local_addr()
        .expect("listener has an addr")
        .to_string();
    let responses = responses
        .into_iter()
        .map(|(path, body)| (path.to_string(), body))
        .collect::<std::collections::BTreeMap<_, _>>();
    let expected_requests = responses.len();
    let handle = thread::spawn(move || {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().expect("test server accepts request");
            let mut request = [0_u8; 4096];
            let bytes_read =
                std::io::Read::read(&mut stream, &mut request).expect("test server reads request");
            let request = String::from_utf8_lossy(&request[..bytes_read]);
            assert!(request.contains("X-Tenant-Id: test-tenant"));
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("request path exists");
            let body = responses
                .get(path)
                .unwrap_or_else(|| panic!("unexpected request path {path}"));

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut stream, response.as_bytes())
                .expect("test server writes response");
        }
    });
    TestServer { addr, handle }
}

fn b64(value: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(value)
}
