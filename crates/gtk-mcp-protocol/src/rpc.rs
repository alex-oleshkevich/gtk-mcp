//! JSON-RPC 2.0 messages and NDJSON framing (one message per line).

use crate::types::{AgentError, ErrorCode, MAX_FRAME_BYTES, RequestMeta};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{self, BufRead, Write};

const JSONRPC: &str = "2.0";
pub const RPC_INVALID_PARAMS: i64 = -32602;
pub const RPC_INVALID_REQUEST: i64 = -32600;
pub const RPC_PARSE_ERROR: i64 = -32700;
/// Application error; the typed error lives in `error.data`.
pub const RPC_APPLICATION: i64 = -32000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<AgentError>,
}

impl From<AgentError> for RpcError {
    fn from(e: AgentError) -> Self {
        let code = if e.code == ErrorCode::InvalidParams { RPC_INVALID_PARAMS } else { RPC_APPLICATION };
        Self { code, message: e.message.clone(), data: Some(e) }
    }
}

impl RpcError {
    /// The typed error; plain JSON-RPC errors map to `invalid_params`.
    pub fn into_agent_error(self) -> AgentError {
        match self.data {
            Some(d) => d,
            None => AgentError::new(ErrorCode::InvalidParams, self.message),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub jsonrpc: String,
    /// `null` when the request id could not be recovered (malformed frame).
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value, deadline_ms: Option<u64>) -> Self {
        let mut params = params;
        if let Some(ms) = deadline_ms {
            if params.is_null() {
                params = json!({});
            }
            if let Some(o) = params.as_object_mut() {
                o.insert("_meta".into(), serde_json::to_value(RequestMeta { deadline_ms: ms }).unwrap());
            }
        }
        Self { jsonrpc: JSONRPC.into(), id, method: method.into(), params }
    }

    pub fn meta(&self) -> Option<RequestMeta> {
        serde_json::from_value(self.params.get("_meta")?.clone()).ok()
    }

    /// Deserialize `params` into `T`, ignoring `_meta`.
    pub fn parse_params<T: DeserializeOwned>(&self) -> Result<T, AgentError> {
        let mut p = self.params.clone();
        if p.is_null() {
            p = json!({});
        }
        if let Some(o) = p.as_object_mut() {
            o.remove("_meta");
        }
        serde_json::from_value(p)
            .map_err(|e| AgentError::invalid_params(format!("invalid params for {}: {e}", self.method)))
    }
}

impl Notification {
    pub fn new(method: &str, params: Value) -> Self {
        Self { jsonrpc: JSONRPC.into(), method: method.into(), params }
    }
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self { jsonrpc: JSONRPC.into(), id: Some(id), result: Some(result), error: None }
    }
    pub fn err(id: Option<u64>, e: AgentError) -> Self {
        Self { jsonrpc: JSONRPC.into(), id, result: None, error: Some(e.into()) }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("malformed message: {0}")]
pub struct ParseError(pub String);

/// Classify one frame. `id` is recovered when possible so the caller can
/// answer a malformed request with `invalid_params`.
pub fn parse_message(line: &[u8]) -> Result<Message, (Option<u64>, ParseError)> {
    let v: Value = serde_json::from_slice(line).map_err(|e| (None, ParseError(e.to_string())))?;
    let id = v.get("id").and_then(Value::as_u64);
    let bad = |m: &str| (id, ParseError(m.to_string()));
    let obj = v.as_object().ok_or_else(|| bad("frame is not a JSON object"))?;
    if obj.get("jsonrpc").and_then(Value::as_str) != Some(JSONRPC) {
        return Err(bad("missing jsonrpc 2.0 marker"));
    }
    let has_method = obj.contains_key("method");
    if has_method && obj.contains_key("id") {
        serde_json::from_value(v).map(Message::Request).map_err(|e| (id, ParseError(e.to_string())))
    } else if has_method {
        serde_json::from_value(v).map(Message::Notification).map_err(|e| (id, ParseError(e.to_string())))
    } else if obj.contains_key("result") || obj.contains_key("error") {
        serde_json::from_value(v).map(Message::Response).map_err(|e| (id, ParseError(e.to_string())))
    } else {
        Err(bad("neither request, notification nor response"))
    }
}

/// One NDJSON line.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Line(Vec<u8>),
    /// A line longer than the cap; its bytes were discarded up to the newline,
    /// so the stream stays usable.
    Oversized { len: usize },
}

pub struct FrameReader<R> {
    inner: R,
    max: usize,
}

impl<R: BufRead> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        Self::with_limit(inner, MAX_FRAME_BYTES)
    }
    pub fn with_limit(inner: R, max: usize) -> Self {
        Self { inner, max }
    }
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Next non-empty frame; `Ok(None)` at EOF.
    pub fn next_frame(&mut self) -> io::Result<Option<Frame>> {
        let mut buf: Vec<u8> = Vec::new();
        let mut total = 0usize;
        let mut oversized = false;
        loop {
            let chunk = match self.inner.fill_buf() {
                Ok(c) => c,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if chunk.is_empty() {
                // EOF
                return Ok(if oversized {
                    Some(Frame::Oversized { len: total })
                } else if buf.iter().all(u8::is_ascii_whitespace) {
                    None
                } else {
                    Some(Frame::Line(buf))
                });
            }
            let (take, done) = match chunk.iter().position(|b| *b == b'\n') {
                Some(i) => (i, true),
                None => (chunk.len(), false),
            };
            total += take;
            if !oversized {
                if total > self.max {
                    oversized = true;
                    buf = Vec::new();
                } else {
                    buf.extend_from_slice(&chunk[..take]);
                }
            }
            self.inner.consume(take + usize::from(done));
            if done {
                if oversized {
                    return Ok(Some(Frame::Oversized { len: total }));
                }
                if buf.iter().all(u8::is_ascii_whitespace) {
                    buf.clear();
                    total = 0;
                    continue;
                }
                return Ok(Some(Frame::Line(buf)));
            }
        }
    }
}

/// Deepest array/object nesting a frame may have: the peer parses with
/// serde_json, whose recursion limit accepts 127 levels and rejects deeper
/// frames without recovering their `id`.
pub const MAX_FRAME_NESTING: usize = 127;

/// Deepest array/object nesting of serialized JSON.
fn nesting(json: &[u8]) -> usize {
    let (mut depth, mut max, mut in_str, mut esc) = (0usize, 0usize, false, false);
    for &b in json {
        if in_str {
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                max = max.max(depth);
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max
}

/// Serialize `msg` as one line. Fails with `InvalidData` when the frame would
/// exceed `MAX_FRAME_BYTES` or nest deeper than `MAX_FRAME_NESTING` (the peer
/// could not parse it), so callers can answer with an error instead.
pub fn write_frame<W: Write, T: Serialize>(w: &mut W, msg: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {} bytes exceeds the {} byte cap", bytes.len(), MAX_FRAME_BYTES),
        ));
    }
    let depth = nesting(&bytes);
    if depth > MAX_FRAME_NESTING {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame nests {depth} levels deep; the limit is {MAX_FRAME_NESTING}"),
        ));
    }
    bytes.push(b'\n');
    w.write_all(&bytes)?;
    w.flush()
}
