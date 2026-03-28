# houdinny

Privacy proxy for AI agents. Rotates requests across tunnel pools so each request exits from a different IP.

## What it does

- **Tunnel rotation** -- every request your agent makes goes through a different tunnel. No single observer sees the full picture.
- **Language-agnostic** -- any agent in any language just sets `HTTP_PROXY` / `HTTPS_PROXY` and goes. Single static binary.
- **Pluggable transports** -- SOCKS5, HTTP CONNECT, WireGuard interface binding. Tor and dVPN planned.
- **Anti-correlation** -- optional timing jitter and packet padding to resist traffic analysis.

## Quick Start

```bash
# 1. Start a SOCKS5 tunnel (SSH)
ssh -D 1080 -N user@myserver

# 2. Run houdinny
cargo run -- -t socks5://127.0.0.1:1080

# 3. Point your agent at it
export HTTP_PROXY=http://127.0.0.1:8080
export HTTPS_PROXY=http://127.0.0.1:8080

# Every request now goes through the tunnel
curl https://httpbin.org/ip
python my_agent.py
```

With multiple tunnels, each request exits from a different IP:

```bash
ssh -D 1080 -N user@server-tokyo &
ssh -D 1081 -N user@server-london &
ssh -D 1082 -N user@server-nyc &

cargo run -- -t socks5://127.0.0.1:1080,socks5://127.0.0.1:1081,socks5://127.0.0.1:1082
```

## Installation

### From source

```bash
git clone https://github.com/puneet2019/houdinny.git
cd houdinny
cargo install --path .
```

### Via cargo

```bash
cargo install houdinny
```

### Docker

```bash
docker build -t houdinny .
docker run --rm -p 8080:8080 houdinny -t socks5://host.docker.internal:1080
```

## Usage

```
houdinny [OPTIONS]

Options:
  -c, --config <CONFIG>      Path to the TOML config file [default: tunnels.toml]
  -l, --listen <LISTEN>      Listen address (overrides the value in the config file)
  -t, --tunnels <TUNNELS>    Comma-separated tunnel URLs
                             (e.g. socks5://127.0.0.1:1080,socks5://127.0.0.1:1081)
                             When provided, the config file is ignored.
  -s, --strategy <STRATEGY>  Routing strategy: random, round-robin [default: random]
  -v, --verbose              Enable debug logging (RUST_LOG=debug)
  -h, --help                 Print help
  -V, --version              Print version
```

### Examples

```bash
# Single SOCKS5 tunnel
houdinny -t socks5://127.0.0.1:1080

# Multiple tunnels with round-robin
houdinny -t socks5://127.0.0.1:1080,socks5://127.0.0.1:1081 -s round-robin

# HTTP CONNECT proxy with auth
houdinny -t http-proxy://user:pass@proxy.brightdata.com:22225

# Custom listen address
houdinny -l 0.0.0.0:9090 -t socks5://127.0.0.1:1080

# From config file
houdinny -c tunnels.toml

# Debug logging
houdinny -t socks5://127.0.0.1:1080 -v
```

## Configuration

Copy `tunnels.example.toml` to `tunnels.toml` and edit:

```toml
[proxy]
listen = "127.0.0.1:8080"
mode = "transparent"          # "transparent" (default) or "mitm"
strategy = "random"           # "random" or "round-robin"

# SOCKS5 (SSH tunnels, proxy services)
# Set up with: ssh -D 1080 -N user@myserver

[[tunnel]]
protocol = "socks5"
address = "127.0.0.1:1080"
label = "ssh-tokyo"

[[tunnel]]
protocol = "socks5"
address = "127.0.0.1:1081"
label = "ssh-london"

# HTTP CONNECT proxy (residential, datacenter)

[[tunnel]]
protocol = "http-proxy"
address = "http://user:pass@gate.brightdata.com:22225"
label = "residential-us"

# WireGuard (requires --features wireguard)
# Interface must already exist (wg-quick up, NordVPN, Mullvad, etc.)

# [[tunnel]]
# protocol = "wireguard"
# interface = "wg0"
# label = "nord-us-east"
```

The `tunnels.toml` file is gitignored by default since it may contain credentials.

CLI flags override config file values. If `-t`/`--tunnels` is provided, the config file is ignored entirely.

## Architecture

```
Agent (any language)
  |
  |  HTTP_PROXY / HTTPS_PROXY
  v
+------------------------------------------+
|  houdinny (single Rust binary)           |
|                                          |
|  proxy server (HTTP CONNECT + plain)     |
|       |                                  |
|  router (random / round-robin)           |
|       |                                  |
|  pool (health checks, add/remove)        |
|       |                                  |
|  +----+--------+--------+----------+     |
|  |    SOCKS5   | HTTP   | WireGuard|     |
|  |    transport| CONNECT| transport|     |
|  +-------------+--------+----------+     |
|                                          |
|  relay (bidirectional byte copy,         |
|         buffered relay for rotation)     |
|                                          |
|  anti-correlation (jitter, padding)      |
|                                          |
|  payment interceptor (402 flow)          |
|                                          |
|  route manager (Linux policy routing,    |
|                  macOS per-socket bind)   |
+------------------------------------------+
  |              |              |
  v              v              v
Tunnel A      Tunnel B      Tunnel C
(Tokyo)       (London)      (New York)
  |              |              |
  v              v              v
        destination servers
```

The proxy handles both HTTP CONNECT (for HTTPS tunneling) and plain HTTP forwarding. For CONNECT, it does transparent TCP relay via `copy_bidirectional`. For plain HTTP, it rewrites the absolute URL to a relative path and forwards through the selected tunnel.

## Transports

| Transport | Protocol | Status | Notes |
|-----------|----------|--------|-------|
| SOCKS5 | `socks5` | Done | SSH tunnels, proxy services. Default feature. |
| HTTP CONNECT | `http-proxy` | Done | Residential/datacenter proxies. Supports Basic auth. Always available. |
| WireGuard | `wireguard` | Done | Binds sockets to existing WG interfaces. Requires `--features wireguard`. |
| Tor | `tor` | Planned | Via `arti-client`. Feature-gated, crate dep defined but transport not wired up yet. |
| dVPN (Sentinel) | `sentinel` | Planned | Decentralized VPN with on-chain session auth. |
| Residential Proxy | -- | Planned | Exit through real ISP IPs. |

Transports are designed to be chained in the future (e.g. WireGuard -> Residential Proxy).

## Cargo Features

| Feature | Default | What it enables |
|---------|---------|-----------------|
| `socks5` | Yes | SOCKS5 transport via `fast-socks5` |
| `tor` | No | Tor transport via `arti-client` + `tor-rtcompat` |
| `wireguard` | No | WireGuard interface binding via `boringtun`, `socket2`, `nix` |
| `http-proxy` | No | (Flag exists but HTTP CONNECT transport is always compiled) |
| `mitm` | No | TLS interception via `rustls` + `rcgen` (for 402 payment flows) |
| `admin` | No | Admin REST API server |
| `mcp` | No | MCP support (implies `admin`) |

Build with specific features:

```bash
cargo build --release --features socks5,wireguard
```

## Admin API

The admin API runs on a separate port (default 8081) and exposes JSON endpoints for runtime inspection.

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Health check. Returns `{"status": "ok", "version": "0.1.0"}` |
| `GET` | `/pool` | Pool status: total/healthy counts, per-tunnel details (id, label, protocol, healthy) |
| `GET` | `/stats` | Relay statistics (placeholder -- returns zeroes) |

Example:

```bash
curl http://127.0.0.1:8081/pool
```

```json
{
  "total": 3,
  "healthy": 2,
  "tunnels": [
    {"id": "socks5-tokyo", "label": "tokyo", "protocol": "socks5", "healthy": true},
    {"id": "socks5-london", "label": "london", "protocol": "socks5", "healthy": true},
    {"id": "wireguard-nord-us", "label": "nord-us", "protocol": "wireguard", "healthy": false}
  ]
}
```

## Building from Source

Requirements: Rust 1.85+ (edition 2024).

```bash
git clone https://github.com/puneet2019/houdinny.git
cd houdinny

# Debug build (default features: socks5)
cargo build

# Release build with all stable transports
cargo build --release --features socks5,wireguard

# Run tests
cargo test

# Build optimized binary (strip, LTO, single codegen unit)
cargo build --release
```

The release profile produces a stripped binary with LTO and `panic = "abort"` for minimal size.

## Roadmap

### Done

- [x] Core HTTP/HTTPS proxy server (CONNECT + plain HTTP)
- [x] SOCKS5 transport (SSH tunnels, proxy services)
- [x] HTTP CONNECT transport (residential/datacenter proxies, Basic auth)
- [x] WireGuard transport (per-socket interface binding, Linux + macOS)
- [x] Tunnel pool with health checks, runtime add/remove
- [x] Router: random and round-robin strategies
- [x] Bidirectional stream relay with byte counting
- [x] Buffered relay for server-side rotation (agent connection stays open)
- [x] Anti-correlation: timing jitter, packet padding (opt-in)
- [x] Payment interceptor: 402 parsing, handler plugin system, x402 detection, L402 detection
- [x] Dummy payment handler (for testing 402 flows)
- [x] Route manager: Linux policy-based routing, network namespaces, macOS noop
- [x] Admin REST API (health, pool, stats)
- [x] TOML config file + CLI flag support
- [x] Dockerfile

### Next

- [ ] Tor transport (via `arti-client`)
- [ ] MITM mode for HTTP response inspection (needed for 402 interception in transparent mode)
- [ ] x402 / L402 real payment handlers
- [ ] Sentinel dVPN transport
- [ ] Anti-correlation integration into the proxy pipeline (jitter/padding are implemented but not wired into the request path)
- [ ] Admin API wired into `main.rs` startup
- [ ] Relay statistics collection (currently placeholder)
- [ ] Tunnel health probing (periodic connectivity checks)
- [ ] Mid-stream tunnel rotation for resumable protocols (HTTP Range downloads)

### Limitations

- **Anti-correlation is opt-in and has costs.** Timing jitter adds latency. Packet padding increases bandwidth. These are trade-offs, not free wins.
- **Mid-stream rotation does not work for LLM streaming APIs.** OpenAI, Anthropic, etc. do not implement SSE resume (`Last-Event-ID`). Streams are pinned to one tunnel. This becomes viable if the ecosystem adopts resumable streaming.
- **The server still knows it's you.** Tunnel rotation hides you from network observers (ISP, government), not from the API server itself -- you're still sending the same API key.
- **Route management commands are generated, not executed.** Linux policy routing and namespace commands require root. houdinny generates the `ip` commands; you run them.

## License

MIT OR Apache-2.0
