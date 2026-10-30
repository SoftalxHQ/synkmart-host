# SynkMart Host

Local LAN authority for offline POS. Runs on the store Windows PC as a background service (or console process while developing).

## Stack

- Rust + Axum HTTP API on `0.0.0.0:8787`
- SQLite in WAL mode (only this process writes the DB)
- Optional cloud bootstrap against Rails (`GET /api/v1/host/seed`)

POS clients (Tauri) never open the SQLite file. They call this host over HTTP.

## Quick start (dev)

```bash
cd synkmart-host
cargo run -- --console --bind 127.0.0.1:8787
```

Health check:

```bash
curl http://127.0.0.1:8787/health
```

Local bootstrap (no cloud):

```bash
curl -X POST http://127.0.0.1:8787/api/v1/setup/local \
  -H 'content-type: application/json' \
  -d '{"business_name":"Demo Shop","admin_name":"Ada","admin_email":"ada@demo.test","admin_password":"password"}'
```

Then open SynkMart POS and sign in with that admin. Extra terminals pair with the code from `GET /api/v1/setup/pairing`.

## Windows service

Build a release binary, then install with the bundled PowerShell script:

```powershell
cargo build --release
.\scripts\install-windows.ps1 -BinaryPath .\target\release\synkmart-host.exe
```

The service name is `SynkMartHost`. Data lives under `%LOCALAPPDATA%\SynkMart\host\store.db`.

## Installer roles

| Mode | What gets installed |
| --- | --- |
| Host + Client | `synkmart-host` Windows service + SynkMart POS (points at `http://127.0.0.1:8787`) |
| Client only | SynkMart POS only; setup wizard pairs to an existing host IP + pairing code |

See [`../synkmart-business/INSTALL.md`](../synkmart-business/INSTALL.md).

## API (milestone 1B)

- `GET /health`
- `POST /api/v1/setup/local` · `POST /api/v1/setup/cloud` · `GET /api/v1/setup/pairing` · `POST /api/v1/setup/pair`
- `POST /api/v1/auth/session` · `DELETE /api/v1/auth/session` · `GET /api/v1/auth/me` · `GET /api/v1/merchant`
- `GET /api/v1/products` · `GET /api/v1/products/{id}` · `GET /api/v1/categories`
- `GET|POST /api/v1/register_sessions` · `POST /api/v1/register_sessions/{id}/close`
- `GET|POST /api/v1/sales` · `GET /api/v1/sales/{id}` (sales accept `Idempotency-Key`)

Cloud sync is intentionally out of scope for this milestone (`sync_status: not_configured`).
