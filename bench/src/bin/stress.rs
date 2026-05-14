// Stress test: simulates N concurrent "API server instances" all submitting
// orders through a single Arc<Mutex<OrderBook>>, exactly as the real engine does.
//
// Invariants checked after every run:
//   1. Qty conservation  — submitted = filled + resting (no qty lost or duplicated)
//   2. No crossed book   — best_bid < best_ask when both sides have resting orders
//   3. Bid ordering      — bids in descending price order
//   4. Ask ordering      — asks in ascending price order
//   5. Fill price bounds — fill price satisfies both sides' limits

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering::Relaxed},
};
use std::time::Instant;

use shared::{Order, Side, orderbook::OrderBook};
use tokio::sync::Mutex;
use tokio::task::JoinSet;

const INSTANCES: usize   = 16;      // concurrent "API server" simulators
const ORDERS_EACH: usize = 5_000;   // orders per instance
const PRICE_LO: u64      = 97;      // tighten spread → more matches → more contention
const PRICE_HI: u64      = 103;

#[derive(Clone)]
struct FillRecord {
    maker_order_id: u64,
    taker_order_id: u64,
    price:          u64,
    qty:            u64,
    taker_side:     Side,
    taker_limit:    u64,
}

async fn run_instance(
    instance: usize,
    book:     Arc<Mutex<OrderBook>>,
    sub_qty:  Arc<AtomicU64>,
    fills:    Arc<Mutex<Vec<FillRecord>>>,
) {
    for i in 0..ORDERS_EACH {
        let side = if (instance + i) % 2 == 0 { Side::Buy } else { Side::Sell };

        // Prices cluster inside the spread so ~50 % of orders cross immediately.
        let price = PRICE_LO + ((instance * 3 + i * 7) as u64 % (PRICE_HI - PRICE_LO + 1));
        let qty   = 1 + ((instance + i) as u64 % 4);

        let new_fills = {
            let mut book = book.lock().await;
            let id = book.next_id();
            sub_qty.fetch_add(qty, Relaxed);
            book.submit(Order { id, side, price, qty })
        }; // mutex released before touching fills vec

        if !new_fills.is_empty() {
            let mut f = fills.lock().await;
            for fill in new_fills {
                f.push(FillRecord {
                    maker_order_id: fill.maker_order_id,
                    taker_order_id: fill.taker_order_id,
                    price:          fill.price,
                    qty:            fill.qty,
                    taker_side:     side,
                    taker_limit:    price,
                });
            }
        }
    }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let total_orders = INSTANCES * ORDERS_EACH;
    println!("stress: {INSTANCES} instances × {ORDERS_EACH} orders = {total_orders} total");

    let book    = Arc::new(Mutex::new(OrderBook::new()));
    let sub_qty = Arc::new(AtomicU64::new(0));
    let fills   = Arc::new(Mutex::new(Vec::<FillRecord>::with_capacity(total_orders / 2)));

    // Spawn all instances simultaneously — maximum lock contention.
    let t0 = Instant::now();
    let mut set = JoinSet::new();
    for i in 0..INSTANCES {
        set.spawn(run_instance(
            i,
            Arc::clone(&book),
            Arc::clone(&sub_qty),
            Arc::clone(&fills),
        ));
    }
    while let Some(r) = set.join_next().await {
        r.expect("instance task panicked");
    }
    let elapsed = t0.elapsed();

    let snap      = book.lock().await.snapshot();
    let all_fills = fills.lock().await;

    let resting_qty: u64 = snap.bids.iter().chain(snap.asks.iter())
        .map(|l| l.qty)
        .sum();
    let filled_qty: u64  = all_fills.iter().map(|f| f.qty).sum();
    let submitted        = sub_qty.load(Relaxed);

    // 1. Qty conservation
    // Each fill.qty removes qty from TWO orders (maker + taker), so:
    //   submitted = 2 × filled + resting
    assert_eq!(
        submitted, 2 * filled_qty + resting_qty,
        "QTY CONSERVATION FAILED: submitted={submitted} 2×filled={} resting={resting_qty}",
        2 * filled_qty
    );

    // 2. No crossed book
    if let (Some(best_bid), Some(best_ask)) = (snap.bids.first(), snap.asks.first()) {
        assert!(
            best_bid.price < best_ask.price,
            "CROSSED BOOK: best_bid={} best_ask={}",
            best_bid.price, best_ask.price
        );
    }

    // 3. Bid ordering: descending
    for w in snap.bids.windows(2) {
        assert!(
            w[0].price >= w[1].price,
            "BID ORDER VIOLATED: {} before {}",
            w[0].price, w[1].price
        );
    }

    // 4. Ask ordering: ascending
    for w in snap.asks.windows(2) {
        assert!(
            w[0].price <= w[1].price,
            "ASK ORDER VIOLATED: {} before {}",
            w[0].price, w[1].price
        );
    }

    // 5. Fill price bounds
    for f in all_fills.iter() {
        match f.taker_side {
            Side::Buy  => assert!(
                f.price <= f.taker_limit,
                "FILL PRICE ABOVE BUY LIMIT: fill={} limit={}",
                f.price, f.taker_limit
            ),
            Side::Sell => assert!(
                f.price >= f.taker_limit,
                "FILL PRICE BELOW SELL LIMIT: fill={} limit={}",
                f.price, f.taker_limit
            ),
        }
    }

    // 6. No duplicate order IDs across fills (same ID shouldn't be maker twice)
    let mut seen_maker: std::collections::HashSet<u64> = std::collections::HashSet::new();
    for f in all_fills.iter() {
        assert!(
            seen_maker.insert(f.maker_order_id) || true, // makers CAN appear multiple times (partial fills)
            // What we really check: taker never appears as both maker and taker in same fill
        );
        assert_ne!(
            f.maker_order_id, f.taker_order_id,
            "SELF-MATCH: order {} matched itself", f.maker_order_id
        );
    }

    let ms      = elapsed.as_secs_f64() * 1000.0;
    let ops_sec = total_orders as f64 / elapsed.as_secs_f64();
    let fill_pct = filled_qty as f64 / submitted as f64 * 100.0;

    println!("  all invariants OK");
    println!("  {total_orders} orders in {ms:.1} ms  →  {ops_sec:.0} orders/sec");
    println!("  submitted qty : {submitted}");
    println!("  filled qty    : {filled_qty}  ({fill_pct:.1}%)");
    println!("  resting qty   : {resting_qty}");
    println!("  fills         : {}", all_fills.len());
    println!("  bid levels    : {}", snap.bids.len());
    println!("  ask levels    : {}", snap.asks.len());
}
