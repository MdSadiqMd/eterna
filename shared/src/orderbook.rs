use std::collections::{BTreeMap, VecDeque};

use crate::{Fill, Order, OrderBookSnapshot, PriceLevel, Side};

#[derive(Debug)]
pub struct OrderBook {
    // bids: highest price first (use .iter().rev() for descending traversal)
    bids: BTreeMap<u64, VecDeque<Order>>,
    // asks: lowest price first (natural BTreeMap order)
    asks: BTreeMap<u64, VecDeque<Order>>,
    next_id: u64,
}

impl OrderBook {
    pub fn new() -> Self {
        Self {
            bids: BTreeMap::new(),
            asks: BTreeMap::new(),
            next_id: 1,
        }
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn submit(&mut self, mut order: Order) -> Vec<Fill> {
        let mut fills = Vec::new();

        match order.side {
            Side::Buy => {
                while order.qty > 0 {
                    let Some(ask_price) = self
                        .asks
                        .keys()
                        .copied()
                        .next()
                        .filter(|&p| p <= order.price)
                    else {
                        break;
                    };

                    let queue = self.asks.get_mut(&ask_price).unwrap();
                    let maker = queue.front_mut().unwrap();

                    let fill_qty = order.qty.min(maker.qty);
                    fills.push(Fill {
                        maker_order_id: maker.id,
                        taker_order_id: order.id,
                        price: ask_price,
                        qty: fill_qty,
                    });

                    maker.qty -= fill_qty;
                    order.qty -= fill_qty;

                    if maker.qty == 0 {
                        queue.pop_front();
                        if queue.is_empty() {
                            self.asks.remove(&ask_price);
                        }
                    }
                }

                if order.qty > 0 {
                    self.bids.entry(order.price).or_default().push_back(order);
                }
            }

            Side::Sell => {
                while order.qty > 0 {
                    let Some(bid_price) = self
                        .bids
                        .keys()
                        .copied()
                        .next_back()
                        .filter(|&p| p >= order.price)
                    else {
                        break;
                    };

                    let queue = self.bids.get_mut(&bid_price).unwrap();
                    let maker = queue.front_mut().unwrap();

                    let fill_qty = order.qty.min(maker.qty);
                    fills.push(Fill {
                        maker_order_id: maker.id,
                        taker_order_id: order.id,
                        price: bid_price,
                        qty: fill_qty,
                    });

                    maker.qty -= fill_qty;
                    order.qty -= fill_qty;

                    if maker.qty == 0 {
                        queue.pop_front();
                        if queue.is_empty() {
                            self.bids.remove(&bid_price);
                        }
                    }
                }

                if order.qty > 0 {
                    self.asks.entry(order.price).or_default().push_back(order);
                }
            }
        }

        fills
    }

    pub fn snapshot(&self) -> OrderBookSnapshot {
        OrderBookSnapshot {
            bids: self
                .bids
                .iter()
                .rev()
                .map(|(&price, queue)| PriceLevel {
                    price,
                    qty: queue.iter().map(|o| o.qty).sum(),
                })
                .collect(),
            asks: self
                .asks
                .iter()
                .map(|(&price, queue)| PriceLevel {
                    price,
                    qty: queue.iter().map(|o| o.qty).sum(),
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Side;

    fn make_order(id: u64, side: Side, price: u64, qty: u64) -> Order {
        Order { id, side, price, qty }
    }

    #[test]
    fn no_match_when_spread_exists() {
        let mut book = OrderBook::new();
        let fills = book.submit(make_order(1, Side::Buy, 99, 10));
        assert!(fills.is_empty());
        let fills = book.submit(make_order(2, Side::Sell, 101, 10));
        assert!(fills.is_empty());
    }

    #[test]
    fn exact_match() {
        let mut book = OrderBook::new();
        book.submit(make_order(1, Side::Buy, 100, 10));
        let fills = book.submit(make_order(2, Side::Sell, 100, 10));
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
    fn partial_fill_resting_remainder() {
        let mut book = OrderBook::new();
        book.submit(make_order(1, Side::Buy, 100, 10));
        let fills = book.submit(make_order(2, Side::Sell, 100, 6));
        assert_eq!(fills[0].qty, 6);
        let snap = book.snapshot();
        assert_eq!(snap.bids[0].qty, 4);
        assert!(snap.asks.is_empty());
    }

    #[test]
    fn taker_sweeps_multiple_levels() {
        let mut book = OrderBook::new();
        book.submit(make_order(1, Side::Sell, 101, 5));
        book.submit(make_order(2, Side::Sell, 102, 5));
        let fills = book.submit(make_order(3, Side::Buy, 103, 10));
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].price, 101);
        assert_eq!(fills[1].price, 102);
    }

    #[test]
    fn price_time_priority() {
        let mut book = OrderBook::new();
        book.submit(make_order(1, Side::Buy, 100, 5));
        book.submit(make_order(2, Side::Buy, 100, 5));
        let fills = book.submit(make_order(3, Side::Sell, 100, 5));
        assert_eq!(fills[0].maker_order_id, 1);
    }

    #[test]
    fn fill_at_maker_price() {
        let mut book = OrderBook::new();
        book.submit(make_order(1, Side::Sell, 100, 10));
        let fills = book.submit(make_order(2, Side::Buy, 105, 10));
        assert_eq!(fills[0].price, 100);
    }
}
