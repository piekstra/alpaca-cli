# The Alpaca Trading API, as `alp` uses it

Alpaca publishes an **official, versioned** Trading API at
docs.alpaca.markets — a luxury most of the family's providers don't offer.
Everything below was verified against the official reference on
**2026-08-27** (the exact pages are cited per section). Claims are labelled:

- **OFFICIAL-DOCS** — read from Alpaca's published reference on the date
  above. Strong, but still weaker than a capture: docs and wire can drift.
- **LIVE-VERIFIED** — confirmed by this CLI against a real account, with the
  date. *None yet* — the owner's first live run should upgrade the labels
  here and reconcile `tests/fixtures/` (see its README).

## 1. Hosts and auth

OFFICIAL-DOCS (reference/getaccount-1):

| Environment | Base URL |
|---|---|
| live | `https://api.alpaca.markets` |
| paper | `https://paper-api.alpaca.markets` |

Auth is two headers on every request: `APCA-API-KEY-ID` and
`APCA-API-SECRET-KEY`. No OAuth dance, no session, no refresh — the pair is
the credential, and it is **full-power** (the same pair can `POST
/v2/orders`). Live and paper issue **different pairs**; a pair sent to the
wrong host answers 401/403 with `{"code": 40110000, "message": "request is
not authorized"}` — which reads like a bad credential, not a wrong host.
That trap is why `alp` keys the keychain entry by environment and verifies
at `auth login` time against the environment the keys will be stored under.

Errors are `{"code": <int>, "message": "<text>"}`; the client surfaces
`message` because the HTTP status alone doesn't distinguish revoked keys
from an account restriction. Rate limit: ~200 requests/minute on the
standard plan; the binary paces at 4 req/s and maps 429 to exit 5 without
retrying. The Worker module ([section 6](#6-the-worker-module)) sends the same
requests unpaced, and reads at most 40 activity pages a call.

## 2. `GET /v2/account` — OFFICIAL-DOCS (reference/getaccount-1)

One account per key pair (no account list, no `--account` flag). All money
fields are **strings** (`"cash": "1000.55"`); `alp account` carries them
into `Money` without touching the text. Fields the CLI reads:
`account_number`, `status`, `currency`, `pattern_day_trader`,
`daytrade_count`, `cash`, `equity`, `portfolio_value`, `buying_power`,
`long_market_value`, `short_market_value`, `created_at`.

## 3. The data feeds

### 3.1 `GET /v2/positions` — OFFICIAL-DOCS (reference/getallopenpositions)

Answers a bare JSON **array** of open positions. Row fields the CLI reads:
`symbol`, `qty` (string, negative for shorts), `qty_available`,
`avg_entry_price`, `side` (`long`/`short`), `market_value` (negative for
shorts), `current_price`, `unrealized_pl`, `asset_class`
(`us_equity`/`us_option`/`crypto`), `exchange`. All numerics are strings.

### 3.3 `GET /v2/account/activities` — OFFICIAL-DOCS (reference/getaccountactivities-2)

The tax feed: one endpoint carrying **two disjoint row shapes**.

- **Trade activities** (`activity_type: "FILL"`): `id`, `transaction_time`
  (RFC 3339), `type` (`fill`/`partial_fill`), `price`, `qty`, `side`,
  `symbol`, `leaves_qty`, `cum_qty`, `order_id`, `order_status`.
- **Non-trade activities** (everything else): `id`, `activity_type`,
  `date` (`YYYY-MM-DD`), `net_amount` (signed string; withholding rows like
  `DIVNRA` are negative), plus per-type extras (`per_share_amount`, `qty`,
  `symbol`, `description`, `status`).

Documented `activity_type` values (2026-08-27): FILL, TRANS, MISC, ACATC,
ACATS, CFEE, CGD, CSD, CSW, DIV, DIVCGL, DIVCGS, DIVFEE, DIVFT, DIVNRA,
DIVROC, DIVTW, DIVTXEX, FEE, INT, INTNRA, INTTW, JNL, JNLC, JNLS, MA, NC,
OPASN, OPCA, OPCSH, OPEXC, OPEXP, OPTRD, PTC, PTR, REO, REORG, SPIN, SPLIT,
FOPT, OCT. The CLI does **not** validate `--type` against this list — prior
family work (schw) found live enums the published schema lacked, and a
client that rejects the unknown breaks on the provider's next release.
`alp dividends list` filters to the DIV\*/INT\* families
(`DIVIDEND_INTEREST_TYPES` in `src/reads.rs`).

**Query parameters**: `activity_types` (comma list; mutually exclusive with
`category`), `date`, `after`, `until`, `direction` (`asc`/`desc`, default
desc), `page_size` (1–100, default 100 — unlimited only with `date`),
`page_token`.

**Pagination**: `page_token` is the **id of the last row** of the current
page; with `desc`, the next page ends before that id. Activity ids look like
`20220203000000000::<uuid>` — a timestamp prefix plus a UUID.

**Trap — the date filters are creation-time and exclusive-bound.**
`after`/`until` are documented as "activities *created* after/before this
date". A row's tax-relevant date (`transaction_time`/`date`) is not its
creation instant, and the bound phrasing is exclusive. Being wrong at a
year boundary silently drops a row, and a dropped row looks exactly like a
complete list. So `alp` **does not use the server's date filters at all**:
it pages newest-first and applies the inclusive `[--since, --until]` filter
client-side, stopping once a page's oldest dated row is two calendar months
past `--since` (grace for late-processed corrections) or a short page ends
the history. Cost: a few extra requests when `--until` is far in the past.
(The Worker module's walk, capped at 40 pages, is the one exception: it
sends an `until` padded by two to three months; see §6.)
A future optimization could use the `date=YYYY-MM-DD` single-day mode
(unlimited page size) for one-day pulls.

**What this feed is not**: a realized-gain report. Alpaca publishes no
cost-basis/lot endpoint here; fills are the raw material for a Schedule
D/Form 8949 workup, and the consolidated 1099 is the source of truth for
filed figures. Cancelled/corrected activity statuses pass through verbatim
(`status: "executed" | "correct" | "canceled"` per the Broker API's
documented NTA statuses) — consumers should not assume every row is final.

### 3.4 `GET /v2/orders` — not wrapped (reachable via `alp api`)

Order history (`status=closed`, `after`/`until`, `limit` ≤ 500) duplicates
what the FILL activities answer for tax work, so it has no dedicated
command; `alp api GET '/v2/orders?status=closed&limit=50'` covers ad-hoc
needs on the same origin, auth, and pacing.

## 4. Rate limits

OFFICIAL-DOCS: 200 requests/minute per key pair on the standard plan
(bursts above that answer 429 with a `retry-after`-less body). The binary's
250 ms floor keeps even a multi-year activities walk far under it.

## 5. Out of scope

The market-data API lives on a **different host** (`data.alpaca.markets`).
`alp api` deliberately refuses it (origin check): quotes/bars are not
account data, and keeping one origin makes "where do the key headers go"
auditable. Wrap it in a separate command group if it's ever needed.

## 6. The Worker module

`alp-mcp-<version>.wasm` (feature `wasm`, `src/worker.rs`; built by
`make wasm` and attached to each release) runs the two MCP tools in
cli-common's deploy-your-own Cloudflare Worker (`connector/`). It sends the
requests above, built by the same client: `GET` only, the two key headers,
`Accept: application/json` and `User-Agent: alp/<version>`, through the
Worker's `fetch`, which follows no redirect (a 3xx is exit 5's upstream
error). The tools read:

| Tool | Requests |
|---|---|
| `alpaca_portfolio` | `GET /v2/account`, then `GET /v2/positions` |
| `alpaca_activities` | `GET /v2/account/activities?direction=desc&page_size=100[&activity_types=<comma list>]&until=<YYYY-MM-01T00:00:00Z>[&page_token=<last id>]`, page after page until a page has fewer than 100 rows or its oldest dated row is two calendar months before `since`, at most 40 pages |

One call reads at most 40 activity pages (`MAX_ACTIVITY_PAGES`, 4,000
rows): Cloudflare's free plan allows 50 outbound requests per invocation,
and past that every `fetch` fails. So that an old window's pages are not
spent on everything since, the module's walk adds a server-side `until`:
midnight UTC on the first day of the third month after the window's end.
That pads the creation-time, exclusive bound (§3's trap) by at least the two
calendar months the far end's stop rule allows; the inclusive filter still
runs on the rows. A window whose walk needs more than 40 pages is a `usage`
error asking for shorter windows or a `types` filter; it never answers with
part of the history. The binary's walk has neither the bound nor the
server-side `until`.

Its settings are the Worker's secrets and vars, by the CLI's own names:
`ALP_API_KEY_ID` and `ALP_API_SECRET_KEY` (both, or the call is an auth
error naming what is missing), `ALP_ENV` (`live` by default, or `paper`) and
`ALP_BASE_URL` (an override; `https://`, or plain `http://` only to a
loopback address, for a local mock). Today's date, for the default window,
is the Worker's clock in UTC.
