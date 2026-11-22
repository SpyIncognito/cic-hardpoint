//! The telemetry schema (compiled from schema/sc_game_telemetry_v1.proto at build time), the
//! gRPC codec over dynamic messages, and the decoded-message-to-JSON mapping that defines the
//! frame shape. Public so fakes and tools can serve or decode the same wire format.

use std::sync::LazyLock;

use prost::Message;
use prost_reflect::{
    DescriptorPool, DynamicMessage, Kind, MapKey, MessageDescriptor, ServiceDescriptor, Value,
};
use serde_json::{Map, Number, Value as Json};
use tonic::{
    Status,
    codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder},
};

pub const DEFAULT_ADDRESS: &str = "127.0.0.1:2942";

pub const SERVICE: &str = "sc.game.telemetry.v1.GameTelemetryService";
pub const TELEMETRY_STREAM_PATH: &str =
    "/sc.game.telemetry.v1.GameTelemetryService/TelemetryStream";
pub const TRIGGER_GAME_ACTION_PATH: &str =
    "/sc.game.telemetry.v1.GameTelemetryService/TriggerGameAction";
pub const LIST_TRIGGERABLE_ACTIONS_PATH: &str =
    "/sc.game.telemetry.v1.GameTelemetryService/ListTriggerableActions";

static POOL: LazyLock<DescriptorPool> = LazyLock::new(|| {
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/telemetry_descriptor.bin"));
    DescriptorPool::decode(bytes.as_slice()).expect("the embedded schema decodes")
});

pub fn pool() -> &'static DescriptorPool {
    &POOL
}

pub fn service() -> ServiceDescriptor {
    POOL.get_service_by_name(SERVICE)
        .expect("the schema defines the service")
}

/// A message of the telemetry package by its short name, e.g. "TelemetryStreamResponse".
///
/// # Panics
/// If the schema has no such message: names are compile-time facts of this crate.
pub fn message(name: &str) -> MessageDescriptor {
    POOL.get_message_by_name(&format!("sc.game.telemetry.v1.{name}"))
        .unwrap_or_else(|| panic!("the schema defines {name}"))
}

/// The message as protobufjs `toObject` gives it with `keepCase`, `enums: String`,
/// `longs: String`, `defaults: false` and `oneofs: false`.
pub fn to_json(message: &DynamicMessage) -> Json {
    let fields = message
        .fields()
        .map(|(field, value)| (field.name().to_owned(), value_to_json(value, &field.kind())))
        .collect();
    Json::Object(fields)
}

fn value_to_json(value: &Value, kind: &Kind) -> Json {
    match value {
        Value::Bool(b) => Json::Bool(*b),
        Value::I32(n) => (*n).into(),
        Value::U32(n) => (*n).into(),
        Value::I64(n) => n.to_string().into(),
        Value::U64(n) => n.to_string().into(),
        // f32 widens to the same f64 a JS number holds. NaN and infinities become null, as
        // JSON.stringify writes them in a TypeScript recording (serde_json has no NaN).
        Value::F32(n) => float(f64::from(*n)),
        Value::F64(n) => float(*n),
        Value::String(s) => s.as_str().into(),
        Value::Bytes(bytes) => bytes.iter().map(|&b| Json::from(b)).collect(),
        Value::EnumNumber(number) => enum_to_json(kind, *number),
        Value::Message(message) => to_json(message),
        Value::List(items) => items.iter().map(|item| value_to_json(item, kind)).collect(),
        Value::Map(entries) => {
            let value_kind = kind
                .as_message()
                .map(|entry| entry.map_entry_value_field().kind())
                .unwrap_or_else(|| kind.clone());
            let map: Map<String, Json> = entries
                .iter()
                .map(|(key, value)| (map_key(key), value_to_json(value, &value_kind)))
                .collect();
            Json::Object(map)
        }
    }
}

/// The number as serde_json reads back what JSON.stringify writes for the same JS number: an
/// integral value is an integer (so 1.0 equals `json!(1)`, as 1 === 1.0 in TypeScript).
fn float(n: f64) -> Json {
    const TWO_POW_63: f64 = 9_223_372_036_854_775_808.0;
    const TWO_POW_64: f64 = 18_446_744_073_709_551_616.0;
    if n.fract() == 0.0 {
        if n.abs() < TWO_POW_63 {
            return (n as i64).into();
        }
        if n > 0.0 && n < TWO_POW_64 {
            return (n as u64).into();
        }
    }
    Number::from_f64(n).map_or(Json::Null, Json::Number)
}

/// A value newer than this schema stays a number, as protobufjs leaves it.
fn enum_to_json(kind: &Kind, number: i32) -> Json {
    kind.as_enum()
        .and_then(|descriptor| descriptor.get_value(number))
        .map_or_else(|| number.into(), |value| value.name().into())
}

fn map_key(key: &MapKey) -> String {
    match key {
        MapKey::Bool(b) => b.to_string(),
        MapKey::I32(n) => n.to_string(),
        MapKey::I64(n) => n.to_string(),
        MapKey::U32(n) => n.to_string(),
        MapKey::U64(n) => n.to_string(),
        MapKey::String(s) => s.clone(),
    }
}

/// A message from JSON in the frame shape (proto field names, enum names, 64-bit integers as
/// strings or numbers). Unknown fields are an error.
pub fn from_json(
    descriptor: MessageDescriptor,
    json: &Json,
) -> Result<DynamicMessage, serde_json::Error> {
    DynamicMessage::deserialize(descriptor, json)
}

/// A tonic codec that encodes any [`DynamicMessage`] and decodes into one of a given type.
#[derive(Debug, Clone)]
pub struct DynamicCodec {
    decode_as: MessageDescriptor,
}

impl DynamicCodec {
    pub fn new(decode_as: MessageDescriptor) -> Self {
        Self { decode_as }
    }
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> Self::Encoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> Self::Decoder {
        DynamicDecoder(self.decode_as.clone())
    }
}

#[derive(Debug)]
pub struct DynamicEncoder;

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: DynamicMessage, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst)
            .map_err(|error| Status::internal(error.to_string()))
    }
}

#[derive(Debug)]
pub struct DynamicDecoder(MessageDescriptor);

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<DynamicMessage>, Status> {
        DynamicMessage::decode(self.0.clone(), src)
            .map(Some)
            .map_err(|error| Status::internal(error.to_string()))
    }
}
