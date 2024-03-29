// order_manager.rs

use log::*;
use serde_json;
use shoonya::transaction::transaction::TransactionManager;
use shoonya::{
    auth::auth::Auth,
    transaction::transaction::Transaction,
    websocket::websocket::{WebSocketApi, WebSocketApp, WebSocketCallback},
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

pub struct OrderManager {
    api: WebSocketApp,
    opened: bool,
    subscribed_symbols: HashSet<String>,
    running: bool,
    auth: Rc<RefCell<Auth>>,
}

pub struct WebSocketCallbackHandler {
    pub redis_transaction: TransactionManager,
    pub pnl_feed_callback: Option<Box<dyn Fn(f64, String) + Send>>,
}

impl WebSocketCallbackHandler {
    pub fn new(
        callback: Option<Box<dyn Fn(f64, String) + Send + 'static>>,
    ) -> WebSocketCallbackHandler {
        WebSocketCallbackHandler {
            redis_transaction: TransactionManager::new(),
            pnl_feed_callback: callback,
        }
    }
}

impl WebSocketCallback for WebSocketCallbackHandler {
    fn on_open(&mut self, res: &serde_json::Value) {
        info!("Websocket Opened {:?}", res);
    }

    fn on_error(&mut self, res: &serde_json::Value) {
        info!("Websocket Error {:?}", res);
    }

    fn subscribe_callback(&mut self, tick_data: &serde_json::Value) {
        self.redis_transaction.on_tick(tick_data);
        let (pnl, pnl_str) = self.redis_transaction.get_pnl();
        if let Some(callback) = self.pnl_feed_callback.as_mut() {
            callback(pnl, pnl_str);
        }
    }

    fn order_callback(&mut self, order_data: &serde_json::Value) {
        self.redis_transaction.on_order(order_data);
    }
}

impl OrderManager {
    pub fn new(api_object: WebSocketApp, auth: Rc<RefCell<Auth>>) -> OrderManager {
        OrderManager {
            api: api_object,
            opened: false,
            subscribed_symbols: HashSet::new(),
            running: false,
            auth,
        }
    }

    fn _open_callback(&mut self) {
        if self.opened {
            info!("Websocket Re-Opened");
            if !self.subscribed_symbols.is_empty() {
                info!("Resubscribing to {:?}", self.subscribed_symbols);
                // Convert HashSet to Vec<String>
                let symbols: Vec<String> = self.subscribed_symbols.iter().cloned().collect();
                self.api.subscribe(&symbols);
            }
        } else {
            info!("Websocket Opened");
        }
        self.opened = true;
    }

    #[allow(dead_code)]
    pub fn subscribe(&mut self, symbols: Vec<String>) {
        // Convert HashSet to Vec<String>
        let symbols: Vec<String> = symbols.iter().cloned().collect();
        self.api.subscribe(&symbols);
        self.subscribed_symbols.extend(symbols);
        info!("Current subscribed_symbols: {:?}", self.subscribed_symbols);
    }

    #[allow(dead_code)]
    pub fn unsubscribe(&mut self, symbols: Vec<String>) {
        let copy = self.subscribed_symbols.clone();
        for symbol in symbols {
            if self.subscribed_symbols.contains(&symbol) {
                info!("Unsubscribed from {}", symbol);
                self.subscribed_symbols.remove(&symbol);
            }
        }
        // Convert HashSet to Vec<String>
        let symbols: Vec<String> = copy.iter().cloned().collect();
        self.api.unsubscribe(&symbols);
        debug!("Current subscribed_symbols: {:?}", self.subscribed_symbols);
    }

    pub fn day_over(&mut self) -> bool {
        let now = chrono::Utc::now() + chrono::Duration::hours(5) + chrono::Duration::minutes(30);
        let end_time = chrono::NaiveTime::from_hms_opt(15, 30, 0).unwrap();
        if now.time() > end_time {
            info!("Day over");
            return true;
        }
        false
    }

    pub fn start(&mut self) {
        self.api.start_websocket(&self.auth.as_ref().borrow());
        self.opened = true;
        self.running = true;
    }
}
