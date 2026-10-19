//! Unix-socket server: accept loop, per-client request handling, PTY
//! attach/forward plumbing.

use crate::pty::PtyEvent;
use crate::registry::Daemon;
use anyhow::Result;
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::{ClientRequest, ServerEvent, SessionRef, PROTOCOL_VERSION};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::BufWriter;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

pub async fn accept_loop(daemon: Arc<Daemon>, listener: UnixListener) {
    loop {
        tokio::select! {
            _ = daemon.shutdown.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let daemon = daemon.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_client(daemon, stream).await {
                            tracing::debug!(error = %e, "client connection ended with error");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            },
        }
    }
}

async fn handle_client(daemon: Arc<Daemon>, stream: UnixStream) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);

    // Single writer task; everything else sends frames through this channel
    // so PTY forwards and RPC replies never interleave mid-frame.
    let (out_tx, mut out_rx) = mpsc::channel::<ServerEvent>(256);
    let closed = tokio_util::sync::CancellationToken::new();
    let relay_client = crate::relay::RelayClient::new(out_tx.clone(), closed.clone());
    let finished = tokio_util::sync::CancellationToken::new();
    let writer_finished = finished.clone();
    let writer_closed = closed.clone();
    let writer_task = tokio::spawn(async move {
        let mut w = BufWriter::new(write_half);
        // Graceful completion may drain final replies, but a slow peer must
        // not retain the writer for one timeout per queued frame.
        tokio::select! {
            _ = async {
                writer_finished.cancelled().await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            } => {},
            _ = async {
        loop {
            let ev = tokio::select! {
                _ = writer_closed.cancelled() => break,
                _ = writer_finished.cancelled(), if !out_rx.is_closed() => {
                    out_rx.close();
                    continue;
                }
                ev = out_rx.recv() => match ev { Some(ev) => ev, None => break },
            };
            let result = tokio::select! {
                _ = writer_closed.cancelled() => break,
                result = tokio::time::timeout(std::time::Duration::from_secs(10), write_frame(&mut w, &ev)) => result,
            };
            if !matches!(result, Ok(Ok(()))) {
                break;
            }
        }
            } => {},
        }
        writer_closed.cancel();
    });

    // Per-connection attach state: forward-task handles keyed by session.
    let mut attached: HashMap<SessionRef, tokio::task::JoinHandle<()>> = HashMap::new();
    let mut handshaken = false;

    let result: Result<()> = tokio::select! {
        _ = closed.cancelled() => Ok(()),
        result = async {
        while let Some(req) = read_frame::<ClientRequest, _>(&mut reader).await? {
            match req {
                ClientRequest::Hello { protocol_version } => {
                    handshaken = protocol_version == PROTOCOL_VERSION;
                    let reply = if handshaken {
                        ServerEvent::HelloOk {
                            protocol_version: PROTOCOL_VERSION,
                            daemon_pid: std::process::id(),
                        }
                    } else {
                        ServerEvent::Incompatible {
                            daemon_protocol_version: PROTOCOL_VERSION,
                        }
                    };
                    let closing = !handshaken;
                    let _ = out_tx.send(reply).await;
                    if closing {
                        break;
                    }
                }
                _ if !handshaken => {
                    let _ = out_tx
                        .send(ServerEvent::Error {
                            req_id: None,
                            message: "handshake required".into(),
                        })
                        .await;
                    break;
                }
                // A request naming something mirrored from a remote host
                // belongs to that host's daemon; the relay carries it there
                // and brings the reply (or the PTY stream) back on out_tx.
                // (RemoveProject on a mirrored row is handled locally: it
                // detaches the anchor rather than deleting on the host.)
                req if !matches!(req, ClientRequest::RemoveProject { .. })
                    && daemon.relay_for_request(&req).is_some() =>
                {
                    if let Some(relay) = daemon.relay_for_request(&req) {
                        relay.forward(req, &relay_client).await;
                    }
                }
                ClientRequest::Subscribe => {
                    let snapshot = daemon.snapshot().unwrap_or(ServerEvent::Snapshot {
                        workspaces: vec![],
                        active_workspace: Default::default(),
                        projects: vec![],
                        worktrees: vec![],
                        agents: vec![],
                        terminals: vec![],
                        notes: vec![],
                        todos: vec![],
                        links: vec![],
                        pr_seen: vec![],
                        ui_state: None,
                    });
                    let _ = out_tx.send(snapshot).await;
                    let mut rx = daemon.events.subscribe();
                    let tx = out_tx.clone();
                    tokio::spawn(async move {
                        loop {
                            let event = tokio::select! {
                                _ = tx.closed() => break,
                                event = rx.recv() => event,
                            };
                            match event {
                                Ok(ev) => {
                                    if tx.send(ev).await.is_err() {
                                        break;
                                    }
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                                    continue
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    });
                }
                ClientRequest::Attach {
                    session: sref,
                    from_seq,
                    cols,
                    rows,
                } => {
                    match daemon.ensure_session(&sref, cols, rows) {
                        Ok(session) => {
                            // Subscribe BEFORE snapshotting so nothing falls in
                            // the gap; the forward task drops frames the
                            // snapshot already covers.
                            let events_rx = session.events.subscribe();
                            let (base_seq, data) = session.snapshot(from_seq);
                            let replay_end = base_seq + data.len() as u64;
                            let _ = out_tx
                                .send(ServerEvent::Scrollback {
                                    session: sref.clone(),
                                    base_seq,
                                    data,
                                })
                                .await;
                            let _ = out_tx
                                .send(ServerEvent::KittyFlags {
                                    session: sref.clone(),
                                    flags: session.kitty_flags(),
                                })
                                .await;
                            let _ = session.resize_with_jiggle(cols, rows);

                            let rebind = attached.remove(&sref);
                            if let Some(old) = &rebind {
                                old.abort();
                            }
                            let handle = tokio::spawn(forward_pty(
                                session.clone(),
                                sref.clone(),
                                events_rx,
                                out_tx.clone(),
                                replay_end,
                            ));
                            // Count this connection once even across
                            // re-attaches to the same session.
                            if rebind.is_none() {
                                daemon.note_attached(&sref);
                            }
                            attached.insert(sref, handle);
                        }
                        Err(e) => {
                            let _ = out_tx
                                .send(ServerEvent::Error {
                                    req_id: None,
                                    message: format!("attach: {e:#}"),
                                })
                                .await;
                        }
                    }
                }
                ClientRequest::Detach { session } => {
                    if let Some(h) = attached.remove(&session) {
                        h.abort();
                        daemon.note_detached(&session);
                    }
                }
                ClientRequest::InspectCollect { req_id, worktree, branch } => {
                    let reply = match daemon.inspect_collect(&worktree, &branch) {
                        Ok(()) => ServerEvent::Ack { req_id, created: None },
                        Err(e) => ServerEvent::Error { req_id: Some(req_id), message: format!("{e:#}") },
                    };
                    let _ = out_tx.send(reply).await;
                }
                ClientRequest::CollectHandoff { req_id, id, expected_status_changed_at, worktree, branch, prompt, timeout_ms } => {
                    let daemon = daemon.clone();
                    let out = out_tx.clone();
                    tokio::spawn(async move {
                        let result = daemon.collect_handoff(&id, &worktree, &branch, expected_status_changed_at, &prompt, std::time::Duration::from_millis(timeout_ms)).await;
                        let reply = match result {
                            Ok(()) => ServerEvent::Ack { req_id, created: None },
                            Err(e) => ServerEvent::Error { req_id: Some(req_id), message: format!("{e:#}") },
                        };
                        let _ = out.send(reply).await;
                    });
                }
                ClientRequest::InputAcknowledged { req_id, id, project, data } => {
                    let session = SessionRef::Agent(id.clone());
                    let result = daemon.validate_agent_project(&id, &project).and_then(|_| {
                        daemon.session(&session)
                            .ok_or_else(|| anyhow::anyhow!("Session has no live PTY"))
                            .and_then(|s| s.write_input_whole(&data))
                    });
                    if result.is_ok() {
                        note_submitted(&session, &data);
                    }
                    reply(&out_tx, req_id, result.map(|_| None)).await;
                }
                ClientRequest::Input { session, data } => {
                    if let Some(s) = daemon.session(&session) {
                        match s.write_input(&data) {
                            Ok(()) => note_submitted(&session, &data),
                            Err(e) => tracing::warn!(error = %e, "pty write failed"),
                        }
                    }
                }
                ClientRequest::Resize {
                    session,
                    cols,
                    rows,
                } => {
                    if let Some(s) = daemon.session(&session) {
                        let _ = s.resize(cols, rows);
                    }
                }
                ClientRequest::Shutdown => {
                    tracing::info!("shutdown requested by client");
                    daemon.shutdown.cancel();
                    break;
                }
                ClientRequest::SaveUiState { json } => {
                    let _ = daemon.store.save_ui_state(&json);
                }
                ClientRequest::MarkPrSeen { url, marker } => {
                    let _ = daemon.store.mark_pr_seen(&url, &marker);
                }
                ClientRequest::GetMetrics { req_id } => {
                    // A machine-wide `ps` sweep takes tens of ms; keep it off
                    // the request loop so Input/Attach frames keep flowing.
                    let pids = daemon.session_pids();
                    let out_tx = out_tx.clone();
                    tokio::spawn(async move {
                        let snapshot =
                            tokio::task::spawn_blocking(move || crate::metrics::collect(pids))
                                .await;
                        if let Ok(snapshot) = snapshot {
                            let _ = out_tx.send(ServerEvent::Metrics { req_id, snapshot }).await;
                        }
                    });
                }
                ClientRequest::ReadSession {
                    req_id,
                    session,
                    lines,
                } => {
                    // A pure look: no ensure_session (a dead session stays
                    // dead), no resize. Rendering ~1MB of ring through vt100
                    // takes a few ms — keep it off the request loop.
                    match daemon.session(&session) {
                        Some(s) => {
                            let (_, data) = s.snapshot(None);
                            let (cols, rows) = s.size();
                            let out_tx = out_tx.clone();
                            tokio::spawn(async move {
                                let text = tokio::task::spawn_blocking(move || {
                                    crate::pty::render::ring_to_text(&data, cols, rows, lines)
                                })
                                .await;
                                if let Ok(text) = text {
                                    let _ = out_tx
                                        .send(ServerEvent::SessionText {
                                            req_id,
                                            cols,
                                            rows,
                                            text,
                                        })
                                        .await;
                                }
                            });
                        }
                        None => {
                            let path = nebula_core::paths::saved_screen_path(&session);
                            let event = match std::fs::read_to_string(path) {
                                Ok(saved) => ServerEvent::SessionText {
                                    req_id,
                                    cols: 0,
                                    rows: 0,
                                    text: stale_screen_text(&saved, lines),
                                },
                                Err(_) => ServerEvent::Error {
                                    req_id: Some(req_id),
                                    message: "session is not running — nothing to read".into(),
                                },
                            };
                            let _ = out_tx.send(event).await;
                        }
                    }
                }
                // ---- entity CRUD: run the op, reply Ack/Error ----
                ClientRequest::AddWorkspace { req_id, name } => {
                    reply(&out_tx, req_id, daemon.add_workspace(&name).map(Some)).await;
                }
                ClientRequest::RemoveWorkspace { req_id, id } => {
                    reply(&out_tx, req_id, daemon.remove_workspace(&id).map(|_| None)).await;
                }
                ClientRequest::RenameWorkspace { req_id, id, name } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.rename_workspace(&id, &name).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::OpenWorkspace { req_id, id } => {
                    reply(&out_tx, req_id, daemon.open_workspace(&id).map(|_| None)).await;
                }
                ClientRequest::AddProject {
                    req_id,
                    path,
                    name,
                    create_missing,
                    host,
                } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon
                            .add_project(&path, name, create_missing, host)
                            .await
                            .map(Some),
                    )
                    .await;
                }
                ClientRequest::RemoveProject { req_id, id } => {
                    reply(&out_tx, req_id, daemon.remove_project(&id).map(|_| None)).await;
                }
                ClientRequest::MoveProject { req_id, id, delta } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.move_project(&id, delta).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::SetProjectDivider {
                    req_id,
                    id,
                    before,
                    present,
                    label,
                } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon
                            .set_project_divider(&id, before, present, label)
                            .map(|_| None),
                    )
                    .await;
                }
                ClientRequest::MoveDivider {
                    req_id,
                    id,
                    before,
                    delta,
                } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.move_divider(&id, before, delta).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::CreateWorktree {
                    req_id,
                    project,
                    branch,
                    base,
                } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon
                            .create_worktree(&project, &branch, base.as_deref())
                            .await
                            .map(Some),
                    )
                    .await;
                }
                ClientRequest::ResolveBranchCheckout {
                    req_id,
                    project,
                    branch,
                } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon
                            .resolve_branch_checkout(&project, &branch)
                            .await
                            .map(Some),
                    )
                    .await;
                }
                ClientRequest::CheckWorktreeRemoval { req_id, id, branch, path, delete } => {
                    let daemon = daemon.clone();
                    let out_tx = out_tx.clone();
                    tokio::spawn(async move {
                        reply(&out_tx, req_id, daemon.check_worktree_removal(&id, &branch, &path, delete).await.map(|_| None)).await;
                    });
                }
                ClientRequest::DeleteWorktree { req_id, id, force } => {
                    // `git worktree remove` can take seconds on a large
                    // checkout; run it off the request loop so Input/Attach
                    // frames keep flowing while it grinds. `worktree_ops`
                    // still serializes it against create/sync.
                    let daemon = daemon.clone();
                    let out_tx = out_tx.clone();
                    tokio::spawn(async move {
                        reply(
                            &out_tx,
                            req_id,
                            daemon.delete_worktree(&id, force).await.map(|_| None),
                        )
                        .await;
                    });
                }
                ClientRequest::CheckoutPrimary {
                    req_id,
                    project,
                    branch,
                    mover,
                    stop_busy,
                } => {
                    let daemon = daemon.clone();
                    let out_tx = out_tx.clone();
                    tokio::spawn(async move {
                        reply(
                            &out_tx,
                            req_id,
                            daemon
                                .checkout_primary(&project, &branch, mover.as_ref(), stop_busy)
                                .await
                                .map(|_| None),
                        )
                        .await;
                    });
                }
                ClientRequest::SetWorktreePinned { req_id, id, pinned } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.set_worktree_pinned(&id, pinned).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::CreateAgent {
                    req_id,
                    worktree,
                    expected_branch,
                    name,
                    kind,
                    model,
                    effort,
                    auto_title,
                    prompt,
                } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon
                            .create_agent(
                                &worktree,
                                expected_branch.as_deref(),
                                &name,
                                kind,
                                model,
                                effort,
                                auto_title,
                                prompt,
                            )
                            .await
                            .map(Some),
                    )
                    .await;
                }
                ClientRequest::PrewarmAgent {
                    worktree,
                    kind,
                    model,
                    effort,
                } => {
                    // Fire-and-forget: boot the CLI while the user is still
                    // typing the session name; CreateAgent adopts it. Runs
                    // off the request loop (the CLI probe can take a bit).
                    let daemon = daemon.clone();
                    tokio::spawn(async move {
                        if let Err(e) = daemon.prewarm_agent(&worktree, kind, model, effort).await {
                            tracing::debug!(error = %e, "prewarm failed");
                        }
                    });
                }
                // Retired: booting a worktree's every session on selection
                // caused spawn storms. Still accepted so older clients keep
                // the same PROTOCOL_VERSION.
                ClientRequest::PrewarmWorktreeSessions { .. } => {}
                ClientRequest::RenameAgent { req_id, id, name } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.rename_agent(&id, &name).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::AutoRenameAgent { req_id, id, name } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.auto_rename_agent(&id, &name).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::MoveAgent {
                    req_id,
                    id,
                    worktree,
                } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.move_agent(&id, &worktree).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::ArchiveAgent { req_id, id } => {
                    reply(&out_tx, req_id, daemon.archive_agent(&id).map(|_| None)).await;
                }
                ClientRequest::UnarchiveAgent { req_id, id } => {
                    reply(&out_tx, req_id, daemon.unarchive_agent(&id).map(|_| None)).await;
                }
                ClientRequest::SetAgentPinned { req_id, id, pinned } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.set_agent_pinned(&id, pinned).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::DeleteAgentScoped { req_id, id, project } => {
                    let result = daemon.validate_agent_project(&id, &project).and_then(|_| daemon.delete_agent(&id));
                    reply(&out_tx, req_id, result.map(|_| None)).await;
                }
                ClientRequest::DeleteAgent { req_id, id } => {
                    reply(&out_tx, req_id, daemon.delete_agent(&id).map(|_| None)).await;
                }
                ClientRequest::RestartAgent { req_id, id } => {
                    reply(&out_tx, req_id, daemon.restart_agent(&id).map(|_| None)).await;
                }
                ClientRequest::CreateTerminal {
                    req_id,
                    worktree,
                    expected_branch,
                    name,
                } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon
                            .create_terminal(&worktree, expected_branch.as_deref(), name)
                            .map(Some),
                    )
                    .await;
                }
                ClientRequest::CreateNote {
                    req_id,
                    owner,
                    text,
                } => {
                    reply(&out_tx, req_id, daemon.create_note(&owner, &text).map(Some)).await;
                }
                ClientRequest::UpdateNote { req_id, id, text } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.update_note(&id, &text).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::SetNoteDone { req_id, id, done } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.set_note_done(&id, done).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::DeleteNote { req_id, id } => {
                    reply(&out_tx, req_id, daemon.delete_note(&id).map(|_| None)).await;
                }
                ClientRequest::CreateTodo {
                    req_id,
                    owner,
                    text,
                } => {
                    reply(&out_tx, req_id, daemon.create_todo(&owner, &text).map(Some)).await;
                }
                ClientRequest::UpdateTodo { req_id, id, text } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.update_todo(&id, &text).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::SetTodoDone { req_id, id, done } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.set_todo_done(&id, done).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::DeleteTodo { req_id, id } => {
                    reply(&out_tx, req_id, daemon.delete_todo(&id).map(|_| None)).await;
                }
                ClientRequest::CreateLink {
                    req_id,
                    worktree,
                    url,
                } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.create_link(&worktree, &url).map(Some),
                    )
                    .await;
                }
                ClientRequest::UpdateLink { req_id, id, url } => {
                    reply(&out_tx, req_id, daemon.update_link(&id, &url).map(|_| None)).await;
                }
                ClientRequest::DeleteLink { req_id, id } => {
                    reply(&out_tx, req_id, daemon.delete_link(&id).map(|_| None)).await;
                }
                ClientRequest::RenameTerminal { req_id, id, name } => {
                    reply(
                        &out_tx,
                        req_id,
                        daemon.rename_terminal(&id, &name).map(|_| None),
                    )
                    .await;
                }
                ClientRequest::CloseTerminal { req_id, id } => {
                    reply(&out_tx, req_id, daemon.close_terminal(&id).map(|_| None)).await;
                }
            }
        }
        Ok(())
        } => result,
    };

    for (sref, h) in attached.drain() {
        h.abort();
        daemon.note_detached(&sref);
    }
    finished.cancel();
    drop(relay_client);
    drop(out_tx);
    let _ = writer_task.await;
    result
}

/// Forward live PTY output/exit to one client, skipping bytes the attach
/// replay already delivered. On broadcast lag, resync with a fresh
/// Scrollback (the client resets its parser on every Scrollback frame).
async fn forward_pty(
    session: Arc<crate::pty::PtySession>,
    sref: SessionRef,
    mut rx: tokio::sync::broadcast::Receiver<PtyEvent>,
    out_tx: mpsc::Sender<ServerEvent>,
    mut min_seq: u64,
) {
    loop {
        match rx.recv().await {
            Ok(PtyEvent::Output { seq, data }) => {
                let end = seq + data.len() as u64;
                if end <= min_seq {
                    continue; // fully covered by the replay
                }
                let skip = min_seq.saturating_sub(seq) as usize;
                let payload = if skip > 0 {
                    data[skip..].to_vec()
                } else {
                    data
                };
                let send_seq = seq + skip as u64;
                min_seq = end;
                if out_tx
                    .send(ServerEvent::Output {
                        session: sref.clone(),
                        seq: send_seq,
                        data: payload,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            Ok(PtyEvent::Exited { exit_code }) => {
                let _ = out_tx
                    .send(ServerEvent::SessionExited {
                        session: sref.clone(),
                        exit_code,
                    })
                    .await;
                break;
            }
            Ok(PtyEvent::KittyFlags { flags }) => {
                if out_tx
                    .send(ServerEvent::KittyFlags {
                        session: sref.clone(),
                        flags,
                    })
                    .await
                    .is_err()
                {
                    break;
                }
            }
            // Daemon-side only: the progress edge drives the status machine
            // and reaches clients as a StatusChanged, not as session output.
            Ok(PtyEvent::Progress { .. }) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                // Catch up from the ring. If the missed bytes are still
                // retained, send them as a plain Output continuation so the
                // client keeps its parser state; only when the gap has fallen
                // off the ring do we force a full replay (parser reset —
                // expensive on the client, so avoid it when possible).
                let wanted = min_seq;
                let (base_seq, data) = session.snapshot(Some(wanted));
                min_seq = base_seq + data.len() as u64;
                let ev = if base_seq == wanted {
                    ServerEvent::Output {
                        session: sref.clone(),
                        seq: base_seq,
                        data,
                    }
                } else {
                    ServerEvent::Scrollback {
                        session: sref.clone(),
                        base_seq,
                        data,
                    }
                };
                if out_tx.send(ev).await.is_err() {
                    break;
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
}

async fn reply(
    out_tx: &mpsc::Sender<ServerEvent>,
    req_id: u64,
    result: anyhow::Result<Option<nebula_core::EntityId>>,
) {
    let ev = match result {
        Ok(created) => ServerEvent::Ack { req_id, created },
        Err(e) => ServerEvent::Error {
            req_id: Some(req_id),
            message: format!("{e:#}"),
        },
    };
    let _ = out_tx.send(ev).await;
}

/// A stopped session's saved screen, tailed to `lines` and headed by a
/// line saying it is not live, so a reader never mistakes it for now.
fn stale_screen_text(saved: &str, lines: Option<usize>) -> String {
    let body: Vec<&str> = saved.lines().collect();
    let keep = lines.map_or(body.len(), |n| n.min(body.len()));
    let mut text =
        String::from("[nebula] session not running — last screen saved when it stopped:");
    for line in &body[body.len() - keep..] {
        text.push('\n');
        text.push_str(line);
    }
    text
}

/// A bare Enter reaching an agent's PTY submits a message, or queues it
/// behind a running tool call. Marking it in the agent's inbox lets the
/// session's own `nebula agent wait` return early instead of sitting on the
/// message. Only a frame that is exactly Enter counts — legacy `\r` or the
/// kitty `CSI 13 u` — so a pasted newline (still draft text) and the text
/// frame of `agent send` (its Enter follows as its own frame) do not.
fn note_submitted(session: &SessionRef, data: &[u8]) {
    use std::io::Write;
    let SessionRef::Agent(id) = session else {
        return;
    };
    if !matches!(data, b"\r" | b"\x1b[13u") {
        return;
    }
    let path = nebula_core::paths::inbox_path(id.as_str());
    let appended = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)?
                .write_all(b"\n")
        });
    if let Err(e) = appended {
        tracing::warn!(error = %e, path = %path.display(), "inbox mark failed");
    }
}
