//! The board link: on a daemon whose config names another host's board,
//! every Board request goes to that host's daemon over the relay transport,
//! speaking Hello and frames with no Subscribe and no mirror. Nothing is
//! queued across a failure: a request the link never wrote is refused as
//! unreachable, one it wrote and lost the answer to as outcome unknown.

use crate::relay::{relay_command, RelayClient};
use anyhow::{bail, Context, Result};
use nebula_core::board::{card_label, BoardOp, BoardPoll, BoardResult, Card, CardRef, Refusal};
use nebula_core::codec::{read_frame, write_frame};
use nebula_core::{ClientRequest, ServerEvent, PROTOCOL_VERSION};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, BufReader, BufWriter};
use tokio::sync::{mpsc, watch, Notify};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const REPLY_DEADLINE: Duration = Duration::from_secs(30);
const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Where the link stands. `connections` counts handshakes, so a watcher
/// sees every (re)connect; `attempts` counts finished connection attempts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkState {
    pub up: bool,
    pub connections: u64,
    pub attempts: u64,
}

struct Pending {
    client: RelayClient,
    local: u64,
    sent: bool,
    admitted: Instant,
}

pub struct BoardLink {
    host: String,
    out: mpsc::Sender<(u64, BoardOp)>,
    admission: Mutex<()>,
    pending: Mutex<HashMap<u64, Pending>>,
    next_req: AtomicU64,
    state: watch::Sender<LinkState>,
    refused: Mutex<Option<String>>,
    kick: Notify,
    stop: CancellationToken,
}

static LINK: Mutex<Option<Arc<BoardLink>>> = Mutex::new(None);

/// The link to `host`, started on first use; a changed board host stops
/// the old link.
pub fn to(host: &str, shutdown: &CancellationToken) -> Arc<BoardLink> {
    let mut slot = LINK.lock().unwrap();
    if let Some(link) = slot.as_ref().filter(|link| link.host == host) {
        return link.clone();
    }
    if let Some(old) = slot.take() {
        old.stop.cancel();
    }
    let link = BoardLink::spawn(host, shutdown.child_token());
    *slot = Some(link.clone());
    link
}

impl BoardLink {
    fn spawn(host: &str, stop: CancellationToken) -> Arc<Self> {
        let (out, out_rx) = mpsc::channel(256);
        let link = Arc::new(Self {
            host: host.to_string(),
            out,
            admission: Mutex::new(()),
            pending: Mutex::new(HashMap::new()),
            next_req: AtomicU64::new(1),
            state: watch::Sender::new(LinkState::default()),
            refused: Mutex::new(None),
            kick: Notify::new(),
            stop,
        });
        tokio::spawn(run(link.clone(), out_rx));
        link
    }

    pub fn state(&self) -> watch::Receiver<LinkState> {
        self.state.subscribe()
    }

    /// Send one Board request; its reply or refusal reaches `client` under
    /// the client's own `local` req_id.
    pub async fn forward(&self, local: u64, mut op: BoardOp, client: &RelayClient) {
        self.connect_now().await;
        localize(&mut op, &self.host);
        let refused = {
            let _admission = self.admission.lock().unwrap();
            if !self.state.borrow().up {
                Some(self.unreachable())
            } else {
                let remote = self.next_req.fetch_add(1, Ordering::Relaxed);
                self.pending.lock().unwrap().insert(
                    remote,
                    Pending {
                        client: client.clone(),
                        local,
                        sent: false,
                        admitted: Instant::now(),
                    },
                );
                match self.out.try_send((remote, op)) {
                    Ok(()) => None,
                    Err(_) => {
                        self.pending.lock().unwrap().remove(&remote);
                        Some(Refusal::Unreachable.message(format!(
                            "board host {} has too many requests in flight",
                            self.host
                        )))
                    }
                }
            }
        };
        if let Some(message) = refused {
            client.deliver(ServerEvent::Error {
                req_id: Some(local),
                message,
            });
        }
    }

    async fn connect_now(&self) {
        let mut state = self.state.subscribe();
        let seen = *state.borrow_and_update();
        if seen.up {
            return;
        }
        self.kick.notify_one();
        let _ = tokio::time::timeout(
            HANDSHAKE_TIMEOUT + Duration::from_secs(5),
            state.wait_for(|now| now.up || now.attempts > seen.attempts),
        )
        .await;
    }

    fn unreachable(&self) -> String {
        Refusal::Unreachable.message(
            self.refused
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| format!("board host {} is unreachable", self.host)),
        )
    }

    fn overdue(&self) -> bool {
        self.pending
            .lock()
            .unwrap()
            .values()
            .any(|p| p.sent && p.admitted.elapsed() >= REPLY_DEADLINE)
    }

    fn answer(&self, event: ServerEvent) {
        let (remote, reply) = match event {
            ServerEvent::BoardReply { req_id, mut result } => {
                relabel(&mut result, &self.host);
                (req_id, Ok(result))
            }
            ServerEvent::Error {
                req_id: Some(req_id),
                message,
            } => (req_id, Err(message)),
            _ => return,
        };
        let Some(pending) = self.pending.lock().unwrap().remove(&remote) else {
            return;
        };
        pending.client.deliver(match reply {
            Ok(result) => ServerEvent::BoardReply {
                req_id: pending.local,
                result,
            },
            Err(message) => ServerEvent::Error {
                req_id: Some(pending.local),
                message,
            },
        });
    }

    fn teardown(&self, out_rx: &mut mpsc::Receiver<(u64, BoardOp)>) {
        let _admission = self.admission.lock().unwrap();
        self.state.send_modify(|s| {
            s.up = false;
            s.attempts += 1;
        });
        while out_rx.try_recv().is_ok() {}
        let unreachable = self.unreachable();
        for (_, pending) in std::mem::take(&mut *self.pending.lock().unwrap()) {
            let message = match pending.sent {
                true => Refusal::OutcomeUnknown.message(format!(
                    "board host {}: the link dropped after the request was sent",
                    self.host
                )),
                false => unreachable.clone(),
            };
            pending.client.deliver(ServerEvent::Error {
                req_id: Some(pending.local),
                message,
            });
        }
    }
}

async fn run(link: Arc<BoardLink>, mut out_rx: mpsc::Receiver<(u64, BoardOp)>) {
    let mut backoff = RECONNECT_MIN;
    loop {
        let started = Instant::now();
        if let Err(e) = connect_and_pump(&link, &mut out_rx).await {
            tracing::warn!(host = %link.host, error = %e, "board link down");
        }
        link.teardown(&mut out_rx);
        if started.elapsed() >= RECONNECT_MAX {
            backoff = RECONNECT_MIN;
        }
        tokio::select! {
            _ = link.stop.cancelled() => return,
            _ = tokio::time::sleep(backoff) => {}
            _ = link.kick.notified() => {}
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

async fn connect_and_pump(
    link: &BoardLink,
    out_rx: &mut mpsc::Receiver<(u64, BoardOp)>,
) -> Result<()> {
    let mut child = relay_command(&link.host, "proxy --connect-only")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .context("spawn board link transport")?;
    let mut writer = BufWriter::new(child.stdin.take().context("board link stdin")?);
    let mut reader = BufReader::new(child.stdout.take().context("board link stdout")?);
    tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(link, &mut reader, &mut writer))
        .await
        .context("board link handshake timed out")??;
    {
        let _admission = link.admission.lock().unwrap();
        link.state.send_modify(|s| {
            s.up = true;
            s.connections += 1;
            s.attempts += 1;
        });
    }
    tracing::info!(host = %link.host, "board link up");
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        let frame = {
            let read = read_frame::<ServerEvent, _>(&mut reader);
            tokio::pin!(read);
            loop {
                tokio::select! {
                    _ = link.stop.cancelled() => return Ok(()),
                    _ = tick.tick() => {
                        if link.overdue() {
                            bail!("board host {} did not answer in time", link.host);
                        }
                    }
                    frame = &mut read => break frame?,
                    queued = out_rx.recv() => {
                        let Some((remote, op)) = queued else {
                            return Ok(());
                        };
                        if let Some(pending) = link.pending.lock().unwrap().get_mut(&remote) {
                            pending.sent = true;
                        }
                        let request = ClientRequest::Board { req_id: remote, op };
                        tokio::time::timeout(HANDSHAKE_TIMEOUT, write_frame(&mut writer, &request))
                            .await
                            .context("board link write timed out")??;
                    }
                }
            }
        };
        let Some(event) = frame else {
            bail!("board host {} closed the link", link.host);
        };
        link.answer(event);
    }
}

async fn handshake<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    link: &BoardLink,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    write_frame(
        writer,
        &ClientRequest::Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    let refused = match read_frame::<ServerEvent, _>(reader).await? {
        Some(ServerEvent::HelloOk {
            protocol_version, ..
        }) if protocol_version == PROTOCOL_VERSION => None,
        Some(ServerEvent::Incompatible {
            daemon_protocol_version,
        }) => Some(format!(
            "board host {} runs protocol {daemon_protocol_version}, this daemon {PROTOCOL_VERSION} — upgrade nebula on one side",
            link.host
        )),
        other => bail!("unexpected handshake reply: {other:?}"),
    };
    *link.refused.lock().unwrap() = refused.clone();
    match refused {
        Some(why) => bail!(why),
        None => Ok(()),
    }
}

fn localize(op: &mut BoardOp, host: &str) {
    for card in op.card_refs_mut() {
        if let CardRef::Number { board, .. } = card {
            if board.as_deref() == Some(host) {
                *board = None;
            }
        }
    }
}

fn relabel(result: &mut BoardResult, host: &str) {
    let cards: Vec<&mut Card> = match result {
        BoardResult::Card(card) => vec![card],
        BoardResult::Cards(page) => page.items.iter_mut().collect(),
        BoardResult::Show(show) => vec![&mut show.card],
        BoardResult::Board(BoardPoll::Board { board, .. }) => board.cards.iter_mut().collect(),
        _ => vec![],
    };
    for card in cards {
        card.label = card_label(card.number, Some(host));
        for blocker in &mut card.blocked_by {
            blocker.push('@');
            blocker.push_str(host);
        }
    }
}
