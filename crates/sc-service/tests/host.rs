//! Peer-to-peer hosting against a real socket on loopback: the host hub with raw members that
//! speak the wire protocol themselves, so a test can choose the source address, leave out the
//! TLS binding, send any card and watch every message that comes back.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use fleet_proto::card::{CARD_SCHEMA, FieldValue, Fields, Sharing, Tone};
use fleet_proto::hello::capability;
use fleet_proto::identity::ClientHandshake;
use fleet_proto::{Card, Hello, Identity, Message, Nonce, PublicKey, RefusalCode, Subscription};
use sc_service::fleet::bans::{BANS_FILE, BanStore};
use sc_service::fleet::host::{FleetHost, Limits};
use sc_service::fleet::{FleetHub, FleetViewStore, tls_binding};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf, split};
use tokio::net::{TcpSocket, TcpStream};
use tokio_rustls::client::TlsStream;

const LOOP: IpAddr = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
const OTHER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 2));
const WAIT: Duration = Duration::from_secs(5);

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

struct Host {
    host: Arc<FleetHost>,
    hub: Arc<FleetHub>,
    identity: Identity,
    port: u16,
    dir: tempfile::TempDir,
}

impl Host {
    async fn start(limits: Limits) -> Self {
        Self::start_in(
            tempfile::tempdir().unwrap(),
            limits,
            Identity::generate().unwrap(),
        )
        .await
    }

    async fn start_in(dir: tempfile::TempDir, limits: Limits, identity: Identity) -> Self {
        let hub = Arc::new(FleetHub::new(
            Arc::new(FleetViewStore::new(None)),
            identity.public(),
        ));
        let host = FleetHost::new(
            &identity,
            BanStore::new(Some(dir.path().join(BANS_FILE))),
            Arc::clone(&hub),
            LOOP,
            limits,
            Some(dir.path().join("app-settings.json")),
        );
        let port = host.start(0, "Host").await.unwrap();
        Self {
            host,
            hub,
            identity,
            port,
            dir,
        }
    }

    fn addr(&self) -> SocketAddr {
        SocketAddr::new(LOOP, self.port)
    }

    fn code(&self) -> String {
        self.host.code().unwrap()
    }

    async fn join(&self, handle: &str) -> Raw {
        let (raw, answer) = Raw::join_as(
            self,
            Identity::generate().unwrap(),
            LOOP,
            Some(&self.code()),
            handle,
        )
        .await;
        assert!(matches!(answer, Message::Joined { .. }), "{answer:?}");
        raw
    }

    async fn until(&self, what: &str, ok: impl Fn(&Self) -> bool) {
        let end = tokio::time::Instant::now() + WAIT;
        while !ok(self) {
            assert!(tokio::time::Instant::now() < end, "timed out: {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

struct Raw {
    identity: Identity,
    reader: BufReader<ReadHalf<TlsStream<TcpStream>>>,
    writer: WriteHalf<TlsStream<TcpStream>>,
}

fn card(member: &str, fields: &[&'static str]) -> Card {
    Card {
        schema: CARD_SCHEMA,
        member: member.to_owned(),
        handle: "anything".to_owned(),
        sharing: Sharing {
            ship: true,
            ..Sharing::default()
        },
        state: None,
        fields: Fields(
            fields
                .iter()
                .map(|f| (*f, FieldValue::text("x", Tone::Ok)))
                .collect(),
        ),
        source: None,
    }
}

impl Raw {
    /// TLS from `from`, the hello (with or without `tls-binding`), and the proof. `Err` is the
    /// host's refusal, or `None` when it closed.
    #[allow(clippy::result_large_err)]
    async fn connect(
        addr: SocketAddr,
        from: IpAddr,
        identity: Identity,
        server: PublicKey,
        bind: bool,
    ) -> Result<Self, Option<Message>> {
        let socket = TcpSocket::new_v4().unwrap();
        socket.bind(SocketAddr::new(from, 0)).unwrap();
        let tcp = socket.connect(addr).await.unwrap();
        let config = sc_service::tls::client_config().unwrap();
        let name = rustls::pki_types::ServerName::try_from("127.0.0.1").unwrap();
        let tls = tokio_rustls::TlsConnector::from(config)
            .connect(name, tcp)
            .await
            .map_err(|_| None)?;
        let binding = tls_binding(tls.get_ref().1).unwrap();
        let (r, w) = split(tls);
        let mut raw = Self {
            identity,
            reader: BufReader::new(r),
            writer: w,
        };
        let mut caps = vec![capability::CARDS, capability::SUBSCRIBE, capability::JOIN];
        if bind {
            caps.push(capability::TLS_BINDING);
        }
        let nonce = Nonce::random().unwrap();
        raw.send(&Message::ClientHello {
            hello: Hello::new("raw", &caps),
            key: raw.identity.public(),
            nonce,
        })
        .await;
        let (key, challenge, proof) = match raw.next().await {
            Some(Message::ServerHello {
                key,
                challenge,
                proof,
                ..
            }) => (key, challenge, proof),
            other => return Err(other),
        };
        let answer = ClientHandshake {
            identity: &raw.identity,
            nonce,
            pinned: Some(server),
            binding: Some(binding),
        }
        .answer(&key, &challenge, &proof, now_ms())
        .unwrap();
        raw.send(&Message::ClientProof { proof: answer }).await;
        match raw.next().await {
            Some(Message::Welcome { .. }) => Ok(raw),
            other => Err(other),
        }
    }

    async fn join_as(
        host: &Host,
        identity: Identity,
        from: IpAddr,
        code: Option<&str>,
        handle: &str,
    ) -> (Self, Message) {
        let mut raw = Self::connect(host.addr(), from, identity, host.identity.public(), true)
            .await
            .unwrap();
        raw.send(&Message::Join {
            code: code.map(str::to_owned),
            handle: handle.to_owned(),
        })
        .await;
        let answer = raw.next().await.expect("an answer to join");
        (raw, answer)
    }

    fn member(&self) -> String {
        self.identity.public().to_string()
    }

    async fn send(&mut self, m: &Message) {
        self.writer.write_all(&m.encode()).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    /// The next message, or `None` when the host closed or nothing came within [`WAIT`].
    async fn next(&mut self) -> Option<Message> {
        let mut line = Vec::new();
        match tokio::time::timeout(WAIT, self.reader.read_until(b'\n', &mut line)).await {
            Ok(Ok(n)) if n > 0 => Message::decode(&line).ok(),
            _ => None,
        }
    }

    /// Reads until a message `pick` takes, or `None`.
    async fn until<T>(&mut self, mut pick: impl FnMut(Message) -> Option<T>) -> Option<T> {
        while let Some(m) = self.next().await {
            if let Some(t) = pick(m) {
                return Some(t);
            }
        }
        None
    }

    /// The next card from `member`.
    async fn card_from(&mut self, member: &str) -> Option<Card> {
        self.until(|m| match m {
            Message::Card { card } if card.member == member => Some(card),
            _ => None,
        })
        .await
    }

    /// The refusal the host closes with.
    async fn refusal(&mut self) -> Option<RefusalCode> {
        self.until(|m| match m {
            Message::Refused { code, .. } => Some(code),
            _ => None,
        })
        .await
        .flatten()
    }
}

fn refusal_code(m: &Message) -> Option<RefusalCode> {
    match m {
        Message::Refused { code, .. } => *code,
        _ => None,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn three_members_join_see_each_other_and_get_no_roles_or_structure() {
    let host = Host::start(Limits::default()).await;
    let mut a = host.join("Vega").await;
    let mut b = host.join("Rook").await;
    let mut c = host.join("Wren").await;
    host.until("four members", |h| h.host.member_count() == 4)
        .await;

    for sender in [&mut a, &mut b, &mut c] {
        let me = sender.member();
        sender
            .send(&Message::Card {
                card: card(&me, &["ship.hull"]),
            })
            .await;
    }
    let (ma, mb, mc) = (a.member(), b.member(), c.member());
    for (viewer, others) in [
        (&mut a, [&mb, &mc]),
        (&mut b, [&ma, &mc]),
        (&mut c, [&ma, &mb]),
    ] {
        let mut seen = std::collections::HashSet::new();
        viewer
            .until(|m| {
                if let Message::Card { card } = m {
                    assert_ne!(card.handle, "anything", "the host rewrites the handle");
                    seen.insert(card.member);
                }
                others.iter().all(|o| seen.contains(*o)).then_some(())
            })
            .await
            .expect("a card from each other member");
    }

    // A roster with handles and keys, no roles, no units, no structure.
    let mut d = host.join("Kite").await;
    let mut saw_structure = false;
    let roster = d
        .until(|m| match m {
            Message::Roster { you, members } => Some((you, members)),
            Message::Structure { .. } => {
                saw_structure = true;
                None
            }
            _ => None,
        })
        .await
        .unwrap();
    assert!(roster.0.is_empty());
    assert_eq!(roster.1.len(), 5);
    assert!(
        roster
            .1
            .iter()
            .all(|e| e.role.is_none() && e.unit.is_none())
    );
    assert!(!saw_structure);
    // The host is admiral and seated in the tree kept in memory.
    let tree = host.host.tree().unwrap();
    let me = host.identity.public().to_string();
    assert_eq!(tree.admiral.as_deref(), Some(me.as_str()));
    assert!(tree.seat_of(&me).is_some());
    assert!(tree.problems().is_empty(), "{:?}", tree.problems());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hello_without_the_tls_binding_is_refused() {
    let host = Host::start(Limits::default()).await;
    let refused = Raw::connect(
        host.addr(),
        LOOP,
        Identity::generate().unwrap(),
        host.identity.public(),
        false,
    )
    .await
    .err()
    .flatten();
    assert!(
        matches!(refused, Some(Message::Refused { .. })),
        "{refused:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codes_rotation_and_rejoin_by_key() {
    let host = Host::start(Limits::default()).await;
    let (_, wrong) = Raw::join_as(
        &host,
        Identity::generate().unwrap(),
        LOOP,
        Some("AAAA-BBBB-CCCC-DDDD"),
        "X",
    )
    .await;
    assert_eq!(refusal_code(&wrong), Some(RefusalCode::BadCode));

    let vega = Identity::generate().unwrap();
    let seed = vega.seed();
    let (mut a, joined) = Raw::join_as(&host, vega, LOOP, Some(&host.code()), "Vega").await;
    assert!(matches!(joined, Message::Joined { .. }));
    let old = host.code();
    let new = host.host.rotate().unwrap();
    assert_ne!(old, new);
    assert_eq!(new.len(), 19);

    let (_, stale) =
        Raw::join_as(&host, Identity::generate().unwrap(), LOOP, Some(&old), "Y").await;
    assert_eq!(refusal_code(&stale), Some(RefusalCode::BadCode));
    // Still connected: a card from a new member reaches the one joined before the rotation.
    let mut b = host.join("Rook").await;
    let mb = b.member();
    b.send(&Message::Card {
        card: card(&mb, &["ship.hull"]),
    })
    .await;
    assert!(a.card_from(&mb).await.is_some());

    // A drop, then a rejoin by key with no code, after the rotation.
    drop(a);
    host.until("vega dropped", |h| {
        h.host.view().members.iter().filter(|m| m.connected).count() == 1
    })
    .await;
    let (_, again) = Raw::join_as(&host, Identity::from_seed(seed), LOOP, None, "Vega").await;
    assert!(matches!(again, Message::Joined { .. }), "{again:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn kick_bans_the_key_and_kills_the_code_while_others_stay() {
    let dir = tempfile::tempdir().unwrap();
    let hid = Identity::generate().unwrap();
    let hseed = hid.seed();
    let host = Host::start_in(dir, Limits::default(), hid).await;
    let bad = Identity::generate().unwrap();
    let bad_seed = bad.seed();
    let (mut a, _) = Raw::join_as(&host, bad, LOOP, Some(&host.code()), "Griefer").await;
    let mut b = host.join("Rook").await;
    let before = host.code();
    host.host.kick(&a.member()).unwrap();
    assert_eq!(a.refusal().await, Some(RefusalCode::Banned));
    assert_ne!(host.code(), before, "kick replaces the code");
    assert_eq!(host.host.bans().len(), 1);
    assert_eq!(host.host.bans()[0].handle, "Griefer");

    // Rook stays and still gets cards.
    let mut c = host.join("Wren").await;
    let mc = c.member();
    c.send(&Message::Card {
        card: card(&mc, &["ship.hull"]),
    })
    .await;
    assert!(b.card_from(&mc).await.is_some());

    // The banned key holding the current, valid code.
    let (_, banned) = Raw::join_as(
        &host,
        Identity::from_seed(bad_seed),
        LOOP,
        Some(&host.code()),
        "Griefer",
    )
    .await;
    assert_eq!(refusal_code(&banned), Some(RefusalCode::Banned));

    // The ban outlives the host: a new session reads it from the file.
    let Host { host: h, dir, .. } = host;
    h.stop().await;
    let host = Host::start_in(dir, Limits::default(), Identity::from_seed(hseed)).await;
    let (_, still) = Raw::join_as(
        &host,
        Identity::from_seed(bad_seed),
        LOOP,
        Some(&host.code()),
        "Griefer",
    )
    .await;
    assert_eq!(refusal_code(&still), Some(RefusalCode::Banned));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(host.dir.path().join(BANS_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
    let key = Identity::from_seed(bad_seed).public().to_string();
    host.host.lift(&key).unwrap();
    let (_, back) = Raw::join_as(
        &host,
        Identity::from_seed(bad_seed),
        LOOP,
        Some(&host.code()),
        "Griefer",
    )
    .await;
    assert!(matches!(back, Message::Joined { .. }), "{back:?}");
    assert!(host.host.kick(&host.identity.public().to_string()).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remove_drops_a_member_who_may_join_again_with_the_current_code() {
    let host = Host::start(Limits::default()).await;
    let id = Identity::generate().unwrap();
    let seed = id.seed();
    let (mut a, _) = Raw::join_as(&host, id, LOOP, Some(&host.code()), "Laggy").await;
    let code = host.code();
    host.host.remove(&a.member()).unwrap();
    assert_eq!(a.refusal().await, Some(RefusalCode::Kicked));
    assert_eq!(host.code(), code, "remove keeps the code");
    assert!(host.host.bans().is_empty());
    assert_eq!(host.host.member_count(), 1);
    // Forgotten: not by key.
    let (_, by_key) = Raw::join_as(&host, Identity::from_seed(seed), LOOP, None, "Laggy").await;
    assert_eq!(refusal_code(&by_key), Some(RefusalCode::SessionEnded));
    let (_, again) =
        Raw::join_as(&host, Identity::from_seed(seed), LOOP, Some(&code), "Laggy").await;
    assert!(matches!(again, Message::Joined { .. }));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_card_for_another_member_id_is_not_relayed() {
    let host = Host::start(Limits::default()).await;
    let mut a = host.join("Vega").await;
    let mut b = host.join("Rook").await;
    let mb = b.member();
    let other = Identity::generate().unwrap().public().to_string();
    a.send(&Message::Card {
        card: card(&other, &["ship.hull"]),
    })
    .await;
    a.send(&Message::Card {
        card: card(&mb, &["ship.hull"]),
    })
    .await;
    let ma = a.member();
    a.send(&Message::Card {
        card: card(&ma, &["ship.fuel"]),
    })
    .await;
    let first = b
        .until(|m| match m {
            Message::Card { card } => Some(card),
            _ => None,
        })
        .await
        .unwrap();
    assert_eq!(first.member, ma, "only the card in a's own name");
    assert!(first.fields.get("ship.fuel").is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_code_flood_from_one_address_does_not_stop_another() {
    let host = Host::start(Limits::default()).await;
    for _ in 0..10 {
        let (_, m) = Raw::join_as(
            &host,
            Identity::generate().unwrap(),
            OTHER,
            Some("AAAA-BBBB-CCCC-DDDD"),
            "F",
        )
        .await;
        assert_eq!(refusal_code(&m), Some(RefusalCode::BadCode));
    }
    // Past the limit even the right code is refused from that address...
    let (_, blocked) = Raw::join_as(
        &host,
        Identity::generate().unwrap(),
        OTHER,
        Some(&host.code()),
        "F",
    )
    .await;
    match &blocked {
        Message::Refused { code, reason } => {
            assert_eq!(*code, Some(RefusalCode::BadCode));
            assert!(reason.contains("Too many"), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    // ...while another address joins.
    host.join("Vega").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_rejoin_after_a_host_restart_is_told_the_session_ended() {
    let dir = tempfile::tempdir().unwrap();
    let hid = Identity::generate().unwrap();
    let hseed = hid.seed();
    let host = Host::start_in(dir, Limits::default(), hid).await;
    let id = Identity::generate().unwrap();
    let seed = id.seed();
    let (mut a, _) = Raw::join_as(&host, id, LOOP, Some(&host.code()), "Vega").await;
    let old = host.code();
    let Host { host: h, dir, .. } = host;
    h.stop().await;
    let bye = a
        .until(|m| matches!(m, Message::Bye { .. }).then_some(()))
        .await;
    assert!(bye.is_some(), "members are told the host stopped");
    let host = Host::start_in(dir, Limits::default(), Identity::from_seed(hseed)).await;
    assert_ne!(host.code(), old);
    let (_, ended) = Raw::join_as(&host, Identity::from_seed(seed), LOOP, None, "Vega").await;
    assert_eq!(refusal_code(&ended), Some(RefusalCode::SessionEnded));
    assert_ne!(
        RefusalCode::SessionEnded.hint(),
        RefusalCode::BadCode.hint()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_handles_get_a_suffix() {
    let host = Host::start(Limits::default()).await;
    let mut handles = vec![];
    for wanted in ["Vega", "vega", "VEGA", "Host"] {
        let (_, joined) = Raw::join_as(
            &host,
            Identity::generate().unwrap(),
            LOOP,
            Some(&host.code()),
            wanted,
        )
        .await;
        let Message::Joined { handle, .. } = joined else {
            panic!("{joined:?}")
        };
        handles.push(handle);
    }
    assert_eq!(handles, ["Vega", "vega-2", "VEGA-3", "Host-2"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_loopback_self_join_counts_as_a_member_and_is_never_limited() {
    let limits = Limits {
        max_members: 3,
        max_failures: 1,
        ..Limits::default()
    };
    let host = Host::start(limits).await;
    // Block loopback for codes: one failure is the limit here.
    let (_, wrong) = Raw::join_as(
        &host,
        Identity::generate().unwrap(),
        LOOP,
        Some("AAAA-BBBB-CCCC-DDDD"),
        "F",
    )
    .await;
    assert_eq!(refusal_code(&wrong), Some(RefusalCode::BadCode));
    // The host's own key over loopback still joins, with a wrong code even.
    let own = Identity::from_seed(host.identity.seed());
    let (_, me) = Raw::join_as(&host, own, LOOP, Some("WRONG"), "Host").await;
    assert!(matches!(me, Message::Joined { .. }), "{me:?}");
    // The host counts: host and two members fill three places.
    for _ in 0..2 {
        let (_, j) = Raw::join_as(
            &host,
            Identity::generate().unwrap(),
            OTHER,
            Some(&host.code()),
            "M",
        )
        .await;
        assert!(matches!(j, Message::Joined { .. }), "{j:?}");
    }
    let (_, full) = Raw::join_as(
        &host,
        Identity::generate().unwrap(),
        OTHER,
        Some(&host.code()),
        "M",
    )
    .await;
    assert_eq!(refusal_code(&full), Some(RefusalCode::Full));
    let own = Identity::from_seed(host.identity.seed());
    let (_, again) = Raw::join_as(&host, own, LOOP, None, "Host").await;
    assert!(matches!(again, Message::Joined { .. }), "{again:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn senders_are_asked_for_what_their_viewers_want_and_viewers_get_only_that() {
    let host = Host::start(Limits::default()).await;
    let mut viewer = host.join("Vega").await;
    let mut sender = host.join("Rook").await;
    viewer
        .send(&Message::Subscribe {
            subscription: Subscription::of(["ship.hull"]),
        })
        .await;
    let asked = sender
        .until(|m| match m {
            Message::Subscribe { subscription } if subscription.fields.len() == 1 => {
                Some(subscription)
            }
            _ => None,
        })
        .await
        .unwrap();
    assert!(asked.wants("ship.hull"));
    let ms = sender.member();
    sender
        .send(&Message::Card {
            card: card(&ms, &["ship.hull", "ship.fuel"]),
        })
        .await;
    let got = viewer.card_from(&ms).await.unwrap();
    assert_eq!(got.fields.ids().collect::<Vec<_>>(), ["ship.hull"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_and_silent_connections_are_capped_and_timed_out() {
    let limits = Limits {
        handshake: Duration::from_millis(400),
        join: Duration::from_millis(800),
        max_pending: 2,
        ..Limits::default()
    };
    let host = Host::start(limits).await;
    let mut idle = vec![];
    for _ in 0..2 {
        idle.push(TcpStream::connect(host.addr()).await.unwrap());
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    // A third, over the cap, is closed at once.
    let mut third = TcpStream::connect(host.addr()).await.unwrap();
    let mut byte = [0u8; 1];
    let n = tokio::time::timeout(
        Duration::from_millis(300),
        tokio::io::AsyncReadExt::read(&mut third, &mut byte),
    )
    .await
    .expect("closed before the handshake timeout")
    .unwrap_or(0);
    assert_eq!(n, 0);
    // The silent two are dropped after the handshake limit, and a member joins again.
    for mut s in idle {
        let n = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::io::AsyncReadExt::read(&mut s, &mut byte),
        )
        .await
        .expect("dropped after the handshake limit")
        .unwrap_or(0);
        assert_eq!(n, 0);
    }
    // Welcomed but no join: dropped after the join limit.
    let mut quiet = Raw::connect(
        host.addr(),
        LOOP,
        Identity::generate().unwrap(),
        host.identity.public(),
        true,
    )
    .await
    .unwrap();
    assert!(quiet.next().await.is_none());
    host.join("Vega").await;
    let _ = &host.hub;
}
