# Test fixtures

**These are doc-derived synthetic payloads, not live captures.**

Built **2026-08-27** from Alpaca's official Trading API reference
(docs.alpaca.markets — `GET /v2/account`, `GET /v2/positions`,
`GET /v2/account/activities`; see `docs/api.md` for the per-endpoint
citations). Alpaca's API is official and versioned, so the documented field
sets are the contract in a way a reverse-engineered portal's never is — but
"documented" is still weaker than "captured": no live response has yet
confirmed these shapes byte-for-byte. When the owner's first live run
happens, any shape difference belongs here as a scrubbed re-capture, and
this header changes to record the capture date.

Known doc-vs-reality risks these fixtures encode conservatively:

- Trade (`FILL`) and non-trade activity rows have **disjoint field sets**
  beyond `id`/`activity_type`; the fixtures include both shapes plus a
  `partial_fill`, a negative-amount withholding row (`DIVNRA`), and a `FEE`
  row, so consumers are tested against absent-not-null keys.
- Money and quantities are **strings** on the wire (`"105.1000"`, `"-5"`);
  every fixture value keeps provider-style precision so the verbatim tests
  mean something.
- `error_401.json` / `error_429.json` follow the documented
  `{"code", "message"}` error body.

## Scrubbing policy

Every identity-bearing or financial value is an obvious dummy, and
`tests/fixture_shapes.rs` enforces this **positively** — "identity-bearing
fields must match a known dummy shape", never a denylist of real values
(a denylist would publish exactly what it exists to keep out).

| Field | Dummy shape |
|---|---|
| `account_number` | exactly `111111111` |
| symbols | `SMPL`, `ETFX` |
| UUIDs (`id`, `asset_id`, `order_id`, …) | near-all-zero (`00000000-0000-4000-8000-…`) |
| activity ids | `<YYYYMMDD>000000000::<dummy-uuid>` |
| API keys | never appear; anything `AK…`/`PK…`-shaped must begin `AKDUMMY`/`PKDUMMY` |
| balances, prices, amounts | round synthetic numbers, no real position sizes |

Never commit a raw capture. If a captured value ever seems worth keeping
verbatim, it isn't.
