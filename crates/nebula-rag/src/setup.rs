//! `nebula rag setup`: the RAG host's runtime as systemd **user** units
//! (linger is on; same pattern as nebula-web.service), outside the nebula
//! daemon so backups survive daemon crashes and RAG updates never need
//! `nebula kill`:
//!
//! - `nebula-rag-sync.timer` → `nebula-rag-sync.service`: `nebula rag sync`
//!   every 10 minutes.
//! - `nebula-rag-litestream.service`: Litestream 0.5.17 (pinned, checksum
//!   verified) replicating rag.db to R2, credentials from rag.env.
//!
//! Writing the files is the default; enabling them is an explicit
//! `--enable`, because only one replicator may ever write an R2 prefix.

use crate::embed::env_path;
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LITESTREAM_VERSION: &str = "0.5.17";
pub const SYNC_SERVICE: &str = "nebula-rag-sync.service";
pub const SYNC_TIMER: &str = "nebula-rag-sync.timer";
pub const LITESTREAM_SERVICE: &str = "nebula-rag-litestream.service";
/// R2 key prefix the production replica lives under.
pub const DEFAULT_PREFIX: &str = "prod";

/// (arch as in the release asset name, sha256 of the tarball) from the
/// release's checksums.txt.
const LITESTREAM_SHA256: &[(&str, &str)] = &[
    (
        "x86_64",
        "cfb371176d164437ae869f8351cfde49bd1804ae71c61923f75c9cba9c9c006d",
    ),
    (
        "arm64",
        "f8ca4a050095c1efbda2c4365172e61bf9d955ea0d9ac42f448b52e51819baa5",
    ),
];

pub struct Layout {
    /// `~/.config/systemd/user`
    pub units_dir: PathBuf,
    /// nebula's data dir (holds rag.db, litestream.yml, bin/).
    pub data_dir: PathBuf,
    pub env_file: PathBuf,
    pub nebula: PathBuf,
    /// Set when the data dir is not the default (tests, parallel installs):
    /// the units must carry it.
    pub data_dir_override: bool,
}

impl Layout {
    pub fn detect(exe: Option<PathBuf>) -> Result<Self> {
        let home = PathBuf::from(std::env::var("HOME").context("HOME unset")?);
        let config = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let nebula = match exe {
            Some(e) => e,
            None => default_exe(&home)?,
        };
        Ok(Self {
            units_dir: config.join("systemd/user"),
            data_dir: nebula_core::paths::data_dir(),
            env_file: env_path(),
            nebula,
            data_dir_override: std::env::var("NEBULA_DATA_DIR").is_ok_and(|v| !v.is_empty()),
        })
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.data_dir.join("bin")
    }

    pub fn litestream(&self) -> PathBuf {
        self.bin_dir().join("litestream")
    }

    pub fn litestream_config(&self) -> PathBuf {
        self.data_dir.join("litestream.yml")
    }

    pub fn db(&self) -> PathBuf {
        self.data_dir.join("rag.db")
    }
}

/// The installed nebula, not a cargo build: units must survive `cargo
/// clean` and follow `nebula upgrade`.
fn default_exe(home: &Path) -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if exe.components().any(|c| c.as_os_str() == "target") {
        return Ok(home.join(".local/bin/nebula"));
    }
    Ok(exe)
}

pub fn sync_service(l: &Layout) -> String {
    let mut env = String::from("Environment=NEBULA_RAG_LOCAL=1\n");
    if l.data_dir_override {
        env.push_str(&format!(
            "Environment=NEBULA_DATA_DIR={}\n",
            l.data_dir.display()
        ));
    }
    format!(
        "[Unit]\n\
         Description=nebula rag sync: index apps' markdown from origin\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         {env}\
         ExecStart={} rag sync\n\
         Nice=10\n\
         TimeoutStartSec=30min\n",
        l.nebula.display()
    )
}

pub fn sync_timer() -> String {
    "[Unit]\n\
     Description=nebula rag sync every 10 minutes\n\
     \n\
     [Timer]\n\
     OnBootSec=2min\n\
     OnUnitActiveSec=10min\n\
     RandomizedDelaySec=30s\n\
     \n\
     [Install]\n\
     WantedBy=timers.target\n"
        .to_string()
}

pub fn litestream_service(l: &Layout) -> String {
    format!(
        "[Unit]\n\
         Description=Litestream: replicate nebula rag.db to R2\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         EnvironmentFile={}\n\
         ExecStart={} replicate -config {}\n\
         Restart=on-failure\n\
         RestartSec=10\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
        l.env_file.display(),
        l.litestream().display(),
        l.litestream_config().display()
    )
}

/// Litestream config; secrets stay in the environment (`${VAR}` is
/// expanded by Litestream). Only one replicator may write `prefix`.
pub fn litestream_config(db: &Path, prefix: &str) -> String {
    format!(
        "# Written by `nebula rag setup`. Secrets come from rag.env.\n\
         dbs:\n  \
           - path: {}\n    \
             replica:\n      \
               type: s3\n      \
               bucket: ${{R2_BUCKET}}\n      \
               path: {}/rag.db\n      \
               endpoint: ${{R2_ENDPOINT}}\n      \
               region: auto\n      \
               access-key-id: ${{R2_ACCESS_KEY_ID}}\n      \
               secret-access-key: ${{R2_SECRET_ACCESS_KEY}}\n",
        db.display(),
        prefix.trim_matches('/')
    )
}

/// Every file setup writes: (path, contents).
pub fn files(l: &Layout, prefix: &str) -> Vec<(PathBuf, String)> {
    vec![
        (l.units_dir.join(SYNC_SERVICE), sync_service(l)),
        (l.units_dir.join(SYNC_TIMER), sync_timer()),
        (l.units_dir.join(LITESTREAM_SERVICE), litestream_service(l)),
        (l.litestream_config(), litestream_config(&l.db(), prefix)),
    ]
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Download the pinned Litestream for this machine into `bin_dir`,
/// refusing a tarball whose sha256 differs from the pinned one.
pub fn install_litestream(bin_dir: &Path) -> Result<PathBuf> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "arm64",
        other => bail!("no pinned Litestream build for {other}"),
    };
    if std::env::consts::OS != "linux" {
        bail!("Litestream setup is for the Linux RAG host");
    }
    let target = bin_dir.join("litestream");
    if let Ok(out) = Command::new(&target).arg("version").output() {
        if String::from_utf8_lossy(&out.stdout).contains(LITESTREAM_VERSION) {
            return Ok(target);
        }
    }
    let want = LITESTREAM_SHA256
        .iter()
        .find(|(a, _)| *a == arch)
        .map(|(_, s)| *s)
        .expect("pinned");
    let name = format!("litestream-{LITESTREAM_VERSION}-linux-{arch}.tar.gz");
    let url = format!(
        "https://github.com/benbjohnson/litestream/releases/download/v{LITESTREAM_VERSION}/{name}"
    );
    std::fs::create_dir_all(bin_dir)?;
    let tmp = bin_dir.join(format!(".litestream-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    let result = (|| -> Result<()> {
        let tarball = tmp.join(&name);
        let st = Command::new("curl")
            .args(["-fsSL", "--max-time", "300", "-o"])
            .arg(&tarball)
            .arg(&url)
            .status()
            .context("run curl")?;
        if !st.success() {
            bail!("download {url} failed");
        }
        let got = sha256_hex(&std::fs::read(&tarball)?);
        if got != want {
            bail!("{name}: sha256 {got} does not match the pinned {want}");
        }
        let st = Command::new("tar")
            .arg("-xzf")
            .arg(&tarball)
            .arg("-C")
            .arg(&tmp)
            .arg("litestream")
            .status()
            .context("run tar")?;
        if !st.success() {
            bail!("extract {name} failed");
        }
        std::fs::rename(tmp.join("litestream"), &target)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result?;
    Ok(target)
}

pub struct Opts {
    pub prefix: String,
    pub dry_run: bool,
    pub enable: bool,
    pub download: bool,
    pub exe: Option<PathBuf>,
}

pub fn run(opts: Opts) -> Result<()> {
    let l = Layout::detect(opts.exe)?;
    let files = files(&l, &opts.prefix);
    if opts.dry_run {
        for (path, body) in &files {
            println!("--- {}\n{body}", path.display());
        }
        return Ok(());
    }
    if !l.env_file.exists() {
        bail!(
            "{} is missing (CF_ACCOUNT_ID, CF_AI_TOKEN, R2_*) — create it (mode 600) first",
            l.env_file.display()
        );
    }
    if !l.nebula.exists() {
        println!(
            "warning: {} does not exist yet — the units call it",
            l.nebula.display()
        );
    }
    for (path, body) in &files {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, body).with_context(|| format!("write {}", path.display()))?;
        println!("wrote {}", path.display());
    }
    if opts.download {
        let bin = install_litestream(&l.bin_dir())?;
        println!(
            "litestream {LITESTREAM_VERSION} → {} (sha256 verified)",
            bin.display()
        );
    }
    if opts.enable {
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", SYNC_TIMER, LITESTREAM_SERVICE])?;
        println!("enabled {SYNC_TIMER} and {LITESTREAM_SERVICE}");
    } else {
        println!(
            "\nnot enabled. Once approved (one replicator per R2 prefix — {}/):\n  \
             systemctl --user daemon-reload && systemctl --user enable --now {SYNC_TIMER} {LITESTREAM_SERVICE}\n\
             or: nebula rag setup --enable",
            opts.prefix
        );
    }
    Ok(())
}

fn systemctl(args: &[&str]) -> Result<()> {
    let st = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("run systemctl")?;
    if !st.success() {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(override_dir: bool) -> Layout {
        Layout {
            units_dir: "/h/.config/systemd/user".into(),
            data_dir: "/h/.local/share/nebula".into(),
            env_file: "/h/.local/share/nebula/rag.env".into(),
            nebula: "/h/.local/bin/nebula".into(),
            data_dir_override: override_dir,
        }
    }

    #[test]
    fn sync_units_run_nebula_rag_sync_every_ten_minutes() {
        let s = sync_service(&layout(false));
        assert!(s.contains("Type=oneshot\n"));
        assert!(s.contains("ExecStart=/h/.local/bin/nebula rag sync\n"));
        assert!(s.contains("Environment=NEBULA_RAG_LOCAL=1\n"));
        assert!(!s.contains("NEBULA_DATA_DIR"));
        assert!(sync_service(&layout(true))
            .contains("Environment=NEBULA_DATA_DIR=/h/.local/share/nebula\n"));
        let t = sync_timer();
        assert!(t.contains("OnUnitActiveSec=10min\n"));
        assert!(t.contains("WantedBy=timers.target\n"));
    }

    #[test]
    fn litestream_unit_reads_secrets_from_rag_env_only() {
        let l = layout(false);
        let s = litestream_service(&l);
        assert!(s.contains("EnvironmentFile=/h/.local/share/nebula/rag.env\n"));
        assert!(s.contains(
            "ExecStart=/h/.local/share/nebula/bin/litestream replicate -config /h/.local/share/nebula/litestream.yml\n"
        ));
        let c = litestream_config(&l.db(), "/_dev/");
        assert!(c.contains("- path: /h/.local/share/nebula/rag.db\n"));
        assert!(c.contains("path: _dev/rag.db\n"));
        assert!(c.contains("bucket: ${R2_BUCKET}\n"));
        assert!(c.contains("secret-access-key: ${R2_SECRET_ACCESS_KEY}\n"));
        assert!(c.contains("region: auto\n"));
    }

    #[test]
    fn setup_writes_three_units_and_the_config() {
        let l = layout(false);
        let names: Vec<String> = files(&l, DEFAULT_PREFIX)
            .iter()
            .map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            vec![
                SYNC_SERVICE,
                SYNC_TIMER,
                LITESTREAM_SERVICE,
                "litestream.yml"
            ]
        );
    }

    #[test]
    fn checksums_are_hex_sha256() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(LITESTREAM_SHA256.iter().all(|(_, s)| s.len() == 64));
    }
}
