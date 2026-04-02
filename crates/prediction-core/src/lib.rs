use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Order {
    pub id: u64,
    pub side: Side,
    pub price: u64,
    pub qty: u64,
    pub remaining_qty: u64,
}

impl Order {
    pub fn new(id: u64, side: Side, price: u64, qty: u64) -> Self {
        Self {
            id,
            side,
            price,
            qty,
            remaining_qty: qty,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fill {
    pub maker_order_id: u64,
    pub taker_order_id: u64,
    pub price: u64,
    pub qty: u64,
}

#[derive(Debug, Default)]
pub struct OrderBook {
    bids: BTreeMap<u64, VecDeque<Order>>,
    asks: BTreeMap<u64, VecDeque<Order>>,
}

impl OrderBook {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn submit(&mut self, mut taker: Order) -> Vec<Fill> {
        let mut fills = Vec::new();
        match taker.side {
            Side::Buy => self.match_buy(&mut taker, &mut fills),
            Side::Sell => self.match_sell(&mut taker, &mut fills),
        }
        if taker.remaining_qty > 0 {
            self.rest(taker);
        }
        fills
    }

    fn match_buy(&mut self, taker: &mut Order, fills: &mut Vec<Fill>) {
        while taker.remaining_qty > 0 {
            let Some(min_ask_price) = self.asks.keys().next().copied() else {
                break;
            };
            if min_ask_price > taker.price {
                break;
            }
            self.consume_level(
                min_ask_price,
                true,
                taker,
                fills,
            );
        }
    }

    fn match_sell(&mut self, taker: &mut Order, fills: &mut Vec<Fill>) {
        while taker.remaining_qty > 0 {
            let Some(max_bid_price) = self.bids.keys().next_back().copied() else {
                break;
            };
            if max_bid_price < taker.price {
                break;
            }
            self.consume_level(
                max_bid_price,
                false,
                taker,
                fills,
            );
        }
    }

    fn consume_level(
        &mut self,
        price: u64,
        is_ask_level: bool,
        taker: &mut Order,
        fills: &mut Vec<Fill>,
    ) {
        let book = if is_ask_level {
            self.asks.get_mut(&price)
        } else {
            self.bids.get_mut(&price)
        }
        .expect("level exists");

        while taker.remaining_qty > 0 {
            let Some(maker) = book.front_mut() else {
                break;
            };
            let trade_qty = taker.remaining_qty.min(maker.remaining_qty);
            fills.push(Fill {
                maker_order_id: maker.id,
                taker_order_id: taker.id,
                price,
                qty: trade_qty,
            });
            maker.remaining_qty -= trade_qty;
            taker.remaining_qty -= trade_qty;
            if maker.remaining_qty == 0 {
                book.pop_front();
            }
            if book.is_empty() {
                break;
            }
        }

        if book.is_empty() {
            if is_ask_level {
                self.asks.remove(&price);
            } else {
                self.bids.remove(&price);
            }
        }
    }

    fn rest(&mut self, order: Order) {
        match order.side {
            Side::Buy => {
                self.bids
                    .entry(order.price)
                    .or_default()
                    .push_back(order);
            }
            Side::Sell => {
                self.asks
                    .entry(order.price)
                    .or_default()
                    .push_back(order);
            }
        }
    }

    pub fn snapshot_levels(&self) -> OrderBookSnapshot {
        let bids: Vec<BookLevel> = self
            .bids
            .iter()
            .rev()
            .map(|(&price, q)| BookLevel {
                price,
                orders: q.iter().map(public_resting_order).collect(),
            })
            .collect();
        let asks: Vec<BookLevel> = self
            .asks
            .iter()
            .map(|(&price, q)| BookLevel {
                price,
                orders: q.iter().map(public_resting_order).collect(),
            })
            .collect();
        OrderBookSnapshot { bids, asks }
    }
}

fn public_resting_order(o: &Order) -> PublicOrder {
    PublicOrder {
        id: o.id,
        side: o.side,
        price: o.price,
        qty: o.remaining_qty,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicOrder {
    pub id: u64,
    pub side: Side,
    pub price: u64,
    pub qty: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderBookSnapshot {
    pub bids: Vec<BookLevel>,
    pub asks: Vec<BookLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookLevel {
    pub price: u64,
    pub orders: Vec<PublicOrder>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn price_time_priority_partial() {
        let mut book = OrderBook::new();
        let _ = book.submit(Order::new(1, Side::Sell, 100, 10));
        let _ = book.submit(Order::new(2, Side::Sell, 100, 5));
        let fills = book.submit(Order::new(3, Side::Buy, 100, 8));
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].qty, 8);
        assert_eq!(fills[0].maker_order_id, 1);
        let snap = book.snapshot_levels();
        let ask100 = snap.asks.iter().find(|l| l.price == 100).unwrap();
        assert_eq!(ask100.orders.len(), 2);
        assert_eq!(ask100.orders[0].id, 1);
        assert_eq!(ask100.orders[0].qty, 2);
        assert_eq!(ask100.orders[1].id, 2);
        assert_eq!(ask100.orders[1].qty, 5);
    }

    #[test]
    fn sell_matches_highest_bid() {
        let mut book = OrderBook::new();
        let _ = book.submit(Order::new(1, Side::Buy, 101, 3));
        let _ = book.submit(Order::new(2, Side::Buy, 100, 5));
        let fills = book.submit(Order::new(3, Side::Sell, 99, 4));
        assert_eq!(fills.len(), 2);
        assert_eq!(fills[0].price, 101);
        assert_eq!(fills[0].qty, 3);
        assert_eq!(fills[1].price, 100);
        assert_eq!(fills[1].qty, 1);
        let snap = book.snapshot_levels();
        let bid100 = snap.bids.iter().find(|l| l.price == 100).unwrap();
        assert_eq!(bid100.orders[0].qty, 4);
    }
}
