//! The Worker module (feature `wasm`), natively: each call goes through the
//! same ABI functions the WebAssembly exports call, with the test playing the
//! Worker (`pk_cli_mcp::wasm::testing`). Every request the module sends is
//! checked for its key headers and host, and Alpaca answers from the
//! synthetic fixtures.
#![cfg(feature = "wasm")]

use alp::worker::tools;
use pk_cli_core::transport::{HttpRequest, HttpResponse};
use pk_cli_mcp::wasm::testing::{call, describe, Asked, Reply};
use serde_json::{json, Value};

const KEY_ID: &str = "AKDUMMY000000000001";
const SECRET: &str = "DUMMYSECRET0001";

fn fixture(name: &str) -> String {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"))
}

fn ok(body: impl Into<String>) -> Reply {
    Reply::Response(HttpResponse::new(200, body))
}

fn keys() -> Value {
    json!({ "ALP_API_KEY_ID": KEY_ID, "ALP_API_SECRET_KEY": SECRET })
}

fn with(mut config: Value, key: &str, value: &str) -> Value {
    config[key] = json!(value);
    config
}

/// The request's path and its query parameters, after checking what every
/// request must carry: GET, both key headers, JSON, the family User-Agent.
fn checked(req: &HttpRequest, origin: &str) -> (String, Vec<(String, String)>) {
    assert_eq!(req.method, "GET");
    assert_eq!(req.header_value("APCA-API-KEY-ID"), Some(KEY_ID));
    assert_eq!(req.header_value("APCA-API-SECRET-KEY"), Some(SECRET));
    assert_eq!(req.header_value("Accept"), Some("application/json"));
    assert_eq!(
        req.header_value("User-Agent"),
        Some(concat!("alp/", env!("CARGO_PKG_VERSION")))
    );
    assert!(req.body.is_none());
    let rest = req
        .url
        .strip_prefix(origin)
        .unwrap_or_else(|| panic!("{} is not on {origin}", req.url));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    let params = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap();
            (k.to_string(), v.to_string())
        })
        .collect();
    (path.to_string(), params)
}

/// Alpaca at `origin`, answering from the fixtures.
fn alpaca(origin: &'static str) -> impl FnMut(Asked) -> Reply {
    move |asked| match asked {
        Asked::Fetch(req) => match checked(&req, origin).0.as_str() {
            "/v2/account" => ok(fixture("account.json")),
            "/v2/positions" => ok(fixture("positions.json")),
            "/v2/account/activities" => ok(fixture("activities.json")),
            other => panic!("unexpected path {other}"),
        },
        Asked::SignEs256 { .. } => panic!("Alpaca's keys are headers; nothing is signed"),
    }
}

fn no_request(asked: Asked) -> Reply {
    panic!("no request may be sent: {asked:?}")
}

fn text(out: &Value) -> &str {
    out["result"]["content"][0]["text"].as_str().unwrap()
}

#[test]
fn the_tools_are_described_for_a_worker_without_any_key() {
    let d = describe(tools);
    let names: Vec<&str> = d["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["alpaca_portfolio", "alpaca_activities"]);
    let instructions = d["instructions"].as_str().unwrap();
    assert!(instructions.contains("Cloudflare Worker"), "{instructions}");
    assert!(instructions.contains("read-only"), "{instructions}");
    assert!(!instructions.contains("auth login"), "{instructions}");
    assert_eq!(d["server"]["name"], "alp");
    assert_eq!(d["server"]["version"], env!("CARGO_PKG_VERSION"));
}

#[test]
fn the_portfolio_reads_the_account_and_positions_on_the_live_host() {
    let mut paths = Vec::new();
    let mut host = alpaca("https://api.alpaca.markets");
    let out = call(
        tools,
        json!({"name": "alpaca_portfolio", "arguments": {}}),
        keys(),
        |asked| {
            if let Asked::Fetch(req) = &asked {
                paths.push(checked(req, "https://api.alpaca.markets").0);
            }
            host(asked)
        },
    );
    let result = &out["result"];
    assert_eq!(result["isError"], false, "{out}");
    let s = &result["structuredContent"];
    assert_eq!(s["account"]["environment"], "live");
    assert_eq!(s["account"]["cash"], "1000.55");
    assert_eq!(s["items"].as_array().unwrap().len(), 2);
    assert_eq!(paths, ["/v2/account", "/v2/positions"]);
}

#[test]
fn paper_reads_the_paper_host_and_a_loopback_override_wins() {
    let out = call(
        tools,
        json!({"name": "alpaca_portfolio", "arguments": {}}),
        with(keys(), "ALP_ENV", "paper"),
        alpaca("https://paper-api.alpaca.markets"),
    );
    assert_eq!(out["result"]["isError"], false, "{out}");
    assert_eq!(
        out["result"]["structuredContent"]["account"]["environment"],
        "paper"
    );

    let out = call(
        tools,
        json!({"name": "alpaca_portfolio", "arguments": {}}),
        with(keys(), "ALP_BASE_URL", "http://127.0.0.1:8789/"),
        alpaca("http://127.0.0.1:8789"),
    );
    assert_eq!(out["result"]["isError"], false, "{out}");
}

/// The keys travel in plain headers, so a plain-HTTP host that is not this
/// machine is refused before any request.
#[test]
fn a_base_url_that_would_expose_the_keys_is_refused_before_any_request() {
    for url in [
        "http://api.alpaca.markets",
        "http://127.0.0.1:1@evil.example",
    ] {
        let out = call(
            tools,
            json!({"name": "alpaca_portfolio", "arguments": {}}),
            with(keys(), "ALP_BASE_URL", url),
            no_request,
        );
        assert_eq!(out["result"]["isError"], true, "{out}");
        assert!(text(&out).starts_with("auth: "), "{out}");
        assert!(text(&out).contains("ALP_BASE_URL"), "{out}");
    }
}

/// The activities walk: newest first, the full page's last id as the next
/// `page_token`, stopping at the short page. The window defaults to the year
/// to date by the Worker's clock.
#[test]
fn activities_page_by_token_and_filter_by_the_window() {
    let today = pk_cli_core::dates::today();
    let (year, date) = (today.0, pk_cli_core::dates::fmt_iso(today));
    let full: Vec<Value> = (0..100)
        .map(|i| {
            json!({
                "id": format!("{}{i:09}::00000000-0000-4000-8000-{i:012}", date.replace('-', "")),
                "activity_type": "INT",
                "date": date,
                "net_amount": "0.01",
            })
        })
        .collect();
    let last_id = full[99]["id"].as_str().unwrap().to_string();
    let older = json!([{
        "id": "old",
        "activity_type": "DIV",
        "date": format!("{}-12-31", year - 1),
        "net_amount": "1.00",
    }]);
    let mut seen = Vec::new();
    let out = call(
        tools,
        json!({"name": "alpaca_activities", "arguments": {"types": ["dividends"], "limit": 200}}),
        keys(),
        |asked| match asked {
            Asked::Fetch(req) => {
                let (path, params) = checked(&req, "https://api.alpaca.markets");
                assert_eq!(path, "/v2/account/activities");
                seen.push(params.clone());
                if params.iter().any(|(k, _)| k == "page_token") {
                    ok(older.to_string())
                } else {
                    ok(Value::Array(full.clone()).to_string())
                }
            }
            Asked::SignEs256 { .. } => unreachable!(),
        },
    );
    let result = &out["result"];
    assert_eq!(result["isError"], false, "{out}");
    let s = &result["structuredContent"];
    assert_eq!(s["since"], format!("{year}-01-01"));
    assert_eq!(
        s["items"].as_array().unwrap().len(),
        100,
        "last year's row is out"
    );
    assert_eq!(seen.len(), 2, "{seen:?}");
    let param = |p: &Vec<(String, String)>, k: &str| {
        p.iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    assert_eq!(param(&seen[0], "direction"), "desc");
    assert_eq!(param(&seen[0], "page_size"), "100");
    assert!(param(&seen[0], "activity_types").starts_with("DIV%2CDIVCGL"));
    assert_eq!(param(&seen[0], "page_token"), "");
    assert_eq!(
        param(&seen[1], "page_token"),
        pk_cli_core::transport::percent_encode(&last_id)
    );
}

/// One call reads at most `MAX_ACTIVITY_PAGES` pages, inside Cloudflare's
/// per-invocation request cap, and says what narrows the walk rather than
/// answering with part of the history.
#[test]
fn a_walk_past_the_page_budget_is_a_usage_error_after_exactly_the_budget() {
    let mut sent = 0;
    let out = call(
        tools,
        json!({"name": "alpaca_activities", "arguments": {"since": "2020-01-01"}}),
        keys(),
        |asked| {
            let Asked::Fetch(req) = asked else {
                unreachable!()
            };
            checked(&req, "https://api.alpaca.markets");
            sent += 1;
            let page: Vec<Value> = (0..100)
                .map(|i| json!({"id": format!("{sent}-{i}"), "activity_type": "INT", "date": "2026-03-01"}))
                .collect();
            ok(Value::Array(page).to_string())
        },
    );
    let t = text(&out);
    assert_eq!(sent, alp::worker::MAX_ACTIVITY_PAGES);
    assert!(sent < 50, "inside Cloudflare's free-plan cap");
    assert!(t.starts_with("usage: "), "{t}");
    assert!(t.contains("from today back to 2020-01-01"), "{t}");
    assert!(t.contains("rows after `until` count too"), "{t}");
    assert!(
        t.contains("`alp transactions list` has no such limit"),
        "{t}"
    );
}

/// An old window on a busy account fails loudly rather than reading less:
/// the walk sends no server-side date bound (it bounds creation time, and
/// would drop a late correction dated in the window), so years of newer rows
/// can use up the budget before the window is reached.
#[test]
fn an_old_window_past_the_budget_is_refused_not_cut_short() {
    let mut sent = 0;
    let out = call(
        tools,
        json!({"name": "alpaca_activities", "arguments": {"since": "2024-01-01", "until": "2024-12-31"}}),
        keys(),
        |asked| {
            let Asked::Fetch(req) = asked else {
                unreachable!()
            };
            let (_, params) = checked(&req, "https://api.alpaca.markets");
            for bound in ["until", "after", "date"] {
                assert!(params.iter().all(|(k, _)| k != bound), "{params:?}");
            }
            sent += 1;
            ok(Value::Array(
                (0..100)
                    .map(|i| json!({"id": format!("{sent}-{i}"), "activity_type": "INT", "date": "2026-03-01"}))
                    .collect(),
            )
            .to_string())
        },
    );
    let t = text(&out);
    assert_eq!(sent, alp::worker::MAX_ACTIVITY_PAGES);
    assert!(t.starts_with("usage: "), "{t}");
    assert!(t.contains("from today back to 2024-01-01"), "{t}");
    assert!(out["result"].get("structuredContent").is_none(), "{out}");
}

#[test]
fn activities_over_the_fixtures_newest_first() {
    let out = call(
        tools,
        json!({"name": "alpaca_activities", "arguments": {"since": "2026-01-01", "until": "2026-12-31"}}),
        keys(),
        alpaca("https://api.alpaca.markets"),
    );
    assert_eq!(out["result"]["isError"], false, "{out}");
    let dates: Vec<&str> = out["result"]["structuredContent"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["date"].as_str().unwrap())
        .collect();
    let mut sorted = dates.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(dates, sorted);
    assert_eq!(dates.len(), 6);
}

#[test]
fn without_keys_each_call_names_the_worker_secrets_and_sends_nothing() {
    for (config, missing) in [
        (
            json!({}),
            "ALP_API_KEY_ID and ALP_API_SECRET_KEY secrets are not set",
        ),
        (
            json!({"ALP_API_KEY_ID": KEY_ID}),
            "ALP_API_KEY_ID is set but ALP_API_SECRET_KEY is not",
        ),
        (
            json!({"ALP_API_KEY_ID": "  ", "ALP_API_SECRET_KEY": SECRET}),
            "ALP_API_SECRET_KEY is set but ALP_API_KEY_ID is not",
        ),
    ] {
        let out = call(
            tools,
            json!({"name": "alpaca_portfolio", "arguments": {}}),
            config,
            no_request,
        );
        let t = text(&out);
        assert_eq!(out["result"]["isError"], true, "{out}");
        assert!(t.starts_with("auth: "), "{t}");
        assert!(t.contains(missing), "{t}");
        assert!(t.contains("Cloudflare Worker's secrets"), "{t}");
        assert!(!t.contains("alp auth login"), "{t}");
    }
}

#[test]
fn a_bad_environment_is_an_auth_error_before_any_request() {
    let out = call(
        tools,
        json!({"name": "alpaca_portfolio", "arguments": {}}),
        with(keys(), "ALP_ENV", "prod"),
        no_request,
    );
    assert!(text(&out).contains("ALP_ENV is `prod`"), "{out}");
}

#[test]
fn rejected_keys_say_to_update_the_worker_secrets() {
    let out = call(
        tools,
        json!({"name": "alpaca_portfolio", "arguments": {}}),
        keys(),
        |_| {
            Reply::Response(HttpResponse::new(
                401,
                r#"{"code": 40110000, "message": "request is not authorized"}"#,
            ))
        },
    );
    let t = text(&out);
    assert!(
        t.starts_with("auth: Alpaca rejected the API keys (HTTP 401: request is not authorized)"),
        "{t}"
    );
    assert!(t.contains("Cloudflare Worker's secrets"), "{t}");
    assert!(!t.contains("alp "), "no CLI command in the Worker: {t}");
}

#[test]
fn a_fetch_with_no_answer_is_an_upstream_error() {
    let out = call(
        tools,
        json!({"name": "alpaca_activities", "arguments": {}}),
        keys(),
        |_| Reply::Failed("TypeError: Network connection lost".into()),
    );
    let t = text(&out);
    assert!(t.starts_with("upstream: "), "{t}");
    assert!(!t.contains(SECRET), "{t}");
}

#[test]
fn bad_arguments_fail_before_any_request() {
    let out = call(
        tools,
        json!({"name": "alpaca_activities", "arguments": {"since": "yesterday"}}),
        keys(),
        no_request,
    );
    assert!(text(&out).starts_with("usage: "), "{out}");
    let out = call(
        tools,
        json!({"name": "alpaca_nope", "arguments": {}}),
        keys(),
        no_request,
    );
    assert_eq!(out["error"]["code"], -32602, "{out}");
}
