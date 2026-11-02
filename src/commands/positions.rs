//! `alp positions list` — current holdings (`position-list/v1`), one
//! [`Position`] per row (the mapping lives in [`crate::reads`], shared with
//! the MCP tools).

use pk_cli_core::{block_on, CliError, Paged};

use crate::commands::Ctx;
use crate::reads::{self, Position};

pub fn list(ctx: &Ctx, limit: Option<u32>) -> Result<(), CliError> {
    let rows = block_on(reads::positions(&ctx.api))?;
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

    #[test]
    fn the_list_envelope_is_tagged() {
        let row = json!({"symbol": "SMPL", "qty": "10.5", "market_value": "1103.55"});
        let page = Paged::new("position", vec![Position::from_json(&row).unwrap()]);
        let v = serde_json::to_value(&page).unwrap();
        assert_eq!(v["schema"], "position-list/v1");
    }
}
