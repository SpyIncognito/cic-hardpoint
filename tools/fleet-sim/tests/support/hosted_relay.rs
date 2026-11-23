//! A small hosted relay for fleet-sim's tests: the identity handshake bound to TLS, then
//! `connect` with connection tokens, bindings by key, `events` and `choose`, `joined`, and the
//! refusal codes, as PROTOCOL.md 4.3 has them. Cards are kept, not relayed. Not the CIC relay.

#![allow(dead_code)]

use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use fleet_proto::hello::capability;
use fleet_proto::identity::{ChallengeIssuer, TlsBinding};
use fleet_proto::invite::new_connect_token;
use fleet_proto::message::MAX_FRAME_BYTES;
use fleet_proto::{Card, EventChoice, Hello, Identity, Message, PublicKey, RefusalCode};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, split};
use tokio::net::TcpListener;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::test_relay::tls;

#[derive(Clone)]
pub struct Account {
    pub id: String,
    pub handle: String,
    /// The open events this account is in: `(id, name)`.
    pub events: Vec<(String, String)>,
    pub kicked: bool,
}

struct Token {
    token: String,
    account: usize,
    event: Option<String>,
    spent: bool,
}

#[derive(Default)]
pub struct Seen {
    /// Every `connect`: whether it had a token, and its event.
    pub connects: Vec<(bool, Option<String>)>,
    pub chooses: Vec<String>,
    /// `(account id, event)` for every `joined`.
    pub joined: Vec<(String, String)>,
    /// Cards by the sender's key.
    pub cards: Vec<(PublicKey, Card)>,
    pub byes: usize,
}

struct State {
    identity: Identity,
    issuer: Mutex<ChallengeIssuer>,
    accounts: Mutex<Vec<Account>>,
    tokens: Mutex<Vec<Token>>,
    bound: Mutex<HashMap<PublicKey, usize>>,
    seen: Mutex<Seen>,
    kill: Notify,
    card_every_ms: u64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

pub struct HostedRelay {
    pub addr: SocketAddr,
    state: Arc<State>,
    task: JoinHandle<()>,
}

impl Drop for HostedRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl HostedRelay {
    pub async fn start(accounts: Vec<Account>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(State {
            identity: Identity::generate().unwrap(),
            issuer: Mutex::default(),
            accounts: Mutex::new(accounts),
            tokens: Mutex::default(),
            bound: Mutex::default(),
            seen: Mutex::default(),
            kill: Notify::new(),
            card_every_ms: 200,
        });
        let acceptor = tls();
        let serving = Arc::clone(&state);
        let task = tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let (state, acceptor) = (Arc::clone(&serving), acceptor.clone());
                tokio::spawn(async move {
                    if let Ok(stream) = acceptor.accept(tcp).await {
                        let binding = sc_service::fleet::tls_binding(stream.get_ref().1).ok();
                        let _ = state.handle(stream, binding).await;
                    }
                });
            }
        });
        Self { addr, state, task }
    }

    /// A new invite for an account (by index), with `event` for a per-event token.
    pub fn invite(&self, account: usize, event: Option<&str>) -> String {
        let token = new_connect_token().unwrap();
        lock(&self.state.tokens).push(Token {
            token: token.clone(),
            account,
            event: event.map(str::to_owned),
            spent: false,
        });
        let mut text = format!(
            "cic://127.0.0.1:{}?key={}&connect={token}",
            self.addr.port(),
            self.state.identity.public()
        );
        if let Some(e) = event {
            text.push_str(&format!("&event={e}"));
        }
        text
    }

    /// An invite with a token no account has.
    pub fn unknown_invite(&self) -> String {
        format!(
            "cic://127.0.0.1:{}?key={}&connect={}",
            self.addr.port(),
            self.state.identity.public(),
            new_connect_token().unwrap()
        )
    }

    pub fn seen(&self) -> MutexGuard<'_, Seen> {
        lock(&self.state.seen)
    }

    pub fn drop_connections(&self) {
        self.state.kill.notify_waiters();
    }

    /// Drops every binding, as a new token or a password reset would.
    pub fn unbind_all(&self) {
        lock(&self.state.bound).clear();
    }

    pub fn kick(&self, account: usize) {
        lock(&self.state.accounts)[account].kicked = true;
    }
}

async fn read<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
) -> io::Result<Option<Message>> {
    let n = (&mut *reader)
        .take(MAX_FRAME_BYTES as u64 + 2)
        .read_until(b'\n', line)
        .await?;
    if n == 0 && line.is_empty() {
        return Ok(None);
    }
    let message = Message::decode(line).map_err(io::Error::other);
    line.clear();
    message.map(Some)
}

async fn write<W: AsyncWrite + Unpin>(w: &mut W, m: &Message) -> io::Result<()> {
    w.write_all(&m.encode()).await?;
    w.flush().await
}

async fn refuse<W: AsyncWrite + Unpin>(w: &mut W, code: RefusalCode) -> io::Result<()> {
    let reason = format!("Refused: {code:?}");
    write(
        w,
        &Message::Refused {
            reason,
            code: Some(code),
        },
    )
    .await
}

impl State {
    /// The account a `connect` names, and the per-event token's event.
    fn bind(
        &self,
        key: &PublicKey,
        token: Option<String>,
        event: Option<&String>,
    ) -> Result<(usize, Option<String>), RefusalCode> {
        let Some(t) = token else {
            return lock(&self.bound)
                .get(key)
                .map(|&a| (a, None))
                .ok_or(RefusalCode::Unbound);
        };
        let mut tokens = lock(&self.tokens);
        let found = tokens
            .iter_mut()
            .find(|k| k.token == t)
            .ok_or(RefusalCode::TokenUnknown)?;
        if found.spent {
            return Err(RefusalCode::TokenSpent);
        }
        if found.event.is_some() && event.is_some() && found.event.as_ref() != event {
            return Err(RefusalCode::WrongEvent);
        }
        found.spent = true;
        lock(&self.bound).insert(*key, found.account);
        Ok((found.account, found.event.clone()))
    }

    async fn handle<S>(self: Arc<Self>, stream: S, binding: Option<TlsBinding>) -> io::Result<()>
    where
        S: tokio::io::AsyncRead + AsyncWrite + Unpin,
    {
        let (reader, mut w) = split(stream);
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        let Some(Message::ClientHello { hello, key, nonce }) = read(&mut reader, &mut line).await?
        else {
            return Ok(());
        };
        let ours = Hello::new(
            "hosted-test-relay",
            &[
                capability::CARDS,
                capability::SUBSCRIBE,
                capability::STRUCTURE,
                capability::CONNECT,
                capability::TLS_BINDING,
            ],
        );
        let (challenge, proof) = lock(&self.issuer)
            .issue(&self.identity, &key, &nonce, binding.as_ref(), now_ms())
            .unwrap();
        write(
            &mut w,
            &Message::ServerHello {
                hello: ours.clone(),
                server: "Hosted test relay".to_owned(),
                key: self.identity.public(),
                challenge,
                proof,
            },
        )
        .await?;
        let Some(Message::ClientProof { proof }) = read(&mut reader, &mut line).await? else {
            return Ok(());
        };
        if lock(&self.issuer)
            .accept(
                &self.identity.public(),
                &key,
                &nonce,
                &challenge,
                &proof,
                binding.as_ref(),
                now_ms(),
            )
            .is_err()
        {
            return Ok(());
        }
        write(
            &mut w,
            &Message::Welcome {
                event: String::new(),
                capabilities: ours.common(&hello),
            },
        )
        .await?;
        let Some(Message::Connect { token, event }) = read(&mut reader, &mut line).await? else {
            return Ok(());
        };
        lock(&self.seen)
            .connects
            .push((token.is_some(), event.clone()));
        let (account, per_event) = match self.bind(&key, token, event.as_ref()) {
            Ok(found) => found,
            Err(code) => return refuse(&mut w, code).await,
        };
        let user = lock(&self.accounts)[account].clone();
        if user.kicked {
            return refuse(&mut w, RefusalCode::Kicked).await;
        }
        let open = &user.events;
        let chosen = if let Some(e) = per_event {
            e
        } else if let Some(e) = event.filter(|e| open.iter().any(|(id, _)| id == e)) {
            e
        } else if open.len() == 1 {
            open[0].0.clone()
        } else {
            let events = open
                .iter()
                .map(|(id, name)| EventChoice {
                    id: id.clone(),
                    name: name.clone(),
                    unit: None,
                    role: None,
                })
                .collect();
            write(&mut w, &Message::Events { events }).await?;
            let Some(Message::Choose { event }) = read(&mut reader, &mut line).await? else {
                return Ok(());
            };
            lock(&self.seen).chooses.push(event.clone());
            if !open.iter().any(|(id, _)| *id == event) {
                return refuse(&mut w, RefusalCode::NoSuchEvent).await;
            }
            event
        };
        let name = open
            .iter()
            .find(|(id, _)| *id == chosen)
            .map(|(_, n)| n.clone())
            .unwrap_or_default();
        lock(&self.seen)
            .joined
            .push((user.id.clone(), chosen.clone()));
        write(
            &mut w,
            &Message::Joined {
                event: chosen,
                name,
                member: Some(user.id.clone()),
                handle: user.handle.clone(),
                unit: None,
                grants: vec![],
                card_every_ms: Some(self.card_every_ms),
            },
        )
        .await?;
        write(
            &mut w,
            &Message::Roster {
                you: vec![],
                members: vec![],
            },
        )
        .await?;
        let mut kicks = tokio::time::interval(std::time::Duration::from_millis(100));
        loop {
            tokio::select! {
                () = self.kill.notified() => return Ok(()),
                _ = kicks.tick() => {
                    if lock(&self.accounts)[account].kicked {
                        return refuse(&mut w, RefusalCode::Kicked).await;
                    }
                }
                m = read(&mut reader, &mut line) => match m? {
                    None => return Ok(()),
                    Some(Message::Card { card }) => lock(&self.seen).cards.push((key, card)),
                    Some(Message::Bye { .. }) => {
                        lock(&self.seen).byes += 1;
                        return Ok(());
                    }
                    Some(_) => {}
                },
            }
        }
    }
}
