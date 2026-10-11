use super::*;
use crate::db;
use crate::embed::fake::FakeEmbedder;

fn rag() -> Rag {
    let mut r = Rag::with_conn(
        db::open_memory().unwrap(),
        Box::new(FakeEmbedder::default()),
    );
    r.neuron_cap = 1e9;
    r
}

fn add(r: &mut Rag, app: Option<&str>, category: Category, text: &str) -> i64 {
    r.obs_add(app, category, None, text, Some("s1")).unwrap().id
}

fn ids(r: &Rag, filter: ObsFilter) -> Vec<i64> {
    r.obs_list(&filter).unwrap().iter().map(|o| o.id).collect()
}

fn with_status(status: Status) -> ObsFilter {
    ObsFilter {
        status: Some(status),
        ..ObsFilter::default()
    }
}

fn counts(pairs: &[(&str, usize)]) -> BTreeMap<String, usize> {
    pairs.iter().map(|(k, n)| (k.to_string(), *n)).collect()
}

#[test]
fn the_same_text_saved_twice_is_two_open_observations() {
    let mut r = rag();
    let text = "cargo test ran twice because the first filter matched nothing";
    let a = r
        .obs_add(Some("nebula"), Category::Tool, None, text, Some("s1"))
        .unwrap();
    let b = r
        .obs_add(Some("nebula"), Category::Tool, None, text, Some("s1"))
        .unwrap();
    assert_ne!(a.id, b.id);
    assert_eq!(
        (a.ws.as_str(), a.app.as_str(), a.title.as_str()),
        ("apps/nebula/observations", "nebula", text)
    );
    assert_eq!((a.category, a.status), (Category::Tool, Status::Open));
    assert_eq!((a.resolution, a.closed_at), (None, None));
    assert_eq!(ids(&r, with_status(Status::Open)), vec![b.id, a.id]);
}

#[test]
fn outside_an_app_an_observation_goes_to_shared() {
    let mut r = rag();
    let o = r
        .obs_add(
            None,
            Category::Skill,
            Some("Skill missed"),
            "body text",
            None,
        )
        .unwrap();
    assert_eq!(
        (o.ws.as_str(), o.app.as_str(), o.title.as_str()),
        ("shared/observations", "shared", "Skill missed")
    );
}

#[test]
fn done_records_the_resolution_and_reopen_clears_it() {
    let mut r = rag();
    let id = add(
        &mut r,
        Some("nebula"),
        Category::Rework,
        "the migration test was rewritten after review",
    );
    let other = add(
        &mut r,
        Some("nebula"),
        Category::Tool,
        "rg missed a symlink",
    );

    let done = r
        .obs_apply(id, Transition::Done(Some("added a lint for it")))
        .unwrap();
    assert_eq!(done.status, Status::Done);
    assert_eq!(done.resolution.as_deref(), Some("added a lint for it"));
    let closed_at = done.closed_at.expect("a closed observation has a time");
    assert!((db::now() - closed_at).abs() <= 2, "{closed_at}");

    assert_eq!(ids(&r, with_status(Status::Open)), vec![other]);
    assert_eq!(ids(&r, with_status(Status::Done)), vec![id]);
    assert_eq!(ids(&r, ObsFilter::default()), vec![other, id]);
    assert_eq!(
        r.search_exact("rewritten", None, 5).unwrap()[0].doc_id,
        id,
        "a closed observation stays searchable"
    );

    let again = r.obs_apply(id, Transition::Done(None)).unwrap();
    assert_eq!(
        again.resolution.as_deref(),
        Some("added a lint for it"),
        "repeating done without a note keeps the resolution"
    );

    let reopened = r.obs_apply(id, Transition::Reopen).unwrap();
    assert_eq!(reopened.status, Status::Open);
    assert_eq!((reopened.resolution, reopened.closed_at), (None, None));
    assert_eq!(ids(&r, with_status(Status::Open)), vec![other, id]);
    assert!(ids(&r, with_status(Status::Done)).is_empty());
}

#[test]
fn dismiss_is_its_own_status() {
    let mut r = rag();
    let id = add(&mut r, None, Category::Search, "grep found nothing useful");
    let o = r
        .obs_apply(id, Transition::Dismiss(Some("one-off")))
        .unwrap();
    assert_eq!(o.status, Status::Dismissed);
    assert_eq!(o.resolution.as_deref(), Some("one-off"));
    assert!(o.closed_at.is_some());
    assert!(ids(&r, with_status(Status::Open)).is_empty());
    assert!(ids(&r, with_status(Status::Done)).is_empty());
    assert_eq!(ids(&r, with_status(Status::Dismissed)), vec![id]);
    assert_eq!(ids(&r, ObsFilter::default()), vec![id]);

    let reopened = r.obs_apply(id, Transition::Reopen).unwrap();
    assert_eq!(reopened.status, Status::Open);
    assert_eq!((reopened.resolution, reopened.closed_at), (None, None));
}

#[test]
fn list_filters_by_category_app_and_since() {
    let mut r = rag();
    let tool = add(
        &mut r,
        Some("nebula"),
        Category::Tool,
        "ssh hop dropped a quote",
    );
    let skill = add(
        &mut r,
        Some("vela"),
        Category::Skill,
        "skill trigger never fired",
    );
    let shared = add(
        &mut r,
        None,
        Category::Tool,
        "curl timed out on a slow host",
    );
    r.conn
        .execute(
            "UPDATE documents SET created = ?2 WHERE id = ?1",
            rusqlite::params![skill, db::now() - 10 * 86_400],
        )
        .unwrap();

    let by = |filter: ObsFilter| ids(&r, filter);
    assert_eq!(
        by(ObsFilter {
            category: Some(Category::Tool),
            ..ObsFilter::default()
        }),
        vec![shared, tool]
    );
    assert_eq!(
        by(ObsFilter {
            app: Some("vela".into()),
            ..ObsFilter::default()
        }),
        vec![skill]
    );
    assert_eq!(
        by(ObsFilter {
            app: Some("shared".into()),
            ..ObsFilter::default()
        }),
        vec![shared]
    );
    assert_eq!(
        by(ObsFilter {
            since: Some(since_cutoff("7d", db::now()).unwrap()),
            ..ObsFilter::default()
        }),
        vec![shared, tool]
    );
    assert_eq!(by(ObsFilter::default()), vec![shared, tool, skill]);
}

#[test]
fn since_reads_days_hours_and_dates() {
    assert_eq!(since_cutoff("7d", 1_000_000).unwrap(), 395_200);
    assert_eq!(since_cutoff("24h", 1_000_000).unwrap(), 913_600);
    assert_eq!(since_cutoff("2026-09-23", 0).unwrap(), 1_790_121_600);
    assert_eq!(since_cutoff("1970-01-01", 0).unwrap(), 0);
    for day in ["2024-02-29", "2025-12-31", "2026-03-01"] {
        assert_eq!(
            crate::store::utc_day(since_cutoff(day, 0).unwrap()),
            day,
            "a date reads back as the same UTC day"
        );
    }
    assert_eq!(
        since_cutoff("soon", 0).unwrap_err().to_string(),
        "bad --since \"soon\" (7d, 24h or 2026-10-01)"
    );
    assert!(since_cutoff("2026-13-01", 0).is_err());
}

#[test]
fn since_refuses_what_is_not_a_real_date() {
    for bad in ["99999999999999-01-01", "2026-02-31", "11-10-26"] {
        assert_eq!(
            since_cutoff(bad, 0).unwrap_err().to_string(),
            format!("bad --since {bad:?} (7d, 24h or 2026-10-01)")
        );
    }
}

#[test]
fn the_app_filter_reads_a_name_the_way_add_does() {
    let mut r = rag();
    let id = add(&mut r, Some("Nebula"), Category::Tool, "a tool failed once");
    add(&mut r, Some("vela"), Category::Tool, "another tool failed");
    assert_eq!(
        ids(
            &r,
            ObsFilter {
                app: Some("Nebula".into()),
                ..ObsFilter::default()
            }
        ),
        vec![id]
    );
}

#[test]
fn closing_again_keeps_the_first_time_and_changing_the_status_resets_it() {
    let mut r = rag();
    let id = add(&mut r, None, Category::Tool, "the build cache went stale");
    r.obs_apply(id, Transition::Done(Some("cleared the cache")))
        .unwrap();
    r.conn
        .execute("UPDATE documents SET closed_at = 1000 WHERE id = ?1", [id])
        .unwrap();

    let again = r
        .obs_apply(id, Transition::Done(Some("and added a check")))
        .unwrap();
    assert_eq!(again.closed_at, Some(1000));
    assert_eq!(again.resolution.as_deref(), Some("and added a check"));

    let dismissed = r.obs_apply(id, Transition::Dismiss(None)).unwrap();
    assert_eq!(dismissed.status, Status::Dismissed);
    assert_eq!(dismissed.resolution, None);
    let closed_at = dismissed
        .closed_at
        .expect("a closed observation has a time");
    assert!((db::now() - closed_at).abs() <= 2, "{closed_at}");
}

#[test]
fn an_empty_observation_is_refused_by_that_name() {
    let mut r = rag();
    assert_eq!(
        r.obs_add(None, Category::Tool, None, "   ", None)
            .unwrap_err()
            .to_string(),
        "empty observation"
    );
}

#[test]
fn an_unknown_category_and_a_plain_note_are_refused() {
    assert_eq!("rework".parse::<Category>().unwrap(), Category::Rework);
    assert_eq!(
        "bug".parse::<Category>().unwrap_err().to_string(),
        "unknown category \"bug\" (one of: correction, rework, search, skill, tool)"
    );
    assert_eq!(
        "closed".parse::<Status>().unwrap_err().to_string(),
        "unknown status \"closed\" (one of: open, done, dismissed)"
    );

    let mut r = rag();
    let ws = r.ws_ensure("apps/x/notes").unwrap();
    let crate::NoteAdded::Created(note) = r
        .add_note(ws, None, "deploy with make deploy-web", None, true)
        .unwrap()
    else {
        panic!("first note must be created");
    };
    let refused = format!("note:{note} is a note, not an observation");
    assert_eq!(r.obs(note).unwrap_err().to_string(), refused);
    assert_eq!(
        r.obs_apply(note, Transition::Done(None))
            .unwrap_err()
            .to_string(),
        refused
    );
    assert_eq!(r.obs(999).unwrap_err().to_string(), "no obs:999");
    assert!(r
        .obs_add(Some("a/b"), Category::Tool, None, "text", None)
        .is_err());
}

#[test]
fn stats_count_by_status_category_and_app() {
    let mut r = rag();
    let a = add(
        &mut r,
        Some("nebula"),
        Category::Tool,
        "first tool failure here",
    );
    add(
        &mut r,
        Some("nebula"),
        Category::Tool,
        "second tool failure here",
    );
    let c = add(
        &mut r,
        Some("vela"),
        Category::Rework,
        "review sent the work back",
    );
    let old = add(&mut r, None, Category::Skill, "an old skill miss");
    r.obs_apply(a, Transition::Done(None)).unwrap();
    r.obs_apply(c, Transition::Dismiss(None)).unwrap();
    r.conn
        .execute(
            "UPDATE documents SET created = ?2 WHERE id = ?1",
            rusqlite::params![old, db::now() - 30 * 86_400],
        )
        .unwrap();

    assert_eq!(
        r.obs_stats(Some(db::now() - 7 * 86_400)).unwrap(),
        ObsStats {
            total: 3,
            by_status: counts(&[("open", 1), ("done", 1), ("dismissed", 1)]),
            by_category: counts(&[("tool", 2), ("rework", 1)]),
            by_app: counts(&[("nebula", 2), ("vela", 1)]),
        }
    );
    let all = r.obs_stats(None).unwrap();
    assert_eq!(all.total, 4);
    assert_eq!(
        all.by_app,
        counts(&[("nebula", 2), ("vela", 1), ("shared", 1)])
    );
}

#[test]
fn a_note_saved_beside_an_observation_does_not_overwrite_it() {
    let mut r = rag();
    let text = "the ssh hop dropped a quoted argument on the way to findl";
    let obs = r
        .obs_add(Some("nebula"), Category::Tool, None, text, None)
        .unwrap();
    let ws = r.ws_require("apps/nebula/observations").unwrap();
    let crate::NoteAdded::Created(note) = r.add_note(ws, None, text, None, true).unwrap() else {
        panic!("a note identical to an observation must be a new note");
    };
    assert_ne!(note, obs.id);
    assert_eq!(r.obs(obs.id).unwrap().status, Status::Open);
}
