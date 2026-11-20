//! JSON-RPC lines as Stratum V1 uses them.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// `mining.notify`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notify {
    /// Pool's job id, echoed in `mining.submit`.
    pub job_id: String,
    /// As sent: 32 bytes hex, in Stratum's swapped-word order.
    pub prevhash: String,
    /// Coinbase part 1, hex.
    pub coinb1: String,
    /// Coinbase part 2, hex.
    pub coinb2: String,
    /// Merkle branches, hex, natural byte order.
    pub merkle_branches: Vec<String>,
    /// Block version, hex.
    pub version: String,
    /// Compact target, hex.
    pub nbits: String,
    /// Block time, hex.
    pub ntime: String,
    /// Drop all previous jobs.
    pub clean_jobs: bool,
}

/// A request we send; `id` correlates the response.
#[derive(Debug, Clone, Serialize)]
pub struct Request {
    /// Correlation id.
    pub id: u64,
    /// Stratum method name.
    pub method: &'static str,
    /// Positional parameters.
    pub params: Value,
}

impl Request {
    /// The line to send, newline included.
    pub fn to_line(&self) -> String {
        let mut s = serde_json::to_string(self).unwrap_or_default();
        s.push('\n');
        s
    }
}

/// The answer to a `mining.ping`, newline included: a JSON-RPC result
/// "pong" with the ping's id, which is what ckpool looks for.
pub fn pong_line(id: &Value) -> String {
    let mut s = serde_json::to_string(&json!({"id": id, "result": "pong", "error": null}))
        .unwrap_or_default();
    s.push('\n');
    s
}

/// A response to one of our requests.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// The request's id.
    pub id: u64,
    /// `result`, `Null` if absent.
    pub result: Value,
    /// `error`, if the pool set one.
    pub error: Option<Value>,
}

/// One line from the pool, classified.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// `mining.notify`: new work.
    Notify(Notify),
    /// `mining.set_difficulty`: share difficulty from now on.
    SetDifficulty(f64),
    /// `mining.set_version_mask`: the rolling mask as u32.
    SetVersionMask(u32),
    /// `mining.ping`: answered with a pong carrying the same id. ckpool
    /// sends it while its node gives it no new block template, to keep
    /// the miners connected (2026-11-20: hours of pings, no new job).
    Ping {
        /// The ping's id, echoed in the pong.
        id: Value,
    },
    /// `client.reconnect`, `mining.set_extranonce`, anything else.
    Other {
        /// Method name as sent.
        method: String,
        /// Positional parameters as sent.
        params: Value,
    },
    /// A response to one of our requests.
    Response(Response),
    /// Not JSON, or JSON that is neither a call nor a response.
    Unparsed(String),
}

#[derive(Deserialize)]
struct Raw {
    id: Option<Value>,
    method: Option<String>,
    params: Option<Value>,
    result: Option<Value>,
    error: Option<Value>,
}

/// Classify one line (with or without trailing newline).
pub fn parse_line(line: &str) -> Incoming {
    let line = line.trim();
    let Ok(raw) = serde_json::from_str::<Raw>(line) else {
        return Incoming::Unparsed(line.to_string());
    };
    if let Some(method) = raw.method {
        let params = raw.params.unwrap_or(Value::Null);
        return match method.as_str() {
            "mining.notify" => match notify_from(&params) {
                Some(n) => Incoming::Notify(n),
                None => Incoming::Unparsed(line.to_string()),
            },
            "mining.set_difficulty" => match params.get(0).and_then(Value::as_f64) {
                Some(d) => Incoming::SetDifficulty(d),
                None => Incoming::Unparsed(line.to_string()),
            },
            "mining.set_version_mask" => match params
                .get(0)
                .and_then(Value::as_str)
                .and_then(|h| u32::from_str_radix(h, 16).ok())
            {
                Some(m) => Incoming::SetVersionMask(m),
                None => Incoming::Unparsed(line.to_string()),
            },
            "mining.ping" => Incoming::Ping {
                id: raw.id.unwrap_or(Value::Null),
            },
            _ => Incoming::Other { method, params },
        };
    }
    if let Some(id) = raw.id.as_ref().and_then(Value::as_u64) {
        return Incoming::Response(Response {
            id,
            result: raw.result.unwrap_or(Value::Null),
            error: raw.error.filter(|e| !e.is_null()),
        });
    }
    Incoming::Unparsed(line.to_string())
}

fn notify_from(p: &Value) -> Option<Notify> {
    let a = p.as_array()?;
    let s = |i: usize| a.get(i)?.as_str().map(str::to_string);
    Some(Notify {
        job_id: s(0)?,
        prevhash: s(1)?,
        coinb1: s(2)?,
        coinb2: s(3)?,
        merkle_branches: a
            .get(4)?
            .as_array()?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        version: s(5)?,
        nbits: s(6)?,
        ntime: s(7)?,
        clean_jobs: a.get(8).and_then(Value::as_bool).unwrap_or(false),
    })
}

/// `mining.configure` asking for version rolling with the BIP 320 mask.
pub fn configure_request(id: u64, mask: u32) -> Request {
    Request {
        id,
        method: "mining.configure",
        params: json!([
            ["version-rolling"],
            { "version-rolling.mask": format!("{mask:08x}"), "version-rolling.min-bit-count": 16 }
        ]),
    }
}

/// `mining.subscribe` with our user agent.
pub fn subscribe_request(id: u64, agent: &str) -> Request {
    Request {
        id,
        method: "mining.subscribe",
        params: json!([agent]),
    }
}

/// `mining.authorize`.
pub fn authorize_request(id: u64, user: &str, pass: &str) -> Request {
    Request {
        id,
        method: "mining.authorize",
        params: json!([user, pass]),
    }
}

/// `mining.suggest_difficulty`.
pub fn suggest_difficulty_request(id: u64, difficulty: u32) -> Request {
    Request {
        id,
        method: "mining.suggest_difficulty",
        params: json!([difficulty]),
    }
}

/// `mining.suggest_difficulty` below 1, for a miner of a few hundred kH/s
/// (the T-Display-S3 devkit hashing on its CPU): the number as is, with
/// at most three decimals, as NerdMiner sends it.
pub fn suggest_difficulty_fraction_request(id: u64, difficulty: f64) -> Request {
    let rounded = (difficulty * 1000.0).round() / 1000.0;
    Request {
        id,
        method: "mining.suggest_difficulty",
        params: json!([rounded]),
    }
}

/// `mining.submit` with version rolling (5th parameter = version bits).
#[allow(clippy::too_many_arguments)]
pub fn submit_request(
    id: u64,
    user: &str,
    job_id: &str,
    extranonce2: &str,
    ntime: u32,
    nonce: u32,
    version_bits: u32,
) -> Request {
    Request {
        id,
        method: "mining.submit",
        params: json!([
            user,
            job_id,
            extranonce2,
            format!("{ntime:08x}"),
            format!("{nonce:08x}"),
            format!("{version_bits:08x}")
        ]),
    }
}

/// What `mining.subscribe` answers: extranonce1 (hex) and extranonce2 size.
pub fn parse_subscribe_result(result: &Value) -> Option<(String, usize)> {
    let a = result.as_array()?;
    let extranonce1 = a.get(1)?.as_str()?.to_string();
    // Exact, not a truncating cast: on the 32-bit target 0x1_0000_0004
    // would otherwise pass the size check as 4.
    let size = usize::try_from(a.get(2)?.as_u64()?).ok()?;
    Some((extranonce1, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_is_answered_with_its_id() {
        let ping = r#"{"params":[],"id":42,"method":"mining.ping"}"#;
        let Incoming::Ping { id } = parse_line(ping) else {
            panic!("not a ping");
        };
        assert_eq!(
            pong_line(&id),
            "{\"error\":null,\"id\":42,\"result\":\"pong\"}\n"
        );
        let Incoming::Ping { id } = parse_line(r#"{"method":"mining.ping","params":[]}"#) else {
            panic!("not a ping");
        };
        assert_eq!(id, Value::Null);
    }

    #[test]
    fn suggest_fraction_keeps_three_decimals() {
        let line = suggest_difficulty_fraction_request(4, 0.005).to_line();
        assert_eq!(
            line,
            "{\"id\":4,\"method\":\"mining.suggest_difficulty\",\"params\":[0.005]}\n"
        );
        let line = suggest_difficulty_fraction_request(4, 0.0049).to_line();
        assert!(line.contains("[0.005]"), "{line}");
    }
}
