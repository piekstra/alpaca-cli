//! `alp positions list` — current holdings (`position-list/v1`).
//!
//! The row shape leads with `symbol`/`quantity`/`market_value`, matching how
//! the sibling brokerage CLIs shape holdings (there is no ratified trading
//! profile yet — PROFILES.md's bar isn't met — so this matches `rhd`'s
//! `position/v1` spirit rather than a shared crate). Quantities and money
//! amounts are the wire's string decimals, untouched.

use pk_cli_core::{CliError, Money, Paged};
use serde::Serialize;
use serde_json::Value;

use crate::commands::{str_at, Ctx};

pub const PATH: &str = "/v2/positions";

/// One holding (`position/v1`).
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

pub fn list(ctx: &Ctx, limit: Option<u32>) -> Result<(), CliError> {
    let rows = match ctx.api.get(PATH, &[])? {
        Value::Array(rows) => rows,
        _ => {
            return Err(CliError::Upstream(
                "expected a JSON array of positions — the response shape has changed; \
                 see docs/api.md"
                    .into(),
            ))
        }
    };
    let mut positions: Vec<Position> = rows.iter().filter_map(Position::from_json).collect();
    if positions.len() < rows.len() {
        // A row we couldn't map is data loss; say so instead of quietly
        // shortening the list.
        eprintln!(
            "warning: {} position row(s) lacked a symbol/qty and were skipped — \
             see docs/api.md",
            rows.len() - positions.len()
        );
    }
    if let Some(n) = limit {
        positions.truncate(n as usize);
    }
    Paged::new("position", positions).emit(ctx.common.json);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire() -> Value {
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
    fn rows_lead_with_symbol_quantity_market_value() {
        let p = Position::from_json(&wire()).unwrap();
        let v = serde_json::to_value(&p).unwrap();
        let keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        assert_eq!(&keys[..3], &["symbol", "quantity", "market_value"]);
        assert_eq!(v["quantity"], "10.5");
        assert_eq!(v["market_value"]["amount"], "1103.55");
        assert_eq!(v["average_entry_price"]["amount"], "100.25");
    }

    #[test]
    fn absent_fields_are_omitted_not_null() {
        let p = Position::from_json(&json!({"symbol": "SMPL", "qty": "1"})).unwrap();
        let v = serde_json::to_value(&p).unwrap();
        assert!(v.get("market_value").is_none());
        assert!(v.get("side").is_none());
    }

    #[test]
    fn a_row_without_symbol_or_qty_is_unmappable() {
        assert!(Position::from_json(&json!({"qty": "1"})).is_none());
        assert!(Position::from_json(&json!({"symbol": "SMPL"})).is_none());
    }

    #[test]
    fn the_list_envelope_is_tagged() {
        let page = Paged::new("position", vec![Position::from_json(&wire()).unwrap()]);
        let v = serde_json::to_value(&page).unwrap();
        assert_eq!(v["schema"], "position-list/v1");
    }
}
