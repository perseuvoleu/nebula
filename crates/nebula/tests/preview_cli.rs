use std::process::Command;

#[test]
fn preview_status_fails_closed_without_a_host_policy() {
    let result = Command::new(env!("CARGO_BIN_EXE_nebula"))
        .args([
            "preview",
            "--offline",
            "--policy",
            "/nonexistent/nebula-preview-policy.json",
            "status",
        ])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&result.stderr).contains("preview host policy"));
    assert!(!result.status.success());
}

use serde_json::{json, Value};
use std::{fs, os::unix::fs::PermissionsExt};

struct Fixture {
    root: tempfile::TempDir,
    policy: Value,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().to_str().unwrap();
        let policy = json!({
            "namespace": "test", "state_dir": format!("{dir}/state"),
            "bind_address": "127.0.0.1", "gateway_image": "nginx:alpine",
            "projects": {"demo": {
                "git_common_dir": dir, "domain": "demo.example.test", "ports": [28101,28102],
                "auth_file": format!("{dir}/owner.htpasswd")
            }}
        });
        fs::write(root.path().join("owner.htpasswd"), "fixture:invalid\n").unwrap();
        fs::set_permissions(
            root.path().join("owner.htpasswd"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        Self { root, policy }
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        let path = self.root.path().join("policy.json");
        fs::write(&path, self.policy.to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        Command::new(env!("CARGO_BIN_EXE_nebula"))
            .args(["preview", "--offline", "--policy"])
            .arg(path)
            .args(["--project", "demo"])
            .args(args)
            .output()
            .unwrap()
    }
}

#[test]
fn a_configured_project_has_a_stopped_preview_without_starting_a_daemon() {
    let fixture = Fixture::new();
    let result = fixture.run(&["status"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let status: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(status["phase"], "stopped");
}

#[test]
fn preview_refuses_a_symlinked_state_directory() {
    let fixture = Fixture::new();
    let unrelated = fixture.root.path().join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    std::os::unix::fs::symlink(&unrelated, fixture.root.path().join("state")).unwrap();
    let result = fixture.run(&["status"]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("symlink"));
}

#[test]
fn host_policy_rejects_route_injection_and_control_domain_reuse() {
    for domain in [
        "code.possu.dev",
        "demo.example.test`) || Host(`code.possu.dev",
        "demo.example.test; return 200",
        "*.possu.dev",
        "demo.example.test\n",
    ] {
        let mut fixture = Fixture::new();
        fixture.policy["projects"]["demo"]["domain"] = json!(domain);
        let result = fixture.run(&["status"]);
        assert!(!result.status.success(), "accepted unsafe domain");
    }
}

#[test]
fn host_policy_rejects_unsafe_mount_paths_and_public_fixture_bindings() {
    for (field, value) in [
        ("state_dir", "/tmp/state,readonly=false"),
        ("bind_address", "0.0.0.0"),
        ("bind_address", "172.18.0.1"),
    ] {
        let mut fixture = Fixture::new();
        fixture.policy[field] = json!(value);
        assert!(
            !fixture.run(&["status"]).status.success(),
            "accepted unsafe policy"
        );
    }
}

#[test]
fn unknown_policy_fields_do_not_become_proxy_configuration() {
    let mut fixture = Fixture::new();
    fixture.policy["routers"] = json!({"injected": {"auth": false}});
    assert!(!fixture.run(&["status"]).status.success());
}
