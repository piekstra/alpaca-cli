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
//! The paging, the inclusive date filter and the row dates live in
//! [`crate::reads`], shared with the MCP tools; why the walk ignores the
//! server's date parameters is documented there.

use pk_cli_core::dates;
use pk_cli_core::{block_on, output, CliError, Paged, RangeArgs};
use serde_json::Value;

use crate::commands::Ctx;
use crate::reads::{self, keep_sorted, DIVIDEND_INTEREST_TYPES};

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
    let rows = block_on(reads::activities(&ctx.api, types, &since, None))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_envelopes_are_tagged() {
        let row = json!({"id": "x", "activity_type": "DIV", "date": "2026-06-30"});
        let v = serde_json::to_value(Paged::new("transaction", vec![row.clone()])).unwrap();
        assert_eq!(v["schema"], "transaction-list/v1");
        let v = serde_json::to_value(Paged::new("dividend", vec![row])).unwrap();
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
