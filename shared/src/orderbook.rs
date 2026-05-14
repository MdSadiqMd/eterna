use std::collections::{BTreeMap, VecDeque};

use crate::{Fill, Order, OrderBookSnapshot, PriceLevel, Side};

// One side of the book (bids or asks).
// levels: sparse BTreeMap — only active price ticks exist as keys.
// best:   cached best price for O(1) top-of-book peek instead of
//         O(log n) BTreeMap min/max on every match iteration.
#[derive(Debug)]
struct HalfBook {
    levels: BTreeMap<u64, VecDeque<Order>>,
    best: Option<u64>,
    side: Side,
}

impl HalfBook {
    fn new(side: Side) -> Self {
        Self { levels: BTreeMap::new(), best: None, side }
    }

    // Does the resting best price cross the incoming taker limit?
    // O(1) — reads cached best.
    fn crosses(&self, taker_limit: u64) -> bool {
        match (self.best, self.side) {
            // Asks: resting sell crosses a buy when ask_price <= buy_limit
            (Some(best), Side::Sell) => best <= taker_limit,
            // Bids: resting buy crosses a sell when bid_price >= sell_limit
            (Some(best), Side::Buy) => best >= taker_limit,
            (None, _) => false,
        }
    }

    // Peek at the front order of the best level without mutating. O(1).
    fn peek_best(&self) -> Option<(u64, u64, u64)> {
        let best = self.best?;
        let front = self.levels.get(&best)?.front()?;
        Some((best, front.id, front.qty))
    }

    // Deduct `qty` from the front order at the best price level.
    // Removes the order if fully filled; removes the price level if empty;
    // updates the cached best pointer. O(1) amortised.
    fn consume(&mut self, qty: u64) {
        let best = match self.best {
            Some(b) => b,
            None => return,
        };
        let queue = match self.levels.get_mut(&best) {
            Some(q) => q,
            None => return,
        };
        if let Some(front) = queue.front_mut() {
            front.qty -= qty;
            if front.qty == 0 {
                queue.pop_front();
            }
        }
        if queue.is_empty() {
            self.levels.remove(&best);
            // O(log n) only when a level is exhausted, not on every fill.
            self.best = match self.side {
                Side::Buy  => self.levels.keys().next_back().copied(),
                Side::Sell => self.levels.keys().next().copied(),
            };
        }
    }

    // Rest an unmatched (or partially matched) order at its price level.
    // O(log n) for BTreeMap insert; O(1) best update.
    fn rest(&mut self, order: Order) {
        let price = order.price;
        self.levels.entry(price).or_default().push_back(order);
        self.best = Some(match self.side {
            Side::Buy  => self.best.map_or(price, |b| b.max(price)),
            Side::Sell => self.best.map_or(price, |b| b.min(price)),
        });
    }

    fn snapshot_levels(&self) -> Vec<PriceLevel> {
        match self.side {
            Side::Buy => self.levels.iter().rev()
                .map(|(&price, q)| PriceLevel { price, qty: q.iter().map(|o| o.qty).sum() })
                .collect(),
            Side::Sell => self.levels.iter()
                .map(|(&price, q)| PriceLevel { price, qty: q.iter().map(|o| o.qty).sum() })
                .collect(),
        }
    }
}

#[derive(Debug)]
pub struct OrderBook {
    bids: HalfBook,
    asks: HalfBook,
    next_id: u64,
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            bids: HalfBook::new(Side::Buy),
            asks: HalfBook::new(Side::Sell),
            next_id: 1,
        }
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    // Match and rest a taker order. Returns every fill produced.
    //
    // Double-match safety: this method must only be called from the engine
    // actor (engine/src/main.rs: engine_task). That actor processes one
    // EngineCmd at a time via a single mpsc::Receiver — there is no concurrent
    // access to this struct. Multiple API server instances all funnel orders
    // through that single channel, so two orders can never race here.
    pub fn submit(&mut self, mut taker: Order) -> Vec<Fill> {
        let mut fills = Vec::new();

        match taker.side {
            Side::Buy => {
                while taker.qty > 0 && self.asks.crosses(taker.price) {
                    // Read maker state before mutably borrowing asks.consume()
                    let (ask_price, maker_id, maker_qty) =
                        self.asks.peek_best().unwrap();

                    let fill_qty = taker.qty.min(maker_qty);
                    fills.push(Fill {
                        maker_order_id: maker_id,
                        taker_order_id: taker.id,
                        price: ask_price,
                        qty: fill_qty,
                    });
                    taker.qty -= fill_qty;
                    self.asks.consume(fill_qty);
                }
                if taker.qty > 0 {
                    self.bids.rest(taker);
                }
            }

            Side::Sell => {
                while taker.qty > 0 && self.bids.crosses(taker.price) {
                    let (bid_price, maker_id, maker_qty) =
                        self.bids.peek_best().unwrap();

                    let fill_qty = taker.qty.min(maker_qty);
                    fills.push(Fill {
                        maker_order_id: maker_id,
                        taker_order_id: taker.id,
                        price: bid_price,
                        qty: fill_qty,
                    });
                    taker.qty -= fill_qty;
                    self.bids.consume(fill_qty);
                }
                if taker.qty > 0 {
                    self.asks.rest(taker);
                }
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
        assert!(book.submit(order(1, Side::Buy,  99, 10)).is_empty());
        assert!(book.submit(order(2, Side::Sell, 101, 10)).is_empty());
    }

    #[test]
    fn exact_match() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Buy, 100, 10));
        let fills = book.submit(order(2, Side::Sell, 100, 10));
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].price, 100);
        assert_eq!(fills[0].qty, 10);
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
    fn best_pointer_updates_after_level_drained() {
        let mut book = OrderBook::new();
        book.submit(order(1, Side::Sell, 100, 5));
        book.submit(order(2, Side::Sell, 101, 5));
        // Drain 100 level completely
        book.submit(order(3, Side::Buy, 100, 5));
        // Best ask should now be 101
        assert_eq!(book.asks.best, Some(101));
    }
}
