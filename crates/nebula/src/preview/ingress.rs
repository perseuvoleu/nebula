use super::{
    config::{Policy, Project, Recipe},
    files,
    runtime::{self, Target},
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

pub(super) const POLICY: &str = "/etc/nebula/previews.json";
const ROUTES: &str = "/etc/dokploy/traefik/dynamic";
const HELPER: &str = "/usr/local/libexec/nebula-preview-publish";

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Published {
    pub slot: String,
    pub generation: String,
}

pub(super) fn nginx(project: &Project, target: &Target, recipe: &Recipe) -> String {
    let mut config = format!(
        r#"worker_processes 1;
pid /tmp/nginx.pid;
error_log /dev/stderr warn;
events {{ worker_connections 256; }}
http {{
  access_log off;
  client_body_temp_path /tmp/body;
  proxy_temp_path /tmp/proxy;
  fastcgi_temp_path /tmp/fastcgi;
  uwsgi_temp_path /tmp/uwsgi;
  scgi_temp_path /tmp/scgi;
  map $http_upgrade $connection_upgrade {{ default upgrade; '' close; }}
  map $http_origin $origin_allowed {{ default 0; '' 1; "https://{domain}" 1; }}
  server {{ listen 8080 default_server; server_name _; return 404; }}
  server {{
    listen 8080;
    server_name {domain};
    if ($http_host != "{domain}") {{ return 404; }}
    if ($origin_allowed = 0) {{ return 403; }}
    auth_basic "Private preview";
    auth_basic_user_file /owner.htpasswd;
    add_header X-Nebula-Preview "{generation}" always;
    add_header Cache-Control "no-store" always;
    client_max_body_size 20m;
    proxy_http_version 1.1;
    proxy_set_header Host "{domain}";
    proxy_set_header X-Forwarded-Host "{domain}";
    proxy_set_header X-Forwarded-Proto https;
    proxy_set_header X-Forwarded-Port 443;
    proxy_set_header X-Forwarded-For $remote_addr;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header Forwarded "";
    proxy_set_header Authorization "";
    proxy_hide_header X-Nebula-Preview;
    proxy_hide_header X-Nebula-Route;
    proxy_set_header Proxy-Authorization "";
    proxy_set_header Upgrade $http_upgrade;
    proxy_set_header Connection $connection_upgrade;
    proxy_read_timeout 3600s;
    proxy_send_timeout 3600s;
    proxy_buffering off;
"#,
        domain = project.domain,
        generation = target.generation
    );
    for (path, service) in &recipe.routes {
        config.push_str(&format!(
            "    location ^~ {path} {{ proxy_pass http://{service}:{}; }}\n",
            recipe.services[service].port
        ));
    }
    config.push_str("  }\n}\n");
    config
}

fn matches_gate(output: &std::process::Output, generation: &str) -> bool {
    let headers = String::from_utf8_lossy(&output.stdout).to_ascii_lowercase();
    output.status.success()
        && headers
            .lines()
            .next()
            .is_some_and(|line| line.split_whitespace().nth(1) == Some("401"))
        && headers
            .lines()
            .any(|line| line.trim() == format!("x-nebula-preview: {generation}"))
        && headers
            .lines()
            .any(|line| line.starts_with("www-authenticate: basic "))
}

pub(super) fn check_gate(policy: &Policy, project: &Project, target: &Target) -> Result<()> {
    let port = project.ports[usize::from(target.slot == "b")];
    let base = format!("http://{}:{port}", policy.bind_address);
    for path in ["/", "/api/", "/socket"] {
        let output = runtime::curl(&[
            "-D".into(),
            "-".into(),
            "-o".into(),
            "/dev/null".into(),
            "-H".into(),
            format!("Host: {}", project.domain),
            format!("{base}{path}"),
        ])?;
        ensure!(
            matches_gate(&output, &target.generation),
            "preview authentication gate did not fail closed"
        );
    }
    let wrong_host = runtime::curl(&[
        "-D".into(),
        "-".into(),
        "-o".into(),
        "/dev/null".into(),
        "-H".into(),
        "Host: invalid.example".into(),
        base,
    ])?;
    ensure!(
        wrong_host.status.success()
            && String::from_utf8_lossy(&wrong_host.stdout)
                .lines()
                .next()
                .is_some_and(|line| line.split_whitespace().nth(1) == Some("404")),
        "preview gateway accepted an unexpected Host"
    );
    Ok(())
}

fn route(policy: &Policy, name: &str, published: &Published) -> Value {
    let project = &policy.projects[name];
    let id = format!("nebula-preview-{}-{name}", policy.namespace);
    let port = project.ports[usize::from(published.slot == "b")];
    json!({"http": {"routers": {id.clone(): {
        "rule": format!("Host(`{}`)", project.domain), "entryPoints": ["websecure"],
        "tls": {"certResolver": "letsencrypt"}, "service": id,
    }}, "services": {id: {"loadBalancer": {"passHostHeader": true,
        "servers": [{"url": format!("http://{}:{port}", policy.bind_address)}]}}}}})
}

pub(super) fn validate_published(published: &Published) -> Result<()> {
    ensure!(
        published.slot == "a" || published.slot == "b",
        "invalid preview slot"
    );
    ensure!(
        published.generation.len() == 26
            && published
                .generation
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
        "invalid preview generation"
    );
    Ok(())
}

// Root privilege is confined to this entry point. It accepts no path, host,
// command, environment override or proxy fragment supplied by the caller.
pub(super) fn publish(name: &str, operation: &str, generation: &str) -> Result<()> {
    ensure!(
        unsafe { libc::geteuid() } == 0,
        "publisher must run through the installed root-owned helper"
    );
    root_controlled(&std::env::current_exe()?)?;
    super::config::identifier(name)?;
    let policy = Policy::load(Path::new(POLICY), false)?;
    let project = policy
        .projects
        .get(name)
        .context("project is not allowlisted")?;
    let directory = Path::new(ROUTES);
    root_controlled(directory)?;
    let _lock = files::lock(&directory.join(format!(".nebula-preview-{name}.lock")))?;
    // Traefik's directory provider ignores .json. JSON serialization is also
    // valid YAML, so use its supported extension without introducing a parser.
    let route_file = directory.join(format!("nebula-preview-{name}.yaml"));
    match directory
        .join(format!("nebula-preview-{name}.json"))
        .symlink_metadata()
    {
        Ok(_) => anyhow::bail!(
            "legacy preview JSON artifact exists; inspect and retire it before publication"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let pending = directory.join(format!(".nebula-preview-{name}.pending"));
    // Metadata lives outside the watched directory. Read back the route itself:
    // it remains authoritative when a publisher dies after atomic replacement.
    let previous = if route_file.exists() {
        Some(files::read_private(&route_file, true)?)
    } else {
        None
    };
    if operation == "status" {
        let published = previous
            .as_ref()
            .map(|bytes| decode_route(&policy, name, bytes))
            .transpose()?;
        if published.is_some() || pending.exists() {
            verify_adoption(project, published.as_ref(), Duration::from_secs(15))?;
            if pending.exists() {
                fs::remove_file(&pending)?;
                fs::File::open(directory)?.sync_all()?;
            }
        }
        println!("{}", serde_json::to_string(&published)?);
        return Ok(());
    }
    ensure!(
        operation == "a" || operation == "b" || operation == "down",
        "invalid publish operation"
    );
    if operation == "down" {
        if previous.is_none() && !pending.exists() {
            return Ok(());
        }
        files::atomic(&pending, b"{}\n")?;
        if previous.is_some() {
            fs::remove_file(&route_file)?;
            fs::File::open(directory)?.sync_all()?;
        }
        verify_adoption(project, None, Duration::from_secs(15))
            .context("route withdrawal is uncertain; managed runtimes retained")?;
        fs::remove_file(&pending)?;
        fs::File::open(directory)?.sync_all()?;
        return Ok(());
    }
    let published = Published {
        slot: operation.into(),
        generation: generation.into(),
    };
    validate_published(&published)?;
    let auth = files::read_private(&project.auth_file, true)
        .context("preview-only owner authentication file")?;
    ensure!(!auth.is_empty(), "owner authentication file is empty");
    let candidate = Target {
        slot: operation.into(),
        generation: generation.into(),
        checkout: Default::default(),
        git_common_dir: Default::default(),
        branch: String::new(),
        commit: String::new(),
        dirty: false,
        owner: String::new(),
        services: Vec::new(),
    };
    check_gate(&policy, project, &candidate)?;
    let mut rendered = route(&policy, name, &published);
    // Use a legitimate response-header middleware to carry the generation in
    // Traefik's schema; no unknown top-level metadata keys are introduced.
    let id = format!("nebula-preview-{}-{name}", policy.namespace);
    let marker = format!("{id}-generation");
    rendered["http"]["routers"][&id]["middlewares"] = json!([marker]);
    rendered["http"]["middlewares"] =
        json!({marker: {"headers": {"customResponseHeaders": {"X-Nebula-Route": generation}}}});
    files::atomic(&pending, b"{}\n")?;
    files::atomic(&route_file, &serde_json::to_vec_pretty(&rendered)?)?;
    // First publication may need ACME issuance. Keep each request bounded and
    // allow a fixed longer initial window; rollback/status remain short. The
    // candidate has already passed backend and fail-closed gateway checks.
    let adoption_window = Duration::from_secs(if previous.is_none() { 120 } else { 15 });
    if let Err(error) = verify_adoption(project, Some(&published), adoption_window) {
        let restored = previous
            .as_ref()
            .map(|bytes| decode_route(&policy, name, bytes))
            .transpose()?;
        if let Some(bytes) = &previous {
            files::atomic(&route_file, bytes)?;
        } else {
            fs::remove_file(&route_file)?;
            fs::File::open(directory)?.sync_all()?;
        }
        verify_adoption(project, restored.as_ref(), Duration::from_secs(15))
            .context("route rollback is uncertain; preserve both runtimes and retry")?;
        fs::remove_file(&pending)?;
        fs::File::open(directory)?.sync_all()?;
        return Err(error.context("Traefik adoption failed; previous route restored and verified"));
    }
    fs::remove_file(&pending)?;
    fs::File::open(directory)?.sync_all()?;
    Ok(())
}

fn verify_adoption(
    project: &Project,
    published: Option<&Published>,
    window: Duration,
) -> Result<()> {
    let deadline = Instant::now() + window;
    loop {
        let response = runtime::curl(&[
            "--resolve".into(),
            format!("{}:443:127.0.0.1", project.domain),
            "-D".into(),
            "-".into(),
            "-o".into(),
            "/dev/null".into(),
            format!("https://{}/", project.domain),
        ])?;
        let adopted = match published {
            Some(target) => matches_gate(&response, &target.generation),
            None => {
                let headers = String::from_utf8_lossy(&response.stdout).to_ascii_lowercase();
                response.status.success()
                    && headers
                        .lines()
                        .next()
                        .is_some_and(|line| line.split_whitespace().nth(1) == Some("404"))
                    && !headers.contains("x-nebula-preview:")
                    && !headers.contains("x-nebula-route:")
            }
        };
        if adopted {
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "live HTTPS route did not match the published state"
        );
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn decode_route(policy: &Policy, name: &str, bytes: &[u8]) -> Result<Published> {
    let route: Value = serde_json::from_slice(bytes)?;
    let id = format!("nebula-preview-{}-{name}", policy.namespace);
    let url = route["http"]["services"][&id]["loadBalancer"]["servers"][0]["url"]
        .as_str()
        .context("unrecognized published route")?;
    let project = &policy.projects[name];
    let slot = if url == format!("http://{}:{}", policy.bind_address, project.ports[0]) {
        "a"
    } else if url == format!("http://{}:{}", policy.bind_address, project.ports[1]) {
        "b"
    } else {
        anyhow::bail!("published target is outside the allowlist")
    };
    let generation = route["http"]["middlewares"][format!("{id}-generation")]["headers"]
        ["customResponseHeaders"]["X-Nebula-Route"]
        .as_str()
        .context("missing published generation")?;
    let published = Published {
        slot: slot.into(),
        generation: generation.into(),
    };
    validate_published(&published)?;
    Ok(published)
}

fn root_controlled(path: &Path) -> Result<()> {
    files::check_path(path)?;
    for ancestor in path.ancestors() {
        let metadata = fs::metadata(ancestor)?;
        ensure!(
            metadata.uid() == 0 && metadata.mode() & 0o022 == 0,
            "publisher paths must be root-controlled"
        );
    }
    Ok(())
}

pub(super) fn current(
    directory: &Path,
    name: &str,
    offline: bool,
    lock: &fs::File,
) -> Result<Option<Published>> {
    if offline {
        let path = directory.join("published.json");
        return if path.exists() {
            Ok(serde_json::from_slice(&files::read_private(&path, false)?)?)
        } else {
            Ok(None)
        };
    }
    let output = helper(name, "status", "", lock)?;
    Ok(serde_json::from_slice(&output)?)
}

fn helper(name: &str, operation: &str, generation: &str, lock: &fs::File) -> Result<Vec<u8>> {
    root_controlled(Path::new(HELPER))?;
    let output = Command::new("/usr/bin/sudo")
        .env_clear()
        .stdin(std::process::Stdio::from(lock.try_clone()?))
        .env("PATH", "/usr/bin:/bin")
        .args(["-n", "--", HELPER, name, operation, generation])
        .output()?;
    ensure!(
        output.status.success(),
        "scoped preview publisher refused the operation; previous runtime retained"
    );
    Ok(output.stdout)
}

pub(super) fn set(
    directory: &Path,
    name: &str,
    offline: bool,
    published: Option<&Published>,
    lock: &fs::File,
) -> Result<()> {
    if let Some(published) = published {
        validate_published(published)?;
    }
    if offline {
        return files::atomic(
            &directory.join("published.json"),
            &serde_json::to_vec(&published)?,
        );
    }
    match published {
        Some(target) => {
            helper(name, &target.slot, &target.generation, lock)?;
        }
        None => {
            helper(name, "down", "", lock)?;
        }
    }
    Ok(())
}
