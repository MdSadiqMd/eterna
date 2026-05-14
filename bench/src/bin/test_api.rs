use std::time::Duration;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::Value;
use tokio::time::sleep;
use tokio_tungstenite::tungstenite::Message;

async fn post_order(
    client: &Client,
    base: &str,
    side: &str,
    price: u64,
    qty: u64,
) -> Value {
    println!(
        "$ curl -s -X POST {base}/orders \\\n\
         \t-H 'Content-Type: application/json' \\\n\
         \t-d '{{\"side\":\"{side}\",\"price\":{price},\"qty\":{qty}}}'"
    );

    match client
        .post(format!("{base}/orders"))
        .json(&serde_json::json!({"side": side, "price": price, "qty": qty}))
        .send()
        .await
    {
        Ok(r) => {
            let v: Value = r.json().await.expect("invalid JSON from /orders");
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            v
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}

async fn get_orderbook(client: &Client, base: &str) -> Value {
    println!("$ curl -s {base}/orderbook");

    match client.get(format!("{base}/orderbook")).send().await {
        Ok(r) => {
            let v: Value = r.json().await.expect("invalid JSON from /orderbook");
            println!("{}", serde_json::to_string_pretty(&v).unwrap());
            v
        }
        Err(e) => {
            eprintln!("ERROR: {e}");
            std::process::exit(1);
        }
    }
}

async fn ws_listener(url: String) {
    match tokio_tungstenite::connect_async(&url).await {
        Ok((ws, _)) => {
            println!("[ws] connected → {url}");
            let (_, mut read) = ws.split();
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        // Pretty-print the fill JSON inline with the demo output
                        let pretty = serde_json::from_str::<Value>(&text)
                            .map(|v| serde_json::to_string_pretty(&v).unwrap())
                            .unwrap_or_else(|_| text.to_string());
                        println!("[ws] fill event:\n{pretty}");
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    _ => {}
                }
            }
        }
        Err(e) => {
            eprintln!("[ws] connection failed: {e}");
            eprintln!("[ws] fill events will not be shown — is the stack running?");
        }
    }
}

#[tokio::main]
async fn main() {
    let base = std::env::var("BASE_URL")
        .unwrap_or_else(|_| "http://localhost".to_string());
    let ws_url = std::env::var("WS_URL")
        .unwrap_or_else(|_| {
            base.replacen("http://", "ws://", 1) + "/ws"
        });

    println!("eterna demo");
    println!("  endpoint : {base}");
    println!("  ws       : {ws_url}");
    println!("  override : BASE_URL=... WS_URL=... just demo");

    // Start WS listener before any orders so it catches the first fill.
    let ws = tokio::spawn(ws_listener(ws_url));
    sleep(Duration::from_millis(300)).await; // give WS time to connect

    let client = Client::new();

    println!("1. place resting bid  buy 10 @ 100");
    post_order(&client, &base, "buy", 100, 10).await;
    println!("");

    println!("2. place resting ask  sell 10 @ 105");
    post_order(&client, &base, "sell", 105, 10).await;
    println!("");

    println!("3. orderbook — expect bids:[100×10]  asks:[105×10]");
    get_orderbook(&client, &base).await;
    println!("");

    println!("4. aggressive sell 6 @ 100 — partial fill against bid");
    post_order(&client, &base, "sell", 100, 6).await;
    sleep(Duration::from_millis(150)).await; // let WS event print before next step
    println!("");

    println!("5. orderbook — expect bids:[100×4]  asks:[105×10]");
    get_orderbook(&client, &base).await;
    println!("");

    println!("6. aggressive buy 10 @ 110 — sweeps ask at 105");
    post_order(&client, &base, "buy", 110, 10).await;
    sleep(Duration::from_millis(150)).await;
    println!("");

    println!("7. orderbook — ask gone, remainder rests at 110");
    get_orderbook(&client, &base).await;
    println!("");

    sleep(Duration::from_millis(200)).await; // flush any trailing WS events
    ws.abort();
}
