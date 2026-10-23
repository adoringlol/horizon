# Horizon browser port (experimental)

This fork adds a real Rust/WebAssembly client entry point, a custom connection/account menu, and a native Rust WebSocket gateway. It **is not the complete Horizon game in a browser yet**. The current client checks servers, implements the offline/online login phases, handles configuration/compression/encryption, receives world chunks and renders a bounded terrain preview with WebGPU. Local resource bundles enable textures and native baked block models. The gateway supports direct connections, SOCKS5 with proxy-side DNS, and HTTP CONNECT.

## Run on Windows

From this checkout, with Rust/Cargo installed:

```powershell
cargo install wasm-bindgen-cli --version 0.2.129 --locked
./web/build.ps1
cargo run --locked --manifest-path web/gateway/Cargo.toml
```

Open **http://127.0.0.1:8787** in a WebGPU-capable browser. The browser bundle is generated into `web/public/pkg/`; it is not checked in. The two Cargo workspaces under `web/` and `web/gateway/` deliberately avoid resolving the desktop launcher, drivers and Wasmtime. They reuse the original `horizon-core`, `horizon-world`, `horizon-protocol`, `horizon-assets` and `player_sync.rs` sources without changing native behavior.

For a quick build use `./web/build.ps1 -Debug`. On other systems run `rustup target add wasm32-unknown-unknown`, then from `web/`:

```sh
cargo build --locked --release --target wasm32-unknown-unknown --lib
wasm-bindgen target/wasm32-unknown-unknown/release/horizon_web.wasm --target web --out-dir public/pkg
cargo run --locked --manifest-path gateway/Cargo.toml
```

On Windows, `./web/package.ps1` creates `web/target/horizon-web-windows.zip` containing the release gateway, browser bundle and `run.ps1`. Extract the whole ZIP and run that script to use it without installing Rust. `-NoBuild` packages an already built release. This bundle has the same experimental terrain-viewer scope as the source build.

The `browser port` workflow builds and uploads `web/public/` as the `horizon-web` artifact. Host that directory over HTTPS and run the gateway separately, or let the gateway serve it directly behind a TLS reverse proxy.

## Menu and route switching

1. Set **Gateway address** to the HTTP(S) origin of your trusted Horizon gateway.
2. Set its key if required. Leave **Egress proxy** empty for the gateway's own connection, or enter `socks5h://host:1080` or `http://host:8080`. Credentials may be included as percent-encoded `username:password@host`.
3. **Save & apply route** saves its non-secret address settings and applies the route. An active game connection is closed and reconnected. A new connection starts with fresh compression, cipher, registry and world state; old browser callbacks and pending account requests cannot replace it.
4. Enter a server address and choose **Check server** or **Connect**. The current client speaks Horizon's **26.3 / protocol 777**; older servers require a compatible translating proxy. The gateway transports bytes and does not translate versions.
5. Drag on the exposed preview to look around. This milestone does not implement player movement or actions.

Selecting a saved route fills the fields; applying it makes the change. Proxy passwords and gateway keys are never saved in browser storage. A profile retains an address with credentials removed. Re-enter credentials when loading that profile. Account tokens never reach the browser. Switching gateway origins creates a separate account session and requires signing in again; switching egress on the same gateway retains the account, and subsequent account/join requests use the new egress.

## Local textures and block models

Choose **Resources → Load original demo assets** when connecting to the fixture server below. These procedural test textures and models are original and match only the fixture's block IDs.

For a real 26.3 server, first prepare resources through Horizon's existing native preparation flow (see the repository's main README). A client JAR by itself is insufficient: block facts, property schemas and rendering tables also come from native preparation. Export that prepared folder:

```powershell
cargo run --locked --manifest-path web/Cargo.toml --example export_resources -- C:/path/to/prepared/26.3 web/target/local.horizon.zip
```

Then import `local.horizon.zip` in **Resources**. The browser reads it locally, without uploading it to the gateway. It stays in memory for this tab; import again after a page reload. Changing proxies retains the loaded resources. A failed import retains the previous bundle. **Clear resources** returns to placeholder cubes without disconnecting.

The exporter copies selected models, blockstates, textures and prepared tables; it does not download or execute game files. Bundles require data version 5023, are limited to 128 MiB compressed / 256 MiB expanded, and use a Git-ignored `.horizon.zip` extension. The exporter refuses `web/public/` as an output location. Keep game resource bundles private and out of public hosting and packages.

The browser reuses native property binding, weighted/multipart selection, parent resolution, model rotations, UV locking, quad baking, and optional prepared face-occlusion rules. Without occlusion tables it conservatively removes only certainly-full opaque faces. WebGPU uses an sRGB atlas, alpha-cutout rendering, and a separate sorted translucent pass. Textures are currently reduced to 16×16 and animation metadata selects the initial frame only. Lighting/AO, biome-aware tint, fluid meshes, special block entities, animation, and complete rendering parity remain pending. Meshing stays bounded to 3×3 chunks and 48 blocks above/below the camera on the main thread.

## Microsoft account routing

The gateway defaults to the public Minecraft Launcher client ID **`00000000402b5328`**, using Live Connect device authorization and the `service::user.auth.xboxlive.com::MBI_SSL` scope. No application setup or client secret is needed to request a sign-in code. Device-code issuance with this ID was verified against Microsoft's live endpoint on 2026-10-23. The Azure v2 endpoint rejects this ID, so it must not be substituted into the Azure flow.

To use your own approved Azure application instead, override the public ID:

```powershell
$env:HORIZON_MSA_CLIENT_ID = 'your-approved-public-client-id'
cargo run --locked --manifest-path web/gateway/Cargo.toml
```

The account chain is Microsoft OAuth, Xbox Live, XSTS, Minecraft token exchange, ownership check, profile, then the session-server join. All these API calls use the selected egress, including the server join after changing egress. There is no direct fallback when a proxy fails. Live Connect's ID, scope, cookie and unprefixed Xbox RPS ticket are paired; custom Azure IDs retain the Azure endpoints and `d=` ticket prefix used by `launcher/src/account.rs`. The flow is pinned to each device code. The Live Connect reference is [PrismarineJS's implementation](https://github.com/PrismarineJS/prismarine-auth/blob/master/src/TokenManagers/LiveTokenManager.js), with its [scope and Xbox ticket selection](https://github.com/PrismarineJS/prismarine-auth/blob/master/src/MicrosoftAuthFlow.js).

The Microsoft verification webpage opens normally in your browser. **Its page requests use the browser's own network settings**, which a web app cannot replace. Route the browser itself through a proxy/VPN if that page must use the same exit. This gateway does not reverse-proxy a Microsoft password page.

Account tokens live only in gateway memory, scoped to an unguessable per-tab session. Sign-out removes them; sessions expire after two hours or a gateway restart. Refresh tokens returned by Live Connect are discarded, and no refresh token is persisted. Sign in again when the account token expires. Completing Microsoft login and online-mode server admission still need verification with an account; device-code issuance and the local fixture do not prove either.

## Gateway configuration

| Environment variable | Default / purpose |
| --- | --- |
| `HORIZON_GATEWAY_BIND` | `127.0.0.1:8787` |
| `HORIZON_GATEWAY_KEY` | Empty locally; at least 24 characters required for a non-loopback bind |
| `HORIZON_WEB_ORIGINS` | Comma-separated exact allowed browser origins; defaults to local `127.0.0.1` and `localhost` on the bind port |
| `HORIZON_GATEWAY_TARGETS` | Optional comma-separated permitted Minecraft hostnames; empty allows any target the gateway can reach |
| `HORIZON_WEB_DIR` | Directory to serve; defaults to `web/public/` |
| `HORIZON_MSA_CLIENT_ID` | Defaults to public Minecraft Launcher `00000000402b5328`; may be overridden with your own approved application |
| `HORIZON_MSA_FLOW` | Auto-selects `live` for the launcher ID, `azure` for custom IDs; optional explicit override |

Remote gateways require HTTPS/WSS at the reverse proxy. A gateway processes account tokens, so use one you operate or trust. Origin checks, a public-bind key requirement, per-session state and bounded handshakes are included. This is an experimental self-hosted service, not an anonymously exposed public relay. No changes to the existing Minecraft/Velocity services are needed.

## Local verification

```powershell
cargo test --locked --manifest-path web/Cargo.toml --lib
cargo test --locked --manifest-path web/gateway/Cargo.toml
cargo run --locked --manifest-path web/Cargo.toml --example fixture_server
```

The fixture binds **127.0.0.1:25579**, uses synthetic original blocks and sends a compressed login/configuration/terrain stream. Check and connect to that address through the browser menu, then load the original demo assets under Resources. It includes cubes, slabs, rotated stairs, cutout leaves and translucent glass. It needs no Minecraft files or account. It is a protocol/preview fixture, not a substitute for testing a real game server.

Verified locally on Windows with Rust 1.99.1 and Chrome: WASM initialization, WebGPU initialization, server-list check, offline login, nine compressed chunks rendered, teleport/batch acknowledgements and reconnect by applying a saved route. The resource milestone additionally verified demo model/texture loading into an active world and clearing resources without disconnecting, with no WebGPU errors reported. All eight browser-core tests pass, including native model import/rotation, nonzero air states, opaque/partial/cutout culling, translucency and invalid archives. The fixture ZIP exporter ran successfully. Automated selection of a local ZIP was blocked by the Chrome extension's file-access setting, so the file-picker path and a real prepared Minecraft bundle remain unverified.

The release gateway with the public launcher ID also obtained a real Microsoft device code and an `authorization_pending` poll response without any ID configuration. Gateway tests cover direct binary transport, route cancellation, SOCKS5 remote DNS, HTTP CONNECT/credentials and account HTTP routing. Connection tests cover fragmentation, changing compression within a message and the RSA/AES login boundary. Completing the Microsoft approval and admission to an online-mode game server remain unverified.

## Remaining port work

- Complete terrain lighting/AO, biome tint, animated textures, fluids and special block/entity rendering; add browser persistence and worker-based resource preparation. The native model decoder/baker and local texture import are now connected to WebGPU, with bounded main-thread meshing.
- Connect the existing player simulation, movement, collisions, input, inventory, interaction, combat, entities, chat sending and sounds to the browser loop. The preview stays at the server-provided position and only changes look.
- Add worker scheduling and asynchronous asset/world storage for world generation and integrated singleplayer.
- Replace desktop integrations and the embedded native Wasmtime host with web-compatible versions or explicit feature gates.
- Complete authentic Microsoft login and a real online-mode game connection through each proxy type (public launcher device-code issuance is verified).
- Establish render/gameplay parity, browser memory/performance budgets and reconnect tests against real 26.3 servers.

Next handoff: connect native player simulation and collision resources to browser input and movement, then entities and inventory. Real prepared 26.3 resource import still needs validation with a complete local installation; the default local installation currently contains metadata only. Keep native desktop code working and reuse the existing mechanics rather than inventing replacement rules. No claim of full game or vanilla rendering parity is made for this milestone.
