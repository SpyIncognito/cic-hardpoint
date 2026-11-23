//! Relay mode end to end on loopback: one service as the game PC on a replayed game, one as its
//! companion, pairing over the real TLS listener, and a companion page that runs on the game PC.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use sc_core::engine::EngineOptions;
use sc_game::{GameAction, GameApi, RecordedFrame, ReplayGame, ReplayOptions};
use sc_service::relay::{LinkState, Mode, RelayOptions, Status};
use sc_service::{Service, ServiceOptions};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn game() -> Arc<ReplayGame> {
    let frame = json!({ "vehicle": { "state": {
        "name": "Test ship",
        "lights_enabled": true,
        "flight": { "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED" }, "instruments": { "speed": 42 } },
    } } });
    let frames = (0..50)
        .map(|i| RecordedFrame {
            t: f64::from(i) * 20.0,
            frame: frame.clone(),
        })
        .collect();
    let actions = ["v_lights", "v_power_set_off"]
        .map(|name| GameAction {
            name: name.into(),
            category: "test".into(),
            holdable: false,
        })
        .to_vec();
    Arc::new(
        ReplayGame::new(
            frames,
            ReplayOptions {
                loop_playback: true,
                actions,
                ..ReplayOptions::default()
            },
        )
        .unwrap(),
    )
}

struct Machine {
    service: Service,
    game: Arc<ReplayGame>,
    _dir: tempfile::TempDir,
}

async fn machine(name: &str, version: &str) -> Machine {
    let game = game();
    let dir = tempfile::tempdir().unwrap();
    let service = Service::start(ServiceOptions {
        port: 0,
        settings_dir: Some(dir.path().to_owned()),
        bus_file: dir.path().join("bus.json"),
        game: Some(Arc::clone(&game) as Arc<dyn GameApi>),
        engine: EngineOptions::default(),
        fleet_connector: None,
        relay: RelayOptions {
            bind: [127, 0, 0, 1].into(),
            port: 0,
            version: version.to_owned(),
            name: name.to_owned(),
        },
    })
    .await
    .unwrap();
    Machine {
        service,
        game,
        _dir: dir,
    }
}

impl Machine {
    fn triggers(&self) -> Vec<String> {
        self.game
            .trigger_calls()
            .into_iter()
            .map(|c| c.name)
            .collect()
    }

    async fn until(&self, what: &str, done: impl Fn(&Status) -> bool) -> Status {
        let mut status = self.service.relay().subscribe();
        let wait = status.wait_for(|s| done(s));
        match tokio::time::timeout(Duration::from_secs(10), wait).await {
            Ok(Ok(s)) => s.clone(),
            _ => panic!("{what}: status stayed {:?}", self.service.relay().status()),
        }
    }

    async fn page(&self) -> Page {
        let port = self.service.port();
        let mut request = format!("ws://127.0.0.1:{port}/portal")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "origin",
            format!("http://127.0.0.1:{port}").parse().unwrap(),
        );
        let (mut ws, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        let hello =
            json!({ "type": "hello", "token": self.service.token().as_str(), "client": "dash" });
        ws.send(Message::Text(hello.to_string().into()))
            .await
            .unwrap();
        Page {
            ws,
            seen: vec![],
            next_id: 1,
        }
    }

    /// The companion's listening address.
    fn address(&self) -> String {
        format!("127.0.0.1:{}", self.service.relay().status().port)
    }
}

struct Page {
    ws: Socket,
    seen: Vec<Value>,
    next_id: u32,
}

impl Page {
    /// The next message, or `None` once the socket closed.
    async fn next(&mut self) -> Option<Value> {
        loop {
            match tokio::time::timeout(Duration::from_secs(5), self.ws.next())
                .await
                .ok()??
            {
                Ok(Message::Text(t)) => {
                    let m: Value = serde_json::from_str(&t).unwrap();
                    self.seen.push(m.clone());
                    return Some(m);
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    async fn until(&mut self, found: impl Fn(&Value) -> bool) -> Value {
        if let Some(m) = self.seen.iter().find(|m| found(m)) {
            return m.clone();
        }
        loop {
            let Some(m) = self.next().await else {
                panic!(
                    "closed before the message; seen {:?}",
                    self.seen
                        .iter()
                        .map(|m| m["type"].clone())
                        .collect::<Vec<_>>()
                );
            };
            if found(&m) {
                return m;
            }
        }
    }

    async fn send(&mut self, msg: Value) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let mut msg = msg;
        msg["id"] = json!(id);
        self.ws
            .send(Message::Text(msg.to_string().into()))
            .await
            .unwrap();
        id
    }

    /// The whole result message for `msg`.
    async fn call(&mut self, msg: Value) -> Value {
        let id = self.send(msg).await;
        self.until(|m| m["type"] == "result" && m["id"] == id).await
    }

    /// Reads until the socket closes.
    async fn closed(&mut self) {
        while self.next().await.is_some() {}
    }
}

/// The companion shows a code; the game PC pairs with it and the companion presses Pair.
async fn pair(game: &Machine, companion: &Machine) {
    let k = companion.service.relay();
    k.set_mode(Mode::Companion, true).await.unwrap();
    k.page_message("relay.code", &json!({})).await.unwrap();
    let code = k.status().pairing.unwrap().code.unwrap();
    let g = Arc::clone(game.service.relay());
    let address = companion.address();
    let pairing = tokio::spawn(async move { g.pair(&address, &code).await });
    companion
        .until("waiting for Pair", |s| {
            s.pairing.as_ref().is_some_and(|p| p.pending.is_some())
        })
        .await;
    let pending = k.status().pairing.unwrap().pending.unwrap();
    assert_eq!(pending.name, "gamepc");
    assert_eq!(
        pending.fingerprint,
        game.service.relay().public_key().fingerprint()
    );
    let answer = k
        .page_message("relay.accept", &json!({ "pair": true }))
        .await
        .unwrap();
    assert_eq!(answer["problems"], json!([]));
    let result = pairing.await.unwrap();
    assert_eq!(result["problems"], json!([]), "{result}");
    assert_eq!(result["peer"]["name"], k.options().name.as_str());
}

async fn linked() -> (Machine, Machine) {
    let game = machine("gamepc", "1.0.0").await;
    let companion = machine("laptop", "1.0.0").await;
    pair(&game, &companion).await;
    game.service
        .relay()
        .set_mode(Mode::Relay, true)
        .await
        .unwrap();
    game.until("relaying", |s| s.state == LinkState::Up).await;
    companion
        .until("showing", |s| s.state == LinkState::Up)
        .await;
    (game, companion)
}

#[tokio::test]
async fn a_companion_page_shows_the_game_pcs_status_and_its_hold_is_timed_there() {
    let (game, companion) = linked().await;
    assert_eq!(game.service.relay().status().text, "relaying to laptop");
    assert_eq!(
        game.service.relay().status().title(),
        "Hardpoint: relaying to laptop"
    );
    assert_eq!(companion.service.relay().status().text, "showing gamepc");

    let mut page = companion.page().await;
    let relay = page.until(|m| m["type"] == "relay").await;
    assert_eq!(relay["via"], "gamepc");
    assert_eq!(relay["mode"], "companion");
    let status = page
        .until(|m| m["type"] == "status" && m["status"]["inShip"] == true)
        .await;
    assert_eq!(status["status"]["ship"], "Test ship");

    // relay.* is answered on the companion itself.
    let own = page.call(json!({ "type": "relay.status" })).await;
    assert_eq!(own["result"]["name"], "laptop");

    let early = page
        .call(json!({ "type": "press", "action": "v_power_set_off", "confirmed": true }))
        .await;
    assert_eq!(early["result"]["status"], "needs-confirm");
    page.send(json!({ "type": "hold.start", "action": "v_power_set_off" }))
        .await;
    tokio::time::sleep(Duration::from_millis(1550)).await;
    let held = page
        .call(json!({ "type": "press", "action": "v_power_set_off", "confirmed": true }))
        .await;
    assert_eq!(held["result"], json!({ "ok": true, "status": "done" }));
    assert_eq!(held["actor"], "laptop/dash");
    assert_eq!(game.triggers(), ["v_power_set_off"]);

    let plain = page
        .call(json!({ "type": "press", "action": "v_lights" }))
        .await;
    assert_eq!(plain["result"]["ok"], true);
    assert_eq!(game.triggers(), ["v_power_set_off", "v_lights"]);
}

#[tokio::test]
async fn the_game_pc_refuses_its_displays_and_relay_settings_to_the_companion() {
    let (_game, companion) = linked().await;
    let mut page = companion.page().await;
    page.until(|m| m["type"] == "status").await;
    page.call(json!({ "type": "dash.get" })).await;
    let dashboard = page.until(|m| m["type"] == "dashboard").await;
    let tab = dashboard["dashboard"]["tabs"][0]["id"].clone();
    for msg in [
        json!({ "type": "dash.setPlacement", "tab": tab, "placement": null }),
        json!({ "type": "dash.setBezel", "tab": tab, "bezel": null }),
    ] {
        let refused = page.call(msg).await;
        let problems = refused["result"]["problems"].as_array().unwrap();
        assert!(
            problems[0].as_str().unwrap().contains("game PC"),
            "{refused}"
        );
    }
    // Editing the dashboard is allowed, as is reading the fleet panel.
    let fleet = page.until(|m| m["type"] == "fleet").await;
    assert!(fleet.get("link").is_some());
    let export = page.call(json!({ "type": "dash.export" })).await;
    assert_eq!(export["result"]["name"], "sc-dashboard.json");
}

#[tokio::test]
async fn a_dropped_link_closes_the_page_and_a_hold_does_not_survive_it() {
    let (game, companion) = linked().await;
    let mut page = companion.page().await;
    page.until(|m| m["type"] == "status").await;
    page.send(json!({ "type": "hold.start", "action": "v_power_set_off" }))
        .await;

    game.service.relay().disconnect().await;
    assert_eq!(game.service.relay().status().text, "not connected");
    page.until(|m| m["type"] == "relay" && m.get("via").is_none())
        .await;
    page.closed().await;
    companion
        .until("waiting again", |s| s.state == LinkState::Listening)
        .await;

    // Without the link the page is told the game PC is not there.
    let mut page = companion.page().await;
    let relay = page.until(|m| m["type"] == "relay").await;
    assert_eq!(relay["state"], "listening");
    let answer = page
        .call(json!({ "type": "press", "action": "v_lights" }))
        .await;
    assert_eq!(answer["error"], "Not connected to the game PC");

    // Once the link is back the page is closed to reconnect, and the old hold is gone.
    game.service.relay().connect().await.unwrap();
    page.closed().await;
    let mut page = companion.page().await;
    page.until(|m| m["type"] == "status").await;
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let late = page
        .call(json!({ "type": "press", "action": "v_power_set_off", "confirmed": true }))
        .await;
    assert_eq!(late["result"]["status"], "needs-confirm");
    assert!(game.triggers().is_empty());
}

#[tokio::test]
async fn the_game_pc_retries_a_companion_that_went_away() {
    let (game, companion) = linked().await;
    companion.service.close().await;
    let status = game
        .until("retrying", |s| s.text.contains("retry in"))
        .await;
    assert_eq!(status.state, LinkState::Connecting);
    assert!(status.retry_ms.is_some());
}

#[tokio::test]
async fn a_wrong_code_fails_then_the_address_waits_and_three_burn_the_code() {
    let game = machine("gamepc", "1.0.0").await;
    let companion = machine("laptop", "1.0.0").await;
    let k = companion.service.relay();
    k.set_mode(Mode::Companion, false).await.unwrap();
    // No code shown: pairing is closed.
    let closed = game
        .service
        .relay()
        .pair(&companion.address(), "1234 5678")
        .await;
    assert!(
        closed["problems"][0]
            .as_str()
            .unwrap()
            .contains("not showing a pairing code"),
        "{closed}"
    );

    k.page_message("relay.code", &json!({})).await.unwrap();
    let code = k.status().pairing.unwrap().code.unwrap();
    let wrong = if code == "0000 0000" {
        "1111 1111"
    } else {
        "0000 0000"
    };
    let g = game.service.relay();
    let first = g.pair(&companion.address(), wrong).await;
    assert!(
        first["problems"][0]
            .as_str()
            .unwrap()
            .contains("check the code"),
        "{first}"
    );
    let busy = g.pair(&companion.address(), &code).await;
    assert!(
        busy["problems"][0].as_str().unwrap().contains("wait 5 s"),
        "{busy}"
    );
    for _ in 0..2 {
        tokio::time::sleep(Duration::from_millis(5100)).await;
        g.pair(&companion.address(), wrong).await;
    }
    let pairing = k.status().pairing.unwrap();
    assert!(pairing.burned);
    assert_eq!(pairing.failures, 3);
    assert_eq!(pairing.code, None);
    tokio::time::sleep(Duration::from_millis(5100)).await;
    let burned = g.pair(&companion.address(), &code).await;
    assert!(
        burned["problems"][0]
            .as_str()
            .unwrap()
            .contains("3 wrong codes"),
        "{burned}"
    );
    assert!(game.service.relay().status().peer.is_none());
    assert!(k.status().peer.is_none());
}

#[tokio::test]
async fn different_versions_refuse_to_pair_and_name_both() {
    let game = machine("gamepc", "1.0.0").await;
    let companion = machine("laptop", "1.1.0").await;
    let k = companion.service.relay();
    k.set_mode(Mode::Companion, false).await.unwrap();
    k.page_message("relay.code", &json!({})).await.unwrap();
    let code = k.status().pairing.unwrap().code.unwrap();
    let refused = game.service.relay().pair(&companion.address(), &code).await;
    assert_eq!(
        refused["problems"][0],
        "Not paired: this game PC runs Hardpoint 1.0.0 and the companion 1.1.0; update both to the same version"
    );
}

#[tokio::test]
async fn a_companion_that_unpaired_or_a_new_key_at_the_address_is_refused() {
    let (game, companion) = linked().await;
    let unpaired = companion
        .service
        .relay()
        .page_message("relay.unpair", &json!({}))
        .await
        .unwrap();
    assert_eq!(unpaired["removed"], true);
    let status = game
        .until("refused", |s| s.state == LinkState::Stopped)
        .await;
    assert_eq!(
        status.text,
        format!("not paired at {}", companion.address())
    );

    // Another machine that trusts this game PC, at the address the game PC now dials, while
    // the game PC still pins the first companion's key.
    let other = machine("other", "1.0.0").await;
    pair(&game, &other).await;
    // One pairing attempt per address per 5 s.
    tokio::time::sleep(Duration::from_millis(5100)).await;
    pair(&game, &companion).await;
    let address = other.address();
    let set = game
        .service
        .relay()
        .page_message("relay.address", &json!({ "address": address }))
        .await
        .unwrap();
    assert_eq!(set["problems"], json!([]));
    let status = game
        .until("pin refused", |s| {
            s.state == LinkState::Stopped && s.text.contains("not paired at")
        })
        .await;
    assert_eq!(status.text, format!("not paired at {address}"));
}

#[tokio::test]
async fn relay_mode_before_pairing_waits_for_it_and_modes_are_kept_in_the_settings() {
    let game = machine("gamepc", "1.0.0").await;
    let mut page = game.page().await;
    page.call(json!({ "type": "dash.get" })).await;
    let relay = page.until(|m| m["type"] == "relay").await;
    assert_eq!(relay["mode"], "off");
    assert_eq!(relay["relayAfterPairing"], false);
    let kept = page
        .call(json!({ "type": "relay.mode", "mode": "relay" }))
        .await;
    assert_eq!(kept["result"]["problems"], json!([]));
    let status = game.service.relay().status();
    assert_eq!((status.mode, status.relay_after_pairing), (Mode::Off, true));
    let bad = page
        .call(json!({ "type": "relay.pair", "address": "a b", "code": "1" }))
        .await;
    assert_eq!(bad["result"]["problems"].as_array().unwrap().len(), 1);

    // A page here closes when the machine becomes a companion, and comes back bridged.
    page.send(json!({ "type": "relay.mode", "mode": "companion" }))
        .await;
    page.closed().await;
    let mut page = game.page().await;
    let relay = page.until(|m| m["type"] == "relay").await;
    assert_eq!(relay["state"], "listening");
    let settings = std::fs::read_to_string(game._dir.path().join("app-settings.json")).unwrap();
    assert!(settings.contains("\"companion\""), "{settings}");
}

#[tokio::test]
async fn relay_chosen_before_pairing_stays_on_through_pairing() {
    let game = machine("gamepc", "1.0.0").await;
    let companion = machine("laptop", "1.0.0").await;
    let g = game.service.relay();
    let answer = g
        .page_message("relay.mode", &json!({ "mode": "relay" }))
        .await
        .unwrap();
    assert_eq!(answer["problems"], json!([]));
    assert!(g.status().relay_after_pairing);
    pair(&game, &companion).await;
    let status = game
        .until("relaying after pairing", |s| s.state == LinkState::Up)
        .await;
    assert_eq!(status.mode, Mode::Relay);
    assert!(!status.relay_after_pairing);
    let settings = std::fs::read_to_string(game._dir.path().join("app-settings.json")).unwrap();
    let saved: Value = serde_json::from_str(&settings).unwrap();
    assert_eq!(saved["relay"]["mode"], "relay", "{settings}");

    // Choosing another mode drops the wish.
    let other = machine("other", "1.0.0").await;
    let o = other.service.relay();
    o.page_message("relay.mode", &json!({ "mode": "relay" }))
        .await
        .unwrap();
    o.page_message("relay.mode", &json!({ "mode": "off" }))
        .await
        .unwrap();
    assert!(!o.status().relay_after_pairing);
}

#[tokio::test]
async fn at_most_8_pages_go_over_one_link() {
    let (_game, companion) = linked().await;
    let mut pages = vec![];
    for _ in 0..8 {
        let mut page = companion.page().await;
        page.until(|m| m["type"] == "status").await;
        pages.push(page);
    }
    let mut ninth = companion.page().await;
    let notice = ninth.until(|m| m["type"] == "notice").await;
    assert_eq!(notice["text"], "Too many pages are open over the link");
    ninth.closed().await;
    drop(pages.pop());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut again = companion.page().await;
    again.until(|m| m["type"] == "status").await;
}
