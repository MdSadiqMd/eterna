use std::cmp::Reverse;
use std::collections::{BTreeMap, VecDeque};

use crate::{Fill, Order, OrderBookSnapshot, PriceLevel, Side};

// Encoding the traversal direction into the BTreeMap key type eliminates all
// per-side branching.  keys().next() and .iter() work uniformly for both sides:
//
//   AskBook  u64          → natural ascending  = lowest ask first
//   BidBook  Reverse<u64> → natural ascending  = highest bid first

trait PriceKey: Ord + Copy + std::fmt::Debug {
    fn encode(price: u64) -> Self;
    fn decode(self) -> u64;
}

impl PriceKey for u64 {
    fn encode(p: u64) -> u64    { p }
    fn decode(self)   -> u64    { self }
}

impl PriceKey for Reverse<u64> {
    fn encode(p: u64)  -> Self  { Reverse(p) }
    fn decode(self)    -> u64   { self.0 }
}

// VecDeque per level — O(1) pop_front, no element shifting.
//
// Free-list pool (self.free): when a level drains, its VecDeque is cleared and
// returned to the pool rather than dropped.  The next new level pops from the
// pool instead of allocating fresh.  This eliminates the alloc/dealloc cycle
// that dominated earlier flamegraphs:
//
//   03.svg (raw VecDeque): dealloc 30% + __bzero 22% = 52% allocator overhead
//   04.svg (SmallVec):     dealloc 21% + memmove 24% = 45% — swapped one cost
//                          for another: remove(0) shifts all elements
//   05.svg (pool):         dealloc and memmove should both disappear

#[derive(Debug)]
struct HalfBook<K: PriceKey> {
    levels: BTreeMap<K, VecDeque<Order>>,
    free:   Vec<VecDeque<Order>>,   // recycled empty queues
    best:   Option<u64>,
}

impl<K: PriceKey> HalfBook<K> {
    fn new() -> Self {
        Self { levels: BTreeMap::new(), free: Vec::new(), best: None }
    }

    // O(1) via cached best + one BTreeMap::get.
    fn peek_best(&self) -> Option<(u64, u64, u64)> {
        let price = self.best?;
        let front = self.levels.get(&K::encode(price))?.front()?;
        Some((price, front.id, front.qty))
    }

    // Deduct qty from the front order at the best level.
    // pop_front is O(1) — VecDeque ring buffer, no shifting.
    // Level pruning is O(log n) but only on exhaustion.
    // Drained VecDeque goes to pool — no heap free.
    fn consume(&mut self, qty: u64) {
        let price = match self.best {
            Some(p) => p,
            None    => return,
        };
        let key   = K::encode(price);
        let queue = match self.levels.get_mut(&key) {
            Some(q) => q,
            None    => return,
        };
        if let Some(front) = queue.front_mut() {
            front.qty -= qty;
            if front.qty == 0 {
                queue.pop_front();
            }
        }
        if queue.is_empty() {
            // Remove from map and recycle the allocation instead of dropping.
            let mut recycled = self.levels.remove(&key).unwrap();
            recycled.clear();           // drop elements, keep capacity
            self.free.push(recycled);
            self.best = self.levels.keys().next().copied().map(K::decode);
        }
    }

    // Rest an order.  New levels pull from the pool — no malloc.
    fn rest(&mut self, order: Order) {
        let price = order.price;
        let key   = K::encode(price);
        let queue = self.levels.entry(key).or_insert_with(|| {
            self.free.pop().unwrap_or_default()
        });
        queue.push_back(order);
        self.best = Some(match self.best {
            None    => price,
            Some(b) => if key < K::encode(b) { price } else { b },
        });
    }

    fn snapshot_levels(&self) -> Vec<PriceLevel> {
        self.levels.iter()
            .map(|(k, queue)| PriceLevel {
                price: k.decode(),
                qty:   queue.iter().map(|o| o.qty).sum(),
            })
            .collect()
    }
}

type BidBook = HalfBook<Reverse<u64>>;
type AskBook = HalfBook<u64>;

#[derive(Debug)]
pub struct OrderBook {
    bids: BidBook,
    asks: AskBook,
    next_id: u64,
}

impl OrderBook {
    pub fn new() -> Self {
        Self { bids: HalfBook::new(), asks: HalfBook::new(), next_id: 1 }
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    // Requires &mut self — caller holds tokio::sync::Mutex<OrderBook>.
    pub fn submit(&mut self, mut taker: Order) -> Vec<Fill> {
        let mut fills = Vec::with_capacity(4);

        match taker.side {
            Side::Buy => {
                while taker.qty > 0 {
                    let Some((ask_price, maker_id, maker_qty)) = self.asks.peek_best()
                    else { break };
                    if ask_price > taker.price { break }

                    let fill_qty = taker.qty.min(maker_qty);
                    fills.push(Fill {
                        maker_order_id: maker_id,
                        taker_order_id: taker.id,
                        price: ask_price,
                        qty:   fill_qty,
                    });
                    taker.qty -= fill_qty;
                    self.asks.consume(fill_qty);
                }
                if taker.qty > 0 { self.bids.rest(taker); }
            }

            Side::Sell => {
                while taker.qty > 0 {
                    let Some((bid_price, maker_id, maker_qty)) = self.bids.peek_best()
                    else { break };
                    if bid_price < taker.price { break }

                    let fill_qty = taker.qty.min(maker_qty);
                    fills.push(Fill {
                        maker_order_id: maker_id,
                        taker_order_id: taker.id,
                        price: bid_price,
                        qty:   fill_qty,
                    });
                    taker.qty -= fill_qty;
                    self.bids.consume(fill_qty);
                }
                if taker.qty > 0 { self.asks.rest(taker); }
            }
        }

        fills
    }

    pub fn snapshot(&self) -> OrderBookSnapshot {
        OrderBookSnapshot {
            bids: self.bids.snapshot_levels(),
            asks: self.asks.snapshot_levels(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn order(id: u64, side: Side, price: u64, qty: u64) -> Order {
        Order { id, side, price, qty }
    }

    #[test]
    fn no_match_when_spread_exists() {
        let mut book = OrderBook::new();
        assert!(book.submit(order(1, Side::Buy,  99,  10)).is_empty());
        assert!(book.submit(order(2, Side::Sell, 101, 10)).is_empty());
    }

    #[test]
    fn exact_match() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Buy, 100, 10));
        let fills = book.submit(order(2, Side::Sell, 100, 10));
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].price, 100);
        assert_eq!(fills[0].qty,   10);
        assert_eq!(fills[0].maker_order_id, 1);
        assert_eq!(fills[0].taker_order_id, 2);
        let snap = book.snapshot();
        assert!(snap.bids.is_empty());
        assert!(snap.asks.is_empty());
    }

    #[test]
    fn partial_fill_leaves_remainder() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Buy, 100, 10));
        let fills = book.submit(order(2, Side::Sell, 100, 6));
        assert_eq!(fills[0].qty, 6);
        assert_eq!(book.snapshot().bids[0].qty, 4);
    }

    #[test]
    fn taker_sweeps_multiple_levels() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Sell, 101, 5));
        book.submit(order(2, Side::Sell, 102, 5));
        let fills = book.submit(order(3, Side::Buy, 103, 10));
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].price, 101);
        assert_eq!(fills[1].price, 102);
    }

    #[test]
    fn price_time_priority() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Buy, 100, 5));
        book.submit(order(2, Side::Buy, 100, 5));
        let fills = book.submit(order(3, Side::Sell, 100, 5));
        assert_eq!(fills[0].maker_order_id, 1);
    }

    #[test]
    fn fill_at_maker_price() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Sell, 100, 10));
        let fills = book.submit(order(2, Side::Buy, 105, 10));
        assert_eq!(fills[0].price, 100);
    }

    #[test]
    fn bid_snapshot_is_highest_first() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Buy, 99,  5));
        book.submit(order(2, Side::Buy, 101, 5));
        book.submit(order(3, Side::Buy, 100, 5));
        let bids = book.snapshot().bids;
        assert_eq!(bids[0].price, 101);
        assert_eq!(bids[1].price, 100);
        assert_eq!(bids[2].price, 99);
    }

    #[test]
    fn ask_snapshot_is_lowest_first() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Sell, 103, 5));
        book.submit(order(2, Side::Sell, 101, 5));
        book.submit(order(3, Side::Sell, 102, 5));
        let asks = book.snapshot().asks;
        assert_eq!(asks[0].price, 101);
        assert_eq!(asks[1].price, 102);
        assert_eq!(asks[2].price, 103);
    }

    #[test]
    fn best_pointer_updates_after_level_drained() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Sell, 100, 5));
        book.submit(order(2, Side::Sell, 101, 5));
        book.submit(order(3, Side::Buy,  100, 5));
        assert_eq!(book.asks.best, Some(101));
    }

    #[test]
    fn pool_reuses_queues() {
        let mut book = OrderBook::new();
        // Fill and drain a level, then refill it — pool should prevent reallocation
        book.submit(order(1, Side::Buy,  100, 5));
        book.submit(order(2, Side::Sell, 100, 5)); // drains bid level → pooled
        assert_eq!(book.bids.free.len(), 1);       // one queue in pool
        book.submit(order(3, Side::Buy,  100, 5)); // new bid → pulled from pool
        assert_eq!(book.bids.free.len(), 0);       // pool consumed
    }
}
