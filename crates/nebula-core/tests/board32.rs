//! Protocol-33 board contract: the positional CreateAgent and Agent tails, the
//! pure board rules, and the JSON the CLI prints, pinned to golden fixtures that
//! web/e2e/fake_nebula.py serves.
use nebula_core::board::*;
use nebula_core::{
    Agent, AgentId, AgentKind, AgentStatus, ArtifactId, ClientRequest, DispatchId, NoteId, TodoId,
    WorktreeId, WriterClaim,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn create_agent(card: Option<&str>, role: Option<Role>, dispatch: Option<&str>) -> ClientRequest {
    ClientRequest::CreateAgent {
        req_id: 7,
        worktree: WorktreeId("w".into()),
        expected_branch: None,
        name: "worker".into(),
        kind: AgentKind::Codex,
        model: None,
        effort: None,
        auto_title: true,
        prompt: None,
        writer: WriterClaim::Off,
        spawned_by: None,
        code_mode_off: false,
        card: card.map(|c| TodoId(c.into())),
        role,
        dispatch: dispatch.map(|d| DispatchId(d.into())),
    }
}

fn wire_slots<T: Serialize>(value: &T) -> Vec<Value> {
    let wire = rmp_serde::to_vec(value).unwrap();
    let decoded: Value = rmp_serde::from_slice(&wire).unwrap();
    match decoded {
        Value::Object(variant) => match variant.into_iter().next().unwrap().1 {
            Value::Array(slots) => slots,
            other => panic!("not positional: {other}"),
        },
        Value::Array(slots) => slots,
        other => panic!("not positional: {other}"),
    }
}

fn round_trip<T: Serialize + DeserializeOwned>(value: &T) -> Value {
    let decoded: T = rmp_serde::from_slice(&rmp_serde::to_vec(value).unwrap()).unwrap();
    serde_json::to_value(decoded).unwrap()
}

#[test]
fn create_agent_tail_keeps_earlier_slots() {
    let only_dispatch = create_agent(None, None, Some("01JE0000000000000000000001"));
    let slots = wire_slots(&only_dispatch);
    assert_eq!(slots.len(), 15, "{slots:?}");
    assert_eq!(slots[14], json!("01JE0000000000000000000001"));
    assert_eq!(
        round_trip(&only_dispatch),
        serde_json::to_value(&only_dispatch).unwrap()
    );

    let only_card = create_agent(Some("01JD0000000000000000000012"), None, None);
    assert_eq!(wire_slots(&only_card).len(), 13);
    assert_eq!(
        round_trip(&only_card),
        serde_json::to_value(&only_card).unwrap()
    );

    let role = create_agent(None, Some(Role::Review), None);
    assert_eq!(wire_slots(&role)[13], json!("review"));
    assert_eq!(round_trip(&role), serde_json::to_value(&role).unwrap());

    assert_eq!(wire_slots(&create_agent(None, None, None)).len(), 9);
}

fn agent(code_mode_off: bool) -> Agent {
    Agent {
        id: AgentId("a".into()),
        worktree_id: WorktreeId("w".into()),
        name: "worker".into(),
        status: AgentStatus::Running,
        archived: false,
        archived_at: 0,
        pinned: false,
        status_changed_at: 0,
        kind: AgentKind::Codex,
        model: None,
        effort: None,
        session_id: None,
        sort_order: 0,
        alive: true,
        branch: None,
        spawned_by: None,
        writer: false,
        card_id: Some(TodoId("01JD0000000000000000000012".into())),
        role: Some(Role::Worker),
        dispatch_id: Some(DispatchId("01JE0000000000000000000001".into())),
        code_mode_off,
    }
}

#[test]
fn agent_tail_code_mode_off_last() {
    let card_tail = [
        json!("01JD0000000000000000000012"),
        json!("worker"),
        json!("01JE0000000000000000000001"),
    ];
    let flagged = wire_slots(&agent(true));
    let n = flagged.len();
    assert_eq!(flagged[n - 1], json!(true), "{flagged:?}");
    assert_eq!(flagged[n - 4..n - 1], card_tail, "{flagged:?}");
    assert_eq!(
        round_trip(&agent(true)),
        serde_json::to_value(agent(true)).unwrap()
    );

    let plain = wire_slots(&agent(false));
    assert_eq!(plain.len(), n - 1);
    assert_eq!(plain[n - 4..], card_tail, "{plain:?}");
    assert_eq!(
        round_trip(&agent(false)),
        serde_json::to_value(agent(false)).unwrap()
    );
}

#[test]
fn parse_card_ref_forms() {
    let number = |n: u64, board: Option<&str>| CardRef::Number {
        number: n,
        board: board.map(String::from),
    };
    let ulid = "01JD0000000000000000000012";
    let accepted = [
        ("#12", number(12, None)),
        ("12", number(12, None)),
        ("12@leaseweb", number(12, Some("leaseweb"))),
        ("#12@leaseweb", number(12, Some("leaseweb"))),
        (ulid, CardRef::Id(TodoId(ulid.into()))),
        (
            "7ZZZZZZZZZZZZZZZZZZZZZZZZZ",
            CardRef::Id(TodoId("7ZZZZZZZZZZZZZZZZZZZZZZZZZ".into())),
        ),
    ];
    for (text, expected) in accepted {
        assert_eq!(parse_card_ref(text), Some(expected), "{text}");
    }
    for text in [
        "#",
        "12@",
        "@leaseweb",
        "",
        "0",
        "#0",
        "-1",
        "1.5",
        "12@lease web",
        "12@a@b",
        "abc",
        "01JD000000000000000000001",
        "#01JD0000000000000000000012",
        "81JD0000000000000000000012",
        "ZZZZZZZZZZZZZZZZZZZZZZZZZZ",
    ] {
        assert_eq!(parse_card_ref(text), None, "{text:?} must be refused");
    }
    assert_eq!(card_label(12, None), "#12");
    assert_eq!(card_label(12, Some("leaseweb")), "#12@leaseweb");
}

fn row(id: &str, role: Option<Role>, status: AgentStatus, alive: bool) -> SessionRow {
    SessionRow {
        agent_id: AgentId(id.into()),
        host: "leaseweb".into(),
        card_id: TodoId("01JD0000000000000000000012".into()),
        role,
        kind: AgentKind::Codex,
        name: id.into(),
        parent: None,
        status,
        status_changed_at: 0,
        writer: false,
        alive,
        archived: false,
        branch: None,
        dispatch_id: None,
    }
}

type StageCase = (
    &'static str,
    Stage,
    Option<SessionRow>,
    SessionRow,
    Vec<SessionRow>,
    Option<Stage>,
);

#[test]
fn auto_stage_transitions() {
    use AgentStatus::{Finished, NeedsFeedback, Running};
    use Stage::*;
    let worker = |status| row("w1", Some(Role::Worker), status, true);
    let reviewer = |status| row("r1", Some(Role::Review), status, true);
    let other_worker = |status, alive| row("w2", Some(Role::Worker), status, alive);
    let dead_worker = |status| row("w1", Some(Role::Worker), status, false);
    let archived_worker = |status| SessionRow {
        archived: true,
        ..worker(status)
    };
    let at = |row: SessionRow, status_changed_at| SessionRow {
        status_changed_at,
        ..row
    };
    let cases: &[StageCase] = &[
        (
            "worker appears on backlog",
            Backlog,
            None,
            worker(Running),
            vec![],
            Some(Doing),
        ),
        (
            "worker appears on next",
            Next,
            None,
            worker(Running),
            vec![],
            Some(Doing),
        ),
        (
            "worker appears on doing",
            Doing,
            None,
            worker(Running),
            vec![],
            None,
        ),
        (
            "worker appears on review",
            Review,
            None,
            worker(Running),
            vec![],
            None,
        ),
        (
            "worker appears on done",
            Done,
            None,
            worker(Running),
            vec![],
            None,
        ),
        (
            "reviewer appears on backlog",
            Backlog,
            None,
            reviewer(Running),
            vec![],
            None,
        ),
        (
            "roleless session appears",
            Backlog,
            None,
            row("x", None, Running, true),
            vec![],
            None,
        ),
        (
            "last worker finishes on doing",
            Doing,
            Some(worker(Running)),
            worker(Finished),
            vec![],
            Some(Review),
        ),
        (
            "worker finishes beside a live running worker",
            Doing,
            Some(worker(Running)),
            worker(Finished),
            vec![worker(Finished), other_worker(Running, true)],
            None,
        ),
        (
            "worker finishes beside a worker that needs feedback",
            Doing,
            Some(worker(Running)),
            worker(Finished),
            vec![other_worker(NeedsFeedback, true)],
            None,
        ),
        (
            "worker finishes beside a dead unfinished worker",
            Doing,
            Some(worker(Running)),
            worker(Finished),
            vec![other_worker(Running, false)],
            Some(Review),
        ),
        (
            "worker finishes beside a live running reviewer",
            Doing,
            Some(worker(Running)),
            worker(Finished),
            vec![reviewer(Running)],
            Some(Review),
        ),
        (
            "duplicate finished report",
            Doing,
            Some(worker(Finished)),
            worker(Finished),
            vec![],
            None,
        ),
        (
            "worker finishes on next",
            Next,
            Some(worker(Running)),
            worker(Finished),
            vec![],
            None,
        ),
        (
            "worker finishes on done",
            Done,
            Some(worker(Running)),
            worker(Finished),
            vec![],
            None,
        ),
        (
            "worker finishes on review",
            Review,
            Some(worker(Running)),
            worker(Finished),
            vec![],
            None,
        ),
        (
            "reviewer finishes on doing",
            Doing,
            Some(reviewer(Running)),
            reviewer(Finished),
            vec![],
            None,
        ),
        (
            "worker appears finished on backlog",
            Backlog,
            None,
            worker(Finished),
            vec![],
            Some(Review),
        ),
        (
            "worker appears finished and dead on next",
            Next,
            None,
            dead_worker(Finished),
            vec![],
            Some(Review),
        ),
        (
            "worker appears finished on backlog beside a live running worker",
            Backlog,
            None,
            worker(Finished),
            vec![other_worker(Running, true)],
            None,
        ),
        (
            "archived worker appears finished on backlog",
            Backlog,
            None,
            archived_worker(Finished),
            vec![],
            None,
        ),
        (
            "dead worker appears on next",
            Next,
            None,
            dead_worker(Running),
            vec![],
            None,
        ),
        (
            "archived worker appears on backlog",
            Backlog,
            None,
            archived_worker(Running),
            vec![],
            None,
        ),
        (
            "worker appears finished on doing",
            Doing,
            None,
            worker(Finished),
            vec![],
            Some(Review),
        ),
        (
            "worker appears finished on doing beside a live running worker",
            Doing,
            None,
            worker(Finished),
            vec![other_worker(Running, true)],
            None,
        ),
        (
            "late finished replay older than the running row",
            Doing,
            Some(at(worker(Running), 10)),
            at(worker(Finished), 5),
            vec![],
            None,
        ),
        (
            "archived worker finishes on doing",
            Doing,
            Some(archived_worker(Running)),
            archived_worker(Finished),
            vec![],
            None,
        ),
        (
            "dead worker finishes on doing",
            Doing,
            Some(dead_worker(Running)),
            dead_worker(Finished),
            vec![],
            None,
        ),
        (
            "worker finishes and exits in one report on doing",
            Doing,
            Some(worker(Running)),
            dead_worker(Finished),
            vec![],
            Some(Review),
        ),
        (
            "worker appears finished and dead on doing",
            Doing,
            None,
            dead_worker(Finished),
            vec![],
            Some(Review),
        ),
        (
            "archived worker appears finished on doing",
            Doing,
            None,
            archived_worker(Finished),
            vec![],
            None,
        ),
        (
            "worker resumes on next after a manual move",
            Next,
            Some(worker(Finished)),
            worker(Running),
            vec![],
            None,
        ),
    ];
    let wrong: Vec<_> = cases
        .iter()
        .filter_map(|(name, stage, before, after, others, expected)| {
            let got = auto_stage(*stage, before.as_ref(), after, others);
            (got != *expected).then(|| format!("{name}: got {got:?}, want {expected:?}"))
        })
        .collect();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

fn e2e_files(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

#[test]
fn e2e_run_validate() {
    let result = include_str!("fixtures/board/e2e_run_result.json");
    let all = e2e_files(&["result.json", "shots/drag.png", "shots/run.png"]);
    let run = validate_e2e_run(result, &all).unwrap();
    assert_eq!(run.steps.len(), 2);
    assert_eq!(run.status, RunStatus::Failed);

    assert!(validate_e2e_run(result, &e2e_files(&["result.json", "shots/drag.png"])).is_err());

    let mut unknown: Value = serde_json::from_str(result).unwrap();
    unknown["steps"][0]["status"] = json!("flaky");
    assert!(validate_e2e_run(&unknown.to_string(), &all).is_err());

    for field in ["title", "status", "started_at", "duration_ms", "steps"] {
        let mut missing: Value = serde_json::from_str(result).unwrap();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            validate_e2e_run(&missing.to_string(), &all).is_err(),
            "{field}"
        );
    }
    let mut no_step_name: Value = serde_json::from_str(result).unwrap();
    no_step_name["steps"][1]
        .as_object_mut()
        .unwrap()
        .remove("name");
    assert!(validate_e2e_run(&no_step_name.to_string(), &all).is_err());

    let object: Value = serde_json::from_str(result).unwrap();
    let positional = json!([
        object["title"],
        object["status"],
        object["started_at"],
        object["duration_ms"],
        object["steps"],
    ]);
    assert!(validate_e2e_run(&positional.to_string(), &all).is_err());

    let mut positional_step = object.clone();
    let step = &object["steps"][0];
    positional_step["steps"][0] = json!([step["name"], step["status"], step["duration_ms"]]);
    assert!(validate_e2e_run(&positional_step.to_string(), &all).is_err());
}

fn author() -> Author {
    Author {
        host: "perseu-mbp".into(),
        agent: Some(AgentId("01JF0000000000000000000001".into())),
        name: "AH 02 Contract Worker".into(),
    }
}

fn checkout() -> Checkout {
    Checkout {
        host: "leaseweb".into(),
        branch: "agent-home-02".into(),
        path: "/home/perseu/nebula-worktrees/agent-home-02".into(),
        worktree: true,
    }
}

fn worker_link() -> SessionLink {
    SessionLink {
        host: "leaseweb".into(),
        id: AgentId("01JF0000000000000000000002".into()),
        name: "worker".into(),
    }
}

fn worker_at() -> SessionAt {
    SessionAt {
        host: "leaseweb".into(),
        id: AgentId("01JF0000000000000000000002".into()),
    }
}

const CARD: &str = "01JD0000000000000000000012";

fn card() -> Card {
    Card {
        id: TodoId(CARD.into()),
        number: 12,
        label: "#12@leaseweb".into(),
        title: "Keep verification results".into(),
        stage: Stage::Doing,
        project: Some("nebula".into()),
        proposed: false,
        archived: false,
        created_by: author(),
        checkout: Some(checkout()),
        blocked_by: vec!["#11@leaseweb".into()],
    }
}

fn artifact() -> Artifact {
    Artifact {
        id: ArtifactId("01JG0000000000000000000001".into()),
        kind: "e2e-run".into(),
        title: "board.spec".into(),
        card: Some(TodoId(CARD.into())),
        project: Some("nebula".into()),
        session: Some(worker_link()),
        checkout: Some(checkout()),
        content: ArtifactContent::Files {
            host: "leaseweb".into(),
            entry: "result.json".into(),
        },
        meta: Default::default(),
        created_at: 1764000300000,
    }
}

fn decision() -> Artifact {
    Artifact {
        id: ArtifactId("01JG0000000000000000000002".into()),
        kind: "decision".into(),
        title: "Copy published files".into(),
        card: None,
        project: Some("nebula".into()),
        session: Some(worker_link()),
        checkout: None,
        content: ArtifactContent::Body {
            body: "The checkout is disposable".into(),
        },
        meta: json!({"rag_note": 1412}).as_object().unwrap().clone(),
        created_at: 1764000400000,
    }
}

fn assert_golden<T: Serialize + DeserializeOwned>(name: &str, fixture: &str, value: T) {
    let expected: Value = serde_json::from_str(fixture).unwrap();
    assert_eq!(
        serde_json::to_value(&value).unwrap(),
        expected,
        "{name}: Rust -> JSON"
    );
    let parsed: T = serde_json::from_str(fixture).unwrap();
    assert_eq!(
        serde_json::to_value(&parsed).unwrap(),
        expected,
        "{name}: JSON -> Rust"
    );
    assert_eq!(
        round_trip(&value),
        expected,
        "{name}: MessagePack round trip"
    );
}

#[test]
fn board_json_golden() {
    assert_golden("card", include_str!("fixtures/board/card.json"), card());
    assert_golden(
        "card_show",
        include_str!("fixtures/board/card_show.json"),
        CardShow {
            card: card(),
            sessions: vec![CardSession {
                host: "leaseweb".into(),
                id: AgentId("01JF0000000000000000000002".into()),
                name: "worker".into(),
                role: Some(Role::Worker),
                kind: AgentKind::Codex,
                status: AgentStatus::Running,
                status_changed_at: 1764000200000,
                writer: true,
                alive: true,
                stale: false,
            }],
            artifacts: vec![artifact()],
            log: vec![CardNote {
                id: NoteId("01JH0000000000000000000001".into()),
                card: TodoId(CARD.into()),
                text: "Started on leaseweb".into(),
                author: author(),
                created_at: 1764000100000,
            }],
        },
    );
    assert_golden(
        "board",
        include_str!("fixtures/board/board.json"),
        BoardPoll::Board {
            version: "01JB0000000000000000000000.53".into(),
            board: BoardPage {
                cards: vec![card()],
                hosts: vec![BoardHost {
                    host: "leaseweb".into(),
                    writers: 1,
                    writer_slots: None,
                    checkouts: vec![HostCheckout {
                        project: "nebula".into(),
                        branch: "main".into(),
                        path: "/home/perseu/nebula".into(),
                        worktree: false,
                    }],
                    needs_you: 0,
                    stale: false,
                }],
                away: Away {
                    finished: vec![TodoId(CARD.into())],
                    needs_you: vec![worker_at()],
                    decisions: vec![ArtifactId("01JG0000000000000000000002".into())],
                },
            },
            next: None,
        },
    );
    assert_golden(
        "board_unchanged",
        include_str!("fixtures/board/board_unchanged.json"),
        BoardPoll::Unchanged {
            version: "01JB0000000000000000000000.52".into(),
        },
    );
    assert_golden(
        "artifact",
        include_str!("fixtures/board/artifact.json"),
        artifact(),
    );
    assert_golden(
        "decision",
        include_str!("fixtures/board/decision.json"),
        decision(),
    );
    assert_golden(
        "run",
        include_str!("fixtures/board/run.json"),
        RunReceipt {
            dispatch: DispatchId("01JE0000000000000000000001".into()),
            state: DispatchState::Started,
            session: Some(worker_at()),
            card: TodoId(CARD.into()),
        },
    );

    let url = Artifact {
        content: ArtifactContent::Url {
            url: "https://vela.possu.dev".into(),
        },
        ..decision()
    };
    assert_eq!(
        serde_json::to_value(&url).unwrap()["content"],
        json!({"url": "https://vela.possu.dev"})
    );
    assert_eq!(round_trip(&url), serde_json::to_value(&url).unwrap());
    let continued = BoardPoll::Board {
        version: "01JB0000000000000000000000.53".into(),
        board: BoardPage {
            cards: vec![],
            hosts: vec![],
            away: Away::default(),
        },
        next: Some("01JB0000000000000000000000.53:1".into()),
    };
    assert_eq!(
        round_trip(&continued),
        serde_json::to_value(&continued).unwrap()
    );
}

#[test]
fn board_request_and_reply_cross_the_wire() {
    let ops = [
        BoardOp::Read(BoardRead::Board {
            scope: BoardScope {
                projects: ProjectScope::Named(vec!["nebula".into()]),
                include_home: true,
            },
            since: Some("01JB0000000000000000000000.52".into()),
            away_since: None,
            page: None,
        }),
        BoardOp::Publish(artifact()),
        BoardOp::Report(HostReport {
            host: "leaseweb".into(),
            writers: 1,
            writer_slots: None,
            checkouts: vec![],
            sessions: vec![row("w1", Some(Role::Worker), AgentStatus::Running, true)],
        }),
    ];
    for op in ops {
        let request = ClientRequest::Board { req_id: 9, op };
        assert_eq!(
            round_trip(&request),
            serde_json::to_value(&request).unwrap()
        );
    }
    let reply = nebula_core::ServerEvent::BoardReply {
        req_id: 9,
        result: BoardResult::Show(Box::new(CardShow {
            card: card(),
            sessions: vec![],
            artifacts: vec![decision()],
            log: vec![],
        })),
    };
    assert_eq!(round_trip(&reply), serde_json::to_value(&reply).unwrap());
}
