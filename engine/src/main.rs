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
    Fill, Order, OrderBookSnapshot, Side, SubmitOrderReq, SubmitOrderResp,
    orderbook::OrderBook,
};
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::info;


// Engine actor — owns the order book; all mutations serialized via channel
enum EngineCmd {
    Submit {
        side: Side,
        price: u64,
        qty: u64,
        reply: oneshot::Sender<(u64, Vec<Fill>)>,
    },
    Snapshot {
        reply: oneshot::Sender<OrderBookSnapshot>,
    },
}

async fn engine_task(mut rx: mpsc::Receiver<EngineCmd>, fill_tx: broadcast::Sender<Fill>) {
    let mut book = OrderBook::new();
    while let Some(cmd) = rx.recv().await {
        match cmd {
            EngineCmd::Submit { side, price, qty, reply } => {
                let id = book.next_id();
                let fills = book.submit(Order { id, side, price, qty });
                for fill in &fills {
                    let _ = fill_tx.send(fill.clone());
                }
                let _ = reply.send((id, fills));
            }
            EngineCmd::Snapshot { reply } => {
                let _ = reply.send(book.snapshot());
            }
        }
    }
}


// HTTP handlers
type AppError = (StatusCode, String);

#[derive(Clone)]
struct AppState {
    engine_tx: mpsc::Sender<EngineCmd>,
    fill_tx: broadcast::Sender<Fill>,
}

async fn post_orders(
    State(state): State<AppState>,
    Json(req): Json<SubmitOrderReq>,
) -> Result<Json<SubmitOrderResp>, AppError> {
    let (tx, rx) = oneshot::channel();
    state
        .engine_tx
        .send(EngineCmd::Submit {
            side: req.side,
            price: req.price,
            qty: req.qty,
            reply: tx,
        })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let (id, _fills) = rx
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(SubmitOrderResp { id }))
}

async fn get_orderbook(
    State(state): State<AppState>,
) -> Result<Json<OrderBookSnapshot>, AppError> {
    let (tx, rx) = oneshot::channel();
    state
        .engine_tx
        .send(EngineCmd::Snapshot { reply: tx })
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let snapshot = rx
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(snapshot))
}


// WebSocket — streams Fill events to subscribers
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
            Err(broadcast::error::RecvError::Closed) => break,
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

    let (engine_tx, engine_rx) = mpsc::channel(1024);
    let (fill_tx, _) = broadcast::channel(1024);

    let _engine = tokio::spawn(engine_task(engine_rx, fill_tx.clone()));

    let addr = std::env::var("ENGINE_ADDR").unwrap_or_else(|_| "0.0.0.0:9000".to_string());

    let app = Router::new()
        .route("/orders", post(post_orders))
        .route("/orderbook", get(get_orderbook))
        .route("/ws", get(ws_handler))
        .with_state(AppState { engine_tx, fill_tx });

    let listener = tokio::net::TcpListener::bind(&addr).await
        .expect("failed to bind engine address");
    info!("engine listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}
