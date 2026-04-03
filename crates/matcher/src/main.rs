use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use prediction_core::{OrderBook, OrderBookSnapshot, Side};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use tokio::sync::{broadcast, mpsc, oneshot};
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

#[derive(Clone)]
struct AppState {
    cmd_tx: mpsc::Sender<Cmd>,
    #[allow(dead_code)]
    fill_tx: broadcast::Sender<String>,
}

enum Cmd {
    Submit {
        side: Side,
        price: u64,
        qty: u64,
        reply: oneshot::Sender<Result<u64, String>>,
    },
    Snapshot {
        reply: oneshot::Sender<OrderBookSnapshot>,
    },
}

#[derive(Debug, Deserialize)]
struct PostOrderBody {
    side: Side,
    price: u64,
    qty: u64,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "matcher=info,tower_http=info".into()),
        )
        .init();

    let (fill_tx, _) = broadcast::channel::<String>(4096);
    let fill_tx_loop = fill_tx.clone();
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Cmd>(1024);

    tokio::spawn(async move {
        let mut book = OrderBook::new();
        let mut next_id: u64 = 1;
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                Cmd::Submit {
                    side,
                    price,
                    qty,
                    reply,
                } => {
                    if qty == 0 {
                        let _ = reply.send(Err("qty must be positive".into()));
                        continue;
                    }
                    let id = next_id;
                    next_id = next_id.saturating_add(1);
                    let order = prediction_core::Order::new(id, side, price, qty);
                    let fills = book.submit(order);
                    if reply.send(Ok(id)).is_err() {
                        warn!("submit reply dropped");
                    }
                    for fill in fills {
                        match serde_json::to_string(&fill) {
                            Ok(json) => {
                                let _ = fill_tx_loop.send(json);
                            }
                            Err(e) => warn!("fill serialize: {e}"),
                        }
                    }
                }
                Cmd::Snapshot { reply } => {
                    let snap = book.snapshot_levels();
                    let _ = reply.send(snap);
                }
            }
        }
    });

    let state = AppState {
        cmd_tx: cmd_tx.clone(),
        fill_tx: fill_tx.clone(),
    };

    let app = Router::new()
        .route("/orders", post(post_order))
        .route("/orderbook", get(get_orderbook))
        .route("/ws", get(ws_upgrade))
        .with_state(state)
        .layer(TraceLayer::new_for_http());

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(3001);
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    info!("matcher listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn post_order(
    State(state): State<AppState>,
    Json(body): Json<PostOrderBody>,
) -> impl IntoResponse {
    if body.qty == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "qty must be positive" })),
        )
            .into_response();
    }
    let (tx, rx) = oneshot::channel();
    if state
        .cmd_tx
        .send(Cmd::Submit {
            side: body.side,
            price: body.price,
            qty: body.qty,
            reply: tx,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "matcher unavailable" })),
        )
            .into_response();
    }
    match rx.await {
        Ok(Ok(id)) => (StatusCode::CREATED, Json(json!({ "id": id }))).into_response(),
        Ok(Err(msg)) => (StatusCode::BAD_REQUEST, Json(json!({ "error": msg }))).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "internal" })),
        )
            .into_response(),
    }
}

async fn get_orderbook(State(state): State<AppState>) -> impl IntoResponse {
    let (tx, rx) = oneshot::channel();
    if state.cmd_tx.send(Cmd::Snapshot { reply: tx }).await.is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({ "error": "matcher unavailable" })),
        )
            .into_response();
    }
    match rx.await {
        Ok(snap) => Json(snap).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "internal" })),
        )
            .into_response(),
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    let rx = state.fill_tx.subscribe();
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
