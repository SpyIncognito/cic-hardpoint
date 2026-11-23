//! A Hardpoint service whose peer-to-peer hosting listens on loopback only, with a replayed game,
//! for trying Hosting by hand and for page screenshots on one machine. A test tool, not part of
//! the app (the app hosts on every interface).
//!
//! `cargo run -p sc-service --example p2p_host -- <settings dir> [pages port]` prints the pages'
//! address; Host in the fleet panel starts hosting. Members on the same machine join with the
//! invite's address changed to 127.0.0.1.

use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;

use sc_core::engine::EngineOptions;
use sc_game::{GameApi, RecordedFrame, ReplayGame, ReplayOptions};
use sc_service::{Service, ServiceOptions};
use serde_json::json;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().ok_or("usage: p2p_host <settings dir> [port]")?);
    let port = args
        .next()
        .map(|p| p.parse::<u16>().map_err(|_| "the port must be a number"))
        .transpose()?
        .unwrap_or(0);
    let frames = (0..40)
        .map(|i| RecordedFrame {
            t: f64::from(i) * 250.0,
            frame: json!({
                "vehicle": { "state": { "name": "Gladius", "health": 0.7 + f64::from(i % 10) / 50.0,
                    "shields": { "level": 0.9 } } },
                "actor": { "vitals": { "health": { "current": 90, "max": 100 },
                    "oxygen": { "current": 1, "max": 1 } } },
            }),
        })
        .collect();
    let game = ReplayGame::new(
        frames,
        ReplayOptions {
            loop_playback: true,
            ..ReplayOptions::default()
        },
    )
    .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let service = Service::start(ServiceOptions {
        port,
        settings_dir: Some(dir.clone()),
        bus_file: dir.join("bus.json"),
        game: Some(Arc::new(game) as Arc<dyn GameApi>),
        engine: EngineOptions::default(),
        fleet_connector: None,
        relay: sc_service::relay::RelayOptions {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ..sc_service::relay::RelayOptions::default()
        },
    })
    .await
    .map_err(|e| e.to_string())?;
    println!("dashboard: http://127.0.0.1:{}/dash", service.port());
    let _ = tokio::signal::ctrl_c().await;
    service.close().await;
    Ok(())
}
