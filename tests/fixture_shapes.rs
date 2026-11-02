//! Contract tests over the fixtures in `tests/fixtures/`, run against the
//! same mapping code the binary uses (the crate builds as the `alp` library),
//! so a provider field rename fails here loudly instead of silently emptying
//! a column — plus the scrubbing rails that keep this repo publishable.
//!
//! Fully offline: no network, no keychain.

use std::path::PathBuf;

use alp::commands::account::account_dto;
use alp::commands::activities::resolve_range;
use alp::reads::{activity_date, keep_sorted, Position, DIVIDEND_INTEREST_TYPES};
use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {name}: {e}"));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {name}: {e}"))
}

fn rows(name: &str) -> Vec<Value> {
    fixture(name).as_array().expect("array fixture").clone()
}

// ------------------------------------------------------------------ account

#[test]
fn the_account_maps_with_verbatim_string_money() {
    let dto = account_dto(&fixture("account.json"));
    assert_eq!(dto["account_number"], "111111111");
    assert_eq!(dto["status"], "ACTIVE");
    assert_eq!(dto["currency"], "USD");
    // The wire's strings, untouched: "0" stays "0", not "0.00".
    assert_eq!(dto["cash"]["amount"], "1000.55");
    assert_eq!(dto["short_market_value"]["amount"], "0");
    assert_eq!(dto["equity"]["currency"], "USD");
    assert_eq!(dto["pattern_day_trader"], false);
    assert_eq!(dto["daytrade_count"], 0);
    assert_eq!(dto["created_at"], "2026-01-02T10:00:00.000000Z");
}

// ---------------------------------------------------------------- positions

#[test]
fn every_position_row_maps_and_leads_with_the_task_columns() {
    let rows = rows("positions.json");
    let mapped: Vec<Position> = rows.iter().filter_map(Position::from_json).collect();
    assert_eq!(mapped.len(), rows.len(), "a position row failed to map");
    for p in &mapped {
        let v = serde_json::to_value(p).unwrap();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(&keys[..3], &["symbol", "quantity", "market_value"]);
    }
}

#[test]
fn a_short_position_keeps_its_negative_string_quantities() {
    let rows = rows("positions.json");
    let short = rows
        .iter()
        .find(|r| r["side"] == "short")
        .expect("fixture carries a short row");
    let p = Position::from_json(short).unwrap();
    let v = serde_json::to_value(&p).unwrap();
    // Verbatim wire strings — the sign and precision survive.
    assert_eq!(v["quantity"], "-5");
    assert_eq!(v["market_value"]["amount"], "-240.00");
    assert_eq!(v["side"], "short");
}

// --------------------------------------------------------------- activities

#[test]
fn the_two_activity_shapes_carry_their_documented_fields() {
    for row in rows("activities.json") {
        let t = row["activity_type"].as_str().expect("activity_type");
        assert!(row["id"].is_string(), "every activity has an id");
        if t == "FILL" {
            for key in ["transaction_time", "price", "qty", "side", "symbol", "type"] {
                assert!(row[key].is_string(), "FILL row missing `{key}`: {row}");
            }
            assert!(
                row.get("net_amount").is_none(),
                "trade rows carry no net_amount — absent, not null"
            );
        } else {
            for key in ["date", "net_amount"] {
                assert!(row[key].is_string(), "{t} row missing `{key}`: {row}");
            }
            assert!(
                row.get("transaction_time").is_none(),
                "non-trade rows carry no transaction_time"
            );
        }
    }
}

#[test]
fn every_activity_row_dates_for_filtering() {
    for row in rows("activities.json") {
        let d = activity_date(&row).unwrap_or_else(|| panic!("undatable: {row}"));
        assert_eq!(d.len(), 10);
        assert!(d.starts_with("2026-"), "{d}");
    }
}

/// THE consumer contract: `transaction-list/v1` items are the wire rows
/// verbatim. tax-cli stores these DTOs as source documents, so selection and
/// ordering are the only things the CLI may do.
#[test]
fn kept_activity_rows_are_byte_identical_to_the_wire() {
    let wire = rows("activities.json");
    let kept = keep_sorted(wire.clone(), "2026-01-01", "2026-12-31", None);
    assert_eq!(kept.len(), wire.len());
    for row in &wire {
        assert!(kept.contains(row), "a row was reshaped or dropped: {row}");
    }
    // Newest first.
    let dates: Vec<String> = kept.iter().filter_map(activity_date).collect();
    let mut sorted = dates.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(dates, sorted);
    // String decimals survive exactly — including trailing wire zeros.
    let fill = kept.iter().find(|r| r["type"] == "fill").unwrap();
    assert_eq!(fill["price"], "105.1000");
}

#[test]
fn the_dividend_filter_matches_the_fixtures_income_rows_only() {
    for row in rows("activities.json") {
        let t = row["activity_type"].as_str().unwrap();
        let is_income = DIVIDEND_INTEREST_TYPES.contains(&t);
        match t {
            "DIV" | "DIVNRA" | "INT" => assert!(is_income, "{t} belongs to dividends list"),
            "FILL" | "FEE" => assert!(!is_income, "{t} must not leak into dividends list"),
            other => panic!("fixture grew an untested activity type {other}"),
        }
    }
}

#[test]
fn range_defaults_are_year_to_date() {
    let (since, until) = resolve_range(&Default::default()).unwrap();
    assert!(since.ends_with("-01-01"));
    assert_eq!(&since[..4], &until[..4]);
}

// ------------------------------------------------------------------- errors

#[test]
fn error_bodies_carry_code_and_message() {
    for name in ["error_401.json", "error_429.json"] {
        let v = fixture(name);
        assert!(v["code"].is_number(), "{name}");
        assert!(v["message"].is_string(), "{name}");
    }
}

// ------------------------------------------------------- the scrubbing rails

fn all_fixtures() -> Vec<(String, Value)> {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("fixtures dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let raw = std::fs::read_to_string(&path).expect("read fixture");
        let v = serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {name}: {e}"));
        out.push((name, v));
    }
    assert!(
        !out.is_empty(),
        "no fixtures found — the scrub check is vacuous"
    );
    out
}

/// The value policy from `tests/fixtures/README.md`.
///
/// Stated positively — "identity-bearing fields must hold a known dummy" —
/// rather than as a denylist of real values, because a denylist would itself
/// publish the values it exists to keep out of the repo.
#[test]
fn fixtures_carry_only_synthetic_identifiers() {
    for (name, value) in all_fixtures() {
        check(&value, None, &name);
    }
}

fn check(v: &Value, key: Option<&str>, path: &str) {
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                check(val, Some(k), path);
            }
        }
        Value::Array(items) => {
            for item in items {
                check(item, key, path);
            }
        }
        Value::String(s) => {
            let ok = match key {
                Some("account_number") => s == "111111111",
                Some("symbol") => s == "SMPL" || s == "ETFX",
                _ => true,
            };
            assert!(
                ok,
                "{path}: key `{}` holds a non-synthetic value {s:?} — see \
                 tests/fixtures/README.md",
                key.unwrap_or("?")
            );

            // Any UUID-shaped string anywhere must be a dummy, whatever key
            // it hides under — activity ids embed one after the `::`.
            let uuid_part = s.rsplit("::").next().unwrap_or(s);
            assert!(
                !looks_like_uuid(uuid_part) || is_dummy_uuid(uuid_part),
                "{path}: key `{}` holds a real-looking UUID {s:?}",
                key.unwrap_or("?")
            );

            // Anything shaped like an Alpaca API key must be a marked dummy.
            for prefix in ["AK", "PK"] {
                if s.starts_with(prefix) && s.len() >= 16 && s.chars().all(|c| c.is_alphanumeric())
                {
                    assert!(
                        s.starts_with(&format!("{prefix}DUMMY")),
                        "{path}: {s:?} is shaped like a real API key"
                    );
                }
            }
        }
        _ => {}
    }
}

fn looks_like_uuid(s: &str) -> bool {
    s.len() == 36
        && s.as_bytes()[8] == b'-'
        && s.as_bytes()[13] == b'-'
        && s.as_bytes()[18] == b'-'
        && s.as_bytes()[23] == b'-'
        && s.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// A dummy UUID is nearly all zeros — at most a few significant digits.
fn is_dummy_uuid(s: &str) -> bool {
    s.chars().filter(|c| *c != '-' && *c != '0').count() <= 4
}

/// The scrub check has to be able to fail, or it proves nothing.
#[test]
fn the_dummy_uuid_rule_rejects_a_real_looking_uuid() {
    assert!(is_dummy_uuid("00000000-0000-4000-8000-000000000031"));
    assert!(!is_dummy_uuid("58542935-67b5-56e1-a3f9-42686e07fa40"));
    assert!(!looks_like_uuid("not-a-uuid"));
}

/// The fixture check only ever guarded `tests/fixtures/`. Personal data leaks
/// into prose and examples at least as easily — a phone number in a doc
/// comment, an account number in a README example — so scan the tracked
/// sources too.
#[test]
fn tracked_sources_carry_no_personal_data() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["ls-files"])
        .output();
    let Ok(out) = out else {
        return; // not a git checkout (vendored build) — nothing to scan
    };
    if !out.status.success() {
        return;
    }

    let files = String::from_utf8_lossy(&out.stdout);
    let mut scanned = 0usize;
    for rel in files.lines() {
        if rel.starts_with("tests/fixtures/") || rel.ends_with(".lock") {
            continue;
        }
        let path = root.join(rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        scanned += 1;

        for (n, line) in text.lines().enumerate() {
            assert!(
                !has_phone_shape(line),
                "{rel}:{}: looks like a real phone number",
                n + 1
            );
        }
    }
    assert!(scanned > 0, "scanned nothing — the check would be vacuous");
}

/// `NNN-NNN-NNNN`, ignoring version-like and date-like runs.
fn has_phone_shape(line: &str) -> bool {
    let digits: Vec<char> = line
        .chars()
        .filter(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    let s: String = digits.into_iter().collect();
    for part in s.split(|c: char| !c.is_ascii_digit() && c != '-') {
        let groups: Vec<&str> = part.split('-').collect();
        if groups.len() == 3 && groups[0].len() == 3 && groups[1].len() == 3 && groups[2].len() == 4
        {
            return true;
        }
    }
    false
}

#[test]
fn the_phone_shape_rule_can_fail() {
    // Assembled rather than written out: this file is scanned by the rule it
    // is testing, and even a fictional 555 number would trip it.
    let sample = format!("call {}-{}-{} now", "555", "867", "5309");
    assert!(has_phone_shape(&sample));

    assert!(!has_phone_shape("version 1.431.4"));
    assert!(!has_phone_shape("2026-08-27"));
    assert!(!has_phone_shape("00000000-0000-4000-8000-000000000001"));
}
