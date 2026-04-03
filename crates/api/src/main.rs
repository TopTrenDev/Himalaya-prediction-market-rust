use axum::{
    body::Bytes,
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{header, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde_json::json;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message as UpMessage;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};
use url::Url;

#[derive(Clone)]
struct ApiState {
    http: reqwest::Client,
    matcher_base: String,
    local_fills: broadcast::Sender<String>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "api=info,tower_http=info".into()),
        )
        .init();

    let matcher_base = std::env::var("MATCHER_URL").unwrap_or_else(|_| "http://127.0.0.1:3001".into());
    let ws_to_matcher = matcher_ws_url(&matcher_base).expect("invalid MATCHER_URL");

    let (local_fills, _) = broadcast::channel::<String>(4096);
    let relay_tx = local_fills.clone();
    let ws_url = ws_to_matcher.clone();
    tokio::spawn(async move {
        loop {
            match tokio_tungstenite::connect_async(&ws_url).await {
                Ok((ws, _)) => {
                    info!("connected fill relay to matcher");
                    let (_, mut read) = ws.split();
                    while let Some(msg) = read.next().await {
                        match msg {
                            Ok(UpMessage::Text(t)) => {
                                let _ = relay_tx.send(t.to_string());
                            }
                            Ok(UpMessage::Close(_)) => break,
                            Ok(_) => {}
                            Err(e) => {
                                warn!("matcher ws read error: {e}");
                                break;
                            }
                        }
                    }
                    warn!("matcher ws disconnected");
                }
                Err(e) => {
                    warn!("matcher ws connect failed: {e}");
                }
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });

    let state = ApiState {
        http: reqwest::Client::new(),
        matcher_base,
        local_fills,
    };

    let app = Router::new()
        .route("/orders", post(post_orders))
        .route("/orderbook", get(get_orderbook))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("api listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

fn matcher_ws_url(http_base: &str) -> Result<String, String> {
    let mut u = Url::parse(http_base).map_err(|e| e.to_string())?;
    match u.scheme() {
        "http" => u.set_scheme("ws").map_err(|_| "ws scheme".to_string())?,
        "https" => u.set_scheme("wss").map_err(|_| "wss scheme".to_string())?,
        s => return Err(format!("unsupported scheme: {s}")),
    }
    u.set_path("/ws");
    u.set_query(None);
    u.set_fragment(None);
    Ok(u.to_string())
}

async fn post_orders(State(state): State<ApiState>, body: Bytes) -> impl IntoResponse {
    let url = format!("{}/orders", state.matcher_base.trim_end_matches('/'));
    let resp = match state
        .http
        .post(url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.to_vec())
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("matcher request failed: {e}") })),
            )
                .into_response();
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("read body: {e}") })),
            )
                .into_response();
        }
    };
    let mut res = (status, bytes).into_response();
    if let Ok(ct) = header::HeaderValue::from_str("application/json") {
        res.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    res
}

async fn get_orderbook(State(state): State<ApiState>) -> impl IntoResponse {
    let url = format!("{}/orderbook", state.matcher_base.trim_end_matches('/'));
    let resp = match state.http.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("matcher request failed: {e}") })),
            )
                .into_response();
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("read body: {e}") })),
            )
                .into_response();
        }
    };
    let mut res = (status, bytes).into_response();
    if let Ok(ct) = header::HeaderValue::from_str("application/json") {
        res.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    res
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<ApiState>) -> impl IntoResponse {
    let rx = state.local_fills.subscribe();
    ws.on_upgrade(move |socket| ws_client(socket, rx))
}

async fn ws_client(mut socket: WebSocket, mut rx: broadcast::Receiver<String>) {
    loop {
        tokio::select! {
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(Message::Ping(p))) => {
                        let _ = socket.send(Message::Pong(p)).await;
                    }
                    Some(Err(_)) => break,
                    _ => {}
                }
            }
            fill = rx.recv() => {
                match fill {
                    Ok(text) => {
                        if socket.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}
