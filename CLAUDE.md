# CLAUDE.md

Read [AGENTS.md](AGENTS.md) — it is the canonical guide for working in this
repo. The two non-negotiables, restated for emphasis:

1. **Treat this repo as public.** No real account data anywhere — source,
   fixtures, docs, commit messages. Runtime output may carry personal data;
   git may not.
2. **The transactions feed is verbatim.** `transaction-list/v1` items are
   Alpaca's activity rows byte-for-byte; never reshape, rename, or round
   them — tax-cli stores them as source documents.
