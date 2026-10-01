use super::*;

fn publish_package(
    dir: &TempDir,
    name: &str,
    extra: &str,
    source: &str,
    extension: &str,
    dependencies: &[&str],
) {
    let package = dir.child(format!("sources/{name}"));
    let deps = dependencies
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    package.child("cabin.toml").write_str(&format!(
        "[package]\nname = \"test/{name}\"\nversion = \"1.0.0\"\nc-standard = \"c11\"\ncxx-standard = \"c++17\"\n{extra}\n[target.{name}]\ntype = \"library\"\nsources = [\"src/lib.{extension}\"]\ndeps = [{deps}]\n"
    )).unwrap();
    package
        .child(format!("src/lib.{extension}"))
        .write_str(source)
        .unwrap();
    cabin()
        .arg("publish")
        .arg("--manifest-path")
        .arg(package.child("cabin.toml").path())
        .arg("--registry-dir")
        .arg(dir.child("registry").path())
        .assert()
        .success();
}

fn write_chain(dir: &TempDir, extension: &str) {
    publish_package(
        dir,
        "last",
        "",
        "int last(void) { return 7; }\n",
        extension,
        &[],
    );
    publish_package(
        dir,
        "leaf",
        r#"
[features]
extra = ["dep:test/last"]
[dependencies]
"test/last" = { version = "^1", optional = true }
"#,
        "int last(void); int leaf(void) { return last(); }\n",
        extension,
        &["test/last"],
    );
    publish_package(
        dir,
        "middle",
        r#"
[features]
extra = ["test/leaf/extra"]
[dependencies]
"test/leaf" = { version = "^1", optional = true, default-features = false }
"#,
        "int leaf(void); int middle(void) { return leaf(); }\n",
        extension,
        &["test/leaf"],
    );
    dir.child("app/cabin.toml")
        .write_str(&format!(
            r#"[package]
name = "app"
version = "0.1.0"
c-standard = "c11"
cxx-standard = "c++17"
[features]
default = ["extra"]
extra = ["test/middle/extra"]
[dependencies]
"test/middle" = {{ version = "^1", default-features = false }}
[target.app]
type = "executable"
sources = ["src/main.{extension}"]
deps = ["test/middle"]
required-features = ["extra"]
[target.basic]
type = "executable"
sources = ["src/basic.{extension}"]
"#
        ))
        .unwrap();
    dir.child(format!("app/src/main.{extension}"))
        .write_str("int middle(void); int main(void) { return middle() - 7; }\n")
        .unwrap();
    dir.child(format!("app/src/basic.{extension}"))
        .write_str("int main(void) { return 0; }\n")
        .unwrap();
}

fn chain_command(dir: &TempDir, command: &str) -> Command {
    let mut cmd = cabin();
    cmd.arg(command)
        .arg("--manifest-path")
        .arg(dir.child("app/cabin.toml").path())
        .arg("--index-path")
        .arg(dir.child("registry").path());
    if matches!(command, "build" | "vendor") {
        cmd.arg("--cache-dir").arg(dir.child("cache").path());
    }
    if command == "build" {
        cmd.arg("--build-dir").arg(dir.child("build").path());
    }
    cmd
}

#[test]
fn enabled_registry_optionals_are_locked_vendored_compiled_and_linked_for_c_and_cxx() {
    require_c_and_cxx_build_tools();
    for extension in ["c", "cc"] {
        let dir = TempDir::new().unwrap();
        write_chain(&dir, extension);
        chain_command(&dir, "resolve")
            .arg("--no-default-features")
            .assert()
            .success();
        let disabled = fs::read_to_string(dir.child("app/cabin.lock").path()).unwrap();
        assert!(!disabled.contains("test/leaf"));
        chain_command(&dir, "build")
            .arg("--locked")
            .assert()
            .failure()
            .stderr(predicate::str::contains("test/leaf"));
        chain_command(&dir, "build")
            .arg("--no-default-features")
            .assert()
            .success();
        let index = cabin_index::load_index(dir.child("registry/packages").path()).unwrap();
        let leaf_checksum = index
            .package(&cabin_core::PackageName::new("test/leaf").unwrap())
            .unwrap()
            .versions[&semver::Version::new(1, 0, 0)]
            .checksum
            .as_ref()
            .unwrap();
        let cache = cabin_artifact::ArtifactCache::new(dir.child("cache").path());
        assert!(!cache.archive_path(leaf_checksum.hex()).exists());
        assert!(!cache.source_dir(leaf_checksum.hex()).exists());
        chain_command(&dir, "resolve").assert().success();
        let resolved = fs::read_to_string(dir.child("app/cabin.lock").path()).unwrap();
        assert!(resolved.contains("test/leaf") && resolved.contains("test/last"));
        chain_command(&dir, "build").assert().success();
        assert_eq!(
            resolved,
            fs::read_to_string(dir.child("app/cabin.lock").path()).unwrap()
        );
        let lock = cabin_lockfile::read_lockfile(dir.child("app/cabin.lock").path()).unwrap();
        assert_eq!(
            lock.find(&cabin_core::PackageName::new("test/leaf").unwrap())
                .unwrap()
                .dependencies,
            vec![cabin_core::PackageName::new("test/last").unwrap()]
        );
        chain_command(&dir, "vendor")
            .arg("--vendor-dir")
            .arg(dir.child("vendor").path())
            .assert()
            .success();
        dir.child("vendor/packages/test/leaf.json")
            .assert(predicate::path::is_file());
        dir.child("vendor/packages/test/last.json")
            .assert(predicate::path::is_file());
        chain_command(&dir, "update")
            .arg("--no-default-features")
            .assert()
            .success();
        assert_eq!(
            disabled,
            fs::read_to_string(dir.child("app/cabin.lock").path()).unwrap()
        );
        chain_command(&dir, "vendor")
            .arg("--no-default-features")
            .arg("--vendor-dir")
            .arg(dir.child("disabled-vendor").path())
            .assert()
            .success();
        dir.child("disabled-vendor/packages/test/leaf.json")
            .assert(predicate::path::missing());
    }
}

#[test]
fn sparse_registry_optionals_resolve_and_build_with_missing_disabled_entries() {
    require_c_and_cxx_build_tools();
    let dir = TempDir::new().unwrap();
    write_chain(&dir, "c");
    let server = super::sparse_http::TestServer::serve(dir.child("registry").path().to_owned());
    let mut cmd = cabin();
    cmd.arg("resolve")
        .arg("--manifest-path")
        .arg(dir.child("app/cabin.toml").path())
        .arg("--index-url")
        .arg(server.url())
        .assert()
        .success();
    let resolved = fs::read_to_string(dir.child("app/cabin.lock").path()).unwrap();
    cabin()
        .arg("build")
        .arg("--manifest-path")
        .arg(dir.child("app/cabin.toml").path())
        .arg("--index-url")
        .arg(server.url())
        .arg("--cache-dir")
        .arg(dir.child("cache").path())
        .arg("--build-dir")
        .arg(dir.child("build").path())
        .assert()
        .success();
    assert_eq!(
        resolved,
        fs::read_to_string(dir.child("app/cabin.lock").path()).unwrap()
    );
    fs::remove_file(dir.child("registry/packages/test/leaf.json").path()).unwrap();
    fs::remove_file(dir.child("registry/packages/test/last.json").path()).unwrap();
    cabin()
        .arg("resolve")
        .arg("--manifest-path")
        .arg(dir.child("app/cabin.toml").path())
        .arg("--index-url")
        .arg(server.url())
        .arg("--no-default-features")
        .assert()
        .success();
    let lock = fs::read_to_string(dir.child("app/cabin.lock").path()).unwrap();
    assert!(!lock.contains("test/leaf") && !lock.contains("test/last"));
}

#[test]
fn build_reports_a_feature_missing_from_the_chosen_registry_manifest() {
    let dir = TempDir::new().unwrap();
    write_chain(&dir, "c");
    let manifest = dir.child("app/cabin.toml");
    let body = fs::read_to_string(manifest.path()).unwrap();
    assert!(body.contains("test/middle/extra"));
    manifest
        .write_str(&body.replace("test/middle/extra", "test/middle/not-declared"))
        .unwrap();
    chain_command(&dir, "resolve").assert().success();
    chain_command(&dir, "build").assert().failure().stderr(
        predicate::str::contains("not-declared")
            .and(predicate::str::contains("does not declare that feature")),
    );
}

#[test]
fn disabled_optional_target_dependency_names_an_enabling_feature_for_path_and_registry() {
    require_c_and_cxx_build_tools();
    let dir = TempDir::new().unwrap();
    write_chain(&dir, "c");
    let manifest = dir.child("app/cabin.toml");
    let body = fs::read_to_string(manifest.path()).unwrap();
    // The root requests the middle target while its optional leaf stays disabled.
    manifest
        .write_str(&body.replace("required-features = [\"extra\"]", ""))
        .unwrap();
    chain_command(&dir, "build")
        .arg("--no-default-features")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("optional dependency \"test/leaf\"")
                .and(predicate::str::contains("extra")),
        );
    manifest
        .write_str(&format!(
            "{}\n[patch]\n\"test/middle\" = {{ path = \"../sources/middle\" }}\n",
            body.replace("required-features = [\"extra\"]", "")
        ))
        .unwrap();
    chain_command(&dir, "build")
        .arg("--no-default-features")
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("optional dependency \"test/leaf\"")
                .and(predicate::str::contains("extra")),
        );
    dir.child("path/cabin.toml")
        .write_str(
            r#"[package]
name = "path"
version = "1.0.0"
c-standard = "c11"
[features]
extra = ["dep:test/leaf"]
[dependencies]
"test/leaf" = { path = "../sources/leaf", optional = true }
[target.path]
type = "executable"
sources = ["main.c"]
deps = ["test/leaf"]
"#,
        )
        .unwrap();
    dir.child("path/main.c")
        .write_str("int main(void) { return 0; }\n")
        .unwrap();
    cabin()
        .arg("build")
        .arg("--manifest-path")
        .arg(dir.child("path/cabin.toml").path())
        .arg("--build-dir")
        .arg(dir.child("path-build").path())
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("optional dependency \"test/leaf\"")
                .and(predicate::str::contains("--features extra")),
        );
}

#[test]
fn patches_use_their_own_features_and_forward_registry_and_local_requests() {
    let dir = TempDir::new().unwrap();
    write_chain(&dir, "c");
    for name in ["fork-default", "self-extra", "path-extra"] {
        publish_package(&dir, name, "", "int unused(void) { return 0; }\n", "c", &[]);
    }
    let index_path = dir.child("registry/packages/test/middle.json");
    let mut middle: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(index_path.path()).unwrap()).unwrap();
    middle["versions"]["1.0.0"]["features"]["features"]["extra"] =
        serde_json::json!(["test/leaf/patch-only"]);
    index_path.write_str(&middle.to_string()).unwrap();
    let index_path = dir.child("registry/packages/test/leaf.json");
    let mut leaf: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(index_path.path()).unwrap()).unwrap();
    leaf["versions"]["1.0.0"]["features"] = serde_json::json!({
        "default": ["upstream"], "features": { "upstream": ["dep:test/missing"] }
    });
    leaf["versions"]["1.0.0"]["dependencies"]["test/missing"] =
        serde_json::json!({ "version": "^1", "optional": true });
    index_path.write_str(&leaf.to_string()).unwrap();
    dir.child("fork/cabin.toml")
        .write_str(
            r#"[package]
name = "test/leaf"
version = "1.0.0"
[features]
default = ["fork-default"]
fork-default = ["dep:test/fork-default"]
self-feature = ["dep:test/self-extra"]
patch-only = ["path/extra"]
[dependencies]
"test/fork-default" = { version = "^1", optional = true }
"test/self-extra" = { version = "^1", optional = true }
path = { path = "../path", optional = true, default-features = false }
[target.leaf]
type = "header-only"
interface-c-standard = "c11"
"#,
        )
        .unwrap();
    dir.child("path/cabin.toml")
        .write_str(
            r#"[package]
name = "path"
version = "1.0.0"
[features]
extra = ["dep:test/path-extra"]
[dependencies]
"test/path-extra" = { version = "^1", optional = true }
"#,
        )
        .unwrap();
    dir.child("app/cabin.toml")
        .write_str(
            r#"[package]
name = "app"
version = "0.1.0"
[dependencies]
"test/middle" = { version = "^1", features = ["extra"] }
"test/leaf" = { version = "^1", features = ["self-feature"] }
[patch]
"test/leaf" = { path = "../fork" }
"#,
        )
        .unwrap();
    chain_command(&dir, "resolve").assert().success();
    let lock = fs::read_to_string(dir.child("app/cabin.lock").path()).unwrap();
    for expected in ["test/fork-default", "test/self-extra", "test/path-extra"] {
        assert!(lock.contains(expected), "missing {expected}: {lock}");
    }
    assert!(!lock.contains("test/missing") && !lock.contains("test/last"));
}

#[test]
fn sparse_optional_dependency_failure_backtracks_and_disabled_edges_make_no_request() {
    let dir = TempDir::new().unwrap();
    dir.child("registry/config.json")
        .write_str(
            r#"{"schema":1,"kind":"file-registry","packages":"packages","artifacts":"artifacts"}"#,
        )
        .unwrap();
    write_index_entry_no_source(
        dir.child("registry/packages").path(),
        "parent",
        "1.0.0",
        &"0".repeat(64),
    );
    let path = dir.child("registry/packages/parent.json");
    let mut parent: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(path.path()).unwrap()).unwrap();
    let mut latest = parent["versions"]["1.0.0"].clone();
    latest["dependencies"] = serde_json::json!({
        "missing": { "version": "^1", "optional": true },
        "disabled": { "version": "^1", "optional": true }
    });
    latest["features"] =
        serde_json::json!({ "default": ["extra"], "features": { "extra": ["dep:missing"] } });
    parent["versions"]["1.1.0"] = latest;
    path.write_str(&parent.to_string()).unwrap();
    dir.child("app/cabin.toml")
        .write_str(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\nparent = \"^1\"\n",
        )
        .unwrap();
    let server = super::sparse_http::TestServer::serve(dir.child("registry").path().to_owned());
    cabin()
        .arg("resolve")
        .arg("--manifest-path")
        .arg(dir.child("app/cabin.toml").path())
        .arg("--index-url")
        .arg(server.url())
        .assert()
        .success();
    let lock = cabin_lockfile::read_lockfile(dir.child("app/cabin.lock").path()).unwrap();
    assert_eq!(
        lock.find(&cabin_core::PackageName::new("parent").unwrap())
            .unwrap()
            .version
            .to_string(),
        "1.0.0"
    );
    assert!(
        server
            .requests()
            .iter()
            .any(|path| path == "/packages/missing.json")
    );
    assert!(
        !server
            .requests()
            .iter()
            .any(|path| path == "/packages/disabled.json")
    );
}

#[test]
fn disabling_the_last_registry_dependency_clears_the_lockfile() {
    require_c_and_cxx_build_tools();
    let dir = TempDir::new().unwrap();
    dir.child("cabin.toml")
        .write_str(
            r#"[package]
name = "app"
version = "0.1.0"
c-standard = "c11"
[features]
default = ["extra"]
extra = ["dep:leaf"]
[dependencies]
leaf = { version = "^1", optional = true }
[target.app]
type = "executable"
sources = ["main.c"]
"#,
        )
        .unwrap();
    dir.child("main.c")
        .write_str("int main(void) { return 0; }\n")
        .unwrap();
    write_index_entry_no_source(dir.child("index").path(), "leaf", "1.0.0", &"0".repeat(64));
    cabin()
        .arg("resolve")
        .arg("--manifest-path")
        .arg(dir.child("cabin.toml").path())
        .arg("--index-path")
        .arg(dir.child("index").path())
        .assert()
        .success();
    cabin()
        .arg("resolve")
        .arg("--manifest-path")
        .arg(dir.child("cabin.toml").path())
        .arg("--no-default-features")
        .assert()
        .success();
    assert!(
        cabin_lockfile::read_lockfile(dir.child("cabin.lock").path())
            .unwrap()
            .packages
            .is_empty()
    );
    cabin()
        .arg("resolve")
        .arg("--manifest-path")
        .arg(dir.child("cabin.toml").path())
        .arg("--index-path")
        .arg(dir.child("index").path())
        .assert()
        .success();
    cabin()
        .arg("build")
        .arg("--manifest-path")
        .arg(dir.child("cabin.toml").path())
        .arg("--no-default-features")
        .arg("--build-dir")
        .arg(dir.child("build").path())
        .assert()
        .success();
    assert!(
        cabin_lockfile::read_lockfile(dir.child("cabin.lock").path())
            .unwrap()
            .packages
            .is_empty()
    );
}

#[test]
fn enabling_an_optional_patch_also_checks_its_version_requirement() {
    let dir = TempDir::new().unwrap();
    dir.child("app/cabin.toml")
        .write_str(
            r#"[package]
name = "app"
version = "0.1.0"
[features]
extra = ["dep:leaf"]
[dependencies]
leaf = { version = "^2", optional = true }
[patch]
leaf = { path = "../leaf" }
"#,
        )
        .unwrap();
    dir.child("leaf/cabin.toml")
        .write_str("[package]\nname = \"leaf\"\nversion = \"1.0.0\"\n")
        .unwrap();
    cabin()
        .arg("resolve")
        .arg("--manifest-path")
        .arg(dir.child("app/cabin.toml").path())
        .assert()
        .success();
    cabin()
        .arg("resolve")
        .arg("--manifest-path")
        .arg(dir.child("app/cabin.toml").path())
        .args(["--features", "extra"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "does not satisfy dependency requirement",
        ));
}
