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
use futures_util::StreamExt;
use shared::{Fill, OrderBookSnapshot, SubmitOrderReq, SubmitOrderResp};
use tokio::sync::broadcast;
use tracing::{info, warn};

// HTTP handlers — proxy to engine
type AppError = (StatusCode, String);

#[derive(Clone)]
struct AppState {
    engine_url: String,
    client: reqwest::Client,
    fill_tx: broadcast::Sender<Fill>,
}

async fn post_orders(
    State(state): State<AppState>,
    Json(req): Json<SubmitOrderReq>,
) -> Result<Json<SubmitOrderResp>, AppError> {
    let resp = state
        .client
        .post(format!("{}/orders", state.engine_url))
        .json(&req)
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
        .json::<SubmitOrderResp>()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(resp))
}

async fn get_orderbook(
    State(state): State<AppState>,
) -> Result<Json<OrderBookSnapshot>, AppError> {
    let resp = state
        .client
        .get(format!("{}/orderbook", state.engine_url))
        .send()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
        .json::<OrderBookSnapshot>()
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(resp))
}


// WebSocket — fans out fills from engine to connected clients
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


// Engine WS subscriber — reconnects on drop
async fn subscribe_engine_ws(engine_ws_url: String, fill_tx: broadcast::Sender<Fill>) {
    loop {
        match tokio_tungstenite::connect_async(&engine_ws_url).await {
            Ok((ws_stream, _)) => {
                info!("connected to engine WS at {engine_ws_url}");
                let (_, mut read) = ws_stream.split();
                while let Some(msg) = read.next().await {
                    match msg {
                        Ok(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                            if let Ok(fill) = serde_json::from_str::<Fill>(&text) {
                                let _ = fill_tx.send(fill);
                            }
                        }
                        Ok(tokio_tungstenite::tungstenite::Message::Close(_)) => break,
                        Err(e) => {
                            warn!("engine WS error: {e}");
                            break;
                        }
                        _ => {}
                    }
                }
                warn!("engine WS disconnected, reconnecting in 1s");
            }
            Err(e) => {
                warn!("engine WS connect failed: {e}, retrying in 1s");
            }
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "api=info".to_string()),
        )
        .init();

    let engine_url =
        std::env::var("ENGINE_URL").unwrap_or_else(|_| "http://localhost:9000".to_string());
    let engine_ws_url =
        std::env::var("ENGINE_WS_URL").unwrap_or_else(|_| "ws://localhost:9000/ws".to_string());
    let api_addr =
        std::env::var("API_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".to_string());

    let (fill_tx, _) = broadcast::channel::<Fill>(1024);

    tokio::spawn(subscribe_engine_ws(engine_ws_url, fill_tx.clone()));

    let state = AppState {
        engine_url,
        client: reqwest::Client::new(),
        fill_tx,
    };

    let app = Router::new()
        .route("/orders", post(post_orders))
        .route("/orderbook", get(get_orderbook))
        .route("/ws", get(ws_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&api_addr).await
        .expect("failed to bind api address");
    info!("api server listening on {api_addr}");
    axum::serve(listener, app).await.unwrap();
}
