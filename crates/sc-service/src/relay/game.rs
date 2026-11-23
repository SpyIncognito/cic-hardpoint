//! The game PC's side: dial the paired companion, keep the link up with backoff, and run one
//! portal session (companion policy) for each page stream the companion opens. Holds and every
//! other per-page state live in those sessions, so they die with the stream.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::stream;
use tokio::io::{AsyncRead, AsyncWrite, split};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use sc_core::js;

use super::handshake::{self, EXPORTER_LABEL, LinkError, STEP_TIMEOUT};
use super::pairing::{self, Code};
use super::peers::{Peer, Role, clean_name};
use super::wire::{self, MAX_STREAMS, Msg, Reader, Reason, StreamKind};
use super::{LINK_TIMEOUT, LinkState, PING_EVERY, Relay, split_address};
use crate::fleet::Backoff;
use crate::pages::MAX_PAGE_MESSAGE_BYTES;
use crate::portal::policy::{LinkGuard, Policy};
use crate::portal::session::{self, now_ms};
use crate::tls;

/// A TLS connection to `address`, split, with its binding.
async fn dial_tls(
    address: &str,
) -> Result<
    (
        Reader<impl AsyncRead + Unpin + Send + 'static>,
        impl AsyncWrite + Unpin + Send + 'static,
        [u8; 32],
    ),
    LinkError,
> {
    let broken = |e: std::io::Error| LinkError::Broken(e.to_string());
    let (host, port) = split_address(address)
        .ok_or_else(|| LinkError::Broken(format!("{address} is not host:port")))?;
    let config = tls::client_config().map_err(broken)?;
    let connected = tokio::time::timeout(STEP_TIMEOUT, tls::connect(&config, &host, port))
        .await
        .map_err(|_| LinkError::Broken(format!("{address} did not answer")))?
        .map_err(broken)?;
    let binding = tls::export(connected.get_ref().1, EXPORTER_LABEL).map_err(broken)?;
    let (r, w) = split(connected);
    Ok((Reader::new(r), w, binding))
}

/// Pairs with the companion at `address` showing `code`.
pub async fn pair(relay: &Relay, address: &str, code: &Code) -> Result<Peer, LinkError> {
    let (mut reader, mut out, binding) = dial_tls(address).await?;
    let paired = pairing::dial(
        &mut reader,
        &mut out,
        &relay.identity,
        &relay.options.name,
        &relay.options.version,
        &binding,
        code,
    )
    .await?;
    Ok(relay.paired_now(
        &paired.name,
        paired.key,
        Role::Companion,
        Some(address.to_owned()),
    ))
}

/// What a failure says on the title bar. A refusal by key reads the same either way: the
/// companion unpaired this PC, or its address now belongs to another machine.
fn failure_text(e: &LinkError, address: &str) -> String {
    match e {
        LinkError::WrongKey
        | LinkError::Refused {
            reason: Reason::NotPaired,
            ..
        } => format!("not paired at {address}"),
        other => other.to_string(),
    }
}

/// Keeps the link to the paired companion until `stop`.
pub async fn run(relay: Arc<Relay>, mut stop: watch::Receiver<bool>) {
    let mut backoff = Backoff::default();
    loop {
        let Some(peer) = relay.peers.get(Role::Companion) else {
            relay.set_link(LinkState::Stopped, "no companion is paired", None);
            return;
        };
        let Some(address) = peer.last_address.clone() else {
            relay.set_link(
                LinkState::Stopped,
                format!("no address for {}", peer.name),
                None,
            );
            return;
        };
        relay.set_link(
            LinkState::Connecting,
            format!("connecting to {}", peer.name),
            None,
        );
        let dialled = async {
            let (mut reader, mut out, binding) = dial_tls(&address).await?;
            handshake::dial(
                &mut reader,
                &mut out,
                &relay.identity,
                &peer.key,
                &relay.options.version,
                &binding,
            )
            .await?;
            Ok::<_, LinkError>((reader, out))
        };
        let outcome = tokio::select! {
            outcome = dialled => outcome,
            _ = stop.changed() => return,
        };
        match outcome {
            Ok((reader, out)) => {
                backoff.reset();
                tracing::info!("[relay] relaying to {} at {address}", peer.name);
                relay.set_link(LinkState::Up, format!("relaying to {}", peer.name), None);
                if serve(&relay, &peer, reader, out, &mut stop).await {
                    return;
                }
                tracing::warn!("[relay] lost the link to {}", peer.name);
            }
            Err(e) if e.is_final() => {
                tracing::warn!("[relay] {address}: {e}");
                relay.set_link(LinkState::Stopped, failure_text(&e, &address), None);
                return;
            }
            Err(e) => tracing::info!("[relay] {address}: {e}"),
        }
        let wait = backoff.next_wait();
        relay.set_link(
            LinkState::Connecting,
            format!(
                "connecting to {} (retry in {} s)",
                peer.name,
                wait.as_secs_f64().ceil()
            ),
            Some(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)),
        );
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            _ = stop.changed() => return,
        }
    }
}

/// Reads messages on their own task, since a read cut off by `select!` would lose a line.
pub(super) fn reader_task(
    mut reader: Reader<impl AsyncRead + Unpin + Send + 'static>,
) -> (mpsc::Receiver<Msg>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(64);
    let task = tokio::spawn(async move {
        loop {
            match reader.next().await {
                Ok(Some(msg)) => {
                    if tx.send(msg).await.is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(e) => {
                    tracing::warn!("[relay] {e}");
                    return;
                }
            }
        }
    });
    (rx, task)
}

pub(super) fn writer_task(
    mut out: impl AsyncWrite + Unpin + Send + 'static,
) -> (mpsc::UnboundedSender<Msg>, JoinHandle<()>) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if wire::write(&mut out, &msg).await.is_err() {
                return;
            }
        }
    });
    (tx, task)
}

/// A page stream's way into its session; dropping it ends the session.
struct Stream {
    to_session: mpsc::UnboundedSender<String>,
}

/// Runs the link until it drops (false) or `stop` (true).
async fn serve(
    relay: &Arc<Relay>,
    peer: &Peer,
    reader: Reader<impl AsyncRead + Unpin + Send + 'static>,
    out: impl AsyncWrite + Unpin + Send + 'static,
    stop: &mut watch::Receiver<bool>,
) -> bool {
    let Some(app) = relay.app() else {
        return true;
    };
    let (mut incoming, read_task) = reader_task(reader);
    let (out, write_task) = writer_task(out);
    let guard = Arc::new(LinkGuard::default());
    let mut streams: HashMap<u32, Stream> = HashMap::new();
    let (ended, mut ended_rx) = mpsc::unbounded_channel::<u32>();
    let mut tick = tokio::time::interval(PING_EVERY);
    let mut heard = Instant::now();
    let close = |stream: u32, reason: &str| Msg::Close {
        stream,
        reason: Some(reason.to_owned()),
    };
    let stopped = loop {
        tokio::select! {
            msg = incoming.recv() => match msg {
                None => break false,
                Some(Msg::Open { stream, kind: StreamKind::Page, client }) => {
                    heard = Instant::now();
                    if streams.len() >= MAX_STREAMS || streams.contains_key(&stream) {
                        let _ = out.send(close(stream, "too many pages over the link"));
                        continue;
                    }
                    let client = js::utf16_prefix(
                        &format!("{}/{}", peer.name, clean_name(&client)),
                        64,
                    );
                    streams.insert(stream, open_session(&app, &guard, &out, &ended, stream, client));
                }
                Some(Msg::Data { stream, text }) => {
                    heard = Instant::now();
                    if text.len() > MAX_PAGE_MESSAGE_BYTES {
                        streams.remove(&stream);
                        let _ = out.send(close(stream, "a page message is too large"));
                    } else if let Some(s) = streams.get(&stream) {
                        let _ = s.to_session.send(text);
                    }
                }
                Some(Msg::Close { stream, .. }) => {
                    heard = Instant::now();
                    streams.remove(&stream);
                }
                Some(Msg::Pong { t, at }) => {
                    heard = Instant::now();
                    guard.measured(t, at, now_ms());
                }
                Some(Msg::Ping { t }) => {
                    heard = Instant::now();
                    let _ = out.send(Msg::Pong { t, at: now_ms() });
                }
                Some(other) => {
                    tracing::warn!("[relay] unexpected on the link: {other:?}");
                    break false;
                }
            },
            Some(stream) = ended_rx.recv() => {
                if streams.remove(&stream).is_some() {
                    let _ = out.send(Msg::Close { stream, reason: None });
                }
            }
            _ = tick.tick() => {
                if heard.elapsed() > LINK_TIMEOUT {
                    break false;
                }
                let _ = out.send(Msg::Ping { t: now_ms() });
            }
            _ = stop.changed() => break true,
        }
    };
    // Every session ends with its stream, and its holds with it.
    streams.clear();
    read_task.abort();
    // Let a last close or refusal go out, then end the connection.
    drop(out);
    let _ = tokio::time::timeout(std::time::Duration::from_millis(200), write_task).await;
    stopped
}

/// One page stream: a session reading what the companion sends, its pushes going back as data.
fn open_session(
    app: &Arc<crate::service::App>,
    guard: &Arc<LinkGuard>,
    out: &mpsc::UnboundedSender<Msg>,
    ended: &mpsc::UnboundedSender<u32>,
    stream: u32,
    client: String,
) -> Stream {
    let (to_session, from_link) = mpsc::unbounded_channel::<String>();
    let (session_out, mut pushes) = mpsc::unbounded_channel::<String>();
    let incoming = Box::pin(stream::unfold(from_link, |mut rx| async move {
        rx.recv().await.map(|text| (text, rx))
    }));
    let wire_out = out.clone();
    let push_task = tokio::spawn(async move {
        while let Some(text) = pushes.recv().await {
            if wire_out.send(Msg::Data { stream, text }).is_err() {
                return;
            }
        }
    });
    let (app, policy, ended) = (
        Arc::clone(app),
        Policy::Companion(Arc::clone(guard)),
        ended.clone(),
    );
    tokio::spawn(async move {
        session::serve_stream(app, client, policy, incoming, session_out).await;
        push_task.abort();
        let _ = ended.send(stream);
    });
    Stream { to_session }
}
