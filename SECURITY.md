# Security

## Reporting

Open a GitHub issue for anything that does not expose a secret. If a report
would itself reveal credential material, email the repository owner instead
of filing publicly.

## Where secrets live

The only secret this tool holds is an **Alpaca API key pair** (key ID +
secret key). It is stored solely in the OS keychain, service `piekstra.alp`,
one entry per environment (`live`, `paper`). Keys enter via an interactive
no-echo prompt, `--stdin`, or `--from-env <VAR>` — never argv, never a file,
never a log. `--verbose` prints request paths and parameters, never headers.

A host with its own secure store (the Claude Desktop extension) passes the
pair instead as `ALP_API_KEY_ID` and `ALP_API_SECRET_KEY`. With both set the
keychain is never opened; with only one set, or either empty, the command
fails (exit 3) rather than fall back to a different pair.

The Worker module (`alp-mcp-<version>.wasm`, for a Cloudflare Worker the
user deploys into their own account) reads the same two names from that
Worker's secrets, and holds them to the same both-or-neither rule. It sends
the keys only to `https://` hosts (or plain HTTP to a loopback address, for
a local mock in development); no service of the authors' ever sees them.

An Alpaca API key pair is a **full-power credential**: the same pair that
reads a position can place a live order or liquidate one. Treat it like a
password, and prefer creating it with trading disabled on the Alpaca
dashboard if the account offers that.

## What this tool does not do

- **It never places a trade, moves money, or changes account settings.**
  The HTTP client type has no write method — read-only is a property of the
  code, not a promise (`src/client.rs`). `alp api` refuses any method other
  than `GET` (exit `2`) and refuses `--data`.
- It talks only to the configured Trading API origin
  (`api.alpaca.markets` or `paper-api.alpaca.markets`, or an explicit
  HTTPS `base_url` override). `alp api` refuses absolute URLs off that
  origin — including lookalike hostnames — before the keychain is read.
  No telemetry, no third-party services.
- `alp mcp` serves two read-only tools over the same client. A tool call
  never signs in, prompts or creates a keychain item: a missing or rejected
  pair is an `auth` error asking the user to run `alp auth login` in a
  terminal. The Worker module serves the same two tools over the same
  client; there a missing or rejected pair is an `auth` error asking the
  user to update the Worker's secrets.
- It hardcodes no secrets. `gitleaks` runs in CI over the full history, and
  an offline test walks `git ls-files` for personal-data shapes.

## Handling credentials safely

Prefer piping from a password manager over typing:

```console
$ op read 'op://Private/alpaca/credential' \
    | alp auth set-credential --stdin --overwrite
```

(the stored item can be `KEYID:SECRET`, two lines, or JSON — see
`alp auth login --help`).

`alp auth logout --forget` removes the selected environment's key pair from
the keychain. Rotating keys on the Alpaca dashboard invalidates the stored
pair immediately; re-run `alp auth login --overwrite`.

## Official-API posture

Unlike most of the family, this wraps an **official, documented** API, so
there is no scraping fragility and no terms-of-service ambiguity. The
residual risks are credential-scope (above) and rate limiting (~200
requests/minute; the client paces itself and maps HTTP 429 to exit 5 rather
than retrying).
