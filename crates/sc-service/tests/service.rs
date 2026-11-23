//! The service end to end on a replayed game: the bus, the pages and a portal or dashboard page.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use sc_core::engine::EngineOptions;
use sc_game::{GameAction, GameApi, RecordedFrame, ReplayGame, ReplayOptions};
use sc_service::{Service, ServiceOptions, busfile};

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Rig {
    service: Service,
    game: Arc<ReplayGame>,
    dir: tempfile::TempDir,
}

impl Rig {
    async fn start() -> Self {
        Self::start_full(None).await
    }

    /// `frames`: what the replayed game sends, one every 20 ms, in a loop.
    async fn start_full(frames: Option<Vec<Value>>) -> Self {
        let frame = json!({ "vehicle": { "state": {
            "name": "Test ship",
            "lights_enabled": true,
            "master_mode": { "mode": "VEHICLE_MASTER_MODE_NAVIGATION" },
            "flight": { "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED" }, "instruments": { "speed": 42, "quantum_fuel": 0.64 } },
            "shields": { "level": 0.9, "faces": [0.9, 0.5, 0.2, 1] },
            "annunciators": { "qdrv": "ANNUNCIATOR_STATE_MINOR_ON", "cool": "ANNUNCIATOR_STATE_MAJOR_FLASHING" },
        } } });
        let list = frames.unwrap_or_else(|| vec![frame; 50]);
        let frames = list
            .into_iter()
            .zip(0..)
            .map(|(frame, i)| RecordedFrame {
                t: f64::from(i) * 20.0,
                frame,
            })
            .collect();
        let actions = [
            "v_lights",
            "v_lights_off",
            "v_power_set_off",
            "v_master_mode_set_scm",
        ]
        .map(|name| GameAction {
            name: name.into(),
            category: "test".into(),
            holdable: false,
        })
        .to_vec();
        let options = ReplayOptions {
            loop_playback: true,
            actions,
            ..ReplayOptions::default()
        };
        let game = Arc::new(ReplayGame::new(frames, options).unwrap());
        let dir = tempfile::tempdir().unwrap();
        let service = Service::start(ServiceOptions {
            port: 0,
            settings_dir: Some(dir.path().to_owned()),
            bus_file: dir.path().join("bus.json"),
            game: Some(Arc::clone(&game) as Arc<dyn GameApi>),
            engine: EngineOptions::default(),
            fleet_connector: None,
            relay: sc_service::relay::RelayOptions::default(),
        })
        .await
        .unwrap();
        Self { service, game, dir }
    }

    fn triggers(&self) -> Vec<String> {
        self.game
            .trigger_calls()
            .into_iter()
            .map(|c| c.name)
            .collect()
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.service.port())
    }

    async fn get(&self, path: &str) -> (u16, String, String) {
        let mut tcp = TcpStream::connect(("127.0.0.1", self.service.port()))
            .await
            .unwrap();
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
        tcp.write_all(request.as_bytes()).await.unwrap();
        let mut response = String::new();
        tcp.read_to_string(&mut response).await.unwrap();
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        (status, head.to_ascii_lowercase(), body.to_owned())
    }

    async fn page(&self, origin: &str) -> Result<Socket, WsError> {
        let mut request = format!("ws://127.0.0.1:{}/portal", self.service.port())
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
        tokio_tungstenite::connect_async(request)
            .await
            .map(|(ws, _)| ws)
    }

    async fn portal(&self, client: &str) -> Page {
        let mut ws = self.page(&self.origin()).await.unwrap();
        send(
            &mut ws,
            &json!({ "type": "hello", "token": self.service.token().as_str(), "client": client }),
        )
        .await;
        Page { ws, seen: vec![] }
    }

    async fn bus(&self) -> Socket {
        let url = format!("ws://127.0.0.1:{}", self.service.bus_port());
        tokio_tungstenite::connect_async(url).await.unwrap().0
    }

    fn bus_token(&self) -> String {
        busfile::read(&self.dir.path().join("bus.json"))
            .unwrap()
            .unwrap()
            .token
    }
}

async fn send(ws: &mut Socket, msg: &Value) {
    ws.send(Message::Text(msg.to_string().into()))
        .await
        .unwrap();
}

/// The next JSON message, or `None` when the socket closed (or nothing came within 3 s).
async fn next(ws: &mut Socket) -> Option<Value> {
    loop {
        match tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .ok()??
        {
            Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

const UNTIL_DEADLINE: Duration = Duration::from_secs(20);

/// The next JSON message, or `None` when the socket closed or the deadline passed.
async fn next_by(ws: &mut Socket, deadline: tokio::time::Instant) -> Option<Value> {
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await.ok()?? {
            Ok(Message::Text(t)) => return Some(serde_json::from_str(&t).unwrap()),
            Ok(Message::Close(_)) | Err(_) => return None,
            Ok(_) => {}
        }
    }
}

struct Page {
    ws: Socket,
    seen: Vec<Value>,
}

impl Page {
    async fn until(&mut self, mut found: impl FnMut(&Value) -> bool) -> Value {
        if let Some(m) = self.seen.iter().find(|m| found(m)) {
            return m.clone();
        }
        // One deadline for the whole wait, not per message: a scenario may stay quiet for seconds
        // on a slow runner before the awaited push comes.
        let deadline = tokio::time::Instant::now() + UNTIL_DEADLINE;
        loop {
            let m = next_by(&mut self.ws, deadline).await.unwrap_or_else(|| {
                panic!(
                    "the message did not come; seen {:?}",
                    self.seen
                        .iter()
                        .map(|m| m["type"].to_string() + " " + &m["id"].to_string())
                        .collect::<Vec<_>>()
                )
            });
            self.seen.push(m.clone());
            if found(&m) {
                return m;
            }
        }
    }

    async fn call(&mut self, id: u32, msg: Value) -> Value {
        let mut msg = msg;
        msg["id"] = json!(id);
        send(&mut self.ws, &msg).await;
        self.until(|m| m["type"] == "result" && m["id"] == id).await["result"].clone()
    }

    async fn drain(&mut self, for_ms: u64) {
        let until = tokio::time::Instant::now() + Duration::from_millis(for_ms);
        while let Ok(Some(Ok(Message::Text(t)))) =
            tokio::time::timeout_at(until, self.ws.next()).await
        {
            self.seen.push(serde_json::from_str(&t).unwrap());
        }
    }
}

#[tokio::test]
async fn the_bus_greets_refuses_and_runs_calls() {
    let rig = Rig::start().await;
    let file = busfile::read(&rig.dir.path().join("bus.json"))
        .unwrap()
        .unwrap();
    assert_eq!(
        (file.port, file.pid, file.version),
        (rig.service.bus_port(), std::process::id(), 1)
    );

    let mut wrong = rig.bus().await;
    send(
        &mut wrong,
        &json!({ "type": "hello", "token": "0".repeat(64), "version": 1, "client": "t" }),
    )
    .await;
    assert_eq!(
        next(&mut wrong).await,
        Some(json!({ "type": "refused", "reason": "wrong token" }))
    );
    assert_eq!(next(&mut wrong).await, None);

    let mut early = rig.bus().await;
    send(
        &mut early,
        &json!({ "type": "call", "id": 1, "call": { "op": "abort" } }),
    )
    .await;
    assert_eq!(
        next(&mut early).await,
        Some(json!({ "type": "refused", "reason": "expected hello" }))
    );

    let mut ws = rig.bus().await;
    send(
        &mut ws,
        &json!({ "type": "hello", "token": rig.bus_token(), "version": 1, "client": "t" }),
    )
    .await;
    let welcome = next(&mut ws).await.unwrap();
    assert_eq!(welcome["type"], "welcome");
    assert!(welcome["sections"].is_array() && welcome["running"].is_null());

    let calls = [
        json!({ "op": "press", "action": "spaceship_radar.v_invoke_ping" }),
        json!({ "op": "press", "action": "v_self_destruct" }),
        json!({ "op": "set", "toggle": "lights", "to": "maybe" }),
        json!({ "op": "explode" }),
        json!({ "op": "abort" }),
        json!({ "op": "sequences.save", "sequence": { "id": "BAD", "name": "", "steps": [] } }),
        json!({ "op": "sequences.list" }),
    ];
    for (id, call) in calls.iter().enumerate() {
        send(&mut ws, &json!({ "type": "call", "id": id, "call": call })).await;
    }
    let mut results = std::collections::BTreeMap::new();
    while results.len() < calls.len() {
        let m = next(&mut ws).await.unwrap();
        if m["type"] == "result" {
            results.insert(m["id"].as_u64().unwrap(), m);
        }
    }
    assert_eq!(
        results[&0]["result"],
        json!({ "ok": true, "status": "done" })
    );
    assert_eq!(results[&1]["result"]["status"], "needs-confirm");
    assert_eq!(results[&2]["error"], "to must be on or off");
    assert_eq!(results[&3]["error"], "unknown op \"explode\"");
    assert_eq!(results[&4]["result"], false);
    assert_eq!(
        results[&5]["result"],
        json!([
            "The id must be 1 to 40 lower-case letters, digits, - or _",
            "The name must be 1 to 40 characters",
            "A sequence needs at least one step"
        ])
    );
    assert!(
        results[&6]["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "landing_prep")
    );
    assert_eq!(rig.triggers(), ["v_invoke_ping"]);

    // Pushes follow the welcome.
    let mut state = false;
    for _ in 0..20 {
        state |= next(&mut ws).await.unwrap()["type"] == "state";
    }
    assert!(state);

    let mut big = rig.bus().await;
    send(
        &mut big,
        &json!({ "type": "hello", "token": rig.bus_token(), "version": 1, "client": "t" }),
    )
    .await;
    next(&mut big).await.unwrap();
    big.send(Message::Text(
        "x".repeat(sc_service::bus::MAX_MESSAGE_BYTES + 1).into(),
    ))
    .await
    .unwrap();
    let code = loop {
        match big.next().await {
            Some(Ok(Message::Close(frame))) => break frame.map(|f| f.code),
            Some(Ok(_)) => {}
            other => panic!("expected a close, got {other:?}"),
        }
    };
    assert_eq!(code, Some(CloseCode::Size));

    let bus_file = rig.dir.path().join("bus.json");
    rig.service.close().await;
    assert!(!bus_file.exists());
}

#[tokio::test]
async fn serves_the_pages_and_only_the_vendor_folder() {
    let rig = Rig::start().await;
    for path in ["/", "/portal", "/dash", "/dash?tab=main"] {
        let (status, head, body) = rig.get(path).await;
        assert_eq!(status, 200, "{path}");
        assert!(
            body.contains(rig.service.token().as_str()) && !body.contains("__TOKEN__"),
            "{path}"
        );
        for header in [
            "cache-control: no-store",
            "x-frame-options: deny",
            "content-security-policy: frame-ancestors 'none'",
            "x-content-type-options: nosniff",
        ] {
            assert!(head.contains(header), "{path} {header}");
        }
    }
    assert!(rig.get("/portal").await.2.contains("Hardpoint Portal"));
    let (status, head, _) = rig.get("/vendor/gridstack/gridstack-all.js").await;
    assert_eq!(status, 200);
    assert!(head.contains("content-type: text/javascript"));
    for bad in [
        "/vendor/../dash.html",
        "/vendor/%2e%2e/dash.html",
        "/vendor/..%2fportal.html",
        "/vendor/gridstack",
        "/vendor/%00",
        "/mfd",
        "/nope",
    ] {
        assert_eq!(rig.get(bad).await.0, 404, "{bad}");
    }
}

#[tokio::test]
async fn a_portal_page_needs_the_origin_and_the_token() {
    let rig = Rig::start().await;
    assert!(rig.page("http://evil.example").await.is_err());
    let mut ws = rig.page(&rig.origin()).await.unwrap();
    send(
        &mut ws,
        &json!({ "type": "hello", "token": "x".repeat(64) }),
    )
    .await;
    assert_eq!(next(&mut ws).await, None);
}

#[tokio::test]
async fn a_portal_page_gets_status_and_catalog_and_runs_actions() {
    let rig = Rig::start().await;
    let mut p = rig.portal("portal").await;
    let status = p
        .until(|m| m["type"] == "status" && m["status"]["inShip"] == true)
        .await["status"]
        .clone();
    assert_eq!(status["ship"], "Test ship");
    let faces: Vec<&Value> = status["shields"]["faces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| &f["text"])
        .collect();
    assert_eq!(faces, ["90%", "50%", "20%", "100%"]);
    assert!(status["fuel"].as_array().unwrap().contains(
        &json!({ "label": "Quantum fuel", "text": "64%", "tone": "ok", "fraction": 0.64 })
    ));
    let catalog = p.until(|m| m["type"] == "catalog").await;
    assert_eq!(catalog["fromGame"], true);
    let off = catalog["gameActions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "v_power_set_off")
        .unwrap();
    assert_eq!(off["confirm"], true);

    assert_eq!(
        p.call(1, json!({ "type": "press", "action": "v_power_set_off" }))
            .await["status"],
        "needs-confirm"
    );
    p.until(|m| m["type"] == "notice" && m["text"].as_str().unwrap().contains("hold"))
        .await;
    let result = p.until(|m| m["type"] == "result" && m["id"] == 1).await;
    assert_eq!(result["actor"], "portal");

    p.call(
        2,
        json!({ "type": "hold.start", "action": "v_power_set_off" }),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(1600)).await;
    assert_eq!(
        p.call(
            3,
            json!({ "type": "press", "action": "v_power_set_off", "confirmed": true })
        )
        .await,
        json!({ "ok": true, "status": "done" })
    );
    assert_eq!(rig.triggers(), ["v_power_set_off"]);

    tokio::time::sleep(Duration::from_millis(1000)).await;
    for id in 10..18 {
        send(
            &mut p.ws,
            &json!({ "id": id, "type": "press", "action": "v_lights" }),
        )
        .await;
    }
    p.until(|m| m["type"] == "result" && m["id"] == 17).await;
    p.drain(1500).await;
    assert_eq!(rig.triggers().len(), 6);
    assert!(
        p.seen
            .iter()
            .any(|m| m["type"] == "notice" && m["text"].as_str().unwrap().contains("Too many"))
    );
}

#[tokio::test]
async fn a_dashboard_page_edits_imports_and_exports() {
    let rig = Rig::start().await;
    let mut p = rig.portal("dash").await;
    let mut other = rig.portal("dash").await;
    other.call(1, json!({ "type": "dash.get" })).await;
    p.drain(300).await;
    assert!(!p.seen.iter().any(|m| m["type"] == "values"));
    assert_eq!(
        p.call(1, json!({ "type": "dash.get" })).await["tabs"][0]["id"],
        "main"
    );
    let dashboard = p.until(|m| m["type"] == "dashboard").await;
    assert_eq!(dashboard["limits"]["columns"], 24);
    let values = p
        .until(|m| m["type"] == "values" && m["inShip"] == true)
        .await;
    assert_eq!(values["values"]["ship.speed"]["text"], "42.0 m/s");
    assert_eq!(values["values"]["caution.cool"]["lamp"], "major-flash");
    assert_eq!(values["toggles"]["gear"]["lamp"], "on");

    let widget = |action: &str| json!({ "id": "x", "kind": "action", "x": 0, "y": 0, "w": 2, "h": 2, "bind": { "kind": "action", "id": action }, "look": "square" });
    let bad = p.call(2, json!({ "type": "dash.saveTab", "tab": { "id": "t", "name": "T", "rows": 14, "widgets": [widget("v_not_real")] } })).await;
    assert_eq!(
        bad,
        json!(["Tab \"T\", widget 1 (\"x\"): unknown game action \"v_not_real\""])
    );
    p.until(|m| {
        m["type"] == "notice" && m["text"].as_str().unwrap().contains("unknown game action")
    })
    .await;

    let file = json!({ "schemaVersion": 1, "tabs": [{ "id": "shared", "name": "Shared", "rows": 14, "widgets": [widget("v_power_set_off")] }] }).to_string();
    assert_eq!(
        p.call(
            3,
            json!({ "type": "dash.import", "text": file, "confirm": true })
        )
        .await["applied"],
        false
    );
    let shown = p
        .call(4, json!({ "type": "dash.import", "text": file }))
        .await;
    assert_eq!(shown["summary"]["confirmButtons"], 1);
    assert_eq!(
        p.call(
            5,
            json!({ "type": "dash.import", "text": file, "confirm": true })
        )
        .await["applied"],
        true
    );
    let exported = p.call(6, json!({ "type": "dash.export" })).await;
    let text: Value = serde_json::from_str(exported["text"].as_str().unwrap()).unwrap();
    assert_eq!(text["tabs"][0]["id"], "shared");
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(rig.dir.path().join("dashboards.json")).unwrap(),
    )
    .unwrap();
    // The file holds every dashboard (version 2); the export is the one on screen (version 1).
    assert_eq!(saved["schemaVersion"], 2);
    assert_eq!(saved["dashboards"][0]["tabs"], text["tabs"]);
    assert_eq!(text["schemaVersion"], 1);
    // Every open dashboard page hears of the change, not only the one that made it.
    other
        .until(|m| m["type"] == "dashboard" && m["dashboard"]["tabs"][0]["id"] == "shared")
        .await;
}

#[tokio::test]
async fn a_dashboard_page_sets_how_times_are_shown_and_every_page_hears() {
    let rig = Rig::start().await;
    let file = rig.dir.path().join("app-settings.json");
    std::fs::write(&file, r#"{ "splash": false }"#).unwrap();
    let mut p = rig.portal("dash").await;
    let mut other = rig.portal("dash").await;
    other.call(1, json!({ "type": "dash.get" })).await;
    p.call(1, json!({ "type": "dash.get" })).await;
    let first = p.until(|m| m["type"] == "time").await;
    assert_eq!(first["time"], json!({ "zone": "auto", "clock": "24h" }));
    assert!(
        first["zones"]
            .as_array()
            .unwrap()
            .contains(&json!("America/Denver"))
    );

    let bad = p
        .call(
            2,
            json!({ "type": "time.set", "time": { "zone": "America/Nowhere", "clock": "24h" } }),
        )
        .await;
    assert_eq!(
        bad["problems"],
        json!(["Unknown time zone \"America/Nowhere\": pick one from the list"])
    );
    assert_eq!(bad["time"]["zone"], "auto");
    p.until(|m| m["type"] == "notice" && m["text"].as_str().unwrap().contains("Unknown time zone"))
        .await;

    let good = json!({ "zone": "America/Denver", "clock": "12h" });
    let set = p.call(3, json!({ "type": "time.set", "time": good })).await;
    assert_eq!(
        (set["problems"].clone(), set["time"].clone()),
        (json!([]), good.clone())
    );
    other
        .until(|m| m["type"] == "time" && m["time"] == good)
        .await;
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved, json!({ "splash": false, "time": good }));
}

/// A bus client after its welcome; `capabilities` goes in its hello.
async fn bus_client(rig: &Rig, capabilities: Value) -> Socket {
    let mut ws = rig.bus().await;
    send(
        &mut ws,
        &json!({ "type": "hello", "token": rig.bus_token(), "version": 1, "client": "test", "capabilities": capabilities }),
    )
    .await;
    assert_eq!(next(&mut ws).await.unwrap()["type"], "welcome");
    ws
}

/// Every message a bus client gets within `for_ms`.
async fn received(ws: &mut Socket, for_ms: u64) -> Vec<Value> {
    let until = tokio::time::Instant::now() + Duration::from_millis(for_ms);
    let mut seen = Vec::new();
    while let Ok(Some(Ok(m))) = tokio::time::timeout_at(until, ws.next()).await {
        if let Message::Text(t) = m {
            seen.push(serde_json::from_str(&t).unwrap());
        }
    }
    seen
}

#[tokio::test]
async fn keybind_functions_are_gone_and_a_client_that_says_keys_is_never_asked() {
    let rig = Rig::start().await;
    // An older plugin still says it can press keys, beside a capability nobody knows.
    let mut plugin = bus_client(&rig, json!(["future", "keys"])).await;
    let mut p = rig.portal("dash").await;
    p.call(1, json!({ "type": "dash.get" })).await;
    let dashboard = p.until(|m| m["type"] == "dashboard").await;
    assert!(dashboard.get("keybinds").is_none());
    assert!(dashboard["kinds"].get("keybind").is_none());
    let values = p.until(|m| m["type"] == "values").await;
    assert!(values.get("keys").is_none());

    // A page that still sends a keybind gets the answer any unknown message gets.
    assert_eq!(
        p.call(2, json!({ "type": "keybind", "fn": "pips-engine-up" }))
            .await,
        Value::Null
    );
    let pushed = received(&mut plugin, 300).await;
    assert!(
        pushed.iter().all(|m| m["type"] != "keybind"),
        "no keybind push: {pushed:?}"
    );
    // The plugin's calls are served as before.
    send(
        &mut plugin,
        &json!({ "type": "call", "id": 1, "call": { "op": "abort" } }),
    )
    .await;
    let answer = loop {
        let m = next(&mut plugin).await.expect("a result");
        if m["type"] == "result" {
            break m;
        }
    };
    assert_eq!(answer["result"], false);
    assert!(rig.triggers().is_empty());
}

#[tokio::test]
async fn the_values_carry_recording_only_while_the_app_records() {
    let rig = Rig::start().await;
    let mut p = rig.portal("dash").await;
    p.call(1, json!({ "type": "dash.get" })).await;
    let first = p.until(|m| m["type"] == "values").await;
    assert!(first.get("recording").is_none());

    rig.service.set_recording(true);
    p.until(|m| m["type"] == "values" && m["recording"] == true)
        .await;
    rig.service.set_recording(false);
    let after = p
        .until(|m| m["type"] == "values" && m.get("recording").is_none())
        .await;
    assert!(after.get("keys").is_none());
}

#[tokio::test]
async fn the_fleet_panel_offers_joining_the_consent_and_the_viewers_groups() {
    let rig = Rig::start().await;
    let mut page = rig.portal("dash").await;
    page.call(1, json!({ "type": "dash.get" })).await;
    let fleet = page.until(|m| m["type"] == "fleet").await;
    let me = rig.service.fleet_client().public_key();
    assert_eq!(fleet["me"]["member"], me.to_string());
    assert_eq!(fleet["me"]["fingerprint"], me.fingerprint());
    assert_eq!(fleet["link"]["state"], "none");
    assert_eq!(fleet["link"]["consent"], json!([]));
    assert_eq!(fleet["sharing"], false);
    assert_eq!(fleet["layout"], "personal");
    assert_eq!(fleet["groups"][0]["name"], "Ungrouped");
    assert_eq!(fleet["wings"][0]["groups"][0]["name"], "Ungrouped");
    assert!(fleet.get("demo").is_none());

    // The send consent, field by field, and its preview.
    let r = page
        .call(
            2,
            json!({ "type": "fleet.consent", "fields": ["ship.shields", "char.health"] }),
        )
        .await;
    assert_eq!(r["problems"], json!([]));
    page.until(|m| {
        m["type"] == "fleet" && m["link"]["consent"] == json!(["ship.shields", "char.health"])
    })
    .await;
    let r = page.call(3, json!({ "type": "fleet.preview" })).await;
    assert_eq!(r["card"]["member"], me.to_string());
    let fields: Vec<&str> = r["card"]["fields"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(fields, ["ship.shields", "char.health"]);
    let saved = std::fs::read_to_string(rig.dir.path().join("fleet-consent.json")).unwrap();
    assert!(saved.contains("ship.shields"), "{saved}");
    let r = page
        .call(
            4,
            json!({ "type": "fleet.consent", "fields": ["ship.secret"] }),
        )
        .await;
    assert_eq!(r["problems"].as_array().unwrap().len(), 1);

    // A bad invite is answered at once.
    let r = page
        .call(
            5,
            json!({ "type": "fleet.join", "invite": "https://x", "handle": "Vega" }),
        )
        .await;
    assert_eq!(r["problems"], json!(["an invite starts with cic://"]));

    // The viewer's own groups.
    let added = page
        .call(6, json!({ "type": "fleet.group.add", "name": "Wing 1" }))
        .await;
    assert_eq!(added["problems"], json!([]));
    let r = page
        .call(7, json!({ "type": "fleet.group.add", "name": "<b>" }))
        .await;
    assert_eq!(r["problems"].as_array().unwrap().len(), 1);
    page.until(|m| m["type"] == "fleet" && m["groups"][0]["name"] == "Wing 1")
        .await;
    let saved = std::fs::read_to_string(rig.dir.path().join("fleet-view.json")).unwrap();
    assert!(saved.contains("\"Wing 1\""));
    assert!(rig.dir.path().join("fleet-identity.key").exists());

    // Structure changes are made in the CIC portal: a page that still asks gets the
    // unknown-message error, and no change is made.
    for (id, msg) in [
        (
            8,
            json!({ "type": "fleet.change", "change": { "op": "place", "member": "m", "unit": null, "role": null } }),
        ),
        (9, json!({ "type": "fleet.moves", "member": "m" })),
    ] {
        let kind = msg["type"].as_str().unwrap().to_owned();
        let mut msg = msg;
        msg["id"] = json!(id);
        send(&mut page.ws, &msg).await;
        let answer = page.until(|m| m["type"] == "result" && m["id"] == id).await;
        assert_eq!(answer["error"], format!("unknown fleet message {kind}"));
        assert!(answer.get("result").is_none_or(Value::is_null), "{answer}");
    }
    rig.service.close().await;
}

/// A Corsair as a synthetic stream: wings and gear, coupled available, ESP not available, no
/// VTOL. The recordings are scrubbed, so class names are made up to look like the game's.
fn corsair_frame() -> Value {
    json!({ "vehicle": { "state": {
        "name": "Corsair",
        "class_name": "DRAK_Corsair",
        "manufacturer": "Drake Interplanetary",
        "master_mode": { "mode": "VEHICLE_MASTER_MODE_SCM" },
        "flight": {
            "state": { "gear": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_RETRACTED", "transformation": "VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED" },
            "settings": { "ifcs_coupled": { "enabled": true, "active": true }, "ifcs_esp": {}, "ifcs_g_safe": { "enabled": true } },
        },
    } } })
}

#[tokio::test]
async fn values_carry_the_ship_and_its_greyed_functions_and_a_ship_change_switches_the_dashboard() {
    let rig = Rig::start_full(Some(vec![corsair_frame(); 50])).await;
    let mut p = rig.portal("dash").await;
    let got = p.call(1, json!({ "type": "dash.get" })).await;
    assert_eq!(got["schemaVersion"], 1, "the page still gets one dashboard");
    let values = p
        .until(|m| {
            m["type"] == "values" && m["ship"].is_object() && m["support"]["esp"].is_object()
        })
        .await;
    assert_eq!(values["ship"]["key"], "Drake Interplanetary / Corsair");
    assert_eq!(values["ship"]["className"], "DRAK_Corsair");
    assert_eq!(values["ship"]["dashboard"], Value::Null);
    assert_eq!(
        values["support"]["esp"],
        json!({ "state": "unavailable", "reason": "not available" })
    );
    // A greyed toggle's click is the game's own toggle, never set-to-state.
    assert_eq!(
        values["toggles"]["esp"]["click"],
        json!({ "type": "press", "action": "v_ifcs_toggle_esp" })
    );
    assert!(values["support"].get("gear").is_none());

    // Make the Corsair's dashboard from the current ship; it is shown at once.
    let made = p.call(2, json!({ "type": "dash.createForShip" })).await;
    assert_eq!(made["problems"], json!([]));
    assert_eq!(made["dashboard"]["theme"], "drake");
    let push = p
        .until(|m| m["type"] == "dashboard" && m["board"]["id"] == "drak_corsair")
        .await;
    assert_eq!(push["dashboards"].as_array().unwrap().len(), 2);
    assert_eq!(
        push["generic"]["tabs"][0]["id"], "main",
        "for detached windows"
    );
    assert_eq!(push["pinned"], true);
    let list = p.call(3, json!({ "type": "dash.list" })).await;
    assert_eq!(list["active"], "drak_corsair");
    assert_eq!(list["switchOnShipChange"], false);
    assert_eq!(
        p.call(4, json!({ "type": "dash.remove", "dashboard": "generic" }))
            .await["problems"],
        json!(["The generic dashboard cannot be removed"])
    );
    // Once the ship is recognised (2 s), a hand pick of the generic one stays, switching on or
    // not, until the ship changes.
    p.drain(2600).await;
    p.call(5, json!({ "type": "dash.setSwitch", "on": true }))
        .await;
    p.call(6, json!({ "type": "dash.select", "dashboard": "generic" }))
        .await;
    p.drain(1500).await;
    assert_eq!(rig.service.dashboards().list().active, "generic");
    let file: Value = serde_json::from_str(
        &std::fs::read_to_string(rig.dir.path().join("dashboards.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(file["switchOnShipChange"], true);
    assert_eq!(file["dashboards"][1]["ship"]["className"], "DRAK_Corsair");
    // Learned support is saved for the class name and the game's build key.
    let r = p
        .call(
            7,
            json!({ "type": "support.reset", "className": "DRAK_Corsair" }),
        )
        .await;
    assert_eq!(r["problems"], json!([]));
    rig.service.close().await;
}

#[tokio::test]
async fn a_ship_shows_its_friendly_name_or_the_users_own_everywhere() {
    let frame = json!({ "vehicle": { "state": {
        "name": "DRAK_Corsair_877723283935",
        "class_name": "DRAK_Corsair",
        "master_mode": { "mode": "VEHICLE_MASTER_MODE_SCM" },
    } } });
    let rig = Rig::start_full(Some(vec![frame; 400])).await;
    let file = rig.dir.path().join("app-settings.json");
    std::fs::write(&file, r#"{ "splash": false }"#).unwrap();
    let mut p = rig.portal("dash").await;
    let mut other = rig.portal("dash").await;
    other.call(1, json!({ "type": "dash.get" })).await;
    p.call(1, json!({ "type": "dash.get" })).await;
    let names = p.until(|m| m["type"] == "shipNames").await;
    assert_eq!(names["names"], json!({}));
    let values = p
        .until(|m| m["type"] == "values" && m["ship"].is_object())
        .await;
    assert_eq!(values["values"]["ship.name"]["text"], "Drake Corsair");
    assert_eq!(values["ship"]["name"], "Drake Corsair");
    assert_eq!(values["ship"]["key"], "Drake Interplanetary / Corsair");
    let status = p
        .until(|m| m["type"] == "status" && m["status"]["inShip"] == true)
        .await;
    assert_eq!(status["status"]["ship"], "Drake Corsair");

    let bad = p
        .call(
            2,
            json!({ "type": "shipNames.set", "className": "DRAK_Corsair", "name": "<b>" }),
        )
        .await;
    assert_eq!(bad["problems"].as_array().unwrap().len(), 1);
    p.until(|m| m["type"] == "notice" && m["text"].as_str().unwrap().contains("ship name"))
        .await;

    let set = p
        .call(
            3,
            json!({ "type": "shipNames.set", "className": "DRAK_Corsair", "name": "Old Betsy" }),
        )
        .await;
    assert_eq!(set["problems"], json!([]));
    other
        .until(|m| m["type"] == "shipNames" && m["names"]["DRAK_Corsair"] == "Old Betsy")
        .await;
    let values = p
        .until(|m| m["type"] == "values" && m["ship"]["name"] == "Old Betsy")
        .await;
    assert_eq!(values["values"]["ship.name"]["text"], "Old Betsy");
    assert_eq!(values["ship"]["derivedName"], "Drake Corsair");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(
        saved,
        json!({ "splash": false, "shipNames": { "DRAK_Corsair": "Old Betsy" } })
    );

    let made = p.call(4, json!({ "type": "dash.createForShip" })).await;
    assert_eq!(made["dashboard"]["name"], "Old Betsy");
    let push = p
        .until(|m| m["type"] == "dashboard" && m["board"]["id"] == "drak_corsair")
        .await;
    assert_eq!(push["dashboards"][1]["shipName"], "Old Betsy");

    let clear = p
        .call(
            5,
            json!({ "type": "shipNames.set", "className": "DRAK_Corsair", "name": "" }),
        )
        .await;
    assert_eq!(clear["problems"], json!([]));
    p.until(|m| m["type"] == "values" && m["ship"]["name"] == "Drake Corsair")
        .await;
    let push = p
        .until(|m| m["type"] == "dashboard" && m["dashboards"][1]["shipName"] == "Drake Corsair")
        .await;
    assert_eq!(
        push["dashboards"][1]["name"], "Old Betsy",
        "the dashboard keeps its own name"
    );
    rig.service.close().await;
}

#[tokio::test]
async fn a_new_ship_with_switching_on_shows_its_dashboard_after_two_seconds() {
    let mut frames = vec![json!({ "vehicle": { "state": { "class_name": "ANVL_Arrow" } } }); 200];
    frames.extend(vec![corsair_frame(); 400]);
    let rig = Rig::start_full(Some(frames)).await;
    let store = rig.service.dashboards();
    let known = sc_service::dashboard::Known::default();
    let corsair = sc_service::dashboard::ShipRef {
        manufacturer: "Drake Interplanetary".into(),
        model: "Corsair".into(),
        class_name: Some("DRAK_Corsair".into()),
    };
    store.create_for_ship(corsair, None, None, &known).unwrap();
    store.select("generic", true).unwrap();
    store.set_switch(Some(&json!(true))).unwrap();
    let mut p = rig.portal("dash").await;
    p.call(1, json!({ "type": "dash.get" })).await;
    // 4 s of Arrow (no dashboard: generic), then the Corsair: its own after 2 s.
    let push = p
        .until(|m| m["type"] == "dashboard" && m["board"]["id"] == "drak_corsair")
        .await;
    assert_eq!(push["board"]["ship"]["model"], "Corsair");
    assert_eq!(push["pinned"], false);
    let saved: Value = serde_json::from_str(
        &std::fs::read_to_string(rig.dir.path().join("ship-support.json")).unwrap(),
    )
    .unwrap();
    let ships: Vec<&str> = saved["ships"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["className"].as_str().unwrap())
        .collect();
    assert!(ships.contains(&"ANVL_Arrow"));
    assert!(
        saved["ships"][0]["build"]
            .as_str()
            .unwrap()
            .starts_with("actions-")
    );
    rig.service.close().await;
}
