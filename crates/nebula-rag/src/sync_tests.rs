use super::*;
use crate::db;
use crate::embed::fake::FakeEmbedder;
use std::path::PathBuf;

struct Fixture {
    _dir: tempfile::TempDir,
    /// Where commits are made and pushed from (the laptop, say).
    work: PathBuf,
    /// The checkout the app points at (findl's).
    checkout: PathBuf,
    rag: Rag,
}

fn sh(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "init.defaultBranch=main",
        ])
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let root = root.as_path();
        sh(root, &["init", "-q", "--bare", "origin.git"]);
        sh(root, &["clone", "-q", "origin.git", "work"]);
        let work = root.join("work");
        let mut f = Fixture {
            work,
            checkout: root.join("checkout"),
            rag: {
                let mut r = Rag::with_conn(
                    db::open_memory().unwrap(),
                    Box::new(FakeEmbedder::default()),
                );
                r.neuron_cap = 1e9;
                r
            },
            _dir: dir,
        };
        f.commit(files, &[]);
        sh(root, &["clone", "-q", "origin.git", "checkout"]);
        f.rag.app_add("demo", &f.checkout, None, false).unwrap();
        f
    }

    fn commit(&mut self, files: &[(&str, &[u8])], deletes: &[&str]) {
        for (path, body) in files {
            let p = self.work.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
        for d in deletes {
            std::fs::remove_file(self.work.join(d)).unwrap();
        }
        sh(&self.work, &["add", "-A"]);
        sh(&self.work, &["commit", "-q", "-m", "c"]);
        sh(&self.work, &["push", "-q", "origin", "HEAD:main"]);
    }

    fn sync(&mut self) -> Result<Report> {
        let app = self.rag.app("demo").unwrap();
        sync_app(&mut self.rag, &app, false)
    }

    fn docs(&self) -> Vec<String> {
        let mut st = self
            .rag
            .conn
            .prepare("SELECT path FROM documents WHERE app = 'demo' ORDER BY path")
            .unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }
}

#[test]
fn indexes_committed_markdown_on_origin_not_the_working_tree() {
    let big = vec![b'a'; MAX_FILE_BYTES as usize + 1];
    let mut f = Fixture::new(&[
        ("README.md", b"# Demo\nhow to deploy the demo app"),
        ("docs/spec.md", b"# Spec\nsettled decisions"),
        ("docs/drafts/wip.md", b"# WIP\nignored by ragignore"),
        (".ragignore", b"drafts/\n"),
        ("vendor/lib/README.md", b"# vendored\nnot ours"),
        ("src/main.rs", b"fn main() {}"),
        ("blob.md", b"bin\0ary"),
        ("huge.md", &big),
    ]);
    // Uncommitted work in the findl checkout must never be indexed.
    std::fs::write(
        f.checkout.join("README.md"),
        "# Demo\nSOMEONE ELSE'S UNCOMMITTED EDIT",
    )
    .unwrap();
    std::fs::write(f.checkout.join("local.md"), "# untracked").unwrap();

    let r = f.sync().unwrap();
    assert_eq!((r.indexed, r.removed, r.skipped), (2, 0, 2), "{r:?}");
    assert_eq!(f.docs(), vec!["README.md", "docs/spec.md"]);
    assert!(f
        .rag
        .search_exact("UNCOMMITTED", None, 5)
        .unwrap()
        .is_empty());
    let hit = &f.rag.search_exact("deploy", None, 5).unwrap()[0];
    assert_eq!(
        (hit.ws.as_str(), hit.source.as_str()),
        ("apps/demo/docs", "demo/README.md#Demo")
    );

    assert!(f.sync().unwrap().up_to_date);
}

#[test]
fn later_syncs_only_touch_the_files_that_changed() {
    let mut f = Fixture::new(&[
        ("a.md", b"# A\nalpha one"),
        ("b.md", b"# B\nbravo"),
        ("c.md", b"# C\ncharlie"),
    ]);
    f.sync().unwrap();
    f.commit(
        &[("a.md", b"# A\nalpha two"), ("d.md", b"# D\ndelta")],
        &["b.md"],
    );
    let r = f.sync().unwrap();
    assert_eq!((r.indexed, r.removed, r.unchanged), (2, 1, 0), "{r:?}");
    assert_eq!(f.docs(), vec!["a.md", "c.md", "d.md"]);
    assert!(f.rag.search_exact("one", None, 5).unwrap().is_empty());
    assert_eq!(f.rag.search_exact("two", None, 5).unwrap().len(), 1);
}

#[test]
fn a_ragignore_change_reconciles_every_file() {
    let mut f = Fixture::new(&[("a.md", b"# A\nalpha"), ("notes/b.md", b"# B\nbravo")]);
    f.sync().unwrap();
    f.commit(&[(".ragignore", b"notes/\n")], &[]);
    let r = f.sync().unwrap();
    assert_eq!((r.removed, r.unchanged), (1, 1), "{r:?}");
    assert_eq!(f.docs(), vec!["a.md"]);
}

#[test]
fn a_sync_stopped_by_the_neuron_cap_resumes_without_redoing_work() {
    let mut f = Fixture::new(&[("a.md", b"# A\nalpha"), ("b.md", b"# B\nbravo")]);
    // Room for the first file only.
    f.rag.neuron_cap =
        crate::embed::estimate_neurons(&["demo/a.md > A\n\nalpha".to_string()]) * 1.5;
    let err = f.sync().unwrap_err();
    assert!(is_cap(&err), "{err}");
    assert_eq!(f.docs(), vec!["a.md"]);
    assert!(
        f.rag.app("demo").unwrap().last_commit.is_none(),
        "an unfinished run must not advance"
    );

    f.rag.neuron_cap = 1e9;
    let r = f.sync().unwrap();
    assert_eq!((r.indexed, r.unchanged), (1, 1), "{r:?}");
    assert_eq!(f.docs(), vec!["a.md", "b.md"]);
}
