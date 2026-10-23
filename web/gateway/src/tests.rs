use super::*;
use futures_util::StreamExt;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message as WsMessage, client::IntoClientRequest},
};

async fn setup() -> (String, tokio::task::JoinHandle<()>) {
    let app = App {
        clients: Default::default(),
        key: "test-key".into(),
        origins: vec!["http://horizon.test".into()],
        targets: vec!["127.0.0.1".into()],
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, router(app, PathBuf::new()))
            .await
            .unwrap();
    });
    (format!("http://{addr}"), task)
}

#[tokio::test]
async fn origin_and_gateway_key_are_required_and_sessions_are_distinct() {
    let (base, task) = setup().await;
    let http = reqwest::Client::new();
    let r = http
        .post(format!("{base}/api/session"))
        .json(&json!({"gateway_key":"test-key"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::FORBIDDEN);
    let r = http
        .post(format!("{base}/api/session"))
        .header("origin", "http://horizon.test")
        .json(&json!({"gateway_key":"wrong"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    let mut tokens = vec![];
    for _ in 0..2 {
        let v: Value = http
            .post(format!("{base}/api/session"))
            .header("origin", "http://horizon.test")
            .json(&json!({"gateway_key":"test-key"}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        tokens.push(v["session"].as_str().unwrap().to_owned());
    }
    assert_ne!(tokens[0], tokens[1]);
    task.abort();
}

#[tokio::test]
async fn websocket_forwards_bytes_and_route_change_closes_previous_tunnel() {
    let (base, task) = setup().await;
    let http = reqwest::Client::new();
    let v: Value = http
        .post(format!("{base}/api/session"))
        .header("origin", "http://horizon.test")
        .json(&json!({"gateway_key":"test-key"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let token = v["session"].as_str().unwrap();
    let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = echo.local_addr().unwrap().port();
    let echo_task = tokio::spawn(async move {
        let (mut s, _) = echo.accept().await.unwrap();
        let mut b = [0; 4];
        s.read_exact(&mut b).await.unwrap();
        s.write_all(&b).await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let mut req = format!("{}/api/tunnel", base.replace("http:", "ws:"))
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "http://horizon.test".parse().unwrap());
    let (mut ws, _) = connect_async(req).await.unwrap();
    ws.send(WsMessage::Text(
        json!({"session": token, "host":"127.0.0.1", "port":port})
            .to_string()
            .into(),
    ))
    .await
    .unwrap();
    let ready = ws.next().await.unwrap().unwrap();
    assert!(ready.to_text().unwrap().contains("connected"));
    ws.send(WsMessage::Binary(vec![0, 1, 128, 255].into()))
        .await
        .unwrap();
    assert_eq!(
        ws.next().await.unwrap().unwrap().into_data().as_ref(),
        &[0, 1, 128, 255]
    );
    let r = http
        .post(format!("{base}/api/route"))
        .header("origin", "http://horizon.test")
        .bearer_auth(token)
        .json(&json!({"proxy":""}))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success());
    let msg = tokio::time::timeout(Duration::from_secs(2), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(msg.to_text().unwrap().contains("route or account changed"));
    task.abort();
    echo_task.abort();
}

#[tokio::test]
async fn socks5_uses_proxy_dns_and_forwards_game_traffic() {
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = proxy.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut s, _) = proxy.accept().await.unwrap();
        let mut greeting = [0; 3];
        s.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        s.write_all(&[5, 0]).await.unwrap();
        let mut header = [0; 5];
        s.read_exact(&mut header).await.unwrap();
        assert_eq!(&header[..4], &[5, 1, 0, 3]);
        let mut name = vec![0; header[4] as usize + 2];
        s.read_exact(&mut name).await.unwrap();
        assert_eq!(&name[..name.len() - 2], b"only.proxy.can.resolve");
        s.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
            .await
            .unwrap();
        s.write_all(b"proxy").await.unwrap();
    });
    let route = route::Route {
        proxy: format!("socks5h://{addr}"),
    };
    let mut s = route
        .connect("only.proxy.can.resolve", 25565)
        .await
        .unwrap();
    let mut buf = [0; 5];
    s.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"proxy");
    task.await.unwrap();
}

#[tokio::test]
async fn http_connect_authenticates_and_keeps_bytes_after_proxy_headers() {
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = proxy.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut s, _) = proxy.accept().await.unwrap();
        let mut request = vec![];
        while !request.ends_with(b"\r\n\r\n") {
            request.push(s.read_u8().await.unwrap());
        }
        let request = String::from_utf8(request).unwrap();
        assert!(request.starts_with("CONNECT server.invalid:25565 HTTP/1.1\r\n"));
        assert!(request.contains("Proxy-Authorization: Basic dXNlckBleGFtcGxlOnBhc3M=\r\n"));
        s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nfirst-game-bytes")
            .await
            .unwrap();
    });
    let route = route::Route {
        proxy: format!("http://user%40example:pass@{addr}"),
    };
    let mut s = route.connect("server.invalid", 25565).await.unwrap();
    let mut bytes = vec![];
    s.read_to_end(&mut bytes).await.unwrap();
    assert_eq!(bytes, b"first-game-bytes");
    task.await.unwrap();
}

#[tokio::test]
async fn account_http_uses_selected_egress_instead_of_resolving_target_locally() {
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = proxy.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut s, _) = proxy.accept().await.unwrap();
        let mut request = vec![];
        while !request.ends_with(b"\r\n\r\n") {
            request.push(s.read_u8().await.unwrap());
        }
        let request = String::from_utf8(request).unwrap();
        assert!(
            request.starts_with("GET http://account.only.proxy.can.resolve/profile HTTP/1.1\r\n")
        );
        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await.unwrap();
    });
    let route = route::Route {
        proxy: format!("http://{addr}"),
    };
    let response = route
        .http()
        .unwrap()
        .get("http://account.only.proxy.can.resolve/profile")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    task.await.unwrap();
}
