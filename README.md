# alpaca-cli (`alp`)

Read-only CLI for an [Alpaca](https://alpaca.markets) brokerage account, over
the official [Trading API](https://docs.alpaca.markets). Built to feed tax
work: realized-gains raw material (fills), dividend/interest income, current
positions, and the account summary — each as a stable, schema-tagged JSON DTO.

Unofficial; not affiliated with or endorsed by AlpacaDB, Inc. or Alpaca Securities LLC.

Conforms to **piekstra-cli spec v1** (`piekstra-cli/1`; see
[`cli-common`](https://github.com/piekstra/cli-common)).

**Read-only.** There is no order-entry, transfer, or settings code path in
this binary, and `alp api` accepts `GET` only. If a write is ever added, this
claim changes in the same commit.

## Install

```console
$ git clone https://github.com/piekstra/alpaca-cli && cd alpaca-cli
$ make install     # cargo install + macOS dev-signing (keeps keychain grants stable)
```

## Setup

Generate an API key pair on the Alpaca dashboard, then:

```console
$ alp auth login            # prompts for key ID + secret key, verifies, stores in the OS keychain
$ alp auth status           # auth-status/v1; add --verify to spend one request proving the keys work
```

A run can also take the pair from the environment, which wins over the
keychain: set both `ALP_API_KEY_ID` and `ALP_API_SECRET_KEY` (one without
the other is an error, never a fall-back).

Live is the default environment. Paper trading has its **own key pair** and
its own keychain slot:

```console
$ alp config set env paper  # or per-invocation: alp --env paper …  (env var: ALP_ENV)
$ alp --env paper auth login
```

## Commands

| Command | JSON schema | What it answers |
|---|---|---|
| `alp account` | `account/v1` | Account number, status, cash/equity/buying power (string-decimal `Money`) |
| `alp positions [list] [--limit N]` | `position-list/v1` | Holdings: symbol, quantity, market value, entry/current price, unrealized P/L |
| `alp transactions list [--since D --until D --limit N] [--type T]...` | `transaction-list/v1` | Account activities **verbatim** — fills, dividends, interest, fees; defaults to year-to-date |
| `alp dividends list [--since D --until D --limit N]` | `dividend-list/v1` | The DIV\*/INT\* slice of the same feed — 1099-DIV/INT raw material |
| `alp api GET <PATH>` | (provider JSON) | Raw read-only passthrough, Trading API origin only |
| `alp mcp [--list-tools]` | (MCP over stdio) | The read-only tools for Claude ([below](#use-with-claude)) |
| `alp auth login/status/logout/set-credential` | `auth-status/v1` | Credential lifecycle |
| `alp config path/show/set/unset` | — | `env` (`live`/`paper`), `base_url` |
| `alp info` | `cli-info/v1` | Machine discovery (offline) |
| `alp self-update [--check]` | `self-update/v1` | GitHub-release update |
| `alp completions <shell>` | — | Shell completions |

Every command takes `--json`; exit codes follow the family contract
(0 ok · 2 usage · 3 auth · 4 not found · 5 upstream · 6 confirmation).

### The transactions feed is verbatim

`transaction-list/v1` items are Alpaca's activity rows exactly as sent —
every key, every string decimal. Two shapes share the feed: trade activities
(`activity_type: "FILL"`, dated by `transaction_time`) and non-trade
activities (`DIV`, `DIVNRA`, `INT`, `FEE`, …, dated by `date`, amount in
`net_amount`). **Branch on `activity_type`**, not on which keys exist. This
is order/income history, not a realized-gain report — Alpaca publishes no
cost-basis endpoint, and the consolidated 1099 remains the source of truth
for filed figures (see `docs/api.md`).

```console
$ alp transactions list --since 2026-01-01 --until 2026-12-31 --json   # the tax-year pull
$ alp transactions list --type FILL --json                             # trades only
$ alp dividends list --json                                            # income only
```

## Use with Claude

`alp mcp` serves two read-only tools over MCP, so Claude can answer
questions about your own Alpaca account. There is no hosted service: the
tools run on your computer, in your copy of `alp`, or in a Cloudflare Worker
in your own account ([below](#claude-on-the-web-and-mobile)), and call
Alpaca's Trading API directly with your key pair.

| Tool | Answers |
| --- | --- |
| `alpaca_portfolio` | What do I hold, and what is it worth? Balances (equity, cash, buying power) and open positions, narrowed by `symbol` |
| `alpaca_activities` | What happened over a period? Fills, dividends, interest, fees and the rest of the activities feed, by `since`/`until` dates (year to date by default), activity `types` (`dividends` for every dividend and interest type) and `symbol` |

Results are concise by default (`response_format: "detailed"` adds ids,
order detail and timestamps), 25 records a page with a cursor for more, and
never larger than about 32,000 characters. Activity descriptions come from
Alpaca and other parties; the server tells Claude to treat them as data,
not instructions. The tools read the environment `alp` is set to
(`--env`, `ALP_ENV` or `alp config set env`; live by default).

**Claude Code.** Install `alp` and store a key pair (above), then either
add the `alpaca-cli` plugin from the
[piekstra plugin marketplace](https://github.com/piekstra/claude-plugins):

```console
$ claude plugin marketplace add piekstra/claude-plugins
$ claude plugin install alpaca-cli@piekstra-plugins
```

or register the server yourself:

```console
$ claude mcp add alpaca -- alp mcp
```

**Claude Desktop.** Download `alp-<version>.mcpb` from the
[latest release](https://github.com/piekstra/alpaca-cli/releases/latest)
and open it. Claude Desktop asks for the key ID, the secret key and the
environment (`live` or `paper`), keeps the keys in its own secure store,
and passes them to the bundled binary as `ALP_API_KEY_ID`,
`ALP_API_SECRET_KEY` and `ALP_ENV`. No separate install and no keychain are
involved.

Any other MCP client that launches a local server can run `alp mcp`. If the
key pair is missing or rejected, the tools say so and ask you to run
`alp auth login` in a terminal; they never sign in by themselves.

### Claude on the web and mobile

claude.ai and the Claude apps cannot start a program on your computer. They
reach the same two tools through the
[deploy-your-own connector](https://github.com/piekstra/cli-common/tree/main/connector):
a Cloudflare Worker in your own free Cloudflare account. Each release
attaches the tools as a WebAssembly module (`alp-mcp-<version>.wasm`, with
its `.sha256`), and the Worker runs the same `alp` code. The Worker holds your
key pair as its own secrets and calls Alpaca directly; no service of ours
sees the keys or the data.

| Worker setting | Value |
| --- | --- |
| `ALP_API_KEY_ID` (secret) | the key ID |
| `ALP_API_SECRET_KEY` (secret) | the secret key |
| `ALP_ENV` (variable, optional) | `live` (the default) or `paper` |

Leave both keys `none` and the connector serves no Alpaca tools. To add
Alpaca to a connector you already deployed, follow
[Adding Alpaca to a deployment you already have](https://github.com/piekstra/cli-common/tree/main/connector#adding-alpaca-to-a-deployment-you-already-have).
Alpaca has no read-only keys: the module sends `GET` requests only, but a
pair that leaks from the Worker can trade, so regenerate it in the Alpaca
dashboard at once if that happens. When the keys are missing or rejected,
the tools ask you to update the Worker's secrets.

In the Worker, one `alpaca_activities` call reads at most 40 pages of the
feed (4,000 rows), newest first, and rows newer than `until` count toward
them. A window further back is refused with a suggestion: a later `since`, a
`types` filter, or `alp transactions list`, which has no such limit.

## Development

```console
$ make verify   # fmt-check + clippy -D warnings + offline tests + wasm32 clippy + smoke — the gate
$ make wasm     # the MCP tools as a WebAssembly module for a Cloudflare Worker
```

Tests are fully offline (no network, no keychain). Fixtures are synthetic —
see `tests/fixtures/README.md` for the scrubbing policy, which a test
enforces. `docs/api.md` records the endpoints and their traps.

## License

MIT or Apache-2.0, at your option.
