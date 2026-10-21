//! The HTTP face: AxeOS routes plus a `/sim/` namespace for tests.
//!
//! Routes a tuner uses:
//!
//! | method | path | |
//! |---|---|---|
//! | GET | `/api/system/info` | the telemetry document |
//! | GET | `/api/system/asic` | option lists |
//! | PATCH | `/api/system` | `{"frequency": …, "coreVoltage": …}` |
//! | POST | `/api/system/restart` | reboot |
//!
//! Routes a test uses:
//!
//! | method | path | |
//! |---|---|---|
//! | GET | `/sim/state` | ground truth |
//! | POST | `/sim/fault` | `{"chip": {…}}` or `{"board": {…}}` |
//! | POST | `/sim/clear_faults` | remove chip faults |
//! | POST | `/sim/advance` | `{"seconds": n}` — jump ahead |
//!
//! Faults are the enums of `warpaxe-sim`, spelled as tagged JSON:
//! `{"chip": {"HangAbove": {"hz": 600000000}}}`,
//! `{"board": {"SilentVrShutdown": {"above_hz": 600000000}}}`.

use crate::device::Device;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tiny_http::{Header, Method, Request, Response, Server};
use warpaxe_sim::{BoardFault, Fault};

/// Shared device behind the server.
pub type Shared = Arc<Mutex<Device>>;

#[derive(Deserialize)]
#[allow(non_snake_case)]
struct PatchBody {
    frequency: Option<f64>,
    coreVoltage: Option<f64>,
}

#[derive(Deserialize)]
enum ChipFaultReq {
    NoResponse { for_us: u64 },
    GarbageNonces { per_mille: u16 },
    BitFlipTx { per_mille: u16 },
    HangAbove { hz: u32 },
    DuplicateNonce { per_mille: u16 },
    RegReadCorrupt { reg: u8 },
    BaudSwitchIgnored,
    HashErrors { per_mille: u16 },
    WeakDomain { domain: u8, per_mille: u16 },
}

impl From<ChipFaultReq> for Fault {
    fn from(f: ChipFaultReq) -> Self {
        match f {
            ChipFaultReq::NoResponse { for_us } => Fault::NoResponse { for_us },
            ChipFaultReq::GarbageNonces { per_mille } => Fault::GarbageNonces { per_mille },
            ChipFaultReq::BitFlipTx { per_mille } => Fault::BitFlipTx { per_mille },
            ChipFaultReq::HangAbove { hz } => Fault::HangAbove { hz },
            ChipFaultReq::DuplicateNonce { per_mille } => Fault::DuplicateNonce { per_mille },
            ChipFaultReq::RegReadCorrupt { reg } => Fault::RegReadCorrupt { reg },
            ChipFaultReq::BaudSwitchIgnored => Fault::BaudSwitchIgnored,
            ChipFaultReq::HashErrors { per_mille } => Fault::HashErrors { per_mille },
            ChipFaultReq::WeakDomain { domain, per_mille } => {
                Fault::WeakDomain { domain, per_mille }
            }
        }
    }
}

#[derive(Deserialize)]
enum BoardFaultReq {
    SilentVrShutdown { above_hz: u64 },
    ChipSensorStuck { at_c: f64 },
    AmbientRise { by_k: f64 },
}

impl From<BoardFaultReq> for BoardFault {
    fn from(f: BoardFaultReq) -> Self {
        match f {
            BoardFaultReq::SilentVrShutdown { above_hz } => {
                BoardFault::SilentVrShutdown { above_hz }
            }
            BoardFaultReq::ChipSensorStuck { at_c } => BoardFault::ChipSensorStuck { at_c },
            BoardFaultReq::AmbientRise { by_k } => BoardFault::AmbientRise { by_k },
        }
    }
}

#[derive(Deserialize)]
struct FaultBody {
    chip: Option<ChipFaultReq>,
    board: Option<BoardFaultReq>,
}

#[derive(Deserialize)]
struct AdvanceBody {
    seconds: u64,
}

fn json(status: u16, body: serde_json::Value) -> Response<std::io::Cursor<Vec<u8>>> {
    let text = body.to_string();
    Response::from_string(text)
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
}

fn read_body(req: &mut Request) -> String {
    let mut s = String::new();
    let _ = req.as_reader().read_to_string(&mut s);
    s
}

/// Handle one request against the device.
pub fn handle(dev: &Shared, mut req: Request) {
    let method = req.method().clone();
    let path = req.url().split('?').next().unwrap_or("").to_string();
    let resp = match (&method, path.as_str()) {
        (Method::Get, "/api/system/info") => json(200, dev.lock().unwrap().system_info()),
        (Method::Get, "/api/system/asic") => json(200, dev.lock().unwrap().asic_info()),
        (Method::Patch, "/api/system") => {
            let body = read_body(&mut req);
            match serde_json::from_str::<PatchBody>(&body) {
                Ok(p) => {
                    let f = p.frequency.map(|x| x.round() as u16);
                    let v = p.coreVoltage.map(|x| x.round() as u16);
                    dev.lock().unwrap().set_point(f, v);
                    json(200, serde_json::json!({}))
                }
                Err(e) => json(400, serde_json::json!({"error": e.to_string()})),
            }
        }
        (Method::Post, "/api/system/restart") => {
            dev.lock().unwrap().restart();
            json(200, serde_json::json!({}))
        }
        (Method::Get, "/sim/state") => json(200, dev.lock().unwrap().sim_state()),
        (Method::Post, "/sim/fault") => {
            let body = read_body(&mut req);
            match serde_json::from_str::<FaultBody>(&body) {
                Ok(f) => {
                    let mut d = dev.lock().unwrap();
                    if let Some(c) = f.chip {
                        d.inject_chip(c.into());
                    }
                    if let Some(b) = f.board {
                        d.inject_board(b.into());
                    }
                    json(200, serde_json::json!({}))
                }
                Err(e) => json(400, serde_json::json!({"error": e.to_string()})),
            }
        }
        (Method::Post, "/sim/clear_faults") => {
            dev.lock().unwrap().clear_chip_faults();
            json(200, serde_json::json!({}))
        }
        (Method::Post, "/sim/advance") => {
            let body = read_body(&mut req);
            match serde_json::from_str::<AdvanceBody>(&body) {
                Ok(a) => {
                    dev.lock().unwrap().tick(a.seconds * 1000);
                    json(
                        200,
                        serde_json::json!({"now_ms": dev.lock().unwrap().now_ms()}),
                    )
                }
                Err(e) => json(400, serde_json::json!({"error": e.to_string()})),
            }
        }
        _ => json(404, serde_json::json!({"error": "not found"})),
    };
    let _ = req.respond(resp);
}

/// Serve forever on `server`.
pub fn serve(server: &Server, dev: &Shared) {
    for req in server.incoming_requests() {
        handle(dev, req);
    }
}
