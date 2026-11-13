//! Every form of the grammar gives the value `serde_json::json!` gives:
//! each case is expanded by both macros from the same tokens.

use std::collections::BTreeMap;

macro_rules! same {
    ($($t:tt)+) => {{
        let ours = warpaxe_jsonval::json!($($t)+);
        let theirs = serde_json::json!($($t)+);
        assert_eq!(ours, theirs);
        assert_eq!(ours.to_string(), theirs.to_string());
        ours
    }};
}

#[derive(serde::Serialize)]
struct Fan {
    rpm: u16,
    name: &'static str,
    duty: Option<u8>,
}

#[test]
fn the_bare_values() {
    same!(null);
    same!(true);
    same!(false);
    same!(0);
    same!(-7);
    same!(1.5);
    same!("text");
    same!([]);
    same!({});
    same!(u64::MAX);
    same!(i64::MIN);
    same!(0.1f32);
    same!(f64::NAN);
    same!(Some(3u8));
    same!(None::<u8>);
    same!(String::from("owned"));
    same!(vec![1u16, 2, 3]);
    same!(Fan {
        rpm: 4200,
        name: "chip",
        duty: None
    });
}

#[test]
fn arrays() {
    let x = 5;
    same!([1, 2, 3]);
    same!([1, 2, 3,]);
    same!([null, true, false]);
    same!([null]);
    same!([[1, [2, []]], {}, {"a": [x]}]);
    same!([x, x + 1, x * 2,]);
    same!(["a", String::from("b"), 'c']);
    same!([if x > 3 { "big" } else { "small" }, x.to_string()]);
}

#[test]
fn objects() {
    let (a, b) = (1u32, 2.5f32);
    let name = String::from("gaia");
    let v = same!({
        "a": a,
        "b": b,
        "name": name,
        "none": None::<u32>,
        "null": null,
        "yes": true,
        "no": false,
        "list": [a, b, null, {"deep": [true]}],
        "inner": { "x": a + 1, "y": { "z": [] } },
        "empty": {},
        "calc": if a > 0 { u64::from(a) * 2 } else { 0 },
        "closure": (1..=a).map(|n| n + 1).sum::<u32>(),
        "method": name.to_uppercase(),
        "fan": Fan { rpm: 1800, name: "vr", duty: Some(40) },
        "tuple": (1, "two", 3.0),
    });
    assert_eq!(v["inner"]["x"], 2);
    // Without and with the trailing comma.
    same!({"one": 1});
    same!({"one": 1,});
    same!({"one": 1, "two": [2], "three": {"3": 3}});
}

#[test]
fn keys_that_are_expressions() {
    let key = String::from("fromString");
    let part = "part";
    same!({ key.clone(): 1, (format!("{part}-2")): 2, part: 3 });
    same!({ (key.as_str()): [1] });
}

#[test]
fn a_repeated_key_keeps_the_later_entry() {
    let v = same!({ "k": 1, "other": 0, "k": 2 });
    assert_eq!(v["k"], 2);
    assert_eq!(v.as_object().unwrap().len(), 2);
}

#[test]
fn values_made_of_values() {
    let inner = serde_json::json!({"deep": [1, 2]});
    let list: Vec<serde_json::Value> = (0..3).map(|i| warpaxe_jsonval::json!({ "i": i })).collect();
    let mut map = BTreeMap::new();
    map.insert("x", 1);
    same!({
        "inner": inner,
        "list": list,
        "map": map,
        "mapped": (0..3).map(|i| serde_json::json!({"n": i, "odd": i % 2 == 1})).collect::<Vec<_>>(),
        "chosen": if list.len() > 2 { serde_json::json!(1.5) } else { serde_json::json!("few") },
    });
}

#[test]
fn entries_are_evaluated_once_and_in_order() {
    let mut seen = Vec::new();
    let mut step = |n: u32| {
        seen.push(n);
        n
    };
    let v = warpaxe_jsonval::json!({ "c": step(1), "a": step(2), "b": [step(3), step(4)] });
    assert_eq!(seen, [1, 2, 3, 4]);
    assert_eq!(v, serde_json::json!({ "a": 2, "b": [3, 4], "c": 1 }));
}

/// A document shaped like the firmware's `/api/system/info`: many keys,
/// values of every kind.
#[test]
fn a_wide_document() {
    let (temp, hashrate, shares, host) = (56.25f32, 6524.8f64, 123_456u64, String::from("warpaxe"));
    let fans = [
        Fan {
            rpm: 1800,
            name: "chip",
            duty: Some(59),
        },
        Fan {
            rpm: 0,
            name: "vr",
            duty: None,
        },
    ];
    same!({
        "temp": temp, "vrTemp": temp.round() as u32, "hashRate": hashrate, "hashRate_10m": hashrate,
        "sharesAccepted": shares, "sharesRejected": 0, "hostname": host, "ssid": "",
        "overheat_mode": u8::from(temp > 70.0), "fans": fans, "fanrpm": fans[0].rpm,
        "hashrateMonitor": { "asics": [{ "total": hashrate / 4.0, "domains": [1.0, 2.0, 3.0, 4.0] }] },
        "bestDiff": if shares > 0 { serde_json::json!(4.29e7) } else { serde_json::json!(0) },
        "stratumURL": "pool.example", "stratumPort": 3333u16, "isUsingFallbackStratum": false,
        "selfHeals": ["chainRestart", "domainHeal"], "uptimeSeconds": 86_400u64,
        "wifiRSSI": -61i8, "freeHeap": 31_232u32, "version": "0.1.44", "boardVersion": "nerdqaxe-plus-plus",
        "lastBlock": null, "yieldPercent": Some(97.3f64), "psuWatts": None::<u16>,
    });
}
