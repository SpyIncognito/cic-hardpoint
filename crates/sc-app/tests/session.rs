//! The app's portal session against the real service: it reads the dashboard as a page does, and
//! an action it sends is answered, with a refusal traced back to the tab that sent it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::sync::{mpsc, watch};

use sc_app::client::{self, Event};
use sc_service::{Service, ServiceOptions};

#[tokio::test(flavor = "multi_thread")]
async fn reads_the_dashboard_and_sends_actions() {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::start(ServiceOptions {
        port: 0,
        settings_dir: None,
        bus_file: dir.path().join("bus.json"),
        ..ServiceOptions::standard()
    })
    .await
    .unwrap();

    let state = client::Shared::default();
    let (out, outgoing) = mpsc::unbounded_channel();
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (close, closing) = watch::channel(false);
    let session = tokio::spawn(client::run(
        service.port(),
        service.token().as_str().to_owned(),
        Arc::clone(&state),
        outgoing,
        move |e| {
            let _ = events_tx.send(e);
        },
        closing,
    ));

    let first = tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .unwrap();
    assert_eq!(first, Some(Event::Dashboard));
    assert!(
        client::lock(&state).tab("main").is_some(),
        "the starter tab"
    );

    // No game is connected, so the service refuses the press.
    out.send((
        json!({ "type": "press", "action": "v_toggle_landing_system" }),
        Some("mip".into()),
    ))
    .unwrap();
    let refused = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(Event::Refused { tab, text }) = events.recv().await {
                return (tab, text);
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(refused.0.as_deref(), Some("mip"));
    assert!(!refused.1.is_empty());

    // A placement and a bezel mapping saved the way the page saves them come back to the app.
    out.send((
        json!({ "type": "dash.setPlacement", "tab": "main",
                "placement": { "displayLabel": "MFD Left", "fullscreen": true } }),
        None,
    ))
    .unwrap();
    out.send((
        json!({ "type": "dash.setBezel", "tab": "main", "bezel": {
            "gamepadId": "F16 MFD 1 (Vendor: 044f Product: b351)",
            "buttons": [{ "button": 2, "bind": { "kind": "mode", "to": "SCM" }, "label": "SCM" }] } }),
        None,
    ))
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if events.recv().await == Some(Event::Dashboard) {
                let s = client::lock(&state);
                let main = s.tab("main").unwrap();
                if let (Some(p), Some(b)) = (&main.placement, &main.bezel) {
                    assert_eq!(p.display_label, "MFD Left");
                    assert_eq!(b.buttons[0].button, 2);
                    assert_eq!(
                        b.buttons[0].bind,
                        sc_app::model::BezelBind::Mode { to: "SCM".into() }
                    );
                    return;
                }
            }
        }
    })
    .await
    .expect("placement and bezel pushed back");

    let _ = close.send(true);
    session.await.unwrap();
    service.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wrong_token_gets_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let service = Service::start(ServiceOptions {
        port: 0,
        settings_dir: None,
        bus_file: dir.path().join("bus.json"),
        ..ServiceOptions::standard()
    })
    .await
    .unwrap();
    let (_out, outgoing) = mpsc::unbounded_channel();
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let (close, closing) = watch::channel(false);
    let session = tokio::spawn(client::run(
        service.port(),
        "0".repeat(64),
        client::Shared::default(),
        outgoing,
        move |e| {
            let _ = events_tx.send(e);
        },
        closing,
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(1500), events.recv())
            .await
            .is_err()
    );
    let _ = close.send(true);
    session.await.unwrap();
    service.close().await;
}
