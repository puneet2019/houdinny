# houdinny

Privacy proxy for AI agents. Rotates requests across tunnel pools so each request exits from a different IP.

## What it does

- **Tunnel rotation** -- every request your agent makes goes through a different tunnel. No single observer sees the full picture.
- **Language-agnostic** -- any agent in any language just sets `HTTP_PROXY` / `HTTPS_PROXY` and goes. Single static binary.
- **Pluggable transports** -- SOCKS5, HTTP CONNECT, WireGuard interface binding. Tor planned.
- **Import CLI** -- one command to pull WireGuard configs from NordVPN or set up SSH SOCKS5 tunnels.
- **MCP server** -- exposes tunnel pool management tools over JSON-RPC for AI agents.

## Quick Start

### Docker (recommended)

```bash
# 1. Clone and configure
git clone https://github.com/puneet2019/houdinny.git
cd houdinny
cp .env.example .env
# Edit .env — add your NordVPN access token

# 2. Copy the Docker tunnel config template
cp tunnels.docker.example.toml tunnels.docker.toml

# 3. Start everything
docker compose up -d

# 4. Whitelist the Docker network and start SOCKS5 proxies inside VPN containers
docker exec houdinny-vpn-1 nordvpn whitelist add subnet 172.20.0.0/16
docker exec houdinny-vpn-2 nordvpn whitelist add subnet 172.20.0.0/16
docker exec houdinny-vpn-3 nordvpn whitelist add subnet 172.20.0.0/16
docker exec -d houdinny-vpn-1 microsocks -p 1080 -b 0.0.0.0
docker exec -d houdinny-vpn-2 microsocks -p 1080 -b 0.0.0.0
docker exec -d houdinny-vpn-3 microsocks -p 1080 -b 0.0.0.0

# 5. Restart houdinny so it picks up the now-available SOCKS5 proxies
docker restart houdinny

# 6. Point your agent at it
export HTTP_PROXY=http://localhost:8080
export HTTPS_PROXY=http://localhost:8080
curl https://httpbin.org/ip
```

The Docker setup runs 3 NordVPN containers (US, Germany, Japan) with NordLynx, each exposing a SOCKS5 proxy via microsocks. houdinny round-robins across them.

To add more countries, duplicate a `vpn-N` + matching `[[tunnel]]` entry in `docker-compose.yml` and `tunnels.docker.toml`.

### Binary (standalone)

```bash
# 1. Start a SOCKS5 tunnel (SSH)
ssh -D 1080 -N user@myserver &

# 2. Run houdinny
houdinny -t socks5://127.0.0.1:1080

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

houdinny -t socks5://127.0.0.1:1080,socks5://127.0.0.1:1081,socks5://127.0.0.1:1082
```

## Installation

### GitHub Releases (binary download)

Download pre-built binaries from [GitHub Releases](https://github.com/puneet2019/houdinny/releases).

### Docker (ghcr.io)

```bash
docker pull ghcr.io/aquaqualis/houdinny:latest
docker run --rm -p 8080:8080 ghcr.io/aquaqualis/houdinny -t socks5://host.docker.internal:1080
```

### cargo install

```bash
cargo install houdinny
```

### From source

```bash
git clone https://github.com/puneet2019/houdinny.git
cd houdinny
cargo install --path .
```

Requirements: Rust 1.85+ (edition 2024).

## Usage

```
houdinny [OPTIONS] [COMMAND]

Commands:
  import  Import tunnel configurations from providers

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

## Import CLI

The `import` subcommand pulls tunnel configurations from providers and writes them to your `tunnels.toml`.

### `houdinny import nord` -- NordVPN WireGuard tunnels

Fetches your NordLynx (WireGuard) private key and recommended servers from the NordVPN API, then generates `[[tunnel]]` entries.

```bash
# Import 3 servers (auto-selected by NordVPN)
houdinny import nord --token=YOUR_NORDVPN_TOKEN

# Import 5 servers from specific countries
houdinny import nord --token=YOUR_TOKEN --count=5 --countries=us,de,jp

# Write to a specific config file
houdinny import nord --token=YOUR_TOKEN --output=my-tunnels.toml
```

```
Usage: houdinny import nord [OPTIONS] --token <TOKEN>

Options:
      --token <TOKEN>          NordVPN access token (from https://my.nordaccount.com/)
      --count <COUNT>          Number of server configs to fetch [default: 3]
      --countries <COUNTRIES>  Comma-separated country codes (e.g., us,de,jp)
      --output <OUTPUT>        Output config file to write/append to [default: tunnels.toml]
```

Get your access token from [NordVPN manual configuration](https://my.nordaccount.com/dashboard/nordvpn/manual-configuration/).

If the output file exists, new `[[tunnel]]` entries are appended. If it does not exist, a fresh config file with a default `[proxy]` section is created.

### `houdinny import ssh` -- SSH SOCKS5 tunnels

Generates tunnel config for SSH-based SOCKS5 proxies and optionally starts the SSH tunnels in the background.

```bash
# Generate config for a single SSH tunnel
houdinny import ssh --host=my-vps.example.com

# Multiple ports (one SSH tunnel per port)
houdinny import ssh --host=bastion --user=deploy --ports=1080,1081,1082

# Generate config AND start the SSH tunnels immediately
houdinny import ssh --host=my-tailscale-host --ports=1080,1081 --start
```

```
Usage: houdinny import ssh [OPTIONS] --host <HOST>

Options:
      --host <HOST>      SSH host (hostname or IP, e.g., tailscale hostname)
      --user <USER>      SSH user [default: root]
      --ports <PORTS>    Local SOCKS5 ports to use (one SSH tunnel per port) [default: 1080]
      --output <OUTPUT>  Output config file to write/append to [default: tunnels.toml]
      --start            Actually start the SSH tunnels (ssh -D -N -f)
```

When `--start` is used, each tunnel is launched in the background with `ssh -D <port> -N -f <user>@<host>`.

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

### Docker Configuration

For docker-compose, use `tunnels.docker.example.toml` as your starting point. Each VPN container in `docker-compose.yml` needs a matching `[[tunnel]]` entry pointing at the container's IP:

```toml
[proxy]
listen = "0.0.0.0:8080"
mode = "transparent"
strategy = "round-robin"

[[tunnel]]
protocol = "socks5"
address = "172.20.0.10:1080"
label = "vpn-1"

[[tunnel]]
protocol = "socks5"
address = "172.20.0.11:1080"
label = "vpn-2"
```

## Admin API

The admin API is feature-gated behind `--features admin`. It runs on a separate port (default 8081) and exposes JSON endpoints for runtime inspection.

**Note:** The admin API is not yet wired into `main.rs` startup. The server code exists (`src/admin/mod.rs`) but must be manually spawned. This is tracked in the roadmap.

### Endpoints

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Health check |
| `GET` | `/pool` | Pool status with tunnel details |
| `GET` | `/stats` | Relay statistics (placeholder -- returns zeroes) |

### Examples

```bash
# Health check
curl http://127.0.0.1:8081/health
```

```json
{"status": "ok", "version": "0.1.0"}
```

```bash
# Pool status
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

```bash
# Relay stats (placeholder)
curl http://127.0.0.1:8081/stats
```

```json
{"connections_total": 0, "connections_active": 0, "bytes_relayed": 0}
```

Wrong HTTP method returns 405. Unknown routes return 404.

## MCP Server

The MCP (Model Context Protocol) server is feature-gated behind `--features mcp`. It exposes houdinny tools to AI agents over JSON-RPC 2.0 via stdio.

**Note:** Like the admin API, the MCP server is not yet wired into `main.rs` startup. The server code exists (`src/mcp/mod.rs`) and can be used programmatically.

### Tools

| Name | Description | Parameters |
|------|-------------|------------|
| `pool_status` | Get tunnel pool status (total, healthy, per-tunnel details) | None |
| `pool_add` | Add a tunnel to the pool at runtime | `protocol`, `address`, `label` (all required) |
| `pool_remove` | Remove a tunnel from the pool by its ID | `id` (required) |
| `health_check` | Health check with version and tunnel counts | None |

`pool_add` logs the request but does not yet create a transport at runtime (transport construction requires more context than the MCP layer currently has).

### Protocol

The server reads JSON-RPC 2.0 requests from stdin (one per line) and writes responses to stdout.

Supported methods:
- `tools/list` -- enumerate available tools
- `tools/call` -- invoke a tool by name

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
|  admin API (pool status, health)         |
|                                          |
|  MCP server (JSON-RPC over stdio)        |
|                                          |
|  import CLI (nord, ssh)                  |
+------------------------------------------+
  |              |              |
  v              v              v
Tunnel A      Tunnel B      Tunnel C
(Tokyo)       (London)      (New York)
  |              |              |
  v              v              v
        destination servers
```

### Modules

| Module | Description |
|--------|-------------|
| `proxy` | HTTP/HTTPS proxy server (CONNECT + plain HTTP forwarding) |
| `router` | Route selection (random, round-robin) |
| `pool` | Tunnel pool with runtime add/remove |
| `transport` | Transport trait + implementations (socks5, http_proxy, wireguard, sentinel stub, tor) |
| `relay` | Bidirectional stream relay with byte counting; buffered relay for server-side rotation |
| `anticorr` | Anti-correlation: timing jitter, packet padding (opt-in) |
| `payment` | 402 response parsing, handler plugin system, dummy handler, x402 stub |
| `route` | Route manager: Linux policy routing / namespace command generation, macOS noop |
| `config` | TOML config parsing, CLI-to-config conversion |
| `admin` | Admin REST API (feature-gated, not yet wired into main) |
| `mcp` | MCP server over JSON-RPC 2.0 (feature-gated, not yet wired into main) |
| `import` | CLI importers: `nord` (NordVPN WireGuard), `ssh` (SSH SOCKS5) |
| `error` | Error types |

The proxy handles both HTTP CONNECT (for HTTPS tunneling) and plain HTTP forwarding. For CONNECT, it does transparent TCP relay via `copy_bidirectional`. For plain HTTP, it rewrites the absolute URL to a relative path and forwards through the selected tunnel.

## Transports

| Transport | Protocol | Status | Notes |
|-----------|----------|--------|-------|
| SOCKS5 | `socks5` | Working | SSH tunnels, proxy services. Default feature. |
| HTTP CONNECT | `http-proxy` | Working | Residential/datacenter proxies. Supports Basic auth. Always available. |
| WireGuard | `wireguard` | Working | Binds sockets to existing WG interfaces. Requires `--features wireguard`. |
| Sentinel dVPN | `sentinel` | Stub | Code exists but `provision()` returns an error. Requires Cosmos SDK integration. |
| Tor | `tor` | Feature-gated | Crate deps defined, transport code exists. Requires `--features tor`. Uses `arti-client`. |

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

## Docker Setup (detailed)

### How it works

The `docker-compose.yml` defines:

1. **VPN containers** (`vpn-1`, `vpn-2`, `vpn-3`) -- each runs the [bubuntux/nordvpn](https://github.com/bubuntux/nordvpn) image with NordLynx (WireGuard), connecting to a different country.
2. **houdinny container** -- reads `tunnels.docker.toml` and round-robins across the VPN containers.

All containers share a Docker bridge network (`172.20.0.0/16`) with static IPs.

### Environment

The `.env` file needs one variable:

```
NORD_TOKEN=your-nordvpn-access-token-here
```

Get your token from [NordVPN manual configuration](https://my.nordaccount.com/dashboard/nordvpn/manual-configuration/).

### Post-startup steps

After `docker compose up -d`, the VPN containers are running but houdinny cannot reach them yet. You need to:

1. **Whitelist the Docker subnet** so NordVPN allows internal traffic:

```bash
docker exec houdinny-vpn-1 nordvpn whitelist add subnet 172.20.0.0/16
docker exec houdinny-vpn-2 nordvpn whitelist add subnet 172.20.0.0/16
docker exec houdinny-vpn-3 nordvpn whitelist add subnet 172.20.0.0/16
```

2. **Start microsocks** inside each VPN container (exposes a SOCKS5 proxy that houdinny can connect to):

```bash
docker exec -d houdinny-vpn-1 microsocks -p 1080 -b 0.0.0.0
docker exec -d houdinny-vpn-2 microsocks -p 1080 -b 0.0.0.0
docker exec -d houdinny-vpn-3 microsocks -p 1080 -b 0.0.0.0
```

3. **Restart houdinny** so it can connect to the now-available SOCKS5 proxies:

```bash
docker restart houdinny
```

### Verify

```bash
# Each curl should show a different IP
curl -x http://localhost:8080 https://httpbin.org/ip
curl -x http://localhost:8080 https://httpbin.org/ip
curl -x http://localhost:8080 https://httpbin.org/ip
```

## Roadmap

### Working

- Core HTTP/HTTPS proxy server (CONNECT + plain HTTP)
- SOCKS5 transport (SSH tunnels, proxy services)
- HTTP CONNECT transport (residential/datacenter proxies, Basic auth)
- WireGuard transport (per-socket interface binding, Linux + macOS)
- Tunnel pool with runtime add/remove
- Router: random and round-robin strategies
- Bidirectional stream relay with byte counting
- Buffered relay for server-side rotation (agent connection stays open)
- Anti-correlation: timing jitter, packet padding (implemented, not wired into request path)
- Payment interceptor: 402 parsing, handler plugin system, dummy handler for testing
- Route manager: Linux policy routing / namespace command generation, macOS noop
- TOML config file + CLI flag support
- Import CLI: NordVPN WireGuard, SSH SOCKS5
- Admin REST API (code exists, feature-gated)
- MCP server with 4 tools (code exists, feature-gated)
- Dockerfile + docker-compose with NordVPN

### Not yet done

- Admin API / MCP server wired into `main.rs` startup
- Tor transport tested end-to-end (feature-gated code exists via `arti-client`)
- Sentinel dVPN transport (stub -- requires Cosmos SDK integration)
- x402 / L402 real payment handlers (stubs exist)
- MITM mode for HTTP response inspection (needed for 402 interception in transparent mode)
- Anti-correlation integration into the proxy pipeline
- Relay statistics collection (admin `/stats` endpoint returns zeroes)
- Tunnel health probing (periodic connectivity checks)
- Mid-stream tunnel rotation for resumable protocols

### Limitations

- **Anti-correlation is opt-in and has costs.** Timing jitter adds latency. Packet padding increases bandwidth. These are trade-offs, not free wins.
- **Mid-stream rotation does not work for LLM streaming APIs.** OpenAI, Anthropic, etc. do not implement SSE resume (`Last-Event-ID`). Streams are pinned to one tunnel.
- **The server still knows it's you.** Tunnel rotation hides you from network observers (ISP, government), not from the API server itself -- you're still sending the same API key.
- **Route management commands are generated, not executed.** Linux policy routing and namespace commands require root. houdinny generates the `ip` commands; you run them.
- **Admin API and MCP server exist but are not started automatically.** They are not yet wired into `main.rs`. The code is complete and tested but requires manual integration to use.

## License

MIT OR Apache-2.0
