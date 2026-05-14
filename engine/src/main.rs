use axum::{
    Json, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use shared::{
    Fill, Order, OrderBookSnapshot, SubmitOrderReq, SubmitOrderResp,
    orderbook::OrderBook,
};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex};
use tracing::info;

type AppError = (StatusCode, String);

// Arc<Mutex<OrderBook>> is the explicit serialisation point for all order
// submissions. Every POST /orders — from any number of concurrent API server
// instances — must acquire this lock before entering submit(). Two orders
// can therefore never race inside the matching loop: double-matching is
// structurally impossible because the lock must be held to mutate the book.
#[derive(Clone)]
struct AppState {
    book:    Arc<Mutex<OrderBook>>,
    fill_tx: broadcast::Sender<Fill>,
}

async fn post_orders(
    State(state): State<AppState>,
    Json(req): Json<SubmitOrderReq>,
) -> Result<Json<SubmitOrderResp>, AppError> {
    // Lock scope — acquire, assign ID, match, release.
    // id and fills are moved out before the guard drops so the Mutex
    // is never held across an .await (broadcast send below).
    let (id, fills) = {
        let mut book = state.book.lock().await;
        let id    = book.next_id();
        let fills = book.submit(Order {
            id,
            side:  req.side,
            price: req.price,
            qty:   req.qty,
        });
        (id, fills)
    }; // ← Mutex released here, before any async work

    for fill in &fills {
        let _ = state.fill_tx.send(fill.clone());
    }

    Ok(Json(SubmitOrderResp { id }))
}

async fn get_orderbook(
    State(state): State<AppState>,
) -> Result<Json<OrderBookSnapshot>, AppError> {
    let book = state.book.lock().await;
    Ok(Json(book.snapshot()))
}

async fn ws_handler(
    State(state): State<AppState>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_ws(socket, state.fill_tx.subscribe()))
}

async fn handle_ws(mut socket: WebSocket, mut fill_rx: broadcast::Receiver<Fill>) {
    loop {
        match fill_rx.recv().await {
            Ok(fill) => {
                let Ok(json) = serde_json::to_string(&fill) else { continue };
                if socket.send(Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed)    => break,
        }
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "engine=info".to_string()),
        )
        .init();

    let (fill_tx, _) = broadcast::channel(1024);

    let state = AppState {
        book:    Arc::new(Mutex::new(OrderBook::new())),
        fill_tx,
    };

    let addr = std::env::var("ENGINE_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:9000".to_string());

    let app = Router::new()
        .route("/orders",    post(post_orders))
        .route("/orderbook", get(get_orderbook))
        .route("/ws",        get(ws_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&addr).await
        .expect("failed to bind engine address");
    info!("engine listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}
