use super::*;
use crate::embed::fake::FakeEmbedder;

fn rag() -> Rag {
    let mut r = Rag::with_conn(
        db::open_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    r.neuron_cap = 1e9;
    r
}

fn doc(r: &mut Rag, ws: &str, path: &str, body: &str) -> i64 {
    let ws_id = r.ws_ensure(ws).unwrap();
    let d = NewDoc {
        ws_id,
        kind: Kind::Doc,
        app: Some("app"),
        path: Some(path),
        title: path,
        body,
        blob: None,
        source: None,
    };
    r.index_doc(&d, path).unwrap()
}

fn sources(hits: &[Hit]) -> Vec<String> {
    hits.iter().map(|h| h.source.clone()).collect()
}

#[test]
fn search_is_scoped_to_the_requested_subtrees() {
    let mut r = rag();
    doc(
        &mut r,
        "apps/alpha/docs",
        "deploy.md",
        "# Deploy\nrelease the alpha service with docker compose",
    );
    doc(
        &mut r,
        "apps/alphabet/docs",
        "deploy.md",
        "# Deploy\nrelease the alphabet service with docker compose",
    );
    doc(
        &mut r,
        "shared/infra",
        "docker.md",
        "# Docker\ncompose restart policy for every service",
    );

    let scope = r
        .subtree_ids(&["apps/alpha".into(), "shared".into()])
        .unwrap()
        .unwrap();
    let hits = r
        .search("release service docker compose", Some(&scope), 5)
        .unwrap();
    let got = sources(&hits);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got.contains(&"app/deploy.md#Deploy".to_string()));
    assert!(
        hits.iter().all(|h| h.ws != "apps/alphabet/docs"),
        "apps/alphabet is not under apps/alpha"
    );

    let all = r.search("release service docker compose", None, 5).unwrap();
    assert_eq!(all.len(), 3);
    let unknown = r.subtree_ids(&["apps/none".into()]).unwrap().unwrap();
    assert!(r.search("docker", Some(&unknown), 5).unwrap().is_empty());
}

#[test]
fn search_ranks_the_closest_chunk_first_and_reports_its_heading() {
    let mut r = rag();
    doc(
        &mut r,
        "apps/x/docs",
        "README.md",
        "# Install\n## zig\nbuilding needs zig 0.15.2 on the path\n# Usage\nrun nebula in a terminal",
    );
    let hits = r
        .search("which zig version does building need", None, 2)
        .unwrap();
    assert_eq!(hits[0].source, "app/README.md#zig");
    assert_eq!(hits[0].heading, "README.md > Install > zig");
    assert!(hits[0].score > hits[1].score);
}

#[test]
fn reindexing_a_document_replaces_its_chunks() {
    let mut r = rag();
    let a = doc(
        &mut r,
        "apps/x/docs",
        "a.md",
        "# One\nfirst version text about apples",
    );
    let b = doc(
        &mut r,
        "apps/x/docs",
        "a.md",
        "# One\nsecond version text about oranges",
    );
    assert_eq!(a, b);
    let n: i64 = r
        .conn
        .query_row("SELECT count(*) FROM chunks", [], |x| x.get(0))
        .unwrap();
    let v: i64 = r
        .conn
        .query_row("SELECT count(*) FROM chunk_vec", [], |x| x.get(0))
        .unwrap();
    assert_eq!((n, v), (1, 1));
    assert!(r.search_exact("apples", None, 5).unwrap().is_empty());
    assert_eq!(r.search_exact("oranges", None, 5).unwrap().len(), 1);
}

#[test]
fn exact_mode_matches_terms_and_phrases_and_ignores_fts_syntax() {
    let mut r = rag();
    doc(
        &mut r,
        "apps/x/docs",
        "a.md",
        "set NEBULA_GHOSTTY=0 to fall back to vt100",
    );
    doc(
        &mut r,
        "shared",
        "b.md",
        "fall into the vt100 parser only when needed",
    );
    assert_eq!(r.search_exact("NEBULA_GHOSTTY", None, 5).unwrap().len(), 1);
    assert_eq!(r.search_exact("vt100", None, 5).unwrap().len(), 2);
    assert_eq!(
        r.search_exact("\"fall back to vt100\"", None, 5)
            .unwrap()
            .len(),
        1
    );
    let shared = r.subtree_ids(&["shared".into()]).unwrap().unwrap();
    assert_eq!(
        r.search_exact("vt100", Some(&shared), 5).unwrap()[0].source,
        "app/b.md"
    );
    // FTS operators and stray quotes in user input stay literal.
    assert!(r.search_exact("vt100 AND (NOT \"", None, 5).is_ok());
}

#[test]
fn a_near_identical_note_updates_the_existing_one() {
    let mut r = rag();
    let ws = r.ws_ensure("apps/x/notes").unwrap();
    let text = "Deploy the web app with make deploy-web after bumping the version in Cargo.toml";
    let NoteAdded::Created(id) = r.add_note(ws, None, text, Some("s1"), true).unwrap() else {
        panic!("first note must be created");
    };
    let again = r
        .add_note(
            ws,
            Some("Deploy web"),
            &format!("{text}."),
            Some("s2"),
            true,
        )
        .unwrap();
    match again {
        NoteAdded::Updated(same, sim) => {
            assert_eq!(same, id);
            assert!(sim >= DEDUP_SIMILARITY);
        }
        other => panic!("expected an update, got {other:?}"),
    }
    let n = r.note(id).unwrap();
    assert_eq!(
        (n.title.as_str(), n.source.as_deref()),
        ("Deploy web", Some("s2"))
    );
    let other = r
        .add_note(
            ws,
            None,
            "Romanian queries need bge-m3, qwen3 lost the eval",
            None,
            true,
        )
        .unwrap();
    assert!(matches!(other, NoteAdded::Created(o) if o != id));
    // --no-dedup always creates.
    assert!(matches!(
        r.add_note(ws, None, text, None, false).unwrap(),
        NoteAdded::Created(_)
    ));
    assert_eq!(r.notes(None, false).unwrap().len(), 3);
}

#[test]
fn dedup_only_looks_at_the_target_workspace() {
    let mut r = rag();
    let a = r.ws_ensure("apps/a/notes").unwrap();
    let b = r.ws_ensure("apps/b/notes").unwrap();
    let text = "Always run cargo test with two threads on findl";
    r.add_note(a, None, text, None, true).unwrap();
    assert!(matches!(
        r.add_note(b, None, text, None, true).unwrap(),
        NoteAdded::Created(_)
    ));
}

#[test]
fn removed_notes_leave_search_and_are_purged_after_30_days() {
    let mut r = rag();
    let ws = r.ws_ensure("shared").unwrap();
    let NoteAdded::Created(id) = r
        .add_note(
            ws,
            None,
            "litestream restore needs -f to overwrite",
            None,
            true,
        )
        .unwrap()
    else {
        panic!()
    };
    r.note_rm(id).unwrap();
    assert!(r
        .search("litestream restore overwrite", None, 5)
        .unwrap()
        .is_empty());
    assert!(r.notes(None, false).unwrap().is_empty());
    assert_eq!(r.notes(None, true).unwrap().len(), 1);
    assert!(r.note_rm(id).is_err());
    assert_eq!(r.purge().unwrap(), 0, "fresh deletions stay");
    r.conn
        .execute(
            "UPDATE documents SET deleted_at = deleted_at - ?1",
            [PURGE_AFTER_SECS + 1],
        )
        .unwrap();
    assert_eq!(r.purge().unwrap(), 1);
    assert!(r.note(id).is_err());
}

#[test]
fn moving_a_note_moves_it_between_search_scopes() {
    let mut r = rag();
    let a = r.ws_ensure("apps/a/notes").unwrap();
    r.ws_ensure("shared/diverse").unwrap();
    let NoteAdded::Created(id) = r
        .add_note(
            a,
            None,
            "rsync needs a trailing slash on the source",
            None,
            true,
        )
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(r.note_mv(id, "shared/diverse").unwrap(), "shared/diverse");
    let shared = r.subtree_ids(&["shared".into()]).unwrap().unwrap();
    let apps = r.subtree_ids(&["apps".into()]).unwrap().unwrap();
    assert_eq!(
        r.search("rsync trailing slash", Some(&shared), 5)
            .unwrap()
            .len(),
        1
    );
    assert!(r
        .search("rsync trailing slash", Some(&apps), 5)
        .unwrap()
        .is_empty());
    assert!(r.note_mv(id, "shared/nope").is_err());
}

#[test]
fn editing_a_note_reindexes_it() {
    let mut r = rag();
    let ws = r.ws_ensure("shared").unwrap();
    let NoteAdded::Created(id) = r
        .add_note(
            ws,
            Some("Web UI port"),
            "port 8080 serves the web ui",
            None,
            true,
        )
        .unwrap()
    else {
        panic!()
    };
    r.note_edit(id, None, Some("port 9090 serves the web ui"), None)
        .unwrap();
    assert!(r.search_exact("8080", None, 5).unwrap().is_empty());
    assert_eq!(
        r.search_exact("9090", None, 5).unwrap()[0].source,
        format!("note:{id}")
    );
}

#[test]
fn workspace_create_refuses_near_duplicates_of_siblings() {
    let r = rag();
    r.ws_create("shared/conventions", false).unwrap();
    let err = r
        .ws_create("shared/convention", false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("shared/conventions"), "{err}");
    assert!(r.ws_create("shared/convention", true).unwrap().1);
    // Same leaf under another parent is fine; existing path is a no-op.
    assert!(r.ws_create("apps/x/conventions", false).unwrap().1);
    assert!(!r.ws_create("shared/conventions", false).unwrap().1);
}

#[test]
fn workspace_rm_needs_force_for_a_non_empty_subtree() {
    let mut r = rag();
    doc(&mut r, "apps/x/docs", "a.md", "hello world text");
    assert!(r.ws_rm("apps/x", false).is_err());
    assert_eq!(r.ws_rm("apps/x", true).unwrap(), 1);
    assert!(r.ws_id("apps/x/docs").unwrap().is_none());
    let n: i64 = r
        .conn
        .query_row("SELECT count(*) FROM chunk_vec", [], |x| x.get(0))
        .unwrap();
    assert_eq!(n, 0);
    r.ws_ensure("shared/empty").unwrap();
    assert_eq!(r.ws_rm("shared/empty", false).unwrap(), 0);
}

#[test]
fn indexing_stops_at_the_daily_neuron_cap() {
    let mut r = rag();
    r.neuron_cap = 0.0001;
    let ws = r.ws_ensure("shared").unwrap();
    let err = r
        .add_note(ws, None, "some fact worth keeping", None, true)
        .unwrap_err();
    assert!(crate::embed::is_cap(&err), "{err}");
    assert!(r.notes(None, false).unwrap().is_empty());
    // Queries are not capped.
    assert!(r.search("anything", None, 5).is_ok());
    assert!(r.neurons_today().unwrap() > 0.0);
}

#[test]
fn utc_days_and_ages() {
    assert_eq!(utc_day(0), "1970-01-01");
    assert_eq!(utc_day(1_790_121_600), "2026-09-23");
    assert_eq!(age(now() - 3 * 86_400), "3d");
}

#[test]
fn a_file_cannot_replace_a_synced_doc_of_the_same_path() {
    let mut r = rag();
    let ws_id = doc(
        &mut r,
        "apps/x/docs",
        "README.md",
        "# Readme\nfrom the repo",
    );
    let ws_id = r
        .conn
        .query_row("SELECT ws_id FROM documents WHERE id = ?1", [ws_id], |x| {
            x.get(0)
        })
        .unwrap();
    let file = NewDoc {
        ws_id,
        kind: Kind::File,
        app: None,
        path: Some("README.md"),
        title: "README.md",
        body: "local copy",
        blob: None,
        source: None,
    };
    let err = r.index_doc(&file, "README.md").unwrap_err().to_string();
    assert!(err.contains("already a doc"), "{err}");
    assert_eq!(r.search_exact("repo", None, 5).unwrap().len(), 1);
}

#[test]
fn removing_an_app_workspace_unregisters_the_app() {
    let mut r = rag();
    r.ws_ensure("apps/x/docs").unwrap();
    r.conn
        .execute(
            "INSERT INTO apps(name, repo, created) VALUES ('x', '/r', 0)",
            [],
        )
        .unwrap();
    r.ws_rm("apps/x", true).unwrap();
    assert!(r.apps().unwrap().is_empty());
}

/// Workers AI without a cost in its response.
struct NoCost;
impl Embedder for NoCost {
    fn embed(&self, texts: &[String]) -> Result<crate::embed::Embedded> {
        Ok(crate::embed::Embedded {
            vectors: texts
                .iter()
                .map(|t| crate::embed::fake::vector(t))
                .collect(),
            neurons: 0.0,
            tokens: 0,
        })
    }
}

#[test]
fn usage_falls_back_to_the_estimate_when_the_api_reports_no_cost() {
    let r = Rag::with_conn(db::open_memory().unwrap(), Box::new(NoCost));
    let texts = vec!["some text to embed".to_string(); 40];
    r.embed(&texts, true).unwrap();
    let want = estimate_neurons(&texts[..32]) + estimate_neurons(&texts[32..]);
    assert!((r.neurons_today().unwrap() - want).abs() < 1e-9);
}
