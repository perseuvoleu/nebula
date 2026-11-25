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
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, BufReader, BufWriter};
use tokio::sync::{mpsc, watch, Notify};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a written request waits for its answer before it alone ends
/// as outcome unknown.
fn reply_deadline() -> Duration {
    std::env::var("NEBULA_BOARD_REPLY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_secs(30))
}
/// Unanswered requests the link holds, as the relay caps its own.
const MAX_PENDING: usize = 256;
const RECONNECT_MIN: Duration = Duration::from_secs(2);
const RECONNECT_MAX: Duration = Duration::from_secs(30);

/// Where the link stands. `connections` counts handshakes, so a watcher
/// sees every (re)connect; `attempts` counts connections that ended,
/// whether or not they came up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkState {
    pub up: bool,
    pub connections: u64,
    pub attempts: u64,
}

struct Pending {
    client: RelayClient,
    local: u64,
    written: Option<Instant>,
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

/// The daemon's link to its board host, if its config names one.
#[derive(Default)]
pub struct LinkSlot(Mutex<Option<Arc<BoardLink>>>);

impl LinkSlot {
    /// The link to `board`'s host when that is another host, started on
    /// first use. Any other link stops, so a board set to local or unset
    /// leaves no reconnect loop behind.
    pub fn for_board(
        &self,
        board: Option<&str>,
        shutdown: &CancellationToken,
    ) -> Option<Arc<BoardLink>> {
        let host = board.filter(|board| *board != "local");
        let mut slot = self.0.lock().unwrap();
        if let Some(link) = slot
            .as_ref()
            .filter(|link| Some(link.host.as_str()) == host)
        {
            return Some(link.clone());
        }
        if let Some(old) = slot.take() {
            old.stop.cancel();
        }
        let link = BoardLink::spawn(host?, shutdown.child_token());
        *slot = Some(link.clone());
        Some(link)
    }
}

impl BoardLink {
    fn spawn(host: &str, stop: CancellationToken) -> Arc<Self> {
        let (out, out_rx) = mpsc::channel(MAX_PENDING);
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
            } else if self.pending.lock().unwrap().len() >= MAX_PENDING {
                Some(Refusal::Unreachable.message(format!(
                    "board host {} has too many requests in flight",
                    self.host
                )))
            } else {
                let remote = self.next_req.fetch_add(1, Ordering::Relaxed);
                self.pending.lock().unwrap().insert(
                    remote,
                    Pending {
                        client: client.clone(),
                        local,
                        written: None,
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

    /// One Board request on the daemon's own behalf, such as a host report.
    pub async fn request(&self, op: BoardOp) -> Result<BoardResult> {
        let (tx, mut rx) = mpsc::channel(1);
        let client = RelayClient::new(tx, CancellationToken::new());
        self.forward(0, op, &client).await;
        match rx.recv().await {
            Some(ServerEvent::BoardReply { result, .. }) => Ok(result),
            Some(ServerEvent::Error { message, .. }) => bail!("{message}"),
            other => bail!("board host {} answered {other:?}", self.host),
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

    /// Ends each request written longer than the deadline ago; the link
    /// stays up for the rest.
    fn expire(&self, deadline: Duration) {
        let overdue: Vec<Pending> = {
            let mut pending = self.pending.lock().unwrap();
            let late: Vec<u64> = pending
                .iter()
                .filter(|(_, p)| p.written.is_some_and(|at| at.elapsed() >= deadline))
                .map(|(remote, _)| *remote)
                .collect();
            late.iter().filter_map(|r| pending.remove(r)).collect()
        };
        for pending in overdue {
            pending.client.deliver(ServerEvent::Error {
                req_id: Some(pending.local),
                message: Refusal::OutcomeUnknown
                    .message(format!("board host {} did not answer in time", self.host)),
            });
        }
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
            } => (req_id, Err(qualify(&message, &self.host))),
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

    fn teardown(&self, out_rx: &mut mpsc::Receiver<(u64, BoardOp)>, refused: Option<String>) {
        let _admission = self.admission.lock().unwrap();
        *self.refused.lock().unwrap() = refused;
        self.state.send_modify(|s| {
            s.up = false;
            s.attempts += 1;
        });
        while out_rx.try_recv().is_ok() {}
        let unreachable = self.unreachable();
        for (_, pending) in std::mem::take(&mut *self.pending.lock().unwrap()) {
            let message = match pending.written.is_some() {
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
        let refused = match connect_and_pump(&link, &mut out_rx).await {
            Ok(()) => None,
            Err(e) => {
                tracing::warn!(host = %link.host, error = %e, "board link down");
                e.downcast_ref::<LinkRefused>().map(|r| r.0.clone())
            }
        };
        link.teardown(&mut out_rx, refused);
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
    if std::env::var("NEBULA_RELAY_CMD")
        .is_ok_and(|line| line.contains("proxy") && !line.contains("{proxy}"))
    {
        return Err(LinkRefused(
            "NEBULA_RELAY_CMD runs proxy without {proxy}, which would drop the board link's --connect-only"
                .into(),
        )
        .into());
    }
    let mut child = relay_command(&link.host, "proxy --connect-only")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("spawn board link transport")?;
    let mut writer = BufWriter::new(child.stdin.take().context("board link stdin")?);
    let mut reader = BufReader::new(child.stdout.take().context("board link stdout")?);
    let mut errors = child.stderr.take().context("board link stderr")?;
    let shook = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(link, &mut reader, &mut writer))
        .await
        .context("board link handshake timed out")
        .and_then(|shook| shook);
    if let Err(e) = shook {
        if e.is::<LinkRefused>() {
            return Err(e);
        }
        let mut said = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(1), errors.read_to_end(&mut said)).await;
        if String::from_utf8_lossy(&said).contains("--connect-only") {
            return Err(LinkRefused(format!(
                "board host {} runs a nebula without `proxy --connect-only` — upgrade nebula there",
                link.host
            ))
            .into());
        }
        return Err(e);
    }
    tokio::spawn(async move { tokio::io::copy(&mut errors, &mut tokio::io::sink()).await });
    {
        let _admission = link.admission.lock().unwrap();
        link.state.send_modify(|s| {
            s.up = true;
            s.connections += 1;
        });
    }
    tracing::info!(host = %link.host, "board link up");
    crate::board_report::resend();
    let deadline = reply_deadline();
    let mut tick = tokio::time::interval(deadline.min(Duration::from_secs(5)));
    loop {
        let frame = {
            let read = read_frame::<ServerEvent, _>(&mut reader);
            tokio::pin!(read);
            loop {
                tokio::select! {
                    _ = link.stop.cancelled() => return Ok(()),
                    _ = tick.tick() => link.expire(deadline),
                    frame = &mut read => break frame?,
                    queued = out_rx.recv() => {
                        let Some((remote, op)) = queued else {
                            return Ok(());
                        };
                        let request = ClientRequest::Board { req_id: remote, op };
                        tokio::time::timeout(HANDSHAKE_TIMEOUT, write_frame(&mut writer, &request))
                            .await
                            .context("board link write timed out")??;
                        if let Some(pending) = link.pending.lock().unwrap().get_mut(&remote) {
                            pending.written = Some(Instant::now());
                        }
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
    let peer = match read_frame::<ServerEvent, _>(reader).await? {
        Some(ServerEvent::HelloOk {
            protocol_version, ..
        }) => protocol_version,
        Some(ServerEvent::Incompatible {
            daemon_protocol_version,
        }) => daemon_protocol_version,
        other => bail!("unexpected handshake reply: {other:?}"),
    };
    if peer == PROTOCOL_VERSION {
        return Ok(());
    }
    Err(LinkRefused(format!(
        "board host {} runs protocol {peer}, this daemon {PROTOCOL_VERSION} — upgrade nebula on one side",
        link.host
    ))
    .into())
}

#[derive(Debug)]
struct LinkRefused(String);

impl std::fmt::Display for LinkRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LinkRefused {}

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
        for blocker in card
            .blocked_by
            .iter_mut()
            .chain(card.cleared_blockers.iter_mut())
        {
            blocker.push('@');
            blocker.push_str(host);
        }
    }
}

/// The board's own refusals name cards as `#12`; off the board host they
/// read `#12@host`, as relabeled replies do.
fn qualify(message: &str, host: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(at) = rest.find('#') {
        let (before, from) = rest.split_at(at);
        out.push_str(before);
        let digits = from[1..].bytes().take_while(u8::is_ascii_digit).count();
        let (label, after) = from.split_at(1 + digits);
        out.push_str(label);
        if digits > 0 && !after.starts_with('@') {
            out.push('@');
            out.push_str(host);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}
