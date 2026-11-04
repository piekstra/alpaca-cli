//! The `alp mcp` tools, offline: every tool runs in process against the
//! synthetic fixtures through the `Source` seam, and cli-common's conformance
//! helper checks the definitions and every result's shape and size.

use std::sync::Mutex;

use alp::config::Env;
use alp::mcp::{AlpacaTools, Source, ACTIVITIES_TOOL, PORTFOLIO_TOOL};
use pk_cli_core::CliError;
use pk_cli_mcp::conformance::{assert_conforms, assert_fits, call, definition_size};
use pk_cli_mcp::CallToolResult;
use serde_json::{json, Value};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {path}: {e}"));
    serde_json::from_str(&raw).unwrap()
}

fn rows(name: &str) -> Vec<Value> {
    fixture(name).as_array().cloned().unwrap()
}

struct Fixtures {
    account: Value,
    positions: Vec<Value>,
    activities: Vec<Value>,
    /// The `(types, since)` of every activities fetch.
    fetches: Mutex<Vec<(Option<String>, String)>>,
    /// When set, every call fails with this.
    fail: Mutex<Option<CliError>>,
}

impl Fixtures {
    fn new() -> Self {
        Self {
            account: fixture("account.json"),
            positions: rows("positions.json"),
            activities: rows("activities.json"),
            fetches: Mutex::new(Vec::new()),
            fail: Mutex::new(None),
        }
    }

    fn with_activities(mut self, rows: Vec<Value>) -> Self {
        self.activities = rows;
        self
    }

    fn fetches(&self) -> Vec<(Option<String>, String)> {
        self.fetches.lock().unwrap().clone()
    }

    fn failure(&self) -> Result<(), CliError> {
        match self.fail.lock().unwrap().as_ref() {
            None => Ok(()),
            Some(CliError::Auth(m)) => Err(CliError::Auth(m.clone())),
            Some(CliError::Upstream(m)) => Err(CliError::Upstream(m.clone())),
            Some(other) => Err(CliError::Other(other.to_string())),
        }
    }
}

impl Source for Fixtures {
    async fn account(&self) -> Result<Value, CliError> {
        self.failure()?;
        Ok(self.account.clone())
    }

    async fn positions(&self) -> Result<Vec<Value>, CliError> {
        self.failure()?;
        Ok(self.positions.clone())
    }

    /// As the live walk does, the server-side type filter applies here.
    async fn activities(&self, types: Option<&str>, since: &str) -> Result<Vec<Value>, CliError> {
        self.fetches
            .lock()
            .unwrap()
            .push((types.map(String::from), since.to_string()));
        self.failure()?;
        let wanted: Option<Vec<&str>> = types.map(|t| t.split(',').collect());
        Ok(self
            .activities
            .iter()
            .filter(|r| {
                wanted
                    .as_ref()
                    .is_none_or(|w| w.contains(&r["activity_type"].as_str().unwrap_or_default()))
            })
            .cloned()
            .collect())
    }
}

fn tools() -> AlpacaTools<Fixtures> {
    AlpacaTools::new(Fixtures::new(), Env::Live)
}

fn err(r: &CallToolResult) -> &str {
    assert!(r.is_error(), "expected an error, got {}", r.text());
    assert!(r.structured().is_none());
    r.text()
}

fn field(items: &Value, key: &str) -> Vec<String> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i[key].as_str().unwrap_or("-").to_string())
        .collect()
}

// ---- the definitions --------------------------------------------------------

#[test]
fn the_tools_conform_to_the_mcp_profile() {
    assert_conforms(&tools());
}

#[test]
fn the_tools_are_reads_and_stay_small() {
    let t = tools();
    let list = pk_cli_mcp::tools_list_json(&t);
    for tool in list["tools"].as_array().unwrap() {
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{tool}");
        assert_eq!(tool["annotations"]["destructiveHint"], false, "{tool}");
        assert!(tool.get("_meta").is_none(), "no write markers on a read");
    }
    let portfolio = definition_size(&t, PORTFOLIO_TOOL);
    let activities = definition_size(&t, ACTIVITIES_TOOL);
    eprintln!("definition sizes: {PORTFOLIO_TOOL} {portfolio}, {ACTIVITIES_TOOL} {activities}");
    assert!(portfolio < 3_000, "{portfolio}");
    assert!(activities < 3_000, "{activities}");
}

// ---- alpaca_portfolio -------------------------------------------------------

#[test]
fn portfolio_concise_is_balances_and_positions_without_ids() {
    let t = tools();
    let r = call(&t, PORTFOLIO_TOOL, json!({}));
    let out = assert_fits(&t, PORTFOLIO_TOOL, &r);
    let account = &out["account"];
    assert_eq!(account["environment"], "live");
    assert_eq!(account["currency"], "USD");
    assert_eq!(account["equity"], "4300.55", "the wire's string, untouched");
    assert_eq!(account["short_market_value"], "0");
    assert!(account.get("account_number").is_none(), "concise: no ids");
    assert_eq!(out["returned"], 2);
    assert_eq!(field(&out["items"], "symbol"), ["SMPL", "ETFX"]);
    let smpl = &out["items"][0];
    assert_eq!(smpl["quantity"], "10.5");
    assert_eq!(smpl["market_value"], "1103.55");
    assert_eq!(smpl["average_entry_price"], "100.25");
    assert!(smpl.get("side").is_none() && smpl.get("asset_class").is_none());
    assert_eq!(out["items"][1]["quantity"], "-5", "a short is negative");
    assert!(out.get("next_cursor").is_none());
}

#[test]
fn portfolio_detailed_adds_the_account_number_and_position_detail() {
    let t = tools();
    let r = call(
        &t,
        PORTFOLIO_TOOL,
        json!({"response_format": "detailed", "symbol": "etfx"}),
    );
    let out = assert_fits(&t, PORTFOLIO_TOOL, &r);
    assert_eq!(out["account"]["account_number"], "111111111");
    assert_eq!(out["account"]["pattern_day_trader"], false);
    assert_eq!(out["returned"], 1);
    let etfx = &out["items"][0];
    assert_eq!(etfx["symbol"], "ETFX", "symbol matches case-blind");
    assert_eq!(etfx["side"], "short");
    assert_eq!(etfx["asset_class"], "us_equity");
}

#[test]
fn the_environment_is_the_one_the_server_reads() {
    let t = AlpacaTools::new(Fixtures::new(), Env::Paper);
    let r = call(&t, PORTFOLIO_TOOL, json!({"limit": 1}));
    let out = assert_fits(&t, PORTFOLIO_TOOL, &r);
    assert_eq!(out["account"]["environment"], "paper");
    let cursor = out["next_cursor"].as_str().unwrap();
    let r = call(&t, PORTFOLIO_TOOL, json!({"limit": 1, "cursor": cursor}));
    let out = assert_fits(&t, PORTFOLIO_TOOL, &r);
    assert_eq!(field(&out["items"], "symbol"), ["ETFX"]);
    assert!(out.get("next_cursor").is_none());
}

/// `alp positions list` warns on stderr and goes on; a model sees no
/// stderr, so a row it cannot read fails the call instead of quietly
/// shortening the portfolio.
#[test]
fn a_position_row_without_a_symbol_fails_the_call_rather_than_vanishing() {
    let mut fixtures = Fixtures::new();
    fixtures
        .positions
        .push(json!({"qty": "1", "market_value": "1.00"}));
    let t = AlpacaTools::new(fixtures, Env::Live);
    let text = err(&call(&t, PORTFOLIO_TOOL, json!({}))).to_string();
    assert!(text.starts_with("upstream:"), "{text}");
    assert!(text.contains("1 of 3 position rows"), "{text}");
}

// ---- alpaca_activities ------------------------------------------------------

#[test]
fn activities_concise_over_a_year_newest_first() {
    let t = tools();
    let r = call(
        &t,
        ACTIVITIES_TOOL,
        json!({"since": "2026-01-01", "until": "2026-12-31"}),
    );
    let out = assert_fits(&t, ACTIVITIES_TOOL, &r);
    assert_eq!(out["since"], "2026-01-01");
    assert_eq!(out["until"], "2026-12-31");
    assert_eq!(out["returned"], 6);
    assert_eq!(
        field(&out["items"], "type"),
        ["FILL", "DIV", "DIVNRA", "INT", "FILL", "FEE"]
    );
    let sell = &out["items"][0];
    assert_eq!(sell["date"], "2026-08-14");
    assert_eq!(sell["side"], "sell");
    assert_eq!(sell["price"], "105.1000", "the wire's precision");
    assert!(sell.get("id").is_none() && sell.get("order_id").is_none());
    let withheld = &out["items"][2];
    assert_eq!(withheld["net_amount"], "-1.85");
    assert_eq!(
        withheld["description"],
        "REV: Dividend adjusted (NRA Withheld)"
    );
    assert!(withheld.get("status").is_none(), "executed is implied");
    // No type filter: the whole feed back past since.
    assert_eq!(t.source().fetches(), [(None, "2026-01-01".to_string())]);
}

#[test]
fn dividends_stands_for_every_dividend_and_interest_type() {
    let t = tools();
    let r = call(
        &t,
        ACTIVITIES_TOOL,
        json!({"types": ["dividends"], "since": "2026-01-01", "until": "2026-12-31"}),
    );
    let out = assert_fits(&t, ACTIVITIES_TOOL, &r);
    assert_eq!(field(&out["items"], "type"), ["DIV", "DIVNRA", "INT"]);
    let (types, _) = &t.source().fetches()[0];
    let types = types.as_deref().unwrap();
    assert!(
        types.starts_with("DIV,") && types.contains("INTNRA"),
        "{types}"
    );
}

/// A page boundary falls inside a day, and a row posts before the next page. A cursor holds no state
/// at Alpaca, so a second set of fixtures stands for the later read.
#[test]
fn an_activity_that_posts_between_pages_repeats_nothing_and_skips_nothing() {
    let row = |id: &str, date: &str| {
        json!({"id": id, "activity_type": "DIV", "date": date, "net_amount": "1.00",
               "description": "Sample"})
    };
    let before = vec![
        row("d", "2026-03-04"),
        row("c", "2026-03-03"),
        row("b", "2026-03-03"),
        row("a", "2026-03-01"),
    ];
    let mut after = vec![row("e", "2026-03-05")];
    after.extend(before.clone());
    let args = json!({"since": "2026-01-01", "until": "2026-12-31", "limit": 2,
                      "response_format": "detailed"});
    let ids = |page: &Value| -> Vec<String> {
        page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    let with_rows =
        |rows: Vec<Value>| AlpacaTools::new(Fixtures::new().with_activities(rows), Env::Live);

    let t = with_rows(before);
    let answer = call(&t, ACTIVITIES_TOOL, args.clone());
    let first = assert_fits(&t, ACTIVITIES_TOOL, &answer);
    let mut seen = ids(first);
    assert_eq!(seen[0], "d");
    let later = with_rows(after);
    let mut cursor = first["next_cursor"].clone();
    while cursor.is_string() {
        let mut next = args.clone();
        next["cursor"] = cursor;
        let answer = call(&later, ACTIVITIES_TOOL, next);
        let page = assert_fits(&later, ACTIVITIES_TOOL, &answer);
        seen.extend(ids(page));
        cursor = page["next_cursor"].clone();
    }
    let mut sorted = seen.clone();
    sorted.sort();
    assert_eq!(sorted, ["a", "b", "c", "d"], "{seen:?}");
}

#[test]
fn activities_narrow_by_type_symbol_and_dates_inclusive() {
    let t = tools();
    let r = call(
        &t,
        ACTIVITIES_TOOL,
        json!({"types": ["fill"], "symbol": "smpl", "since": "2026-03-14", "until": "2026-08-14", "response_format": "detailed"}),
    );
    let out = assert_fits(&t, ACTIVITIES_TOOL, &r);
    assert_eq!(field(&out["items"], "date"), ["2026-08-14", "2026-03-14"]);
    let partial = &out["items"][1];
    assert_eq!(partial["fill_type"], "partial_fill");
    assert_eq!(partial["leaves_qty"], "4");
    assert!(partial["order_id"]
        .as_str()
        .unwrap()
        .starts_with("00000000-"));
    assert_eq!(
        t.source().fetches(),
        [(Some("FILL".to_string()), "2026-03-14".to_string())]
    );
}

#[test]
fn the_default_window_is_the_year_to_date() {
    let t = tools();
    let r = call(&t, ACTIVITIES_TOOL, json!({"until": "2026-06-30"}));
    let out = assert_fits(&t, ACTIVITIES_TOOL, &r);
    assert_eq!(out["since"], "2026-01-01");
    assert_eq!(
        field(&out["items"], "date"),
        [
            "2026-06-30",
            "2026-06-30",
            "2026-04-01",
            "2026-03-14",
            "2026-01-31"
        ]
    );
}

#[test]
fn a_page_of_long_descriptions_stops_under_the_budget_and_says_how_to_narrow() {
    let template = rows("activities.json")[3].clone();
    let many: Vec<Value> = (0..200)
        .map(|i| {
            let mut row = template.clone();
            row["id"] = json!(format!(
                "20260401000000000::00000000-0000-4000-8000-{i:012}"
            ));
            row["description"] = json!(format!("Sample note {i} {}", "x".repeat(400)));
            row
        })
        .collect();
    let t = AlpacaTools::new(Fixtures::new().with_activities(many), Env::Live);
    let r = call(
        &t,
        ACTIVITIES_TOOL,
        json!({"since": "2026-01-01", "until": "2026-12-31", "limit": 200}),
    );
    let out = assert_fits(&t, ACTIVITIES_TOOL, &r);
    assert_eq!(out["truncated"], true);
    let returned = out["returned"].as_u64().unwrap();
    assert!((10..200).contains(&returned), "{returned}");
    let hint = out["hint"].as_str().unwrap();
    assert!(
        hint.contains("since/until, types or symbol") && hint.contains("next_cursor"),
        "{hint}"
    );
}

/// The size a model pays for one ordinary call.
#[test]
fn concise_results_are_small() {
    let t = tools();
    for (name, args) in [
        (PORTFOLIO_TOOL, json!({})),
        (
            ACTIVITIES_TOOL,
            json!({"since": "2026-01-01", "until": "2026-12-31"}),
        ),
    ] {
        let r = call(&t, name, args);
        let size = r.text().chars().count();
        eprintln!("concise {name}: {size} characters");
        assert!(size < 2_000, "{name}: {size}");
    }
}

// ---- errors: actionable, and before any request -------------------------------

#[test]
fn bad_arguments_fail_before_any_request() {
    let t = tools();
    for (args, needle) in [
        (json!({"since": "03/01/2026"}), "since must be a date"),
        (
            json!({"since": "2026-03-01", "until": "2026-02-01"}),
            "is after until",
        ),
        (json!({"types": ["FILL,DIV"]}), "letters and digits"),
        (json!({"limit": 0}), "limit must be between"),
        (json!({"symbl": "SMPL"}), "unknown argument `symbl`"),
        (json!({"cursor": "page-2"}), "next_cursor exactly"),
    ] {
        let text = err(&call(&t, ACTIVITIES_TOOL, args.clone())).to_string();
        assert!(text.starts_with("usage:"), "{args}: {text}");
        assert!(text.contains(needle), "{args}: {text}");
    }
    assert!(t.source().fetches().is_empty());
}

#[test]
fn a_missing_key_tells_the_model_to_have_the_user_sign_in_in_a_terminal() {
    let t = tools();
    *t.source().fail.lock().unwrap() = Some(alp::credential::missing(Env::Live));
    let text = err(&call(&t, PORTFOLIO_TOOL, json!({}))).to_string();
    assert!(text.starts_with("auth:"), "{text}");
    assert!(
        text.contains("alp auth login") && text.contains("in a terminal"),
        "{text}"
    );
    assert!(text.contains("ALP_API_KEY_ID"), "{text}");
    assert!(text.contains("never sign in"), "{text}");
}

#[test]
fn an_upstream_failure_says_the_arguments_are_fine() {
    let t = tools();
    *t.source().fail.lock().unwrap() = Some(CliError::Upstream(
        "rate limited by Alpaca (HTTP 429)".into(),
    ));
    let text = err(&call(&t, ACTIVITIES_TOOL, json!({}))).to_string();
    assert!(text.starts_with("upstream:"), "{text}");
    assert!(text.contains("arguments are not the problem"), "{text}");
}
