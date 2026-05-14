use shared::{Order, Side, orderbook::OrderBook};

fn main() {
    let mut book = OrderBook::new();

    // Phase 1: build a dense book across 20 price levels
    for i in 0..10_000u64 {
        let id = book.next_id();
        book.submit(Order { id, side: Side::Buy,  price: 90 + (i % 10), qty: 5 + (i % 5) });
        let id = book.next_id();
        book.submit(Order { id, side: Side::Sell, price: 100 + (i % 10), qty: 5 + (i % 5) });
    }

    // Phase 2: aggressive orders that sweep multiple levels — stresses the
    // matching loop, BTreeMap iteration, and VecDeque pop_front
    for i in 0..500_000u64 {
        let id = book.next_id();
        if i % 2 == 0 {
            // buy sweeps asks
            book.submit(Order { id, side: Side::Buy,  price: 115, qty: 3 + (i % 8) });
        } else {
            // sell sweeps bids
            book.submit(Order { id, side: Side::Sell, price: 85,  qty: 3 + (i % 8) });
        }

        // Periodically replenish both sides so the book never empties
        if i % 50 == 0 {
            for j in 0..10u64 {
                let id = book.next_id();
                book.submit(Order { id, side: Side::Buy,  price: 90 + j, qty: 10 });
                let id = book.next_id();
                book.submit(Order { id, side: Side::Sell, price: 100 + j, qty: 10 });
            }
        }
    }

    let snap = book.snapshot();
    println!(
        "done — {} bid levels, {} ask levels",
        snap.bids.len(),
        snap.asks.len()
    );
}
