# houdinny — Future Work & Known Gaps

This is the honest status ledger for houdinny. The [README](README.md) documents
what works today; this file tracks everything that is **planned, half-implemented,
stubbed, or known-broken**, with pointers to the relevant code.

If you add or finish one of these, update both this file and the README's
**Working** list.

## Suggested priorities

1. Wire the built-but-inert subsystems into the running binary (anti-correlation,
   admin API, MCP server) — most of the code is already written and tested.
2. Add timeouts to the proxy/relay paths (currently a hung peer pins a task forever).
3. Add active tunnel health probing so dead tunnels drop out of rotation.

---

## 1. Built but not wired into the running binary

These modules have complete, unit-tested code but **no call site** in the live
request path (`src/main.rs` / `src/proxy/mod.rs`). Verified: none of the types
below are referenced from `main.rs` or `proxy/mod.rs`.

- [ ] **Anti-correlation** (`src/anticorr/mod.rs`) — `AntiCorrelationLayer`
  (timing jitter + packet padding) is never constructed or called. Wire
  `pre_request()` / `pad_outgoing()` / `unpad_incoming()` into the proxy
  forwarding path and drive it from a config section. Also: `AntiCorrStats.padding_added`
  is hardcoded to `0` in `pre_request()` — report the real byte count.
- [ ] **Admin REST API** (`src/admin/mod.rs`) — spawn `AdminServer::run()` from
  `main.rs` behind `--features admin` (default port 8081). Add authentication
  and/or enforce loopback-only binding before exposing it (see Security).
- [ ] **MCP server** (`src/mcp/mod.rs`) — spawn the stdio JSON-RPC loop behind
  `--features mcp`. Note `pool_add` is a placeholder (below).
- [ ] **MITM mode** (`ProxyMode::Mitm` in `src/config/mod.rs`) — the mode is
  parsed and printed in the startup banner but **never branched on**; the proxy
  always does a transparent blind TCP relay. Implement TLS termination via
  `rustls` + `rcgen` (`--features mitm`). This is a prerequisite for HTTP 402
  payment interception (the payment layer can only see plaintext in MITM mode).
- [ ] **Buffered / rotation-capable relay** (`src/relay/mod.rs`) — `BufferedRelay`
  and `RelayStream` exist and are tested, but the proxy uses inline
  `tokio::io::copy_bidirectional` / `copy` instead. Wire `BufferedRelay` in to
  enable swapping the upstream tunnel while keeping the agent connection open.
- [ ] **Route manager** (`src/route/mod.rs`) — `LinuxRouteManager` /
  `NamespaceManager` only **generate** `ip` / `ip netns` command strings; they
  never execute them (root required) and `create_route_manager()` is not invoked
  at startup. Decide between an opt-in privileged executor vs. printing the
  commands for the operator to run.

## 2. Stubs and incomplete implementations

- [ ] **Sentinel dVPN transport** (`src/transport/sentinel.rs`) — `provision()`
  and `connect()` return errors ("requires Cosmos SDK integration"). Needs:
  query node pricing → on-chain DVPN deposit → await confirmation → receive
  WireGuard credentials → establish the tunnel. `wallet_key_path` is stored but
  never read.
- [ ] **Real payment handlers** (`src/payment/mod.rs`) — only `DummyPaymentHandler`
  works (it fabricates a random hex token). `X402PaymentHandler::pay()` returns
  "x402 payments not yet implemented"; L402 is only *detected*, never paid. There
  is no cryptographic proof generation/verification. Also:
  - `handle_payment()` does **not** actually retry — `max_retries` (default 3)
    has a getter but is unused; it makes a single `pay()` attempt.
  - First matching handler wins and a broad handler shadows later ones — ordering
    is a footgun.
- [ ] **MCP `pool_add` tool** (`src/mcp/mod.rs`) — validates and logs the request
  but returns "queued" without ever building a transport or touching the pool.
  Needs runtime transport construction/provisioning.
- [ ] **Admin `/stats` endpoint** (`src/admin/mod.rs`) — returns hardcoded zeroes.
  Needs real relay metrics (total/active connections, bytes relayed), which means
  the relay path has to accumulate counters somewhere shared.

## 3. Transport gaps

- [ ] **Tor** (`src/transport/tor.rs`) — feature-gated code exists via
  `arti-client`, but is not tested end-to-end. Validate bootstrap, per-circuit
  isolation (distinct `IsolationToken` per transport), and exit-IP diversity.
- [ ] **WireGuard** (`src/transport/wireguard.rs`) — MVP only: binds outgoing
  sockets to an **externally-managed** interface (`wg-quick`, NordVPN, Mullvad).
  houdinny does not implement the WireGuard protocol or manage keys itself. A
  self-managed WG transport (via `boringtun`) is future work.

## 4. Robustness & correctness

- [ ] **No timeouts** anywhere in `src/proxy/mod.rs` / `src/relay/mod.rs` — a slow
  or hung peer (slowloris, a `Content-Length` body that never arrives) pins a
  task indefinitely. Add connect/read/idle timeouts.
- [ ] **Plain-HTTP forwarding limits** (`src/proxy/mod.rs`) — only `Content-Length`
  bodies are handled (no chunked / `Transfer-Encoding`); no keep-alive (one
  request per connection, relays until upstream EOF); proxy-specific headers
  (e.g. `Proxy-Connection`) are forwarded verbatim instead of stripped.
- [ ] **HTTP CONNECT transport edge cases** (`src/transport/http_proxy.rs`) —
  accepts only status `== 200` (`http_proxy.rs:183`), rejecting other 2xx; the
  doc comment says "2xx". `from_url` accepts a URL with no port (fails later at
  connect) and mis-splits passwords containing `@` (uses rightmost `@`).
- [ ] **No active health probing** — health is passive: `Pool` and `Router` only
  read `Transport::healthy()`, and a failed `connect()` does **not** mark a
  transport unhealthy (`healthy` only flips in `close()`). Add periodic
  connectivity checks and mark-down on connect failure.
- [ ] **Round-robin fairness** (`src/router/mod.rs`) — `counter.fetch_add(1) % healthy.len()`
  is fair only for a *stable* healthy set; when the set size changes, rotation
  position shifts and tunnels can be skipped/repeated.
- [ ] **Per-connection rotation only** — tunnel choice is made once per accepted
  connection; there is no mid-stream rotation. LLM streaming APIs (OpenAI,
  Anthropic) don't support SSE resume (`Last-Event-ID`), so a stream stays pinned
  to one tunnel for its lifetime.

## 5. Known bugs & doc drift

- [ ] **`import nord` country-id bug** — `JP` and `KR` both map to NordVPN country
  id `114` (`src/import/nord.rs:93` and `:106`); Korea is wrong.
- [ ] **`least-connections` strategy is documented but unimplemented** —
  `tunnels.example.toml:9` advertises it, but the `Strategy` enum only supports
  `random` / `round-robin` (selecting it would error). Either implement it or
  drop it from the example.
- [ ] **`import nord` doc comment is misleading** — the comment claims an
  `Authorization: token:<TOKEN>` header, but the code uses HTTP Basic auth
  (`.basic_auth("token", Some(token))`).

## 6. Security TODOs

- [ ] **No auth on either control plane** — the admin API has no token check and
  the MCP server trusts whatever is on stdio. `pool_remove` mutates the live pool.
  Add authentication (and enforce loopback-only for admin) before wiring either in.
- [ ] **Payment spend controls** — an upstream attacker controls the 402 response
  (amount/currency/pay-to/network). Before any real payment handler ships, add
  spend caps, an address allowlist, and confirmation gating so houdinny can't be
  coerced into paying arbitrary amounts.
- [ ] **Secret hygiene** — tokens and WireGuard keys are stored as plain `String`
  with no zeroization; `tunnels.toml` is written without `0600` permissions; the
  NordLynx private key is duplicated in plaintext across every generated tunnel
  entry. `setup-wg.sh` also contains hardcoded WireGuard keys (and is macOS-only
  due to BSD `sed -i ''`).

## 7. Packaging & distribution

- [ ] **Not on crates.io** — the release CI has no `cargo publish` step, so
  `cargo install houdinny` does not work. Either publish to crates.io or keep
  recommending `cargo install --git …` / `--path .` (README updated accordingly).
- [ ] **Verify published artifacts** — confirm GitHub Releases binaries and the
  `ghcr.io/puneet2019/houdinny` image are actually produced by
  `.github/workflows/release.yml` on tag.
