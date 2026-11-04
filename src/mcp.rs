//! `alp mcp`: an Alpaca account's balances, positions and activity as MCP
//! tools (cli-common DESIGN.md §1.9, the MCP profile), for Claude Code,
//! Claude Desktop and any MCP client.
//!
//! Two read-only tools over the same client and mapping code the commands
//! use: `alpaca_portfolio` (the account's balances and open positions) and
//! `alpaca_activities` (fills, dividends, interest, fees and the rest of the
//! activities feed over a date range). Nothing here can write: the tools are
//! reads, and the client underneath has no verb but GET.
//!
//! Provider access goes through [`Source`], the per-target seam of
//! cli-common's MCP profile (DESIGN.md §2.3): async methods with no `Send`
//! bound, so one toolset serves both hosts. [`LiveSource`] is `alp mcp`'s:
//! it reads the key pair once per process (environment first, then keychain)
//! and only when a tool is called, so listing the tools touches no
//! credential. [`ClientSource`] is the Worker module's ([`crate::worker`]):
//! the same client over the Worker's `fetch`. The tests run every tool
//! offline against the fixtures through the seam.

use pk_cli_core::dates::{self, Civil};
use pk_cli_core::transport::Transport;
use pk_cli_core::CliError;
use pk_cli_mcp::{
    parse_args, ListArgs, Order, Page, PageRequest, PositionKey, PositionRequest, Server, ToolDef,
    ToolError, ToolOutput, Toolset,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::client::Alpaca;
use crate::config::Env;
use crate::reads::{self, activity_date, keep_sorted, str_at, Position, DIVIDEND_INTEREST_TYPES};

pub const PORTFOLIO_TOOL: &str = "alpaca_portfolio";
pub const ACTIVITIES_TOOL: &str = "alpaca_activities";

/// `types: ["dividends"]` stands for every dividend and interest type.
pub const DIVIDENDS_ALIAS: &str = "dividends";

/// Where the tools get Alpaca's raw rows: the live API, or fixtures.
#[allow(async_fn_in_trait)] // deliberate: no `Send` bound, as in `Toolset`
pub trait Source {
    /// The account object (`GET /v2/account`).
    async fn account(&self) -> Result<Value, CliError>;
    /// Every open position (`GET /v2/positions`).
    async fn positions(&self) -> Result<Vec<Value>, CliError>;
    /// Activity rows newest first, from the newest back past `since`, as
    /// `alp transactions list` pages them (`GET /v2/account/activities`);
    /// only `types` (a comma list) when given.
    async fn activities(&self, types: Option<&str>, since: &str) -> Result<Vec<Value>, CliError>;
}

/// The live API over any transport, from a client built up front: the Worker
/// module's source. A key pair that could not be read is kept as the error
/// each call answers, so describing the tools never needs one.
pub struct ClientSource<T> {
    client: Result<Alpaca<T>, CliError>,
    max_pages: Option<usize>,
}

impl<T: Transport> ClientSource<T> {
    pub fn new(client: Result<Alpaca<T>, CliError>) -> Self {
        Self {
            client,
            max_pages: None,
        }
    }

    /// At most `pages` activity pages a call (see [`reads::activities`]).
    pub fn with_max_pages(mut self, pages: usize) -> Self {
        self.max_pages = Some(pages);
        self
    }

    fn client(&self) -> Result<&Alpaca<T>, CliError> {
        self.client.as_ref().map_err(Clone::clone)
    }
}

impl<T: Transport> Source for ClientSource<T> {
    async fn account(&self) -> Result<Value, CliError> {
        reads::account(self.client()?).await
    }

    async fn positions(&self) -> Result<Vec<Value>, CliError> {
        reads::positions(self.client()?).await
    }

    async fn activities(&self, types: Option<&str>, since: &str) -> Result<Vec<Value>, CliError> {
        reads::activities(self.client()?, types, since, self.max_pages).await
    }
}

#[cfg(feature = "native")]
pub use live::LiveSource;

#[cfg(feature = "native")]
mod live {
    use std::sync::Mutex;

    use pk_cli_core::CliError;
    use pk_cli_secrets::CredentialStore;
    use serde_json::Value;

    use super::Source;
    use crate::client::{Alpaca, NativeAlpaca};
    use crate::config::Env;
    use crate::credential::{self, ApiKeys};
    use crate::reads;

    /// Reads the key pair: `credential::require` live, a stub in tests.
    type KeyLoader = Box<dyn Fn() -> Result<ApiKeys, CliError> + Send + Sync>;

    /// The live API for `alp mcp`. The key pair is read once, on the first
    /// call, and kept for the life of the server; a pair Alpaca rejects is
    /// dropped so the next call reads it again (after the user fixed it in a
    /// terminal).
    pub struct LiveSource {
        base_url: String,
        verbose: bool,
        load: KeyLoader,
        keys: Mutex<Option<ApiKeys>>,
    }

    impl LiveSource {
        /// Reads the pair as every command does ([`credential::require`]:
        /// environment, then keychain).
        pub fn new(env: Env, base_url: String, creds: CredentialStore, verbose: bool) -> Self {
            Self::with_loader(
                base_url,
                verbose,
                Box::new(move || credential::require(&creds, env)),
            )
        }

        /// With the pair from `load`, for tests of the caching.
        pub fn with_loader(base_url: String, verbose: bool, load: KeyLoader) -> Self {
            Self {
                base_url,
                verbose,
                load,
                keys: Mutex::new(None),
            }
        }

        /// A client holding the cached pair, reading it first if none is
        /// cached. One client per call: its request pacing is per process
        /// call, and the client is not `Sync`.
        pub(crate) fn client(&self) -> Result<NativeAlpaca, CliError> {
            let keys = {
                // Held across the read, so parallel first calls wait for one
                // keychain read (and at most one macOS approval) instead of
                // racing to raise several.
                let mut keys = self.keys.lock().unwrap_or_else(|e| e.into_inner());
                if keys.is_none() {
                    *keys = Some((self.load)()?);
                }
                keys.clone().expect("set above")
            };
            Alpaca::new(self.base_url.clone(), keys, self.verbose)
        }

        /// `out`, after dropping the cached pair if Alpaca rejected it.
        pub(crate) fn forget_rejected<T>(&self, out: Result<T, CliError>) -> Result<T, CliError> {
            if matches!(&out, Err(e) if e.is_auth()) {
                *self.keys.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            out
        }
    }

    /// The blocking client inside `async fn` is deliberate: the stdio server
    /// polls each call on its own blocking thread with no Tokio reactor
    /// (pk-cli-mcp's `Toolset` contract), where the transport's futures
    /// finish on their first poll.
    impl Source for LiveSource {
        async fn account(&self) -> Result<Value, CliError> {
            let api = self.client()?;
            self.forget_rejected(reads::account(&api).await)
        }

        async fn positions(&self) -> Result<Vec<Value>, CliError> {
            let api = self.client()?;
            self.forget_rejected(reads::positions(&api).await)
        }

        async fn activities(
            &self,
            types: Option<&str>,
            since: &str,
        ) -> Result<Vec<Value>, CliError> {
            let api = self.client()?;
            self.forget_rejected(reads::activities(&api, types, since, None).await)
        }
    }
}

/// Today's date in UTC, where the toolset reads it: the system clock
/// natively; the Worker's clock in the module, since `SystemTime` panics on
/// `wasm32-unknown-unknown`.
pub type Today = fn() -> Civil;

/// The toolset `alp mcp` serves.
pub struct AlpacaTools<S: Source> {
    source: S,
    env: Env,
    today: Today,
}

impl<S: Source> AlpacaTools<S> {
    /// `env` is the environment the source reads (`--env`, `ALP_ENV` or
    /// config), reported in every portfolio result. Today comes from the
    /// system clock.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn new(source: S, env: Env) -> Self {
        Self::with_today(source, env, dates::today)
    }

    /// As [`AlpacaTools::new`], with today's date from `today`.
    pub fn with_today(source: S, env: Env, today: Today) -> Self {
        Self { source, env, today }
    }

    pub fn source(&self) -> &S {
        &self.source
    }
}

// ---- alpaca_portfolio -------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PortfolioArgs {
    /// Only the position in this symbol, e.g. "AAPL".
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(flatten)]
    pub list: ListArgs,
}

#[derive(Serialize)]
struct PortfolioFilter<'a> {
    symbol: Option<&'a str>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct AccountSummary {
    /// `live` or `paper`.
    pub environment: String,
    pub currency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub equity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buying_power: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub long_market_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub short_market_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_equity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern_day_trader: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daytrade_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

impl AccountSummary {
    fn from(v: &Value, env: Env, detailed: bool) -> Self {
        let detail = |key: &str| if detailed { str_at(v, key) } else { None };
        Self {
            environment: env.as_str().to_string(),
            currency: str_at(v, "currency").unwrap_or_else(|| "USD".into()),
            status: str_at(v, "status"),
            equity: str_at(v, "equity"),
            cash: str_at(v, "cash"),
            buying_power: str_at(v, "buying_power"),
            long_market_value: str_at(v, "long_market_value"),
            short_market_value: str_at(v, "short_market_value"),
            account_number: detail("account_number"),
            last_equity: detail("last_equity"),
            pattern_day_trader: v
                .get("pattern_day_trader")
                .and_then(Value::as_bool)
                .filter(|_| detailed),
            daytrade_count: v
                .get("daytrade_count")
                .and_then(Value::as_u64)
                .filter(|_| detailed),
            created_at: detail("created_at"),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PositionItem {
    pub symbol: String,
    /// Negative for a short position.
    pub quantity: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub market_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub average_entry_price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unrealized_pl: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity_available: Option<String>,
}

impl PositionItem {
    fn from(p: Position, detailed: bool) -> Self {
        let amount = |m: Option<pk_cli_core::Money>| m.map(|m| m.amount);
        let detail = |s: Option<String>| if detailed { s } else { None };
        Self {
            symbol: p.symbol,
            quantity: p.quantity,
            market_value: amount(p.market_value),
            average_entry_price: amount(p.average_entry_price),
            unrealized_pl: amount(p.unrealized_pl),
            current_price: detail(amount(p.current_price)),
            side: detail(p.side),
            asset_class: detail(p.asset_class),
            exchange: detail(p.exchange),
            quantity_available: detail(p.quantity_available),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PortfolioOut {
    pub account: AccountSummary,
    #[serde(flatten)]
    pub page: Page<PositionItem>,
}

// ---- alpaca_activities ------------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ActivitiesArgs {
    /// First day, YYYY-MM-DD (inclusive). Default: 1 January of until's year.
    #[serde(default)]
    pub since: Option<String>,
    /// Last day, YYYY-MM-DD (inclusive). Default: today.
    #[serde(default)]
    pub until: Option<String>,
    /// Only these Alpaca activity types, e.g. ["FILL"] for trades or ["DIV",
    /// "INT"]. "dividends" stands for every dividend and interest type (DIV*,
    /// INT*).
    #[serde(default)]
    pub types: Option<Vec<String>>,
    /// Only activity in this symbol.
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(flatten)]
    pub list: ListArgs,
}

#[derive(Serialize)]
struct ActivitiesFilter<'a> {
    since: &'a str,
    until: &'a str,
    types: &'a [String],
    symbol: Option<&'a str>,
}

#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct ActivityItem {
    /// Absent only on a row Alpaca sent undated (kept, never dropped).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub net_amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Concise: only when not `executed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fill_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub order_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transaction_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_share_amount: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leaves_qty: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cum_qty: Option<String>,
}

impl ActivityItem {
    /// Read from the verbatim activity row: a fill carries side, qty and
    /// price; everything else net_amount and description. Nothing is
    /// computed or rounded.
    fn from(row: &Value, detailed: bool) -> Self {
        let detail = |key: &str| if detailed { str_at(row, key) } else { None };
        let status = match (str_at(row, "status"), detailed) {
            (Some(s), false) if s == "executed" => None,
            (s, _) => s,
        };
        Self {
            date: activity_date(row),
            kind: str_at(row, "activity_type"),
            symbol: str_at(row, "symbol"),
            side: str_at(row, "side"),
            qty: str_at(row, "qty"),
            price: str_at(row, "price"),
            net_amount: str_at(row, "net_amount"),
            description: str_at(row, "description").filter(|d| !d.trim().is_empty()),
            status,
            id: detail("id"),
            fill_type: detail("type"),
            order_id: detail("order_id"),
            order_status: detail("order_status"),
            transaction_time: detail("transaction_time"),
            per_share_amount: detail("per_share_amount"),
            leaves_qty: detail("leaves_qty"),
            cum_qty: detail("cum_qty"),
        }
    }
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ActivitiesOut {
    /// The window read, after defaults.
    pub since: String,
    pub until: String,
    #[serde(flatten)]
    pub page: Page<ActivityItem>,
}

// ---- the toolset ------------------------------------------------------------

impl<S: Source> Toolset for AlpacaTools<S> {
    fn server(&self) -> Server {
        Server::new("alp", env!("CARGO_PKG_VERSION"), "Alpaca", "alpaca").instructions(
            "Alpaca access here is read-only: no tool can trade, transfer or change anything. \
             Amounts are decimal strings at Alpaca's own precision; keep every digit when \
             adding them.",
        )
    }

    fn tools(&self) -> Vec<ToolDef> {
        vec![
            ToolDef::read(
                PORTFOLIO_TOOL,
                "Alpaca balances and positions",
                "Shows the user's Alpaca brokerage account: its balances and open positions. Use \
                 it for what they hold at Alpaca, what it is worth, and how much cash or buying \
                 power is left. Returns account (environment, currency, status, equity, cash, \
                 buying_power, long_market_value, short_market_value; detailed adds \
                 account_number, last_equity, pattern_day_trader, daytrade_count, created_at) \
                 and items, one per position, with symbol, quantity (negative when short), \
                 market_value, average_entry_price and unrealized_pl (detailed adds \
                 current_price, side, asset_class, exchange, quantity_available), plus returned \
                 and next_cursor. Narrow with symbol.",
            )
            .input::<PortfolioArgs>()
            .output::<PortfolioOut>(),
            ToolDef::read(
                ACTIVITIES_TOOL,
                "Alpaca activity",
                "Lists the account's activity over a date range, newest first: trade fills, \
                 dividends, interest, fees, transfers and corporate actions. Use it for what \
                 happened in a period, a tax year's income, or the trades behind a gain. Each \
                 item has date and type (FILL, DIV, INT, FEE, ...) and, as the type has them, \
                 symbol, side, qty and price (fills), net_amount (signed cash) and description; \
                 status appears when not executed. Detailed adds id, fill_type, order_id, \
                 order_status, transaction_time, per_share_amount, leaves_qty and cum_qty. \
                 Returns since, until, items, returned and next_cursor. The range defaults to \
                 the year to date; narrow with since/until, types or symbol.",
            )
            .input::<ActivitiesArgs>()
            .output::<ActivitiesOut>(),
        ]
    }

    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        match name {
            PORTFOLIO_TOOL => self.portfolio(parse_args(args)?).await,
            ACTIVITIES_TOOL => self.activities(parse_args(args)?).await,
            other => Err(ToolError::other(format!("no handler for {other}"))),
        }
    }
}

impl<S: Source> AlpacaTools<S> {
    async fn portfolio(&self, args: PortfolioArgs) -> Result<ToolOutput, ToolError> {
        let symbol = non_empty(args.symbol.as_deref());
        let page = PageRequest::new(&args.list, &PortfolioFilter { symbol })?;
        let detailed = args.list.format().is_detailed();
        let account = self.source.account().await?;
        let rows = self.source.positions().await?;
        let positions: Vec<Position> = rows.iter().filter_map(Position::from_json).collect();
        // `alp positions list` warns and goes on; a model has no stderr, so a
        // row that cannot be read fails the call rather than quietly
        // shortening the portfolio.
        if positions.len() < rows.len() {
            return Err(ToolError::upstream(format!(
                "{} of {} position rows from Alpaca lack a symbol or quantity, so the \
                 portfolio would be incomplete; the response shape has changed (docs/api.md)",
                rows.len() - positions.len(),
                rows.len()
            )));
        }
        let matched: Vec<PositionItem> = positions
            .into_iter()
            .filter(|p| symbol.is_none_or(|s| p.symbol.eq_ignore_ascii_case(s)))
            .map(|p| PositionItem::from(p, detailed))
            .collect();
        ToolOutput::new(&PortfolioOut {
            account: AccountSummary::from(&account, self.env, detailed),
            page: page.page(matched, "symbol")?,
        })
    }

    async fn activities(&self, args: ActivitiesArgs) -> Result<ToolOutput, ToolError> {
        // Everything the caller can get wrong fails before a request.
        let (since, until) = window(
            non_empty(args.since.as_deref()),
            non_empty(args.until.as_deref()),
            (self.today)(),
        )?;
        let types = expand_types(args.types.as_deref().unwrap_or_default())?;
        let symbol = non_empty(args.symbol.as_deref());
        let page = PositionRequest::new(
            &args.list,
            &ActivitiesFilter {
                since: &since,
                until: &until,
                types: &types,
                symbol,
            },
        )?;
        let detailed = args.list.format().is_detailed();

        let filter = (!types.is_empty()).then(|| types.join(","));
        let rows = self.source.activities(filter.as_deref(), &since).await?;
        let matched = keep_sorted(rows, &since, &until, None)
            .iter()
            .filter(|r| {
                symbol
                    .is_none_or(|s| str_at(r, "symbol").is_some_and(|x| x.eq_ignore_ascii_case(s)))
            })
            .map(|r| {
                let key = PositionKey::new(
                    activity_date(r).unwrap_or_default(),
                    &str_at(r, "id").unwrap_or_default(),
                );
                (key, ActivityItem::from(r, detailed))
            })
            .collect();
        ToolOutput::new(&ActivitiesOut {
            since,
            until,
            page: page.page(matched, Order::Descending, "since/until, types or symbol")?,
        })
    }
}

fn non_empty(s: Option<&str>) -> Option<&str> {
    s.map(str::trim).filter(|s| !s.is_empty())
}

/// The inclusive window, defaulted as `alp transactions list` defaults it
/// (the year to date), with errors in the tool's own words. Both ends come
/// back zero-padded (`2026-1-5` is `2026-01-05`), so the string comparisons
/// that filter rows are date comparisons.
fn window(
    since: Option<&str>,
    until: Option<&str>,
    today: Civil,
) -> Result<(String, String), ToolError> {
    let parse = |name: &str, v: Option<&str>| {
        v.map(|d| {
            dates::parse_iso(d).map(dates::fmt_iso).map_err(|_| {
                ToolError::usage(format!("{name} must be a date as YYYY-MM-DD, got `{d}`"))
            })
        })
        .transpose()
    };
    let since = parse("since", since)?;
    let until = parse("until", until)?.unwrap_or_else(|| dates::fmt_iso(today));
    let since = since.unwrap_or_else(|| format!("{}-01-01", &until[..4]));
    if since > until {
        return Err(ToolError::usage(format!(
            "since ({since}) is after until ({until})"
        )));
    }
    Ok((since, until))
}

/// Uppercased, `dividends` expanded, duplicates dropped. A type is letters
/// and digits only: it travels in a comma list, and Alpaca's own types are
/// all of that shape. Unknown types are not refused (docs/api.md §3.3: the
/// provider adds types faster than its schema says).
fn expand_types(types: &[String]) -> Result<Vec<String>, ToolError> {
    let mut out: Vec<String> = Vec::new();
    for t in types {
        let t = t.trim();
        let expanded: Vec<String> = if t.eq_ignore_ascii_case(DIVIDENDS_ALIAS) {
            DIVIDEND_INTEREST_TYPES
                .iter()
                .map(|s| s.to_string())
                .collect()
        } else if t.is_empty() {
            vec![]
        } else if t.chars().all(|c| c.is_ascii_alphanumeric()) {
            vec![t.to_ascii_uppercase()]
        } else {
            return Err(ToolError::usage(format!(
                "types holds `{t}`; an activity type is letters and digits, e.g. FILL, DIV, INT"
            )));
        };
        for e in expanded {
            if !out.contains(&e) {
                out.push(e);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dividends_expands_and_types_are_uppercased_once() {
        let got =
            expand_types(&["Dividends".into(), "div".into(), " fill ".into(), "".into()]).unwrap();
        assert_eq!(got[0], "DIV");
        assert!(got.contains(&"INTNRA".to_string()));
        assert_eq!(got.last().unwrap(), "FILL");
        assert_eq!(got.iter().filter(|t| *t == "DIV").count(), 1);
        let e = expand_types(&["FILL,DIV".into()]).unwrap_err();
        assert!(e.message().contains("letters and digits"), "{e}");
    }

    const TODAY: Civil = (2026, 8, 15);

    #[test]
    fn the_window_defaults_to_the_year_to_date_and_checks_its_order() {
        let (s, u) = window(None, Some("2025-06-30"), TODAY).unwrap();
        assert_eq!((s.as_str(), u.as_str()), ("2025-01-01", "2025-06-30"));
        let (s, u) = window(None, None, TODAY).unwrap();
        assert_eq!((s.as_str(), u.as_str()), ("2026-01-01", "2026-08-15"));
        let e = window(Some("01/02/2026"), None, TODAY).unwrap_err();
        assert!(
            e.message()
                .starts_with("since must be a date as YYYY-MM-DD"),
            "{e}"
        );
        let e = window(Some("2026-02-01"), Some("2026-01-01"), TODAY).unwrap_err();
        assert!(e.message().contains("is after until"), "{e}");
    }

    /// An unpadded date compares as a string, so it is padded first:
    /// `"2026-1-5" > "2026-01-31"` as text, though 5 January is earlier.
    #[test]
    fn unpadded_dates_are_padded_before_any_comparison() {
        let (s, u) = window(Some("2026-1-5"), Some("2026-01-31"), TODAY).unwrap();
        assert_eq!((s.as_str(), u.as_str()), ("2026-01-05", "2026-01-31"));
        let (s, _) = window(None, Some("2026-3-1"), TODAY).unwrap();
        assert_eq!(s, "2026-01-01");
    }

    #[cfg(feature = "native")]
    mod live {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        use pk_cli_core::CliError;

        use crate::config::Env;
        use crate::credential::{self, ApiKeys};
        use crate::mcp::LiveSource;

        fn keys() -> ApiKeys {
            ApiKeys {
                key_id: "AKDUMMY000000000001".into(),
                secret_key: "DUMMYSECRET0001".into(),
            }
        }

        /// A source whose loader counts its reads.
        fn counting() -> (LiveSource, Arc<AtomicUsize>) {
            let reads = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&reads);
            let source = LiveSource::with_loader(
                "https://api.alpaca.markets".into(),
                false,
                Box::new(move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(keys())
                }),
            );
            (source, reads)
        }

        /// No request is sent: building a client reads the pair, and
        /// `forget_rejected` sees what a request would have answered.
        #[test]
        fn the_pair_is_read_once_and_again_only_after_a_rejection() {
            let (source, reads) = counting();
            source.client().unwrap();
            source.client().unwrap();
            assert_eq!(reads.load(Ordering::SeqCst), 1, "cached after the first");

            let rejected: Result<(), CliError> =
                source.forget_rejected(Err(CliError::Auth("HTTP 401".into())));
            assert!(rejected.is_err());
            source.client().unwrap();
            assert_eq!(reads.load(Ordering::SeqCst), 2, "re-read after a 401");

            let failed: Result<(), CliError> =
                source.forget_rejected(Err(CliError::Upstream("HTTP 503".into())));
            assert!(failed.is_err());
            source.client().unwrap();
            assert_eq!(reads.load(Ordering::SeqCst), 2, "kept after a 503");
        }

        #[test]
        fn a_failed_read_is_the_call_s_error_and_is_tried_again_next_call() {
            let source = LiveSource::with_loader(
                "https://api.alpaca.markets".into(),
                false,
                Box::new(|| Err(credential::missing(Env::Live))),
            );
            let e = source.client().err().unwrap();
            assert_eq!(e.exit_code(), 3);
            assert!(source.client().is_err());
        }
    }
}
