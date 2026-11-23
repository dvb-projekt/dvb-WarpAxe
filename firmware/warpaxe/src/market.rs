//! BTC price and fee rates for the display's Bitcoin page, from
//! mempool.space — only while `btcPrice` is on. Everything else on that
//! page comes from the pool's job; this is the one place the miner asks a
//! third party.

use std::sync::Mutex;
use std::time::Duration;

use esp_idf_svc::http::client::Configuration;

use crate::{config, diag};

/// Between fetches.
const EVERY: Duration = Duration::from_secs(600);

#[derive(Clone, Debug, Default)]
pub struct Market {
    /// Price in `currency`, whole units.
    pub price: Option<(String, f64)>,
    /// Fee rates sat/vB: next block, half hour, hour.
    pub fees: Option<(f64, f64, f64)>,
}

static MARKET: Mutex<Option<Market>> = Mutex::new(None);

/// The last values, if the switch is on and a fetch has worked.
pub fn get() -> Option<Market> {
    if !config::get().btc_price {
        return None;
    }
    MARKET.lock().unwrap().clone()
}

pub fn start() {
    diag::spawn_psram("market", 8192, || loop {
        let cfg = config::get();
        if cfg.btc_price {
            match fetch(&cfg.price_currency) {
                Ok(m) => *MARKET.lock().unwrap() = Some(m),
                Err(e) => log::warn!("market: {e}"),
            }
            std::thread::sleep(EVERY);
        } else {
            *MARKET.lock().unwrap() = None;
            std::thread::sleep(Duration::from_secs(30));
        }
    })
    .expect("market thread");
}

fn get_json(url: &str) -> anyhow::Result<serde_json::Value> {
    // Over IPv6 when the pool's rule allows it (docs/17).
    let mut c = crate::net::http::get(
        url,
        &Configuration {
            crt_bundle_attach: Some(crate::tls_trust::attach),
            timeout: Some(Duration::from_secs(10)),
            ..Default::default()
        },
        &[],
    )?;
    if c.status() != 200 {
        anyhow::bail!("{url}: HTTP {}", c.status());
    }
    let mut body = Vec::new();
    let mut buf = [0u8; 512];
    loop {
        let n = c.read(&mut buf)?;
        if n == 0 || body.len() > 8192 {
            break;
        }
        body.extend_from_slice(&buf[..n]);
    }
    Ok(serde_json::from_slice(&body)?)
}

fn fetch(currency: &str) -> anyhow::Result<Market> {
    let prices = get_json("https://mempool.space/api/v1/prices")?;
    let fees = get_json("https://mempool.space/api/v1/fees/recommended")?;
    let cur = currency.to_ascii_uppercase();
    let price = prices.get(&cur).and_then(|v| v.as_f64()).map(|p| (cur, p));
    let f = |k: &str| fees.get(k).and_then(|v| v.as_f64());
    let fees = match (f("fastestFee"), f("halfHourFee"), f("hourFee")) {
        (Some(a), Some(b), Some(c)) => Some((a, b, c)),
        _ => None,
    };
    Ok(Market { price, fees })
}
