//! The Rust side of the recording parity check: one canonical JSON line per frame of a telemetry
//! recording (every value and toggle view, the portal status and the dashboard values model), with
//! the recording's `t` as the clock. The TS side (recording-parity.mjs and its diff tool) stays in the
//! Stream Deck plugin repo under tools/parity.
//!
//! ```text
//! SC_RECORDING=<recording.jsonl> SC_PARITY_OUT=<out.jsonl> cargo run --example recording_parity
//! ```
//!
//! Without `SC_RECORDING` it does nothing, so builds without the private recordings still pass.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::sync::Arc;
use std::time::Duration;

use sc_core::engine::TOGGLES;
use sc_core::state::VALUES;
use sc_core::{ControlCore, ControlCoreOptions};
use sc_game::{ReplayGame, ReplayOptions};
use sc_service::dashboard::build_values;
use sc_service::json::to_js_value;
use sc_service::portal::status::build_status;
use serde_json::{Map, Value, json};

fn sorted(v: Value) -> Value {
    match v {
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        Value::Object(fields) => {
            let mut fields: Vec<_> = fields.into_iter().collect();
            fields.sort_by(|(a, _), (b, _)| a.cmp(b));
            Value::Object(fields.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        other => other,
    }
}

// Time is paused, so the state clock moves only when the loop advances it to the next frame's `t`.
#[tokio::main(flavor = "current_thread", start_paused = true)]
async fn main() -> std::io::Result<()> {
    let Ok(input) = std::env::var("SC_RECORDING") else {
        eprintln!("SC_RECORDING is not set; nothing to do");
        return Ok(());
    };
    let output = std::env::var("SC_PARITY_OUT").unwrap_or_else(|_| "recording-parity.jsonl".into());
    let game =
        ReplayGame::new(Vec::new(), ReplayOptions::default()).expect("default speed is valid");
    let core = ControlCore::new(ControlCoreOptions {
        game: Some(Arc::new(game)),
        ..ControlCoreOptions::default()
    });
    let mut out = BufWriter::new(File::create(output)?);
    let mut clock = 0_u64;
    let lines = BufReader::new(File::open(input)?).lines();
    for (i, line) in lines
        .filter(|l| l.as_ref().map_or(true, |l| !l.trim().is_empty()))
        .enumerate()
    {
        let line = line?;
        let record: Value = serde_json::from_str(&line)?;
        let t = record["t"].as_u64().expect("t is whole milliseconds");
        tokio::time::advance(Duration::from_millis(t - clock)).await;
        clock = t;
        core.ingest(&record["frame"]);
        let values: Map<String, Value> = VALUES
            .iter()
            .map(|d| (d.id.to_owned(), to_js_value(&core.value(&d.id))))
            .collect();
        let toggles: Map<String, Value> = TOGGLES
            .iter()
            .map(|d| (d.id.to_owned(), to_js_value(&core.view_toggle(d.id))))
            .collect();
        let line = json!({
            "i": i,
            "t": t,
            "values": values,
            "toggles": toggles,
            "status": to_js_value(&build_status(&core)),
            "push": to_js_value(&build_values(&core)),
        });
        serde_json::to_writer(&mut out, &sorted(line))?;
        out.write_all(b"\n")?;
    }
    out.flush()
}
