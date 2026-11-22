use std::{env, fs, path::PathBuf};

use prost::Message;

// The schema lives once, in schema/ at the repository root; the plugin repo vendors it from there.
const SCHEMA_DIR: &str = "../../schema";
const SCHEMA_FILE: &str = "sc_game_telemetry_v1.proto";

fn main() {
    println!("cargo:rerun-if-changed={SCHEMA_DIR}/{SCHEMA_FILE}");
    let descriptors = protox::compile([SCHEMA_FILE], [SCHEMA_DIR]).expect("schema compiles");
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    fs::write(
        out.join("telemetry_descriptor.bin"),
        descriptors.encode_to_vec(),
    )
    .expect("descriptor written");
}
