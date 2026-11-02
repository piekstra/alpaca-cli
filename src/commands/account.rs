//! `alp account` — the trading account summary (`account/v1`).
//!
//! One account per key pair: the Trading API scopes everything to the account
//! that issued the keys, so there is no `accounts list` and no `--account`
//! flag. Money fields arrive from Alpaca as **string decimals** and are
//! carried into `Money` untouched — no parsing, no rounding.

use pk_cli_core::{block_on, output, CliError, Money};
use serde_json::{Map, Value};

use crate::commands::Ctx;
use crate::reads::{self, str_at};

/// The money fields the DTO surfaces, in render order.
const MONEY_FIELDS: &[&str] = &[
    "cash",
    "equity",
    "portfolio_value",
    "buying_power",
    "long_market_value",
    "short_market_value",
];

pub fn show(ctx: &Ctx) -> Result<(), CliError> {
    let v = block_on(reads::account(&ctx.api))?;
    let dto = account_dto(&v);
    output::emit(ctx.common.json, "account", dto, output::render);
    Ok(())
}

/// Map the wire account onto `account/v1`. Identifying fields lead; every
/// field is omitted when the wire lacks it rather than emitted as null.
pub fn account_dto(v: &Value) -> Value {
    let currency = str_at(v, "currency").unwrap_or_else(|| "USD".into());
    let mut m = Map::new();
    for key in ["account_number", "status", "currency"] {
        if let Some(s) = str_at(v, key) {
            m.insert(key.into(), Value::String(s));
        }
    }
    if let Some(b) = v.get("pattern_day_trader").and_then(Value::as_bool) {
        m.insert("pattern_day_trader".into(), Value::Bool(b));
    }
    if let Some(n) = v.get("daytrade_count").and_then(Value::as_u64) {
        m.insert("daytrade_count".into(), n.into());
    }
    for key in MONEY_FIELDS {
        if let Some(amount) = str_at(v, key) {
            let money = Money {
                amount,
                currency: currency.clone(),
            };
            m.insert(
                (*key).into(),
                serde_json::to_value(&money).unwrap_or_default(),
            );
        }
    }
    if let Some(s) = str_at(v, "created_at") {
        m.insert("created_at".into(), Value::String(s));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn wire() -> Value {
        json!({
            "id": "00000000-0000-4000-8000-000000000010",
            "account_number": "111111111",
            "status": "ACTIVE",
            "currency": "USD",
            "cash": "1000.55",
            "portfolio_value": "4300.55",
            "equity": "4300.55",
            "buying_power": "2001.10",
            "long_market_value": "3300.00",
            "short_market_value": "0",
            "pattern_day_trader": false,
            "daytrade_count": 0,
            "created_at": "2026-01-02T10:00:00Z"
        })
    }

    #[test]
    fn money_amounts_are_the_wire_strings_untouched() {
        let dto = account_dto(&wire());
        // "0" stays "0" — no reformatting to "0.00", no float round trip.
        assert_eq!(dto["short_market_value"]["amount"], "0");
        assert_eq!(dto["cash"]["amount"], "1000.55");
        assert_eq!(dto["cash"]["currency"], "USD");
    }

    #[test]
    fn identifying_fields_lead_and_absent_fields_are_omitted() {
        let dto = account_dto(&wire());
        let keys: Vec<&String> = dto.as_object().unwrap().keys().collect();
        assert_eq!(keys[0], "account_number");
        assert_eq!(keys[1], "status");

        let sparse = account_dto(&json!({"account_number": "111111111"}));
        assert!(sparse.get("cash").is_none(), "no null noise");
        assert!(sparse.get("pattern_day_trader").is_none());
    }
}
