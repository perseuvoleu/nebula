use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Policy {
    pub namespace: String,
    pub state_dir: PathBuf,
    pub bind_address: Ipv4Addr,
    pub gateway_image: String,
    pub projects: BTreeMap<String, Project>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Project {
    pub git_common_dir: PathBuf,
    pub domain: String,
    pub ports: [u16; 2],
    pub auth_file: PathBuf,
}

impl Policy {
    pub fn load(path: &Path, offline: bool) -> Result<Self> {
        let bytes =
            super::files::read_private(path, !offline).context("read preview host policy")?;
        let policy: Self = serde_json::from_slice(&bytes).context("parse preview host policy")?;
        identifier(&policy.namespace)?;
        ensure!(
            policy.bind_address.is_loopback() || policy.bind_address.is_private(),
            "ingress address must be loopback or private IPv4"
        );
        ensure!(
            !offline || policy.bind_address.is_loopback(),
            "offline fixtures must bind loopback"
        );
        ensure!(
            policy.state_dir.is_absolute(),
            "state directory must be absolute"
        );
        image(&policy.gateway_image)?;
        let mut ports = std::collections::BTreeSet::new();
        let mut domains = std::collections::BTreeSet::new();
        for (name, project) in &policy.projects {
            identifier(name)?;
            domain(&project.domain)?;
            ensure!(
                project.domain != "code.possu.dev",
                "the mobile control domain is reserved"
            );
            ensure!(domains.insert(&project.domain), "duplicate project domain");
            ensure!(
                project.git_common_dir.is_absolute(),
                "Git common directory must be absolute"
            );
            ensure!(
                std::fs::canonicalize(&project.git_common_dir)? == project.git_common_dir,
                "Git common directory must be canonical"
            );
            for port in project.ports {
                ensure!(
                    (1024..=65535).contains(&port) && ports.insert(port),
                    "invalid or duplicate gateway port"
                );
            }
            // Authentication is mandatory even for offline fixtures; Docker mounts this
            // file only in the trusted gateway, never in application containers.
            super::files::check_path(&project.auth_file)?;
        }
        Ok(policy)
    }
}

pub(super) fn identifier(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && value.len() <= 32
            && value.as_bytes()[0].is_ascii_lowercase()
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid preview identifier"
    );
    Ok(())
}

fn domain(value: &str) -> Result<()> {
    ensure!(
        value.len() <= 253 && value.contains('.'),
        "invalid preview domain"
    );
    for label in value.split('.') {
        ensure!(
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "invalid preview domain"
        );
    }
    Ok(())
}

fn image(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && !value.starts_with('-')
            && value.len() < 256
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-/:@".contains(&b)),
        "invalid container image"
    );
    Ok(())
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Recipe {
    pub services: BTreeMap<String, Service>,
    pub routes: BTreeMap<String, String>,
    pub health_timeout_seconds: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Service {
    pub image: String,
    pub dockerfile: Option<String>,
    pub command: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub port: u16,
    pub health: Health,
    pub user: String,
    pub data: BTreeMap<String, String>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Health {
    Http { path: String },
    Command { argv: Vec<String> },
}

impl Recipe {
    pub fn load(checkout: &Path) -> Result<Self> {
        let bytes = super::files::read_private(&checkout.join(".nebula-preview.json"), false)?;
        let recipe: Self = serde_json::from_slice(&bytes).context("parse .nebula-preview.json")?;
        ensure!(
            (1..=8).contains(&recipe.services.len()),
            "recipe needs one to eight services"
        );
        ensure!(
            (1..=16).contains(&recipe.routes.len()) && recipe.routes.contains_key("/"),
            "recipe requires a / route and at most sixteen routes"
        );
        ensure!(
            (1..=300).contains(&recipe.health_timeout_seconds),
            "health timeout must be 1..300 seconds"
        );
        for (name, service) in &recipe.services {
            identifier(name)?;
            ensure!(name != "gate", "gate is a reserved service name");
            image(&service.image)?;
            if let Some(file) = &service.dockerfile {
                relative_path(file)?;
            }
            ensure!(
                !service.command.is_empty() && !service.command[0].is_empty(),
                "service needs a command"
            );
            ensure!(service.port != 0, "service port must be nonzero");
            let ids: Vec<_> = service.user.split(':').collect();
            ensure!(
                ids.len() == 2 && ids.iter().all(|id| id.parse::<u32>().is_ok()),
                "service user must be numeric uid:gid"
            );
            for key in service.environment.keys() {
                ensure!(
                    !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'),
                    "invalid environment name"
                );
            }
            for (name, mount) in &service.data {
                identifier(name)?;
                ensure!(
                    mount.starts_with("/app/") || mount.starts_with("/var/lib/"),
                    "data mounts must be below /app or /var/lib"
                );
                relative_path(&mount[1..])?;
            }
            match &service.health {
                Health::Http { path } => url_path(path)?,
                Health::Command { argv } => ensure!(
                    !argv.is_empty() && !argv[0].is_empty(),
                    "health command is empty"
                ),
            }
        }
        for (path, target) in &recipe.routes {
            url_path(path)?;
            ensure!(
                recipe.services.contains_key(target),
                "route references unknown service"
            );
        }
        Ok(recipe)
    }
}

fn relative_path(value: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && !value.starts_with('/')
            && value.split('/').all(|part| !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))),
        "unsafe recipe path"
    );
    Ok(())
}

fn url_path(value: &str) -> Result<()> {
    if !value.starts_with('/')
        || value.contains("..")
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_-.".contains(&b))
    {
        bail!("invalid literal route or health path");
    }
    Ok(())
}
