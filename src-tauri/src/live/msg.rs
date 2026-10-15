// SPDX-License-Identifier: Apache-2.0
//! Studio Live messages (docs/LIVE-PROTOCOL.md in the site repo; spec docs/STUDIO-LIVE-SPEC.md § 5).
//!
//! Every frame body is one MessagePack map, the envelope `{t, s, c, n, q, b}`; absent fields are left out. `b` is the
//! type's body and stays a dynamic value here: the server reads only the fields it needs and passes the app's ops
//! through untouched (it never interprets an op).

use rmpv::Value;
use serde::{Deserialize, Serialize};

/// Protocol version spoken by this build. A peer with another major version is refused in `hello`.
pub const PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Envelope {
    pub t: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub s: String,
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub c: u32,
    #[serde(default, skip_serializing_if = "is_zero64")]
    pub n: u64,
    #[serde(default, skip_serializing_if = "is_zero64")]
    pub q: u64,
    #[serde(default = "nil", skip_serializing_if = "Value::is_nil")]
    pub b: Value,
}

fn nil() -> Value {
    Value::Nil
}

impl Default for Envelope {
    fn default() -> Self {
        Envelope { t: String::new(), s: String::new(), c: 0, n: 0, q: 0, b: Value::Nil }
    }
}

fn is_zero32(v: &u32) -> bool {
    *v == 0
}
fn is_zero64(v: &u64) -> bool {
    *v == 0
}

impl Envelope {
    pub fn new(t: &str) -> Self {
        Envelope { t: t.into(), ..Default::default() }
    }
    pub fn body(mut self, b: Value) -> Self {
        self.b = b;
        self
    }
    pub fn encode(&self) -> Vec<u8> {
        rmp_serde::to_vec_named(self).expect("envelope encodes")
    }
    /// Decodes exactly one envelope; trailing bytes are an error (docs/LIVE-PROTOCOL.md § 13, `bad`).
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let mut de = rmp_serde::Deserializer::new(std::io::Cursor::new(bytes));
        let env = Envelope::deserialize(&mut de).map_err(|e| format!("bad frame: {e}"))?;
        if de.get_ref().position() as usize != bytes.len() {
            return Err("bad frame: trailing bytes".into());
        }
        Ok(env)
    }
}

/// Builds a map body from (key, value) pairs: `map(&[("reason", "held".into())])`.
pub fn map(pairs: Vec<(&str, Value)>) -> Value {
    Value::Map(pairs.into_iter().map(|(k, v)| (Value::from(k), v)).collect())
}

/// Reads a field of a map body.
pub fn get<'a>(b: &'a Value, key: &str) -> Option<&'a Value> {
    match b {
        Value::Map(m) => m.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        _ => None,
    }
}
pub fn get_str<'a>(b: &'a Value, key: &str) -> Option<&'a str> {
    get(b, key).and_then(|v| v.as_str())
}
pub fn get_u64(b: &Value, key: &str) -> Option<u64> {
    get(b, key).and_then(|v| v.as_u64())
}
pub fn get_arr<'a>(b: &'a Value, key: &str) -> &'a [Value] {
    match get(b, key) {
        Some(Value::Array(a)) => a,
        _ => &[],
    }
}
