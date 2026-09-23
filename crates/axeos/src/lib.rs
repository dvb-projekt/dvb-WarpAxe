//! An AxeOS-compatible miner that is not there: the simulator behind
//! the HTTP API a tuner speaks. See [`device::Device`] for the model and
//! [`server`] for the routes; `main.rs` is a thin CLI around both.

#![forbid(unsafe_code)]

pub mod device;
pub mod server;
