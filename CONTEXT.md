# houdinny

A Rust library/proxy that makes agent traffic untraceable by hopping requests across rotating VPN tunnels, with pluggable payment handling for machine-to-machine APIs. Single static binary — any agent in any language just points `HTTP_PROXY` at it.

## Problem

AI agents are the new API consumers. They hit services, pay for access, and orchestrate workflows. But:

- **Network layer is a privacy leak.** Even with private payments, an observer can correlate agent activity by IP, timing, and traffic patterns.
- **Payment layer is visible.** Most agent payment flows (Stripe, on-chain txns) expose who paid whom, when, and how much.
- **No unified solution exists.** VPN tools don't understand payments. Payment tools don't understand network privacy. Agents need both.

## What houdinny does

Sits between your agent and the internet as a local proxy:

1. **Hops traffic across rotating VPN tunnels** — each request goes through a random tunnel. No single observer sees the full picture.
2. **Handles 402 Payment Required flows** — when an API demands payment, houdinny delegates to a payment plugin, pays, and retries. The agent doesn't care how.
3. **Multiplexes streaming connections** — rotates tunnels mid-stream by reconnecting underneath while the agent sees one continuous stream.
4. **Anti-correlation** — timing jitter, packet padding, decoy traffic. Makes it impossible to link requests from the same agent.

## Why Rust

- **Per-socket interface binding** — need fine-grained control over which network interface each connection uses. Go's `net` package abstracts this away.
- **Stream multiplexing** — relay proxy that holds agent-side connection open while rotating server-side connections underneath. Precise buffer control needed.
- **Route/namespace management** — `netlink` syscalls for policy-based routing. Rust's `rtnetlink` crate handles this without C.
- **WireGuard userspace** — `boringtun` (Cloudflare's WireGuard in Rust). Production-proven.
- **Precise timing** — anti-correlation needs exact control over when packets go out. `tokio::time`.
- **Cross-compiles** — single static binary for any OS/arch. No runtime deps.
- **No C needed** — Rust's FFI to libc covers any syscall. No reason to drop lower.

## Architecture

```
houdinny (single Rust binary)
├── proxy server (HTTP/SOCKS5)        ← any agent connects here
├── stream relay / multiplexer        ← SSE resume, chunk reassembly
├── tunnel pool                       ← manages WireGuard/SOCKS/Tor tunnels
│   ├── boringtun (WireGuard)
│   ├── socks5 client
│   └── tor client
├── route manager                     ← netlink, namespaces, policy routing
├── anti-correlation engine           ← jitter, padding, decoys
└── payment handler                   ← MPP / 402 flow
```

### Layer diagram

```
┌─────────────────────────────────────┐
│            Your App / Agent          │
│  (any language — just set HTTP_PROXY)│
└──────────────┬──────────────────────┘
               │
┌──────────────▼──────────────────────┐
│     Payment Plugins (top layer)      │
│  ┌────────┐ ┌──────┐ ┌───────────┐ │
│  │Zcash   │ │Stripe│ │ Lightning │ │  ← handles 402s
│  │(shielded)│      │ │ (BTC)     │ │
│  └────────┘ └──────┘ └───────────┘ │
├──────────────────────────────────────┤
│       houdinny (core proxy)          │
│  Router + Health + Anti-Correlation  │
│  + Stream Relay / Multiplexer        │
├──────────────────────────────────────┤
│   Transport Plugins (bottom layer)   │
│  ┌────┐ ┌───┐ ┌─────┐ ┌──────────┐│
│  │ WG │ │Tor│ │SOCKS│ │ dVPN     ││  ← tunnels
│  └────┘ └───┘ └─────┘ └──────────┘│
│                                      │
│   Transports can be chained:         │
│   WG → Residential Proxy             │
│   Sentinel → Tor                     │
│   etc.                               │
├──────────────────────────────────────┤
│      Route Manager (system layer)    │
│  netlink, namespaces, policy routing │
│  per-tunnel routing tables           │
│  tun/tap device management           │
└─────────────────────────────────────┘
```

## Core Traits (Rust)

```rust
/// Transport plugin — bottom layer (tunnels)
#[async_trait]
trait Transport: Send + Sync {
    async fn connect(&self, addr: &str) -> Result<Box<dyn AsyncReadWrite>>;
    fn healthy(&self) -> bool;
    fn id(&self) -> &str;
    async fn close(&self) -> Result<()>;
}

/// Payment plugin — top layer (handles 402s)
#[async_trait]
trait PaymentHandler: Send + Sync {
    fn can_handle(&self, response: &Response) -> bool;
    async fn pay(&self, details: &PaymentRequest) -> Result<PaymentProof>;
    fn proof_header(&self) -> (&str, &str);
}

/// Router — picks which tunnel per request
trait Router: Send + Sync {
    fn pick(&self, req: &Request) -> Result<Arc<dyn Transport>>;
}
```

## Transport Plugins

| Plugin | What | Status |
|--------|------|--------|
| SOCKS5 | Connect through any SOCKS5 proxy | Build first (simplest) |
| WireGuard | Userspace via boringtun | Phase 2 |
| Tor | Route through Tor circuits | Phase 3 |
| dVPN (Sentinel etc.) | Decentralized VPN networks | Phase 4 |
| Residential Proxy | Exit through real ISP IPs | Phase 5 |

Transports can be **chained**: `WireGuard → Residential Proxy` means traffic enters a WG tunnel and exits through a residential IP. Nobody sees the full path.

## Payment Plugins

| Plugin | What | Status |
|--------|------|--------|
| Zcash (shielded) | Private payments via shielded txns | Demo |
| Lightning (BTC) | Fast BTC micropayments | Future |
| Stripe | Traditional fiat payments | Future |
| MPP (Machine Payment Protocol) | HTTP 402-based agent payments | Core focus |

## How it relates to MPP

MPP (Machine Payment Protocol) defines how agents pay for API access via HTTP 402. houdinny is the **transport + payment layer** that makes MPP private:

- MPP says: "agent gets 402, pays, retries with proof"
- houdinny says: "each of those steps happens through a different tunnel, with different timing, and the payment itself is shielded"

This potentially solves what projects like Zimppy (Zcash MPP) are doing, but at a lower layer. Zimppy handles private payments. houdinny handles private payments AND private networking. They could be complementary (Zimppy as a payment plugin) or houdinny could subsume the payment side entirely.

## Streaming — Tunnel Rotation Mid-Stream

The hard problem: long-lived connections (SSE, WebSockets, gRPC) are bound to a single TCP connection. Rotate the tunnel and the connection breaks.

### The idea: relay proxy with hidden reconnection

The agent sees one stable connection. houdinny holds the agent-side open and rotates the server-side underneath.

```
Agent ←── single stable connection ──→ houdinny (relay)
                                           │
                              rotates underneath, invisible to agent
                                           │
                          Tunnel A ──→ API (tokens 1-50)
                          Tunnel B ──→ API (tokens 51-120)
                          Tunnel C ──→ API (tokens 121-200)

Each segment comes from a different IP.
Agent sees one continuous stream.
Observer sees 3 unrelated short connections.
```

### Reality check

**What works today:**

| Protocol | Works? | Why |
|----------|--------|-----|
| **HTTP Range downloads** | Yes | `Range: bytes=X-` header. Servers widely support this. Different tunnel per chunk, clean. |
| **Independent requests** | Yes | Each new request goes through a different tunnel. This is the core value and it works perfectly. |
| **Between stream sessions** | Yes | Close one conversation, start next on a different tunnel. Practical and immediate. |

**What doesn't work today:**

| Protocol | Problem |
|----------|---------|
| **SSE (LLM APIs)** | Most LLM APIs (OpenAI, Anthropic) stream JSON chunks but **don't implement `Last-Event-ID` resume**. If you disconnect, the response is gone. The SSE spec supports resume, but implementations don't. |
| **WebSocket** | No standard resume. Server sees new connection = new session. |
| **gRPC streaming** | No native resume. Needs application-level checkpointing. |

**Also: the server knows it's you.** Even if you reconnect from a different IP, you're sending the same API key / session token. Mid-stream rotation hides you from **network observers** (ISP, government), not from the **server itself**.

**And: timing correlation.** Connection A closes on tunnel 1, connection B opens on tunnel 2 to the same server 50ms later. A sophisticated observer can correlate this.

### What's actually achievable (v1)

- Rotate between independent requests (the core product)
- Rotate between stream sessions (close conversation, start next on different tunnel)
- Rotate chunks for Range-supporting downloads
- Pin streams to one tunnel when resume isn't supported

### Future (v2) — requires ecosystem change

Mid-stream rotation becomes real if API providers adopt **resumable streaming** as part of MPP/x402 spec. This could be a spec proposal alongside houdinny — "here's the proxy, and here's the protocol extension that makes it work." houdinny becomes the reference implementation.

### For non-resumable protocols (fallback)

houdinny acts as a **buffering relay**:
1. Holds agent-side connection open
2. Buffers incoming data from server-side
3. When it's time to rotate: closes server-side, opens new connection on new tunnel
4. If protocol supports resume: seamless
5. If not: stream stays pinned to one tunnel (honest default)

## Route Management — Multi-Tunnel Without Conflicts

### The problem

When a VPN connects, it typically hijacks the default route:
```
default route → VPN tunnel (wg0)
ALL traffic goes through VPN, including other tunnels
```

This breaks multi-tunnel setups. Tunnel B's traffic goes through Tunnel A.

### The solution: policy-based routing

Each tunnel gets its own routing table. Packets are marked per-connection and routed by mark.

**Linux — network namespaces (cleanest):**
```
# Each tunnel lives in its own namespace — total isolation
ip netns add tunnel_a
ip netns add tunnel_b
ip link set wg0 netns tunnel_a
ip link set wg1 netns tunnel_b

# houdinny dials into the right namespace per request
```

**Linux — policy routing (no namespaces):**
```
ip route add default via 10.0.1.1 table 100   # tunnel A's table
ip route add default via 10.0.2.1 table 200   # tunnel B's table
ip rule add fwmark 1 table 100
ip rule add fwmark 2 table 200
# houdinny sets SO_MARK on each socket
```

**macOS — per-socket binding:**
```
// No network namespaces on macOS
// Bind each socket to a specific utun interface before connect()
socket.bind_device("utun3")?;
socket.connect(target)?;
```

All managed via Rust's `rtnetlink` (Linux) and `socket2` (macOS) crates. No C code needed.

## Anti-Correlation Features

Just rotating IPs isn't enough. A sophisticated observer can still correlate by:

- **Timing** — "request A and B happened 50ms apart, probably same agent"
- **Packet size** — "that 847-byte request was a payment, that 12KB was data"
- **Traffic patterns** — "this agent always hits weather → flights → hotels in sequence"

houdinny counters with:

| Technique | What it does | Feasible? | Trade-off |
|-----------|-------------|-----------|-----------|
| Timing jitter | Random delay (0-N seconds) before sending | Yes, trivial | **Adds latency.** Agents want speed. A 2s random delay kills real-time UX. Best as opt-in for sensitive requests. |
| Packet padding | Normalize all requests to similar sizes | Partially | Works for HTTP. For HTTPS, observer sees TLS record sizes — padding TLS records is non-trivial. "Good enough" not "NSA-proof". |
| Decoy traffic | Fake requests through random tunnels when idle | Yes | **Costs bandwidth and money** if tunnels are paid per GB. Decoys to where? Random domains look suspicious. |
| Request reordering | Batch requests and send in random order | Only if independent | Agent workflows are usually sequential. Can't reorder dependent requests. |

**Honest assessment**: These features provide "good enough" privacy against passive network observers. They do NOT defeat a sophisticated adversary doing active traffic analysis. Timing jitter is the most practical. The others have real costs.

## IP Layering

Raw IP spoofing (forging source IP) doesn't work for TCP — you never get the response back. What works:

| Technique | What it does |
|-----------|-------------|
| **VPN chaining** | Tunnel inside tunnel. Each layer only sees adjacent layers. |
| **VPN + residential proxy** | Exit through a real home ISP IP. Looks like normal user traffic. |
| **VPN + Tor** | Maximum anonymity, slow. |

houdinny supports chaining natively. Each request can go through a different chain.

## Build Order

| Phase | What | Why |
|-------|------|-----|
| 1 | Core proxy + SOCKS5 transport + tunnel chaining | Prove the concept with `ssh -D` tunnels |
| 2 | Stream relay / multiplexer (SSE first) | The differentiator — mid-stream tunnel rotation |
| 3 | Payment plugin interface + dummy 402 handler | Request-402-pay-retry loop |
| 4 | WireGuard transport (boringtun) | Real tunnels |
| 5 | Route manager (netlink, namespaces) | Multi-tunnel without conflicts |
| 6 | Anti-correlation (jitter, padding) | Make rotation actually private |
| 7 | MPP / Zcash payment plugin | Private payments |
| 8 | dVPN transport (Sentinel etc.) | Decentralized tunnels |
| 9 | Residential proxy transport | Anti-fingerprinting |
| 10 | Full demo: agent + private payments + rotating tunnels + stream rotation |

## Prior Art — What Exists vs What Doesn't

### Already exists (don't rebuild — use as deps or integrate)

| What | Exists as | Use in houdinny |
|------|-----------|-----------------|
| Rotating proxy per request | Tons of libs, ProxyMesh, scraping tools | Concept proven — but they're all scraping-focused, not privacy-focused |
| Tor rotation | [rotating-tor-http-proxy](https://github.com/zhaow-de/rotating-tor-http-proxy) | Tor transport plugin can wrap this |
| WireGuard userspace in Rust | `boringtun` (Cloudflare) | Direct dependency for WG transport |
| HTTP 402 payment protocol | [x402](https://github.com/coinbase/x402) (Coinbase), [L402](https://github.com/lightninglabs/L402) (Lightning Labs) | Payment plugin can implement x402/L402 |
| SSE proxy | Tyk, various Go/Node libs | Reference only — they don't rotate mid-stream |
| SOCKS5 in Rust | `tokio-socks` | Direct dependency for SOCKS5 transport |
| Network namespace mgmt | `rtnetlink` crate | Direct dependency for route manager |

### Partially exists (needs gluing)

| What | State |
|------|-------|
| Multi-tunnel pool with health checks | Exists in commercial proxy services (BrightData, Oxylabs) but not as open-source local proxy |
| Transport chaining (VPN → residential) | Possible manually, no library wraps it |
| Payment + proxy combined | x402/L402 handle payment. Proxy tools handle routing. Nobody combines them. |

### Does NOT exist — houdinny's actual value

| What | Novel? | Honest assessment |
|------|--------|-------------------|
| **Mid-stream tunnel rotation** | Yes, but limited today | Works for Range downloads. Doesn't work for LLM SSE streams (APIs don't support resume). Becomes real if houdinny drives a "resumable streaming" spec addition to MPP/x402. **v2 story.** |
| **Anti-correlation engine in a proxy** | Yes, with trade-offs | Jitter adds latency. TLS padding is hard. Decoys cost money. "Good enough" privacy, not perfect. |
| **Payment-aware privacy proxy** | Yes — this is the real moat | x402/L402 handle payment. VPN tools handle privacy. **Nothing combines them.** Use x402/Lightning for speed (not on-chain Zcash — too slow for real-time). |
| **Tunnel-agnostic plugin system** | Not technically hard | Good engineering, not a moat. But first-mover advantage matters. |

### Summary

~60% of components exist as individual pieces. The real moat is being the **first open-source proxy that combines tunnel rotation + automatic payment handling for agents**. The market is moving toward agent commerce (x402 from Coinbase, L402 from Lightning Labs). Nobody has built the privacy transport layer for it yet.

**Strongest realistic pitch**: "Every request your agent makes goes through a different tunnel. Payments happen automatically via x402/Lightning. One binary, any language, `HTTP_PROXY` and done."

Mid-stream rotation is a v2 story once the ecosystem catches up (or houdinny helps push a resumable streaming spec).

## Friends' Projects (potential integrations / marketing)

- **Zimppy** (https://zimppy.xyz, https://github.com/betterclever/zimppy) — Zcash-based MPP payments for AI agents. Could be a payment plugin, or houdinny could be Zimppy's transport layer.
- **Sentinel** (https://sentinel.co, npm: sentinel-ai-connect) — Decentralized VPN with 1000+ nodes, on-chain session auth. Could be a transport plugin.

## MVP (2-day target)

```rust
// 3 SSH tunnels to different servers
let pool = Pool::new(vec![
    Socks5::new("127.0.0.1:1080"),
    Socks5::new("127.0.0.1:1081"),
    Socks5::new("127.0.0.1:1082"),
]);
let proxy = Proxy::new(pool, Strategy::Random);
proxy.listen("127.0.0.1:8080").await?;

// Any agent in any language:
// export HTTP_PROXY=http://127.0.0.1:8080
// python my_agent.py
// curl https://api.example.com
// Each request hops through a different tunnel
```

## Usage

```bash
# Start houdinny
houdinny --tunnels socks5://127.0.0.1:1080,socks5://127.0.0.1:1081,socks5://127.0.0.1:1082

# Any agent just sets the proxy
export HTTP_PROXY=http://127.0.0.1:8080
export HTTPS_PROXY=http://127.0.0.1:8080

# Done. Every request hops through a different tunnel.
```
