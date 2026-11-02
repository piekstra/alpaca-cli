//! The Trading API reads the commands and the MCP tools share: the account,
//! the open positions and the activities feed, and the mapping and filtering
//! over their rows.
//!
//! Clap-free and written against [`Alpaca`] over any transport, so the same
//! paging and filtering run in the terminal binary and in the Worker module
//! ([`crate::worker`]). The commands drive these futures with
//! `pk_cli_core::block_on`; natively they finish on their first poll.
//!
//! # Why paging ignores the server's date parameters
//!
//! The activities endpoint's `after`/`until` filters are documented against
//! the activity's *creation* datetime with exclusive-bound phrasing
//! ("created before/after this date"). Being wrong about a boundary there
//! silently drops a row at the edge of a tax year, and a dropped row looks
//! exactly like a complete list. So the client pages newest-first with no
//! server-side date bounds and applies the **inclusive** `[since, until]`
//! filter itself, where it can be tested offline. Paging stops once a page's
//! oldest dated row falls two calendar months before `since` — a grace window
//! so an out-of-order row (a correction processed late) cannot end the walk
//! early — or when a short page says the history is exhausted.

use pk_cli_core::transport::Transport;
use pk_cli_core::{CliError, Money};
use serde::Serialize;
use serde_json::Value;

use crate::client::Alpaca;

/// The trading account (`GET`).
pub const ACCOUNT: &str = "/v2/account";
/// Every open position (`GET`).
pub const POSITIONS: &str = "/v2/positions";
/// The account-activities feed (`GET`).
pub const ACTIVITIES: &str = "/v2/account/activities";

/// The activities feed's page ceiling (docs.alpaca.markets, 2026-08-27):
/// without a `date` parameter, "the default and maximum value is 100".
pub const PAGE_SIZE: usize = 100;

/// The dividend/interest activity types (docs/api.md §3.3): every `DIV*`
/// income/withholding/fee variant plus the `INT*` interest variants. This is
/// the `alp dividends list` filter — the 1099-DIV/INT raw material.
pub const DIVIDEND_INTEREST_TYPES: &[&str] = &[
    "DIV", "DIVCGL", "DIVCGS", "DIVFEE", "DIVFT", "DIVNRA", "DIVROC", "DIVTW", "DIVTXEX", "INT",
    "INTNRA", "INTTW",
];

/// A string field, `None` when absent or not a string.
pub fn str_at(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Inclusive ISO-date range check. ISO `YYYY-MM-DD` strings order
/// lexicographically, so plain string comparison is exact.
pub fn date_within(date: &str, since: Option<&str>, until: Option<&str>) -> bool {
    if let Some(s) = since {
        if date < s {
            return false;
        }
    }
    if let Some(u) = until {
        if date > u {
            return false;
        }
    }
    true
}

// ---- account ----------------------------------------------------------------

/// The account object, as the wire carried it.
pub async fn account<T: Transport>(api: &Alpaca<T>) -> Result<Value, CliError> {
    api.get(ACCOUNT, &[]).await
}

// ---- positions --------------------------------------------------------------

/// One holding (`position/v1`).
///
/// The row shape leads with `symbol`/`quantity`/`market_value`, matching how
/// the sibling brokerage CLIs shape holdings (there is no ratified trading
/// profile yet — PROFILES.md's bar isn't met — so this matches `rhd`'s
/// `position/v1` spirit rather than a shared crate). Quantities and money
/// amounts are the wire's string decimals, untouched.
#[derive(Debug, Serialize)]
pub struct Position {
    pub symbol: String,
    /// The wire's `qty`, verbatim (fractional shares arrive as decimals).
    pub quantity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_value: Option<Money>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_entry_price: Option<Money>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_price: Option<Money>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unrealized_pl: Option<Money>,
    /// `long` | `short`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    /// `us_equity` | `us_option` | `crypto` — worth surfacing because the
    /// account can hold all three and their tax treatment differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange: Option<String>,
    /// Shares not tied up in open orders (`qty_available`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity_available: Option<String>,
}

impl Position {
    /// `None` when the row carries no symbol or quantity — a shape change so
    /// fundamental that surfacing a broken row would be worse than counting
    /// it missing (the fixture tests pin the real shape).
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(Position {
            symbol: str_at(v, "symbol")?,
            quantity: str_at(v, "qty")?,
            market_value: money_at(v, "market_value"),
            average_entry_price: money_at(v, "avg_entry_price"),
            current_price: money_at(v, "current_price"),
            unrealized_pl: money_at(v, "unrealized_pl"),
            side: str_at(v, "side"),
            asset_class: str_at(v, "asset_class"),
            exchange: str_at(v, "exchange"),
            quantity_available: str_at(v, "qty_available"),
        })
    }
}

/// A position money field: the wire's string decimal, wrapped as USD. The
/// Trading API reports position values in the account currency, which for
/// every account this tool targets is USD; the amount string itself is never
/// altered.
fn money_at(v: &Value, key: &str) -> Option<Money> {
    str_at(v, key).map(Money::usd)
}

/// Every position row, as the wire carried it.
pub async fn positions<T: Transport>(api: &Alpaca<T>) -> Result<Vec<Value>, CliError> {
    match api.get(POSITIONS, &[]).await? {
        Value::Array(rows) => Ok(rows),
        _ => Err(CliError::Upstream(
            "expected a JSON array of positions — the response shape has changed; \
             see docs/api.md"
                .into(),
        )),
    }
}

// ---- activities -------------------------------------------------------------

/// Page the activities feed newest-first until it is exhausted or safely past
/// `since`; only `types` (a comma list) when given. Rows come back exactly as
/// the wire carried them.
///
/// `max_pages` bounds the walk for a host that caps how many requests one
/// call may send (the Worker); a walk that would need more is a usage error
/// naming what narrows it, never a silently partial history. The commands
/// pass `None`: an export must be complete. A bounded walk still starts at
/// the newest row: a server-side `until` would save pages on an old window,
/// but it bounds creation time, so it would silently drop a row dated in the
/// window and created after it (a late correction).
pub async fn activities<T: Transport>(
    api: &Alpaca<T>,
    types: Option<&str>,
    since: &str,
    max_pages: Option<usize>,
) -> Result<Vec<Value>, CliError> {
    let mut rows: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for pages in 1.. {
        let mut query: Vec<(&str, String)> = vec![
            ("direction", "desc".to_string()),
            ("page_size", PAGE_SIZE.to_string()),
        ];
        if let Some(t) = types {
            query.push(("activity_types", t.to_string()));
        }
        if let Some(tok) = &token {
            query.push(("page_token", tok.clone()));
        }

        let page = match api.get(ACTIVITIES, &query).await? {
            Value::Array(page) => page,
            // An empty window answers with an empty array; anything else is a
            // shape change worth failing loudly on rather than silently
            // treating as "no activity".
            other => {
                return Err(CliError::Upstream(format!(
                    "expected a JSON array of activities, got {} — the response shape has \
                     changed; see docs/api.md",
                    kind_of(&other)
                )))
            }
        };

        let n = page.len();
        let oldest_dated = page.iter().rev().find_map(activity_date);
        let last_id = page.last().and_then(|r| str_at(r, "id"));
        rows.extend(page);

        if page_walk_done(n, oldest_dated.as_deref(), since) {
            break;
        }
        if max_pages.is_some_and(|max| pages >= max) {
            return Err(CliError::Usage(format!(
                "Alpaca's activity from today back to {since} is more than {pages} pages of \
                 {PAGE_SIZE} rows, more than one call may read here (the feed is read from the \
                 newest row, so rows after `until` count too); a later `since` or a `types` \
                 filter shortens it, and `alp transactions list` has no such limit"
            )));
        }
        match last_id {
            // A repeated or missing token would loop forever; refuse instead.
            Some(id) if token.as_deref() != Some(id.as_str()) => token = Some(id),
            _ => break,
        }
    }
    Ok(rows)
}

/// Whether the newest-first walk can stop: the history is exhausted (short
/// page), or the page's oldest dated row is at least two calendar months
/// older than `since` — far enough that an out-of-order row can't be hiding
/// in-range activity beyond it.
fn page_walk_done(page_len: usize, oldest_dated: Option<&str>, since: &str) -> bool {
    if page_len < PAGE_SIZE {
        return true;
    }
    match oldest_dated {
        Some(oldest) => months_between(oldest, since) >= 2,
        // A full page of undated rows: keep walking; the client filter keeps
        // undated rows anyway, and stopping here would be a guess.
        None => false,
    }
}

/// How many calendar months `later` is after `earlier` (negative when it is
/// before). Pure component math on ISO strings — no calendar arithmetic.
fn months_between(earlier: &str, later: &str) -> i32 {
    fn ym(d: &str) -> Option<i32> {
        let y: i32 = d.get(0..4)?.parse().ok()?;
        let m: i32 = d.get(5..7)?.parse().ok()?;
        Some(y * 12 + m)
    }
    match (ym(earlier), ym(later)) {
        (Some(e), Some(l)) => l - e,
        _ => 0,
    }
}

/// Filter to the inclusive range, sort newest-first, apply `limit`.
///
/// Rows with no readable date are kept: dropping them would silently lose
/// activity, which is the worse failure for tax work. Sorting is by activity
/// date so `--limit N` means "the N most recent" rather than trusting the
/// server's ordering.
pub fn keep_sorted(rows: Vec<Value>, since: &str, until: &str, limit: Option<u32>) -> Vec<Value> {
    let mut kept: Vec<Value> = rows
        .into_iter()
        .filter(|r| match activity_date(r) {
            Some(d) => date_within(&d, Some(since), Some(until)),
            // Undated rows are kept rather than silently dropped.
            None => true,
        })
        .collect();
    kept.sort_by_key(|r| std::cmp::Reverse(activity_date(r)));
    if let Some(n) = limit {
        kept.truncate(n as usize);
    }
    kept
}

/// The date an activity belongs to, for filtering and sorting only (the DTO
/// carries the wire's own fields untouched). Trade activities carry
/// `transaction_time` (RFC 3339); non-trade activities carry `date`;
/// `created_at` is the last resort.
pub fn activity_date(row: &Value) -> Option<String> {
    for key in ["transaction_time", "date", "created_at"] {
        if let Some(raw) = str_at(row, key) {
            let d: String = raw.chars().take(10).collect();
            if d.len() == 10 {
                return Some(d);
            }
        }
    }
    None
}

fn kind_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::ApiKeys;
    use serde_json::json;

    #[test]
    fn range_check_is_inclusive_on_both_ends() {
        assert!(date_within(
            "2026-03-14",
            Some("2026-03-14"),
            Some("2026-03-14")
        ));
        assert!(date_within("2026-03-14", None, None));
        assert!(!date_within("2026-03-13", Some("2026-03-14"), None));
        assert!(!date_within("2026-03-15", None, Some("2026-03-14")));
    }

    fn position() -> Value {
        json!({
            "asset_id": "00000000-0000-4000-8000-000000000020",
            "symbol": "SMPL",
            "exchange": "NYSE",
            "asset_class": "us_equity",
            "qty": "10.5",
            "qty_available": "10.5",
            "avg_entry_price": "100.25",
            "side": "long",
            "market_value": "1103.55",
            "current_price": "105.10",
            "unrealized_pl": "50.93"
        })
    }

    #[test]
    fn position_rows_lead_with_symbol_quantity_market_value() {
        let p = Position::from_json(&position()).unwrap();
        let v = serde_json::to_value(&p).unwrap();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(&keys[..3], &["symbol", "quantity", "market_value"]);
        assert_eq!(v["quantity"], "10.5");
        assert_eq!(v["market_value"]["amount"], "1103.55");
        assert_eq!(v["average_entry_price"]["amount"], "100.25");
    }

    #[test]
    fn absent_position_fields_are_omitted_not_null() {
        let p = Position::from_json(&json!({"symbol": "SMPL", "qty": "1"})).unwrap();
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("market_value").is_none());
        assert!(v.get("side").is_none());
    }

    #[test]
    fn a_position_row_without_symbol_or_qty_is_unmappable() {
        assert!(Position::from_json(&json!({"qty": "1"})).is_none());
        assert!(Position::from_json(&json!({"symbol": "SMPL"})).is_none());
    }

    fn fill() -> Value {
        json!({
            "id": "20260314000000000::00000000-0000-4000-8000-000000000001",
            "activity_type": "FILL",
            "transaction_time": "2026-03-14T13:31:02.000Z",
            "type": "fill",
            "price": "150.0000",
            "qty": "10",
            "side": "buy",
            "symbol": "SMPL",
            "leaves_qty": "0",
            "order_id": "00000000-0000-4000-8000-000000000002",
            "cum_qty": "10",
            "order_status": "filled"
        })
    }

    fn div() -> Value {
        json!({
            "id": "20260630000000000::00000000-0000-4000-8000-000000000003",
            "activity_type": "DIV",
            "date": "2026-06-30",
            "net_amount": "12.34",
            "symbol": "ETFX",
            "qty": "25",
            "per_share_amount": "0.4936",
            "status": "executed"
        })
    }

    #[test]
    fn trade_rows_date_by_transaction_time_and_non_trade_by_date() {
        assert_eq!(activity_date(&fill()).as_deref(), Some("2026-03-14"));
        assert_eq!(activity_date(&div()).as_deref(), Some("2026-06-30"));
        assert_eq!(activity_date(&json!({"id": "x"})), None);
    }

    /// The whole point of this feed: rows in equal rows out, byte for byte.
    #[test]
    fn kept_rows_are_verbatim() {
        let kept = keep_sorted(vec![fill(), div()], "2026-01-01", "2026-12-31", None);
        assert_eq!(kept.len(), 2);
        // Newest first…
        assert_eq!(kept[0], div());
        // …and completely untouched: same keys, same string decimals.
        assert_eq!(kept[1], fill());
        assert_eq!(kept[1]["price"], "150.0000");
    }

    #[test]
    fn filtering_is_inclusive_on_both_ends() {
        let rows = || vec![fill()];
        assert_eq!(
            keep_sorted(rows(), "2026-03-14", "2026-03-14", None).len(),
            1
        );
        assert_eq!(
            keep_sorted(rows(), "2026-03-15", "2026-12-31", None).len(),
            0
        );
        assert_eq!(
            keep_sorted(rows(), "2026-01-01", "2026-03-13", None).len(),
            0
        );
    }

    /// Losing a row silently is the worst outcome for tax work, so a row we
    /// can't date is kept rather than filtered away.
    #[test]
    fn undated_rows_survive_filtering() {
        let kept = keep_sorted(
            vec![json!({"id": "mystery"})],
            "2026-01-01",
            "2026-12-31",
            None,
        );
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn limit_takes_the_newest() {
        let kept = keep_sorted(vec![fill(), div()], "2026-01-01", "2026-12-31", Some(1));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0]["activity_type"], "DIV");
    }

    #[test]
    fn the_walk_stops_on_a_short_page_or_two_months_past_since() {
        // Short page: history exhausted.
        assert!(page_walk_done(3, Some("2026-08-01"), "2026-01-01"));
        // Full page still inside the grace window: keep walking.
        assert!(!page_walk_done(100, Some("2025-12-15"), "2026-01-01"));
        // Full page two calendar months before since: safe to stop.
        assert!(page_walk_done(100, Some("2025-11-30"), "2026-01-01"));
        // Full page, no dated rows: never guess.
        assert!(!page_walk_done(100, None, "2026-01-01"));
    }

    /// Answers every request with a full page dated `since`, so only the
    /// budget can end the walk.
    struct Endless(std::cell::Cell<usize>);

    impl Transport for Endless {
        async fn send(
            &self,
            _: pk_cli_core::transport::HttpRequest,
        ) -> Result<pk_cli_core::transport::HttpResponse, CliError> {
            let n = self.0.get();
            self.0.set(n + 1);
            let page: Vec<Value> = (0..PAGE_SIZE)
                .map(|i| json!({"id": format!("{n}-{i}"), "date": "2026-03-01"}))
                .collect();
            Ok(pk_cli_core::transport::HttpResponse::new(
                200,
                Value::Array(page).to_string(),
            ))
        }
    }

    fn endless() -> Alpaca<Endless> {
        let keys = ApiKeys {
            key_id: "AKDUMMY000000000001".into(),
            secret_key: "DUMMYSECRET0001".into(),
        };
        Alpaca::with_transport(
            "https://api.alpaca.markets",
            keys,
            Endless(Default::default()),
        )
    }

    #[test]
    fn a_page_budget_ends_the_walk_with_a_usage_error_not_a_partial_history() {
        let api = endless();
        let e = pk_cli_core::block_on(activities(&api, None, "2026-01-01", Some(3))).unwrap_err();
        assert_eq!(e.exit_code(), 2, "{e}");
        assert!(
            e.to_string().contains("more than 3 pages of 100 rows"),
            "{e}"
        );
        assert!(e.to_string().contains("later `since`"), "{e}");

        // Unbounded, the same walk runs until the history ends (here, until
        // a page reaches two months before `since`).
        let api = endless();
        let rows = pk_cli_core::block_on(activities(&api, None, "2026-05-01", None)).unwrap();
        assert_eq!(rows.len(), PAGE_SIZE);
    }

    #[test]
    fn months_between_is_signed_component_math() {
        assert_eq!(months_between("2025-11-30", "2026-01-01"), 2);
        assert_eq!(months_between("2026-01-31", "2026-01-01"), 0);
        assert_eq!(months_between("2026-03-01", "2026-01-31"), -2);
    }

    #[test]
    fn the_dividend_filter_covers_the_div_and_int_families() {
        for t in ["DIV", "DIVNRA", "DIVROC", "INT", "INTNRA"] {
            assert!(DIVIDEND_INTEREST_TYPES.contains(&t), "{t}");
        }
        // And nothing trade-shaped.
        assert!(!DIVIDEND_INTEREST_TYPES.contains(&"FILL"));
    }
}
