use crate::{
    renderer::Renderer,
    resources::{MAX_ARCHIVE, Resources},
    session::{Effect, Session},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsCast, JsValue, closure::Closure, prelude::wasm_bindgen};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    Event, HtmlCanvasElement, HtmlElement, HtmlInputElement, HtmlSelectElement, MessageEvent,
    WebSocket,
};

type Shared = Rc<RefCell<App>>;
#[derive(Clone, Serialize, Deserialize)]
struct Profile {
    name: String,
    gateway: String,
    proxy: String,
}

struct Network {
    ws: WebSocket,
    _open: Closure<dyn FnMut(Event)>,
    _message: Closure<dyn FnMut(MessageEvent)>,
    _close: Closure<dyn FnMut(Event)>,
    _error: Closure<dyn FnMut(Event)>,
}
impl Drop for Network {
    fn drop(&mut self) {
        self.ws.set_onopen(None);
        self.ws.set_onmessage(None);
        self.ws.set_onclose(None);
        self.ws.set_onerror(None);
        let _ = self.ws.close();
    }
}

struct App {
    profiles: Vec<Profile>,
    gateway: String,
    token: String,
    epoch: u64,
    account: Option<Value>,
    net: Option<Network>,
    session: Option<Session>,
    connected: bool,
    renderer: Option<Renderer>,
    resources: Option<Rc<Resources>>,
    resource_epoch: u64,
    dirty: bool,
    last_mesh: f64,
    last_tick: f64,
    polling: bool,
    dragging: bool,
}

fn window() -> web_sys::Window {
    web_sys::window().expect("browser Window")
}
fn doc() -> web_sys::Document {
    window().document().expect("browser Document")
}
fn element(id: &str) -> web_sys::Element {
    doc().get_element_by_id(id).expect("static menu element")
}
fn input(id: &str) -> String {
    element(id)
        .unchecked_into::<HtmlInputElement>()
        .value()
        .trim()
        .to_owned()
}
fn set_input(id: &str, value: &str) {
    element(id)
        .unchecked_into::<HtmlInputElement>()
        .set_value(value);
}
fn text(id: &str, value: &str) {
    element(id).set_text_content(Some(value));
}
fn hidden(id: &str, value: bool) {
    element(id)
        .unchecked_into::<HtmlElement>()
        .set_hidden(value);
}
fn status(value: &str) {
    text("status", value);
}
fn js_error(value: JsValue) -> String {
    value
        .as_string()
        .unwrap_or_else(|| "The browser operation failed.".into())
}

fn install_resources(app: &Shared, epoch: u64, bytes: &[u8]) {
    if app.borrow().resource_epoch != epoch {
        return;
    }
    match Resources::load(bytes) {
        Ok(resources) => {
            text(
                "resource-status",
                &format!(
                    "{} · {} block states · {} × {} texture atlas. Loaded for this tab.",
                    resources.name,
                    resources.registry.len(),
                    resources.atlas.side,
                    resources.atlas.side
                ),
            );
            let resources = Rc::new(resources);
            let mut app = app.borrow_mut();
            if let Some(renderer) = &mut app.renderer {
                renderer.set_resources(Some(resources.clone()));
            }
            app.resources = Some(resources);
            app.dirty = true;
        }
        Err(e) => text(
            "resource-status",
            &format!("Import failed: {e} Previous resources are still active."),
        ),
    }
}

fn resource_controls(app: &Shared) -> Result<(), JsValue> {
    click(app, "resource-demo", |app| {
        app.borrow_mut().resource_epoch += 1;
        let epoch = app.borrow().resource_epoch;
        install_resources(&app, epoch, &crate::resources::fixture::bundle());
    })?;
    click(app, "resource-clear", |app| {
        let mut app = app.borrow_mut();
        app.resource_epoch += 1;
        app.resources = None;
        if let Some(renderer) = &mut app.renderer {
            renderer.set_resources(None);
        }
        app.dirty = true;
        text(
            "resource-status",
            "No resource bundle loaded. Terrain uses placeholder colors.",
        );
        set_input("resource-file", "");
    })?;
    let weak = Rc::downgrade(app);
    let change = Closure::<dyn FnMut(Event)>::new(move |_| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let Some(file) = element("resource-file")
            .unchecked_into::<HtmlInputElement>()
            .files()
            .and_then(|f| f.get(0))
        else {
            return;
        };
        app.borrow_mut().resource_epoch += 1;
        let epoch = app.borrow().resource_epoch;
        if file.size() > MAX_ARCHIVE as f64 {
            text(
                "resource-status",
                "Import failed: the resource bundle exceeds 128 MiB.",
            );
            return;
        }
        text("resource-status", "Reading and baking local models…");
        spawn_local(async move {
            match JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => {
                    install_resources(&app, epoch, &js_sys::Uint8Array::new(&buffer).to_vec())
                }
                Err(e) if app.borrow().resource_epoch == epoch => {
                    text("resource-status", &js_error(e))
                }
                Err(_) => {}
            }
            // Selecting the same file again should retry a failed import.
            if app.borrow().resource_epoch == epoch {
                set_input("resource-file", "");
            }
        });
    });
    element("resource-file")
        .add_event_listener_with_callback("change", change.as_ref().unchecked_ref())?;
    change.forget();
    Ok(())
}

async fn api(base: &str, token: &str, path: &str, body: Value) -> Result<Value, String> {
    let init = web_sys::RequestInit::new();
    init.set_method("POST");
    init.set_mode(web_sys::RequestMode::Cors);
    init.set_body(&JsValue::from_str(&body.to_string()));
    let request = web_sys::Request::new_with_str_and_init(&format!("{base}{path}"), &init)
        .map_err(js_error)?;
    request
        .headers()
        .set("Content-Type", "application/json")
        .map_err(js_error)?;
    if !token.is_empty() {
        request
            .headers()
            .set("Authorization", &format!("Bearer {token}"))
            .map_err(js_error)?;
    }
    let response: web_sys::Response = JsFuture::from(window().fetch_with_request(&request))
        .await
        .map_err(|_| {
            "Could not reach the gateway. Check its address, TLS and allowed browser origin."
                .to_owned()
        })?
        .dyn_into()
        .map_err(js_error)?;
    let body = JsFuture::from(response.text().map_err(js_error)?)
        .await
        .map_err(js_error)?
        .as_string()
        .unwrap_or_default();
    let value: Value =
        serde_json::from_str(&body).map_err(|_| "The gateway returned an invalid response.")?;
    if !response.ok() {
        return Err(value["error"]
            .as_str()
            .unwrap_or("The gateway refused this request.")
            .to_owned());
    }
    Ok(value)
}

fn close(app: &Shared) {
    let mut app = app.borrow_mut();
    app.epoch += 1;
    app.polling = false;
    app.connected = false;
    app.net = None;
    app.session = None;
    app.dirty = false;
    if let Some(r) = &mut app.renderer {
        r.clear();
    }
    hidden("device", true);
    doc().body().unwrap().set_class_name("");
    text("stats", "");
}

fn account_label(app: &Shared) {
    let app = app.borrow();
    text(
        "account-name",
        app.account
            .as_ref()
            .and_then(|a| a["name"].as_str())
            .unwrap_or("Playing offline"),
    );
}

fn load_profile(app: &Shared) {
    let value = element("profile")
        .unchecked_into::<HtmlSelectElement>()
        .value();
    let profile = value
        .parse::<usize>()
        .ok()
        .and_then(|i| app.borrow().profiles.get(i).cloned());
    if let Some(p) = profile {
        set_input("route-name", &p.name);
        set_input("gateway", &p.gateway);
        set_input("proxy", &p.proxy);
        set_input("gateway-key", "");
    }
}

fn profiles_menu(app: &Shared) {
    let select = element("profile");
    select.set_text_content(None);
    let option = doc().create_element("option").unwrap();
    option.set_attribute("value", "").unwrap();
    option.set_text_content(Some("New route"));
    select.append_child(&option).unwrap();
    for (i, p) in app.borrow().profiles.iter().enumerate() {
        let option = doc().create_element("option").unwrap();
        option.set_attribute("value", &i.to_string()).unwrap();
        option.set_text_content(Some(&p.name));
        select.append_child(&option).unwrap();
    }
}

fn save_profile(app: &Shared) {
    let Ok(gateway) = web_sys::Url::new(&input("gateway")) else {
        return;
    };
    if !matches!(gateway.protocol().as_str(), "http:" | "https:")
        || !gateway.username().is_empty()
        || !gateway.password().is_empty()
        || !gateway.search().is_empty()
        || !gateway.hash().is_empty()
        || !matches!(gateway.pathname().as_str(), "" | "/")
    {
        return;
    }
    let proxy = input("proxy");
    if !proxy.is_empty() {
        let Ok(url) = web_sys::Url::new(&proxy) else {
            return;
        };
        if !matches!(url.protocol().as_str(), "socks5h:" | "http:")
            || url.hostname().is_empty()
            || !url.search().is_empty()
            || !url.hash().is_empty()
            || !matches!(url.pathname().as_str(), "" | "/")
        {
            return;
        }
    }
    // Credentials are usable for this tab, but never persisted with a profile.
    let proxy = if let Some((scheme, rest)) = proxy.split_once("://") {
        format!("{scheme}://{}", rest.rsplit('@').next().unwrap_or(rest))
    } else {
        proxy
    };
    let profile = Profile {
        name: input("route-name"),
        gateway: input("gateway"),
        proxy,
    };
    if profile.name.is_empty() {
        return;
    }
    let selected = element("profile")
        .unchecked_into::<HtmlSelectElement>()
        .value()
        .parse::<usize>()
        .ok();
    let index;
    {
        let mut app = app.borrow_mut();
        if let Some(i) = selected.filter(|i| *i < app.profiles.len()) {
            app.profiles[i] = profile;
            index = i;
        } else {
            index = app.profiles.len();
            app.profiles.push(profile);
        }
        if let Ok(Some(storage)) = window().local_storage() {
            let _ = storage.set_item(
                "horizon.web.routes.v1",
                &serde_json::to_string(&app.profiles).unwrap(),
            );
        }
    }
    profiles_menu(app);
    element("profile")
        .unchecked_into::<HtmlSelectElement>()
        .set_value(&index.to_string());
}

/// Applying a route invalidates callbacks first, then changes the gateway.
/// An older HTTP response may finish, but cannot reclaim this page's session.
async fn apply_route(app: &Shared) -> Result<u64, String> {
    let base = input("gateway").trim_end_matches('/').to_owned();
    let u = web_sys::Url::new(&base).map_err(|_| "Use an http:// or https:// gateway address.")?;
    if !matches!(u.protocol().as_str(), "http:" | "https:")
        || !u.username().is_empty()
        || !u.password().is_empty()
        || !u.search().is_empty()
        || !u.hash().is_empty()
        || !matches!(u.pathname().as_str(), "" | "/")
    {
        return Err("Use the gateway origin, such as https://gateway.example, without credentials or a path.".into());
    }
    if u.protocol() == "http:"
        && !matches!(u.hostname().as_str(), "localhost" | "127.0.0.1" | "[::1]")
    {
        return Err("Use HTTPS for a remote gateway.".into());
    }
    let proxy = input("proxy");
    let key = input("gateway-key");
    close(app);
    status("Applying connection route…");
    let (epoch, old_base, old_token) = {
        let app = app.borrow();
        (app.epoch, app.gateway.clone(), app.token.clone())
    };
    let token = if base != old_base || old_token.is_empty() {
        let reply = api(&base, "", "/api/session", json!({"gateway_key":key})).await?;
        reply["session"]
            .as_str()
            .ok_or("The gateway returned no session.")?
            .to_owned()
    } else {
        old_token
    };
    let reply = api(&base, &token, "/api/route", json!({"proxy":proxy})).await?;
    if app.borrow().epoch != epoch {
        return Err("A newer route change is already in progress.".into());
    }
    {
        let mut app = app.borrow_mut();
        app.gateway = base;
        app.token = token;
        app.account = reply.get("account").filter(|v| !v.is_null()).cloned();
    }
    account_label(app);
    status("Route ready. Account and game requests use this route.");
    Ok(epoch)
}

async fn connect(app: &Shared, ping: bool) -> Result<(), String> {
    // Capture target before awaiting: edits while connecting apply to the next connection.
    let target = input("server");
    let (host, port) = horizon_protocol::servers::parse_address(&target)
        .ok_or("Enter a valid Minecraft server address.")?;
    let epoch = apply_route(app).await?;
    let (base, token, account) = {
        let app = app.borrow();
        (app.gateway.clone(), app.token.clone(), app.account.clone())
    };
    let name = account
        .as_ref()
        .and_then(|a| a["name"].as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| input("username"));
    let uuid = account
        .as_ref()
        .and_then(|a| a["uuid"].as_str())
        .and_then(|s| u128::from_str_radix(s, 16).ok());
    let session = Session::new(&host, port, &name, uuid, ping).map_err(|e| e.to_string())?;
    let ws_url = format!(
        "{}/api/tunnel",
        base.replacen("https:", "wss:", 1)
            .replacen("http:", "ws:", 1)
    );
    let ws = WebSocket::new(&ws_url).map_err(js_error)?;
    ws.set_binary_type(web_sys::BinaryType::Arraybuffer);
    let handshake = json!({"session": token, "host": host, "port":port}).to_string();
    let sender = ws.clone();
    let open = Closure::<dyn FnMut(Event)>::new(move |_| {
        let _ = sender.send_with_str(&handshake);
    });
    let weak = Rc::downgrade(app);
    let message = Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.borrow().epoch != epoch {
            return;
        }
        if let Some(message) = event.data().as_string() {
            let reply: Value = serde_json::from_str(&message).unwrap_or_default();
            if let Some(error) = reply["error"].as_str() {
                app.borrow_mut().connected = false;
                status(error);
                return;
            }
            if reply["connected"].as_bool() == Some(true) {
                app.borrow_mut().connected = true;
                effects(&app, epoch);
            }
        } else if event.data().is_instance_of::<js_sys::ArrayBuffer>() {
            let bytes = js_sys::Uint8Array::new(&event.data()).to_vec();
            let result = app.borrow_mut().session.as_mut().map(|s| s.receive(bytes));
            if let Some(Err(e)) = result {
                close(&app);
                status(&format!("Protocol error: {e}"));
            } else {
                effects(&app, epoch);
            }
        }
    });
    let weak = Rc::downgrade(app);
    let closed = Closure::<dyn FnMut(Event)>::new(move |_| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if app.borrow().epoch != epoch {
            return;
        }
        let was_connected = app.borrow().connected;
        let mut a = app.borrow_mut();
        a.connected = false;
        a.session = None;
        if was_connected {
            status("Disconnected from the server. Choose a route to reconnect.");
        }
    });
    let weak = Rc::downgrade(app);
    let error = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(app) = weak.upgrade()
            && app.borrow().epoch == epoch
        {
            status(
                "The gateway WebSocket failed. Check the gateway address and allowed browser origin.",
            );
        }
    });
    ws.set_onopen(Some(open.as_ref().unchecked_ref()));
    ws.set_onmessage(Some(message.as_ref().unchecked_ref()));
    ws.set_onclose(Some(closed.as_ref().unchecked_ref()));
    ws.set_onerror(Some(error.as_ref().unchecked_ref()));
    let mut a = app.borrow_mut();
    a.session = Some(session);
    a.net = Some(Network {
        ws,
        _open: open,
        _message: message,
        _close: closed,
        _error: error,
    });
    status(if ping {
        "Checking server through the selected route…"
    } else {
        "Connecting through the selected route…"
    });
    Ok(())
}

fn effects(app: &Shared, epoch: u64) {
    if app.borrow().epoch != epoch {
        return;
    }
    let events = app
        .borrow_mut()
        .session
        .as_mut()
        .map(Session::drain)
        .unwrap_or_default();
    for event in events {
        if app.borrow().epoch != epoch {
            break;
        }
        match event {
            Effect::Send(bytes) => {
                let result = app
                    .borrow()
                    .net
                    .as_ref()
                    .map(|n| n.ws.send_with_u8_array(&bytes));
                if matches!(result, Some(Err(_))) {
                    close(app);
                    status("The gateway connection closed while sending.");
                    break;
                }
            }
            Effect::Status(message) => status(&message),
            Effect::Chat(message) => {
                let p = doc().create_element("p").unwrap();
                p.set_text_content(Some(&message));
                let chat = element("chat");
                let _ = chat.append_child(&p);
                while chat.child_element_count() > 8 {
                    if let Some(first) = chat.first_element_child() {
                        first.remove();
                    }
                }
            }
            Effect::WorldChanged => {
                app.borrow_mut().dirty = true;
                doc().body().unwrap().set_class_name("preview");
            }
            Effect::Disconnected(reason) => {
                close(app);
                status(&reason);
            }
            Effect::ServerStatus(raw) => {
                let v: Value = serde_json::from_str(&raw).unwrap_or_default();
                let version = v["version"]["name"].as_str().unwrap_or("unknown version");
                let message = format!(
                    "Server online · {} / {} players · {} · protocol {}",
                    v["players"]["online"], v["players"]["max"], version, v["version"]["protocol"]
                );
                close(app);
                status(&message);
            }
            Effect::Authenticate(hash) => {
                let (base, token) = {
                    let a = app.borrow();
                    (a.gateway.clone(), a.token.clone())
                };
                let app = app.clone();
                status("Authenticating the account through the selected route…");
                spawn_local(async move {
                    let result =
                        api(&base, &token, "/api/auth/join", json!({"server_hash":hash})).await;
                    if app.borrow().epoch != epoch {
                        return;
                    }
                    match result {
                        Ok(_) => {
                            let result = app
                                .borrow_mut()
                                .session
                                .as_mut()
                                .map(Session::authenticated);
                            if let Some(Err(e)) = result {
                                close(&app);
                                status(&e.to_string());
                            } else {
                                effects(&app, epoch);
                            }
                        }
                        Err(e) => {
                            close(&app);
                            status(&e);
                        }
                    }
                });
            }
        }
    }
}

async fn sign_in(app: &Shared) -> Result<(), String> {
    let epoch = apply_route(app).await?;
    let (base, token) = {
        let a = app.borrow();
        (a.gateway.clone(), a.token.clone())
    };
    let v = api(&base, &token, "/api/auth/start", json!({})).await?;
    if app.borrow().epoch != epoch {
        return Ok(());
    }
    text(
        "device-code",
        v["user_code"].as_str().ok_or("No sign-in code returned.")?,
    );
    hidden("device", false);
    status("Finish signing in at microsoft.com/link. Account API requests use your chosen route.");
    app.borrow_mut().polling = true;
    poll_later(
        app.clone(),
        epoch,
        v["interval"].as_u64().unwrap_or(5) as i32 * 1000,
    );
    Ok(())
}

fn poll_later(app: Shared, epoch: u64, delay: i32) {
    let callback = Closure::once_into_js(move || {
        if app.borrow().epoch != epoch || !app.borrow().polling {
            return;
        }
        spawn_local(async move {
            let (base, token) = {
                let a = app.borrow();
                (a.gateway.clone(), a.token.clone())
            };
            let reply = api(&base, &token, "/api/auth/poll", json!({})).await;
            if app.borrow().epoch != epoch {
                return;
            }
            match reply {
                Ok(v) => {
                    if let Some(account) = v.get("account") {
                        app.borrow_mut().account = Some(account.clone());
                        app.borrow_mut().polling = false;
                        hidden("device", true);
                        account_label(&app);
                        status("Signed in. Return to Connections to join a server.");
                    } else {
                        poll_later(app, epoch, delay);
                    }
                }
                Err(e) => {
                    app.borrow_mut().polling = false;
                    hidden("device", true);
                    status(&e);
                }
            }
        });
    });
    let _ = window()
        .set_timeout_with_callback_and_timeout_and_arguments_0(callback.unchecked_ref(), delay);
}

fn click(app: &Shared, id: &str, action: fn(Shared)) -> Result<(), JsValue> {
    let weak = Rc::downgrade(app);
    let callback = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(app) = weak.upgrade() {
            action(app);
        }
    });
    element(id).add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
    callback.forget(); // One listener per static menu button for this page's lifetime.
    Ok(())
}

fn draw_loop(app: Shared) {
    type Animation = Rc<RefCell<Option<Closure<dyn FnMut(f64)>>>>;
    let frame: Animation = Rc::new(RefCell::new(None));
    let again = frame.clone();
    *frame.borrow_mut() = Some(Closure::new(move |now: f64| {
        let (tick, epoch) = {
            let a = app.borrow();
            (now - a.last_tick >= 50.0, a.epoch)
        };
        if tick {
            let result = {
                let mut a = app.borrow_mut();
                a.last_tick = now;
                a.session.as_mut().map(Session::tick)
            };
            if let Some(Err(e)) = result {
                close(&app);
                status(&e.to_string());
            } else {
                effects(&app, epoch);
            }
        }
        {
            let mut a = app.borrow_mut();
            let rebuild = a.dirty && now - a.last_mesh > 1000.0;
            if rebuild {
                // Move the renderer out briefly to borrow the session independently.
                if let Some(mut r) = a.renderer.take() {
                    if let Some(s) = &a.session {
                        r.mesh(s);
                    }
                    a.renderer = Some(r);
                }
                a.dirty = false;
                a.last_mesh = now;
            }
            let (pos, rot) = a
                .session
                .as_ref()
                .map(|s| (s.position, s.rotation))
                .unwrap_or(([0., 64., 0.], (0., 0.)));
            if let Some(r) = &mut a.renderer {
                r.render(pos, rot);
            }
            if let Some(s) = &a.session {
                text(
                    "stats",
                    &format!("{} chunks · {} packets", s.chunks.len(), s.received_packets),
                );
            }
        }
        let _ = window()
            .request_animation_frame(again.borrow().as_ref().unwrap().as_ref().unchecked_ref());
    }));
    let _ =
        window().request_animation_frame(frame.borrow().as_ref().unwrap().as_ref().unchecked_ref());
}

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    let profiles = window()
        .local_storage()
        .ok()
        .flatten()
        .and_then(|s| s.get_item("horizon.web.routes.v1").ok().flatten())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let app = Rc::new(RefCell::new(App {
        profiles,
        gateway: String::new(),
        token: String::new(),
        epoch: 0,
        account: None,
        net: None,
        session: None,
        connected: false,
        renderer: None,
        resources: None,
        resource_epoch: 0,
        dirty: false,
        last_mesh: 0.0,
        last_tick: 0.0,
        polling: false,
        dragging: false,
    }));
    set_input("gateway", &window().location().origin()?);
    profiles_menu(&app);
    resource_controls(&app)?;
    click(&app, "connect", |app| {
        spawn_local(async move {
            if let Err(e) = connect(&app, false).await {
                status(&e);
            }
        })
    })?;
    click(&app, "ping", |app| {
        spawn_local(async move {
            if let Err(e) = connect(&app, true).await {
                status(&e);
            }
        })
    })?;
    click(&app, "apply", |app| {
        let reconnect = app.borrow().connected;
        save_profile(&app);
        spawn_local(async move {
            let result = if reconnect {
                connect(&app, false).await
            } else {
                apply_route(&app).await.map(|_| ())
            };
            if let Err(e) = result {
                status(&e);
            }
        });
    })?;
    click(&app, "disconnect", |app| {
        close(&app);
        status("Disconnected. You can choose another route at any time.");
    })?;
    click(&app, "sign-in", |app| {
        spawn_local(async move {
            if let Err(e) = sign_in(&app).await {
                status(&e);
            }
        })
    })?;
    click(&app, "sign-out", |app| {
        close(&app);
        let (base, token) = {
            let a = app.borrow();
            (a.gateway.clone(), a.token.clone())
        };
        app.borrow_mut().account = None;
        account_label(&app);
        spawn_local(async move {
            if !token.is_empty()
                && let Err(e) = api(&base, &token, "/api/auth/logout", json!({})).await
            {
                status(&e);
                return;
            }
            status("Signed out. Account credentials were removed from the gateway session.");
        });
    })?;
    click(&app, "delete-profile", |app| {
        let i = element("profile")
            .unchecked_into::<HtmlSelectElement>()
            .value()
            .parse::<usize>()
            .ok();
        if let Some(i) = i.filter(|i| *i < app.borrow().profiles.len()) {
            app.borrow_mut().profiles.remove(i);
            if let Ok(Some(s)) = window().local_storage() {
                let _ = s.set_item(
                    "horizon.web.routes.v1",
                    &serde_json::to_string(&app.borrow().profiles).unwrap(),
                );
            }
            profiles_menu(&app);
        }
    })?;
    let weak = Rc::downgrade(&app);
    let change = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(app) = weak.upgrade() {
            load_profile(&app);
        }
    });
    element("profile")
        .add_event_listener_with_callback("change", change.as_ref().unchecked_ref())?;
    change.forget();
    for name in ["connections", "account", "resources", "about"] {
        let button = doc()
            .query_selector(&format!("[data-panel='{name}']"))?
            .unwrap();
        let callback = Closure::<dyn FnMut(Event)>::new(move |_| {
            for panel in ["connections", "account", "resources", "about"] {
                hidden(panel, panel != name);
                let button = doc()
                    .query_selector(&format!("[data-panel='{panel}']"))
                    .unwrap()
                    .unwrap();
                button.set_class_name(if panel == name { "selected" } else { "" });
            }
        });
        button.add_event_listener_with_callback("click", callback.as_ref().unchecked_ref())?;
        callback.forget();
    }
    let canvas: HtmlCanvasElement = element("world").dyn_into()?;
    let weak = Rc::downgrade(&app);
    let down = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(a) = weak.upgrade() {
            a.borrow_mut().dragging = true;
        }
    });
    canvas.add_event_listener_with_callback("mousedown", down.as_ref().unchecked_ref())?;
    down.forget();
    let weak = Rc::downgrade(&app);
    let up = Closure::<dyn FnMut(Event)>::new(move |_| {
        if let Some(a) = weak.upgrade() {
            a.borrow_mut().dragging = false;
        }
    });
    window().add_event_listener_with_callback("mouseup", up.as_ref().unchecked_ref())?;
    up.forget();
    let weak = Rc::downgrade(&app);
    let mouse = Closure::<dyn FnMut(web_sys::MouseEvent)>::new(move |e: web_sys::MouseEvent| {
        if let Some(a) = weak.upgrade() {
            let mut a = a.borrow_mut();
            if a.dragging
                && let Some(s) = &mut a.session
            {
                s.rotation.0 += e.movement_x() as f32 * 0.15;
                s.rotation.1 = (s.rotation.1 + e.movement_y() as f32 * 0.15).clamp(-89.9, 89.9);
            }
        }
    });
    canvas.add_event_listener_with_callback("mousemove", mouse.as_ref().unchecked_ref())?;
    mouse.forget();
    let rendering = app.clone();
    spawn_local(async move {
        match Renderer::new(canvas).await {
            Ok(mut renderer) => {
                let mut app = rendering.borrow_mut();
                renderer.set_resources(app.resources.clone());
                app.renderer = Some(renderer);
                app.dirty = true;
                status("Rust / WASM and WebGPU ready. Choose a server and route.");
            }
            Err(e) => status(&js_error(e)),
        }
    });
    draw_loop(app);
    Ok(())
}
