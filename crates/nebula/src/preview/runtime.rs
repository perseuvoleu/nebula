use super::{
    config::{Health, Policy, Project, Recipe},
    files,
};
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Target {
    pub slot: String,
    pub generation: String,
    pub checkout: PathBuf,
    pub git_common_dir: PathBuf,
    pub branch: String,
    pub commit: String,
    pub dirty: bool,
    pub owner: String,
    pub services: Vec<String>,
}

pub(super) struct Runtime<'a> {
    pub policy: &'a Policy,
    pub project: &'a Project,
    pub name: &'a str,
    pub offline: bool,
    pub lock: &'a fs::File,
    pub directory: PathBuf,
}

fn process(program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", "/nonexistent")
        .env("LANG", "C.UTF-8");
    command
}

pub(super) fn git(checkout: &Path, args: &[&str]) -> Result<String> {
    let result = process("/usr/bin/git")
        .arg("-C")
        .arg(checkout)
        .args(args)
        .output()?;
    ensure!(
        result.status.success(),
        "cannot resolve Git checkout identity"
    );
    Ok(String::from_utf8(result.stdout)?.trim().to_owned())
}

impl Target {
    pub fn identify(checkout: &Path, owner: &str, slot: &str, recipe: &Recipe) -> Result<Self> {
        ensure!(
            !owner.is_empty()
                && owner.len() <= 128
                && owner
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b)),
            "invalid owner identity"
        );
        let checkout = fs::canonicalize(git(checkout, &["rev-parse", "--show-toplevel"])?)?;
        let git_common_dir = fs::canonicalize(git(
            &checkout,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?)?;
        let branch = git(&checkout, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        let commit = git(&checkout, &["rev-parse", "HEAD"])?;
        let dirty = !git(&checkout, &["status", "--porcelain"])?.is_empty();
        Ok(Self {
            slot: slot.into(),
            generation: ulid::Ulid::generate().to_string().to_lowercase(),
            checkout,
            git_common_dir,
            branch,
            commit,
            dirty,
            owner: owner.into(),
            services: recipe.services.keys().cloned().collect(),
        })
    }
}

impl Runtime<'_> {
    pub fn prefix(&self, slot: &str) -> String {
        format!(
            "nebula-preview-{}_{}_{}_{slot}",
            if self.offline { "offline" } else { "live" },
            self.policy.namespace,
            self.name
        )
    }

    fn docker(&self, args: &[String]) -> Result<Output> {
        // Existing host authority, not a sandbox. Never inherit DOCKER_HOST,
        // DOCKER_CONTEXT, PATH, proxy credentials or the agent-control token.
        // Stdin is unused by these noninteractive Docker operations. Passing
        // the locked file there preserves the lock through sudo's close-from
        // policy and the timeout monitor if the CLI is killed mid-operation.
        let deadline = match args.first().map(String::as_str) {
            Some("build") => "600s",
            Some("exec") => "5s",
            _ => "45s",
        };
        let output = process("/usr/bin/sudo")
            .stdin(Stdio::from(self.lock.try_clone()?))
            .args([
                "-n",
                "--",
                "/usr/bin/timeout",
                "--kill-after=5s",
                deadline,
                "/usr/bin/docker",
                "--host",
                "unix:///var/run/docker.sock",
            ])
            .args(args)
            .output()?;
        Ok(output)
    }

    fn checked(&self, args: &[String]) -> Result<String> {
        let output = self.docker(args)?;
        // Docker may repeat command/env values in errors. Do not print captured
        // stderr or argv. Application output is exposed only by explicit logs.
        ensure!(
            output.status.success(),
            "preview Docker operation failed ({})",
            args.first().map(String::as_str).unwrap_or("unknown")
        );
        Ok(String::from_utf8(output.stdout)?)
    }

    fn inspect(&self, kind: &str, name: &str) -> Result<Option<Value>> {
        let output = self.docker(&[kind.into(), "inspect".into(), name.into()])?;
        if !output.status.success() {
            // A daemon failure must never be mistaken for a missing resource.
            self.checked(&[
                "info".into(),
                "--format".into(),
                "{{.ServerVersion}}".into(),
            ])?;
            let error = String::from_utf8_lossy(&output.stderr);
            ensure!(
                error.contains("No such") || error.contains("not found"),
                "cannot inspect preview resource"
            );
            return Ok(None);
        }
        let values: Vec<Value> = serde_json::from_slice(&output.stdout)?;
        Ok(values.into_iter().next())
    }

    fn owned(&self, object: &Value, kind: &str, slot: &str) -> Result<()> {
        let labels = if kind == "container" {
            &object["Config"]["Labels"]
        } else {
            &object["Labels"]
        };
        ensure!(
            labels["dev.nebula.preview"] == self.prefix(slot),
            "refusing to change an unowned Docker resource"
        );
        Ok(())
    }

    pub fn running(&self, target: &Target) -> Result<bool> {
        for service in target
            .services
            .iter()
            .map(String::as_str)
            .chain(std::iter::once("gate"))
        {
            let Some(object) = self.inspect(
                "container",
                &format!("{}-{service}", self.prefix(&target.slot)),
            )?
            else {
                return Ok(false);
            };
            self.owned(&object, "container", &target.slot)?;
            if object["State"]["Running"] != true || object["State"]["Restarting"] == true {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn stop(&self, target: &Target) -> Result<()> {
        // The gateway is removed first. Never enumerate or prune arbitrary host
        // resources; identity and ownership must both match before each removal.
        for service in std::iter::once("gate").chain(target.services.iter().map(String::as_str)) {
            let name = format!("{}-{service}", self.prefix(&target.slot));
            if let Some(object) = self.inspect("container", &name)? {
                self.owned(&object, "container", &target.slot)?;
                self.checked(&["rm".into(), "--force".into(), name])?;
            }
        }
        for suffix in ["back", "edge"] {
            let name = format!("{}-{suffix}", self.prefix(&target.slot));
            if let Some(object) = self.inspect("network", &name)? {
                self.owned(&object, "network", &target.slot)?;
                self.checked(&["network".into(), "rm".into(), name])?;
            }
        }
        Ok(())
    }

    fn snapshot(&self, target: &Target) -> Result<PathBuf> {
        let snapshot = self.directory.join(format!("source-{}", target.generation));
        files::directory(&snapshot)?;
        let output = process("/usr/bin/git")
            .arg("-C")
            .arg(&target.checkout)
            .args([
                "ls-files",
                "-z",
                "--cached",
                "--others",
                "--exclude-standard",
            ])
            .output()?;
        ensure!(output.status.success(), "cannot enumerate preview source");
        let source_root = files::open_directory(&target.checkout)?;
        for name in output.stdout.split(|b| *b == 0).filter(|b| !b.is_empty()) {
            let name = std::str::from_utf8(name).context("preview source requires UTF-8 paths")?;
            let relative = Path::new(name);
            ensure!(
                relative
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "unsafe source path"
            );
            if relative.components().any(|c| {
                let part = c.as_os_str().to_string_lossy();
                part == ".env"
                    || part.starts_with(".env.")
                    || matches!(
                        part.as_ref(),
                        ".git"
                            | ".scratch"
                            | ".nebula-preview.json"
                            | "node_modules"
                            | "target"
                            | ".npmrc"
                            | ".pypirc"
                            | ".netrc"
                    )
            }) {
                continue;
            }
            let mut source = match files::open_source(&source_root, relative) {
                Ok(file) => file,
                Err(e)
                    if e.downcast_ref::<std::io::Error>()
                        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue
                }
                Err(e) => return Err(e),
            };
            let metadata = source.metadata()?;
            let destination = snapshot.join(relative);
            fs::create_dir_all(destination.parent().context("source parent")?)?;
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            std::io::copy(&mut source, &mut output)?;
            fs::set_permissions(
                &destination,
                fs::Permissions::from_mode(if metadata.permissions().mode() & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                }),
            )?;
        }
        // The private parent stays 0700; this directory is mounted by itself.
        fs::set_permissions(&snapshot, fs::Permissions::from_mode(0o755))?;
        Ok(snapshot)
    }

    fn labels(&self, slot: &str) -> Vec<String> {
        vec![
            "--label".into(),
            format!("dev.nebula.preview={}", self.prefix(slot)),
            "--label".into(),
            "traefik.enable=false".into(),
        ]
    }

    pub fn start(&self, target: &Target, recipe: &Recipe) -> Result<()> {
        let prefix = self.prefix(&target.slot);
        let snapshot = self.snapshot(target)?;
        for service in recipe.services.values() {
            for mount in service.data.values() {
                if let Some(relative) = mount.strip_prefix("/app/") {
                    fs::create_dir_all(snapshot.join(relative))?;
                }
            }
        }
        // Docker 28+ isolated gateway mode gives the backend bridge no host
        // address, unlike --internal alone. Only the trusted gate is dual-homed.
        for suffix in ["back", "edge"] {
            let name = format!("{prefix}-{suffix}");
            ensure!(
                self.inspect("network", &name)?.is_none(),
                "preview network already exists"
            );
            let mut args = vec!["network".into(), "create".into()];
            args.extend(self.labels(&target.slot));
            if suffix == "back" {
                args.extend(
                    [
                        "--internal",
                        "--opt",
                        "com.docker.network.bridge.gateway_mode_ipv4=isolated",
                        "--opt",
                        "com.docker.network.bridge.gateway_mode_ipv6=isolated",
                    ]
                    .map(String::from),
                );
            }
            args.push(name);
            self.checked(&args)?;
        }
        for (name, service) in &recipe.services {
            ensure!(name != "gate", "gate is a reserved service name");
            let container = format!("{prefix}-{name}");
            ensure!(
                self.inspect("container", &container)?.is_none(),
                "preview container already exists"
            );
            let image = if let Some(dockerfile) = &service.dockerfile {
                let image = format!("{prefix}-{name}:{}", target.generation);
                self.checked(&[
                    "build".into(),
                    "--quiet".into(),
                    "--network=none".into(),
                    "--label".into(),
                    format!("dev.nebula.preview={prefix}"),
                    "--tag".into(),
                    image.clone(),
                    "--file".into(),
                    snapshot.join(dockerfile).to_string_lossy().into_owned(),
                    snapshot.to_string_lossy().into_owned(),
                ])?;
                image
            } else {
                service.image.clone()
            };
            let env_file = self
                .directory
                .join(format!("env-{}-{name}", target.generation));
            let mut environment = String::new();
            for (key, value) in &service.environment {
                ensure!(
                    !value.contains(['\n', '\r', '\0']),
                    "environment values must be single-line"
                );
                environment.push_str(&format!("{key}={value}\n"));
            }
            files::atomic(&env_file, environment.as_bytes())?;
            let mut args = vec![
                "create".into(),
                "--name".into(),
                container.clone(),
                "--network".into(),
                format!("{prefix}-back"),
                "--network-alias".into(),
                name.clone(),
                "--restart".into(),
                "unless-stopped".into(),
                "--init".into(),
                "--cap-drop=ALL".into(),
                "--security-opt=no-new-privileges".into(),
                "--pids-limit=256".into(),
                "--memory=1g".into(),
                "--cpus=1".into(),
                "--log-driver=json-file".into(),
                "--log-opt=max-size=10m".into(),
                "--log-opt=max-file=2".into(),
                "--user".into(),
                service.user.clone(),
                "--workdir=/app".into(),
                "--env-file".into(),
                env_file.to_string_lossy().into_owned(),
            ];
            // A Dockerfile owns its image filesystem, including compiled output.
            // Only prepared-image recipes overlay the sanitized working copy.
            if service.dockerfile.is_none() {
                args.extend([
                    "--mount".into(),
                    format!("type=bind,src={},dst=/app,readonly", snapshot.display()),
                ]);
            }
            args.extend(self.labels(&target.slot));
            for (data, mount) in &service.data {
                let volume = format!("{prefix}_{name}_{data}");
                if let Some(object) = self.inspect("volume", &volume)? {
                    self.owned(&object, "volume", &target.slot)?;
                } else {
                    let mut create = vec!["volume".into(), "create".into()];
                    create.extend(self.labels(&target.slot));
                    create.push(volume.clone());
                    self.checked(&create)?;
                    // Initialize only this newly created empty test volume.
                    self.checked(&[
                        "run".into(),
                        "--rm".into(),
                        "--label=traefik.enable=false".into(),
                        "--label".into(),
                        format!("dev.nebula.preview={prefix}"),
                        "--network=none".into(),
                        "--cap-drop=ALL".into(),
                        "--cap-add=CHOWN".into(),
                        "--security-opt=no-new-privileges".into(),
                        "--mount".into(),
                        format!("type=volume,src={volume},dst=/data"),
                        "--entrypoint=/bin/chown".into(),
                        self.policy.gateway_image.clone(),
                        service.user.clone(),
                        "/data".into(),
                    ])?;
                }
                args.extend([
                    "--mount".into(),
                    format!("type=volume,src={volume},dst={mount},volume-nocopy"),
                ]);
            }
            args.extend(["--entrypoint".into(), service.command[0].clone(), image]);
            args.extend(service.command[1..].iter().cloned());
            self.checked(&args)?;
            self.checked(&["start".into(), container])?;
        }
        let gate = format!("{prefix}-gate");
        ensure!(
            self.inspect("container", &gate)?.is_none(),
            "preview gateway already exists"
        );
        let port = self.project.ports[usize::from(target.slot == "b")];
        let config = self
            .directory
            .join(format!("gate-{}.conf", target.generation));
        files::atomic(
            &config,
            super::ingress::nginx(self.project, target, recipe).as_bytes(),
        )?;
        let mut args = vec![
            "create".into(),
            "--name".into(),
            gate.clone(),
            "--network".into(),
            format!("{prefix}-edge"),
            "--restart=unless-stopped".into(),
            "--cap-drop=ALL".into(),
            "--security-opt=no-new-privileges".into(),
            "--user".into(),
            format!("{}:{}", unsafe { libc::geteuid() }, unsafe {
                libc::getegid()
            }),
            "--read-only".into(),
            "--tmpfs=/tmp:rw,noexec,nosuid,size=32m".into(),
            "--pids-limit=64".into(),
            "--memory=128m".into(),
            "--cpus=0.5".into(),
            "--log-driver=json-file".into(),
            "--log-opt=max-size=10m".into(),
            "--log-opt=max-file=2".into(),
            "--publish".into(),
            format!("{}:{port}:8080", self.policy.bind_address),
            "--mount".into(),
            format!(
                "type=bind,src={},dst=/owner.htpasswd,readonly",
                self.project.auth_file.display()
            ),
            "--entrypoint=/usr/sbin/nginx".into(),
        ];
        args.extend(self.labels(&target.slot));
        args.extend([
            "--mount".into(),
            format!(
                "type=bind,src={},dst=/etc/nginx/preview.conf,readonly",
                config.display()
            ),
        ]);
        args.extend([
            self.policy.gateway_image.clone(),
            "-c".into(),
            "/etc/nginx/preview.conf".into(),
            "-g".into(),
            "daemon off;".into(),
        ]);
        self.checked(&args)?;
        self.checked(&[
            "network".into(),
            "connect".into(),
            format!("{prefix}-back"),
            gate.clone(),
        ])?;
        self.checked(&["start".into(), gate.clone()])?;
        let deadline = Instant::now() + Duration::from_secs(recipe.health_timeout_seconds);
        loop {
            let mut healthy = true;
            for (name, service) in &recipe.services {
                let mut args = vec!["exec".into()];
                match &service.health {
                    Health::Http { path } => args.extend([
                        gate.clone(),
                        "/usr/bin/curl".into(),
                        "-q".into(),
                        "--silent".into(),
                        "--noproxy".into(),
                        "*".into(),
                        "--max-time".into(),
                        "2".into(),
                        "--output".into(),
                        "/dev/null".into(),
                        "--write-out".into(),
                        "%{http_code}".into(),
                        format!("http://{name}:{}{path}", service.port),
                    ]),
                    Health::Command { argv } => {
                        args.extend([format!("{prefix}-{name}"), "timeout".into(), "2".into()]);
                        args.extend(argv.iter().cloned());
                    }
                }
                let probe = self.docker(&args)?;
                healthy &= probe.status.success()
                    && match &service.health {
                        Health::Http { .. } => String::from_utf8_lossy(&probe.stdout)
                            .parse::<u16>()
                            .is_ok_and(|code| (200..300).contains(&code)),
                        Health::Command { .. } => true,
                    };
            }
            if healthy && self.running(target)? {
                break;
            }
            if Instant::now() >= deadline {
                bail!("candidate failed health checks; previous preview retained");
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        super::ingress::check_gate(self.policy, self.project, target)?;
        Ok(())
    }

    pub fn logs(&self, target: &Target, service: &str, lines: u16) -> Result<()> {
        ensure!(
            service == "gate" || target.services.iter().any(|s| s == service),
            "unknown preview service"
        );
        ensure!((1..=1000).contains(&lines), "logs lines must be 1..1000");
        let name = format!("{}-{service}", self.prefix(&target.slot));
        let object = self
            .inspect("container", &name)?
            .context("preview service is absent")?;
        self.owned(&object, "container", &target.slot)?;
        let output = self.docker(&["logs".into(), "--tail".into(), lines.to_string(), name])?;
        ensure!(output.status.success(), "cannot read preview logs");
        // Docker's line limit alone does not bound a single enormous log line.
        for bytes in [&output.stdout, &output.stderr] {
            std::io::stdout().write_all(&bytes[bytes.len().saturating_sub(64 * 1024)..])?;
        }
        Ok(())
    }
}

pub(super) fn curl(args: &[String]) -> Result<Output> {
    Ok(process("/usr/bin/curl")
        .args([
            "-q",
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--connect-timeout",
            "2",
            "--max-time",
            "4",
        ])
        .args(args)
        .stdin(Stdio::null())
        .output()?)
}

pub(super) fn status_json(target: &Target) -> Value {
    json!(target)
}
