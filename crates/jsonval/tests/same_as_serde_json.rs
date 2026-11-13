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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    from_mhz: u32,
    to_mhz: u32,
    cause: &'static str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Verdict {
    Pass,
    Fail,
}

/// Readings are `f32`: what is not a number becomes `null`, and a
/// reading is printed as the `f64` of the same value (56.3 as
/// 56.29999923706055), which is how serde_json's `Value` keeps it and
/// how the API has always printed it.
#[test]
fn f32_readings() {
    let v = same!({
        "nan": f32::NAN, "inf": f32::INFINITY, "ninf": f32::NEG_INFINITY,
        "negZero": -0.0f32, "zero": 0.0f32,
        "temp": 56.3f32, "tenth": 0.1f32, "third": 1.0f32 / 3.0,
        "max": f32::MAX, "min": f32::MIN_POSITIVE, "whole": 12.0f32,
        "none": None::<f32>, "some": Some(49.7f32), "someNan": Some(f32::NAN),
        "chips": [Some(61.5f32), None::<f32>, Some(f32::NAN)],
        "list": vec![Some(1.25f32), None],
        "f64": 0.1f64, "f64nan": f64::NAN, "f64inf": f64::INFINITY,
        "sum": ([1.5f32, 2.25].iter().sum::<f32>()),
    });
    for key in ["nan", "inf", "ninf", "someNan", "f64nan", "f64inf"] {
        assert!(v[key].is_null(), "{key}");
    }
    assert!(v["chips"][2].is_null());
    let text = v.to_string();
    for part in [
        "\"temp\":56.29999923706055",
        "\"tenth\":0.10000000149011612",
        "\"f64\":0.1,",
        "\"negZero\":-0.0",
        "\"whole\":12.0",
    ] {
        assert!(text.contains(part), "{part} in {text}");
    }
}

#[test]
fn whole_numbers_at_their_limits() {
    let v = same!({
        "u64": u64::MAX, "i64": i64::MIN, "u32": u32::MAX, "i32": i32::MIN,
        "usize": usize::MAX, "u8": u8::MAX, "i8": i8::MIN, "u16": 65_535u16,
        "shares": 18_446_744_073_709_551_615u64, "negative": -1i64,
        "bool": u8::from(true), "list": [u64::MAX, 0, 1],
    });
    assert_eq!(
        v["u64"].to_string(),
        "18446744073709551615",
        "not through an f64"
    );
    assert_eq!(v["i64"].to_string(), "-9223372036854775808");
}

/// Host name, pool password, error texts: whatever a user or a pool
/// puts into a string comes out escaped as the original escapes it,
/// and reads back as the same value.
#[test]
fn strings_with_quotes_backslashes_control_characters_and_utf8() {
    let host = String::from("miner \"one\" \\ two/three");
    let pass = "p\u{0}\u{1}\u{8}\t\n\u{c}\r\u{1f}\u{7f}\u{80}äß€—😀\u{2028}\u{feff}";
    let error = format!("expected {{\"members\": [{}]}}", "addresses");
    let v = same!({
        "hostname": host, "byRef": &host, "asStr": host.as_str(),
        "password": pass, "error": error, "empty": "", "char": 'ß',
        "some": Some(&host), "none": None::<&str>,
        "list": [pass, "", "\"", "\\"],
        "key with \"quotes\" \\ and\nnewline": 1,
        "ключ 鍵 🔑": pass,
    });
    let back: serde_json::Value = serde_json::from_str(&v.to_string()).unwrap();
    assert_eq!(back, v);
    assert_eq!(back["password"], pass);
    assert_eq!(back["hostname"], "miner \"one\" \\ two/three");
}

/// The other things the API hands over: references, options in
/// options, slices and arrays, structs and enums with renamed fields,
/// maps, values built by either macro.
#[test]
fn what_the_api_passes() {
    let hash = String::from("00000000000000000001a2b3");
    let status = String::new();
    let temps: Vec<Option<f32>> = vec![Some(55.5), None];
    let heals: [&str; 3] = ["chain_silent", "chip_silent", "domain_latch"];
    let mut reasons = BTreeMap::new();
    reasons.insert(String::from("stale"), 3u32);
    reasons.insert(String::from("low difficulty"), 1);
    let mut by_number = BTreeMap::new();
    by_number.insert(2u8, "two");
    let last = Some((1u8, 1150u16, 86_400u64));
    let step = Some(Step {
        from_mhz: 525,
        to_mhz: 490,
        cause: "hot",
    });
    let v = same!({
        "hash": (!hash.is_empty()).then_some(&hash),
        "status": (!status.is_empty()).then_some(&status),
        "temps": temps, "tempsRef": &temps, "slice": &temps[..1],
        "selfHeals": heals, "nested": Some(None::<u8>), "unit": (),
        "reasons": reasons.iter().map(|(m, c)| warpaxe_jsonval::json!({"message": m, "count": c})).collect::<Vec<_>>(),
        "reasonsMap": reasons, "byNumber": by_number,
        "last": last.map(|(chip, mv, at)| serde_json::json!({"chip": chip, "mv": mv, "uptime_s": at})),
        "clockStep": step, "noStep": None::<Step>,
        "verdicts": [Verdict::Pass, Verdict::Fail],
        "pick": if temps.len() > 1 { warpaxe_jsonval::json!(1.5) } else { warpaxe_jsonval::json!(0) },
        "stratum": { "usingFallback": false, "pools": [{ "connected": true, "accepted": 12u64, }], },
        "null": null,
    });
    assert_eq!(v["clockStep"]["fromMhz"], 525);
    assert_eq!(v["verdicts"][1], "fail");
    assert_eq!(v["byNumber"]["2"], "two");
    assert!(v["status"].is_null() && v["nested"].is_null() && v["unit"].is_null());
    // Added after the macro, as the firmware does past its size limit.
    let mut ours = warpaxe_jsonval::json!({ "a": 1 });
    let mut theirs = serde_json::json!({ "a": 1 });
    ours["late"] = warpaxe_jsonval::json!([temps, null]);
    theirs["late"] = serde_json::json!([temps, null]);
    assert_eq!(ours, theirs);
}

/// A type JSON has no form for (a map whose keys are not strings or
/// numbers) panics in the conversion, here as in the original: the
/// firmware passes none.
#[test]
fn a_map_json_cannot_hold_panics_as_the_original_does() {
    let mut map = BTreeMap::new();
    map.insert((1u8, 2u8), 3u8);
    let ours = std::panic::catch_unwind(|| warpaxe_jsonval::json!({ "map": map }));
    let theirs = std::panic::catch_unwind(|| serde_json::json!({ "map": map }));
    assert!(ours.is_err() && theirs.is_err());
}
