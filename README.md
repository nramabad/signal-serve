# signal-serve

Pure-Rust Signal REST API daemon. Replaces `signal-cli-rest-api` (JVM) with native binary (~14 MB), no JRE, fast startup, low memory.

## Features

- **REST endpoints**: `/v1/send`, `/v1/receive/{number}`, `/v1/accounts`, `/v1/health`, `/v1/contacts`
- **SSE streaming**: `/v1/receive/{number}` (Server-Sent Events for real-time messages)
- **SQLite store**: Uses `signal-cli` data directory (`~/.local/share/signal-cli` by default)
- **Contact resolution**: UUID + phone number matching (fixes hermes `sendTyping` spam)
- **Cross-compile**: `x86_64` and `aarch64` via `Cross.toml` + `cross`
- **Static binary**: `musl` target, runs on GL-MT6000 (OpenWrt) and macOS

## Quick Start

```bash
# Build (native)
cargo build --release

# Cross-compile for router (aarch64-musl)
cross build --release --target aarch64-unknown-linux-musl

# Run
./target/release/signal-serve serve \
  --store /home/user/.local/share/signal-cli \
  --listen 0.0.0.0:8088
```

## Endpoints

| Method | Path | Description |
|--------|------|-------------|
| GET | `/v1/health` | Health check (204 No Content) |
| GET | `/v1/accounts` | List registered Signal accounts |
| GET | `/v1/contacts` | List contacts (name, number, UUID) |
| POST | `/v1/send` | Send message (JSON: `recipients`, `message`, `attachments?`) |
| GET | `/v1/receive/{number}` | SSE stream for incoming messages |
| POST | `/v1/receive/{number}/ack` | Acknowledge receipt |
| POST | `/v1/link` | Generate QR code for new device linking |
| GET | `/v1/qr/{uuid}` | Get SVG QR code for linking |

### Send Message Example

```bash
curl -X POST http://localhost:8088/v1/send \
  -H "Content-Type: application/json" \
  -d '{"recipients": ["+15551234567"], "message": "Hello from signal-serve!"}'
```

### SSE Stream Example

```bash
curl -N http://localhost:8088/v1/receive/+15551234567
# Returns: data: {"type":"message","envelope":{...}}\n\n
```

## Hermes Integration

Hermes Gateway expects:
- `SIGNAL_HTTP_URL=http://host:8088` (points to signal-serve)
- `SIGNAL_ACCOUNT=+15551234567` (your registered number)
- `SIGNAL_ALLOWED_USERS=+1555...,UUID...` (comma-separated)

signal-serve resolves contacts by **UUID first**, then phone number — matching Hermes' `sendTyping` flow.

## Cross-Compilation (Router)

```bash
# Prerequisites: cross, musl target
cargo install cross
rustup target add aarch64-unknown-linux-musl

# Build
cross build --release --target aarch64-unknown-linux-musl

# Output: target/aarch64-unknown-linux-musl/release/signal-serve
# Copy to router USB: /mnt/usb/signal-serve-bin
```

`Cross.toml` configures:
- `openssl` vendored (avoids cross-compile OpenSSL hell)
- `sqlite3` bundled
- Static musl linking

## Binary Releases

Pre-built static binaries in repo root:
- `signal-serve` — x86_64 Linux (musl)
- `signal-serve-arm64` — aarch64 Linux (musl)
- `signal-serve-bin` — macOS (for local dev)

## Architecture

```
signal-serve (Rust, axum)
    │
    ├── presage (Signal protocol library)
    │   └── libsignal-service (Rust port)
    │
    ├── SQLite store (signal-cli compatible)
    │   └── ~/.local/share/signal-cli/data/
    │
    └── REST + SSE server (port 8088)
```

## Why Not signal-cli-rest-api?

| Aspect | signal-cli-rest-api | signal-serve |
|--------|---------------------|--------------|
| Runtime | Java 17+ (JRE ~200 MB) | Native binary (~14 MB) |
| Startup | ~10-30 sec | <1 sec |
| Memory | ~300-500 MB | ~30-50 MB |
| Cross-compile | Docker multi-arch | `cross` + musl |
| Hermes `sendTyping` | Broken (phone-only match) | Fixed (UUID first) |

## License

MIT