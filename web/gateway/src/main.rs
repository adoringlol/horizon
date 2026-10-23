#![forbid(unsafe_code)]
mod auth;
mod route;

use axum::{
    Json, Router,
    extract::{
        DefaultBodyLimit, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex, watch},
};
use tower_http::{
    cors::{AllowOrigin, CorsLayer},
    services::ServeDir,
};

type Client = Arc<Mutex<ClientState>>;
struct ClientState {
    route: route::Route,
    account: Option<Arc<auth::Account>>,
    device: Option<auth::Device>,
    deadline: Instant,
    epoch: watch::Sender<u64>,
    polling: bool,
}

#[derive(Clone)]
struct App {
    clients: Arc<Mutex<HashMap<String, Client>>>,
    key: String,
    origins: Vec<String>,
    targets: Vec<String>,
}

struct ApiError(StatusCode, String);
impl From<String> for ApiError {
    fn from(e: String) -> Self {
        Self(StatusCode::BAD_REQUEST, e)
    }
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.0,
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({"error": self.1})),
        )
            .into_response()
    }
}
type ApiResult = Result<Json<Value>, ApiError>;

fn check_origin(app: &App, h: &HeaderMap) -> Result<(), ApiError> {
    let origin = h
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !app.origins.iter().any(|o| o == origin) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "This browser origin is not enabled on the gateway.".into(),
        ));
    }
    Ok(())
}

async fn client(app: &App, headers: &HeaderMap) -> Result<Client, ApiError> {
    check_origin(app, headers)?;
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or("");
    lookup(app, token).await
}

async fn lookup(app: &App, token: &str) -> Result<Client, ApiError> {
    let c = app
        .clients
        .lock()
        .await
        .get(token)
        .cloned()
        .ok_or_else(|| {
            ApiError(
                StatusCode::UNAUTHORIZED,
                "Connect to the gateway again.".into(),
            )
        })?;
    if c.lock().await.deadline <= Instant::now() {
        app.clients.lock().await.remove(token);
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "The gateway session expired. Connect again.".into(),
        ));
    }
    Ok(c)
}

#[derive(Deserialize)]
struct Open {
    #[serde(default)]
    gateway_key: String,
}
async fn open(State(app): State<App>, h: HeaderMap, Json(body): Json<Open>) -> ApiResult {
    check_origin(&app, &h)?;
    if !app.key.is_empty() && !same_secret(&app.key, &body.gateway_key) {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "The gateway key is incorrect.".into(),
        ));
    }
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "No secure random source.".into(),
        )
    })?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let c = ClientState {
        route: Default::default(),
        account: None,
        device: None,
        polling: false,
        deadline: Instant::now() + Duration::from_secs(7200),
        epoch: watch::channel(0).0,
    };
    let mut clients = app.clients.lock().await;
    if clients.len() >= 256 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "The gateway is full.".into(),
        ));
    }
    clients.insert(token.clone(), Arc::new(Mutex::new(c)));
    Ok(Json(
        json!({"session": token, "expires_in": 7200, "protocol": 777}),
    ))
}

fn same_secret(a: &str, b: &str) -> bool {
    let mut difference = a.len() ^ b.len();
    for (i, byte) in a.as_bytes().iter().enumerate() {
        difference |= (*byte ^ b.as_bytes().get(i).copied().unwrap_or(0)) as usize;
    }
    difference == 0
}

async fn set_route(
    State(app): State<App>,
    h: HeaderMap,
    Json(route): Json<route::Route>,
) -> ApiResult {
    route.parsed()?;
    let c = client(&app, &h).await?;
    let mut c = c.lock().await;
    c.route = route;
    c.device = None;
    c.polling = false;
    c.epoch.send_modify(|v| *v += 1);
    Ok(Json(
        json!({"account": c.account.as_ref().map(|a| a.public()), "reconnect_required": true}),
    ))
}

async fn sign_in(State(app): State<App>, h: HeaderMap) -> ApiResult {
    let c = client(&app, &h).await?;
    let (route, epoch) = {
        let mut c = c.lock().await;
        c.epoch.send_modify(|v| *v += 1);
        c.device = None;
        c.polling = false;
        let epoch = *c.epoch.borrow();
        (c.route.clone(), epoch)
    };
    let (device, public) = auth::start(&route).await?;
    let mut c = c.lock().await;
    if *c.epoch.borrow() != epoch {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "The route changed. Start sign-in again.".into(),
        ));
    }
    c.device = Some(device);
    Ok(Json(public))
}

async fn poll_sign_in(State(app): State<App>, h: HeaderMap) -> ApiResult {
    let c = client(&app, &h).await?;
    let (route, mut device, epoch) = {
        let mut c = c.lock().await;
        if c.polling {
            return Ok(Json(json!({"pending": true})));
        }
        let device = c
            .device
            .take()
            .ok_or_else(|| ApiError(StatusCode::CONFLICT, "Start sign-in first.".into()))?;
        if device.next_poll > Instant::now() {
            c.device = Some(device);
            return Ok(Json(json!({"pending": true})));
        }
        c.polling = true;
        let epoch = *c.epoch.borrow();
        (c.route.clone(), device, epoch)
    };
    let result = auth::poll(&route, &device).await;
    let mut c = c.lock().await;
    if *c.epoch.borrow() != epoch {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "The route changed. Start sign-in again.".into(),
        ));
    }
    c.polling = false;
    match result? {
        auth::Poll::Done(account) => {
            let public = account.public();
            c.account = Some(Arc::new(account));
            Ok(Json(json!({"account": public})))
        }
        p => {
            if matches!(p, auth::Poll::Slower) {
                device.interval = (device.interval + 5).min(60);
            }
            device.next_poll = Instant::now() + Duration::from_secs(device.interval);
            c.device = Some(device);
            Ok(Json(json!({"pending": true})))
        }
    }
}

async fn logout(State(app): State<App>, h: HeaderMap) -> ApiResult {
    let c = client(&app, &h).await?;
    let mut c = c.lock().await;
    c.account = None;
    c.device = None;
    c.polling = false;
    c.epoch.send_modify(|v| *v += 1);
    Ok(Json(json!({"ok": true})))
}

#[derive(Deserialize)]
struct Join {
    server_hash: String,
}
async fn join(State(app): State<App>, h: HeaderMap, Json(body): Json<Join>) -> ApiResult {
    let c = client(&app, &h).await?;
    let (route, account, epoch) = {
        let c = c.lock().await;
        let account = c
            .account
            .clone()
            .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Sign in first.".into()))?;
        let epoch = *c.epoch.borrow();
        (c.route.clone(), account, epoch)
    };
    account.join(&route, &body.server_hash).await?;
    if *c.lock().await.epoch.borrow() != epoch {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "The route changed during login. Reconnect.".into(),
        ));
    }
    Ok(Json(json!({"ok": true})))
}

async fn websocket(
    State(app): State<App>,
    h: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    check_origin(&app, &h)?;
    Ok(upgrade
        .max_message_size(2 * 1024 * 1024)
        .on_upgrade(|socket| tunnel(socket, app)))
}

#[derive(Deserialize)]
struct Connect {
    session: String,
    host: String,
    port: u16,
}
async fn tunnel(mut ws: WebSocket, app: App) {
    if let Err(e) = tunnel_inner(&mut ws, &app).await {
        let _ = ws
            .send(Message::Text(json!({"error": e}).to_string().into()))
            .await;
    }
    let _ = ws.close().await;
}

async fn tunnel_inner(ws: &mut WebSocket, app: &App) -> Result<(), String> {
    let first = tokio::time::timeout(Duration::from_secs(10), ws.recv())
        .await
        .map_err(|_| "Gateway handshake timed out.")?;
    let Some(Ok(Message::Text(first))) = first else {
        return Err("Send the gateway connection request first.".into());
    };
    if first.len() > 8192 {
        return Err("Gateway connection request is too large.".into());
    }
    let connect: Connect =
        serde_json::from_str(&first).map_err(|_| "Invalid gateway connection request.")?;
    if connect.host.is_empty()
        || connect.host.len() > 253
        || connect.host.chars().any(|c| {
            c.is_control() || c.is_whitespace() || matches!(c, '/' | '\\' | '@' | '#' | '?')
        })
        || connect.port == 0
    {
        return Err("Invalid Minecraft server address.".into());
    }
    if !app.targets.is_empty()
        && !app
            .targets
            .iter()
            .any(|h| h.eq_ignore_ascii_case(&connect.host))
    {
        return Err("This server is not enabled on the gateway.".into());
    }
    let c = lookup(app, &connect.session).await.map_err(|e| e.1)?;
    let (route, mut epoch, deadline) = {
        let c = c.lock().await;
        (c.route.clone(), c.epoch.subscribe(), c.deadline)
    };
    // Cancellation surrounds the entire I/O future, including connection and
    // backpressured writes. A stalled destination cannot delay a route change.
    let transfer = async {
        let mut tcp = route.connect(&connect.host, connect.port).await?;
        ws.send(Message::Text(json!({"connected": true}).to_string().into()))
            .await
            .map_err(|_| "Browser disconnected.")?;
        let mut buf = vec![0; 65536];
        loop {
            tokio::select! {
                incoming = ws.recv() => match incoming {
                    Some(Ok(Message::Binary(bytes))) => tcp.write_all(&bytes).await.map_err(|_| "Minecraft server disconnected.")?,
                    Some(Ok(Message::Ping(bytes))) => ws.send(Message::Pong(bytes)).await.map_err(|_| "Browser disconnected.")?,
                    Some(Ok(Message::Pong(_))) => {},
                    _ => return Ok(()),
                },
                read = tcp.read(&mut buf) => {
                    let n = read.map_err(|_| "Minecraft connection failed.")?;
                    if n == 0 { return Ok(()); }
                    ws.send(Message::Binary(buf[..n].to_vec().into())).await.map_err(|_| "Browser disconnected.")?;
                }
            }
        }
    };
    tokio::select! {
        _ = epoch.changed() => Err("The route or account changed. Reconnect.".into()),
        _ = tokio::time::sleep_until(deadline.into()) => Err("Gateway session expired. Connect again.".into()),
        result = transfer => result,
    }
}

fn router(app: App, static_dir: PathBuf) -> Router {
    let origins: Vec<_> = app.origins.iter().filter_map(|s| s.parse().ok()).collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);
    Router::new()
        .route("/api/session", post(open))
        .route("/api/route", post(set_route))
        .route("/api/auth/start", post(sign_in))
        .route("/api/auth/poll", post(poll_sign_in))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/join", post(join))
        .route("/api/tunnel", get(websocket))
        .route(
            "/health",
            get(|| async { Json(json!({"ok": true, "protocol": 777})) }),
        )
        .fallback_service(ServeDir::new(static_dir))
        .layer(DefaultBodyLimit::max(8192))
        .layer(cors)
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            header::HeaderValue::from_static("no-store"),
        ))
        .with_state(app)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind: SocketAddr = std::env::var("HORIZON_GATEWAY_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()?;
    let key = std::env::var("HORIZON_GATEWAY_KEY").unwrap_or_default();
    if !bind.ip().is_loopback() && key.len() < 24 {
        return Err(
            "Set a HORIZON_GATEWAY_KEY of at least 24 characters before exposing the gateway."
                .into(),
        );
    }
    let origins = std::env::var("HORIZON_WEB_ORIGINS")
        .unwrap_or_else(|_| {
            format!(
                "http://127.0.0.1:{},http://localhost:{}",
                bind.port(),
                bind.port()
            )
        })
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let targets = std::env::var("HORIZON_GATEWAY_TARGETS")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    let app = App {
        clients: Default::default(),
        key,
        origins,
        targets,
    };
    let reaper = app.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;
            let entries: Vec<_> = reaper
                .clients
                .lock()
                .await
                .iter()
                .map(|(k, c)| (k.clone(), c.clone()))
                .collect();
            for (k, c) in entries {
                if c.lock().await.deadline <= Instant::now() {
                    reaper.clients.lock().await.remove(&k);
                }
            }
        }
    });
    let static_dir = std::env::var_os("HORIZON_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../public"));
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("Horizon web gateway: http://{bind}");
    axum::serve(listener, router(app, static_dir)).await?;
    Ok(())
}

#[cfg(test)]
mod tests;
