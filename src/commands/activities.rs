//! `alp transactions list` / `alp dividends list` — the account-activities
//! feed, which is the tax raw material: FILL rows for realized-gains work,
//! DIV*/INT* rows for 1099-DIV/INT reconciliation.
//!
//! # The verbatim contract
//!
//! `transaction-list/v1` and `dividend-list/v1` carry the provider's activity
//! rows **verbatim** under `items`: every key, every string decimal, exactly
//! as Alpaca sent them. Nothing is renamed, reshaped, rounded, or converted —
//! tax-cli captures these DTOs as source documents, and a "helpful" reshape
//! here would be a silent edit to a tax record. The only things this module
//! does to rows are *select* (date range, `--limit`), *sort* (newest first),
//! and *envelope* (`Paged`).
//!
//! Two row shapes share the feed (docs/api.md §3.3): trade activities
//! (`activity_type: "FILL"`, with `transaction_time`/`price`/`qty`/`side`)
//! and non-trade activities (everything else, with `date`/`net_amount`).
//! Consumers must branch on `activity_type`, not on which keys exist.
//!
//! # Why paging ignores the server's date parameters
//!
//! The endpoint's `after`/`until` filters are documented against the
//! activity's *creation* datetime with exclusive-bound phrasing ("created
//! before/after this date"). Being wrong about a boundary there silently
//! drops a row at the edge of a tax year, and a dropped row looks exactly
//! like a complete list. So the client pages newest-first with no server-side
//! date bounds and applies the **inclusive** `[--since, --until]` filter
//! itself, where it can be tested offline. Paging stops once a page's oldest
//! dated row falls two calendar months before `--since` — a grace window so
//! an out-of-order row (a correction processed late) cannot end the walk
//! early — or when a short page says the history is exhausted.

use pk_cli_core::dates;
use pk_cli_core::{output, CliError, Paged, RangeArgs};
use serde_json::Value;

use crate::client::Alpaca;
use crate::commands::{date_within, str_at, Ctx};

pub const PATH: &str = "/v2/account/activities";

/// The server's page ceiling (docs.alpaca.markets, 2026-08-27): without a
/// `date` parameter, "the default and maximum value is 100".
const PAGE_SIZE: usize = 100;

/// The dividend/interest activity types (docs/api.md §3.3): every `DIV*`
/// income/withholding/fee variant plus the `INT*` interest variants. This is
/// the `alp dividends list` filter — the 1099-DIV/INT raw material.
pub const DIVIDEND_INTEREST_TYPES: &[&str] = &[
    "DIV", "DIVCGL", "DIVCGS", "DIVFEE", "DIVFT", "DIVNRA", "DIVROC", "DIVTW", "DIVTXEX", "INT",
    "INTNRA", "INTTW",
];

/// The columns the text table shows, across both row shapes. `--json` always
/// carries every field; the table is a human summary, not the contract.
const TABLE_COLUMNS: &[&str] = &[
    "id",
    "activity_type",
    "symbol",
    "side",
    "qty",
    "price",
    "net_amount",
    "transaction_time",
    "date",
];

/// `alp transactions list` — every activity type unless `--type` narrows it
/// (`transaction-list/v1`).
pub fn list_transactions(ctx: &Ctx, range: &RangeArgs, types: &[String]) -> Result<(), CliError> {
    let filter = if types.is_empty() {
        None
    } else {
        Some(types.join(","))
    };
    list(ctx, range, filter.as_deref(), "transaction")
}

/// `alp dividends list` — the DIV*/INT* slice of the same feed
/// (`dividend-list/v1`).
pub fn list_dividends(ctx: &Ctx, range: &RangeArgs) -> Result<(), CliError> {
    list(
        ctx,
        range,
        Some(&DIVIDEND_INTEREST_TYPES.join(",")),
        "dividend",
    )
}

fn list(ctx: &Ctx, range: &RangeArgs, types: Option<&str>, record: &str) -> Result<(), CliError> {
    let (since, until) = resolve_range(range)?;
    let rows = fetch(&ctx.api, types, &since)?;
    let kept = keep_sorted(rows, &since, &until, range.limit);

    let page = Paged::new(record, kept);
    if ctx.common.json {
        page.emit(true);
    } else {
        // The verbatim rows mix two shapes; the default table would be a
        // sparse union of every key. Same rows, fixed columns.
        let v = serde_json::to_value(&page).unwrap_or(Value::Null);
        let items = output::rows_of(&v, "items");
        output::table(&output::table_view(&items, TABLE_COLUMNS));
    }
    Ok(())
}

/// Resolve the range flags into concrete inclusive bounds.
///
/// The defaults are the tax use case: **year to date**. `--until` defaults to
/// today and `--since` to 1 January of the until-year, so a bare
/// `alp transactions list` answers "what happened in my account this year".
pub fn resolve_range(range: &RangeArgs) -> Result<(String, String), CliError> {
    let until = match &range.until {
        Some(u) => {
            dates::parse_iso(u)?;
            u.clone()
        }
        None => dates::fmt_iso(dates::today()),
    };
    let since = match &range.since {
        Some(s) => {
            dates::parse_iso(s)?;
            s.clone()
        }
        None => format!("{}-01-01", &until[..4]),
    };
    Ok((since, until))
}

/// Page the activities feed newest-first until it is exhausted or safely past
/// `since`. Rows come back exactly as the wire carried them.
pub fn fetch(api: &Alpaca, types: Option<&str>, since: &str) -> Result<Vec<Value>, CliError> {
    let mut rows: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
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

        let page = match api.get(PATH, &query)? {
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

/// Filter to the inclusive range, sort newest-first, apply `--limit`.
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
    use serde_json::json;

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

    #[test]
    fn the_envelopes_are_tagged() {
        let v = serde_json::to_value(Paged::new("transaction", vec![fill()])).unwrap();
        assert_eq!(v["schema"], "transaction-list/v1");
        let v = serde_json::to_value(Paged::new("dividend", vec![div()])).unwrap();
        assert_eq!(v["schema"], "dividend-list/v1");
    }

    #[test]
    fn default_range_is_year_to_date() {
        let (since, until) = resolve_range(&RangeArgs::default()).unwrap();
        let today = dates::fmt_iso(dates::today());
        assert_eq!(until, today);
        assert_eq!(since, format!("{}-01-01", &today[..4]));

        let (since, _) = resolve_range(&RangeArgs {
            until: Some("2025-06-30".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(since, "2025-01-01", "default since follows the until-year");
    }
}
