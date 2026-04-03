# Toy prediction-market order matcher

Rust workspace with a **single matcher process** that owns the order book, plus **stateless HTTP/WebSocket API** processes that forward traffic. This satisfies the take-home requirement that **multiple API instances** can run without **double-matching** liquidity.

## Demo video

- **YouTube:** (https://www.youtube.com/watch?v=RWFN5K2Mm8w&t=1s)

## Run locally

Terminal 1 — matcher (book + matching + canonical WebSocket feed):

```bash
cargo run -p matcher
```

Terminal 2 & 3 — two API instances (set `MATCHER_URL` to the matcher):

```bash
set MATCHER_URL=http://127.0.0.1:3001
set PORT=3000
cargo run -p api
```

```bash
set MATCHER_URL=http://127.0.0.1:3001
set PORT=3002
cargo run -p api
```

On Unix, use `export MATCHER_URL=...` and `export PORT=...`.

### Docker (two API replicas + matcher)

```bash
docker compose up --build
```

- Matcher: `http://localhost:3001`
- API replicas: `http://localhost:3000` and `http://localhost:3002`

## HTTP API

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/orders` | Body: `{"side":"buy"\|"sell","price":<u64>,"qty":<u64>}`. Response: `{"id":<u64>}`. |
| `GET` | `/orderbook` | JSON snapshot: bids (high → low price), asks (low → high), each level lists resting orders with `id`, `side`, `price`, `qty` (open quantity). |

## WebSocket

- **Matcher**: `ws://<matcher-host>:3001/ws` — fill events as JSON objects (one message per fill).
- **API**: `ws://<api-host>:<port>/ws` — each API instance maintains an outbound connection to the matcher and **re-broadcasts** the same JSON fills to its own clients (so you can hit either API replica and still see all fills).

## Data model (assignment fields preserved)

- `Side`: `Buy`, `Sell`
- `Order`: `id`, `side`, `price`, `qty`, plus **`remaining_qty`** for partial rests/fills (allowed extension).
- `Fill`: `maker_order_id`, `taker_order_id`, `price`, `qty` — the brief used `aty`; this code uses **`qty`** and documents the correction here.

## Tests

```bash
cargo test -p prediction-core
```

---

## README questions (assignment)

### 1. How does the system handle multiple API server instances without double-matching an order?

**Only the matcher process mutates the book.** Every `POST /orders` is executed by forwarding the request to that single service, which runs **one async task** that serializes all `Submit` commands on an **unbounded channel**. Matching is therefore **linearizable**: at most one order is applied to the book at a time, so the same resting quantity cannot be matched twice.

API replicas are **stateless proxies** for HTTP. For WebSocket, each API opens **one** long-lived client connection to the matcher and **fans out** messages locally—fills are still produced once, centrally.

*Trade-off:* the matcher is a **single point of failure** and a **throughput ceiling**; scaling out matching would need sharding (per market), leader election, or an atomic store (e.g. Redis + Lua) with a clear serialization story.

### 2. What data structure did you use for the order book and why?

**`BTreeMap<u64, VecDeque<Order>>` on each side.**

- **Price priority:** bids use the map sorted ascending and we always take the **best bid** as `iter().next_back()` (highest price). Asks use `iter().next()` (lowest ask). Both are **O(log n)** to locate a price level.
- **Time priority:** at each price, **`VecDeque`** gives FIFO for resting orders at that level.

Alternatives such as **`BinaryHeap`** are viable if tie-breaking is encoded in the key (e.g. `(Reverse(price), sequence)`); `BTreeMap` keeps price levels explicit and iteration order straightforward for snapshots.

### 3. What breaks first if this were under real production load?

Roughly in order:

1. **Matcher throughput** — single-threaded matching and JSON WebSocket fan-out become CPU- and allocation-bound.
2. **No persistence** — restart drops the book; no recovery or audit trail.
3. **`broadcast` lag** — slow or many WebSocket clients can lag and **drop** intermediate fill messages (`RecvError::Lagged`).
4. **API → matcher availability** — if the matcher is down, every API returns errors; there is no queueing or backpressure contract beyond HTTP failures.
5. **Operational limits** — file descriptors, connection counts, and lack of rate limiting or auth.

### 4. What would you build next if you had another ~4 hours?

- **Persistence:** append-only log of commands and/or periodic snapshots + replay on startup.
- **Integration tests** spanning matcher + two API processes (or Docker Compose) for HTTP and WebSocket.
- **Idempotency** keys on `POST /orders` and structured error types.
- **Metrics** (latency histograms, book depth, match rate) and **health/readiness** endpoints.
- **Multiple markets** (symbol in the URL) with one matcher task per symbol or a partitioned book map.

## Layout

| Crate | Role |
|-------|------|
| `prediction-core` | `Order`, `Fill`, `Side`, and `OrderBook` matching (pure, unit-tested). |
| `matcher` | Axum server, command channel, in-memory book, fill `broadcast`, `/ws`. |
| `api` | Axum reverse proxy + WebSocket relay to local subscribers. |
