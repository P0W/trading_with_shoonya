// order_manager.rs

use log::*;
use serde_json;
use shoonya::{
    auth::auth::Auth,
    websocket::websocket::{WebSocketApi, WebSocketApp, WebSocketCallback},
};
use std::collections::HashSet;

pub struct OrderManager {
    api: WebSocketApp,
    opened: bool,
    subscribed_symbols: HashSet<String>,
    running: bool,
    auth: Auth,
}

pub struct WebSocketCallbackHandler;

impl WebSocketCallback for WebSocketCallbackHandler {
    fn on_open(&self, res: &serde_json::Value) {
        info!("Websocket Opened {:?}", res);
    }

    fn on_error(&self, res: &serde_json::Value) {
        info!("Websocket Error {:?}", res);
    }

    fn subscribe_callback(&self, res: &serde_json::Value) {
        info!("Subscribed to {:?}", res);
    }

    fn order_callback(&self, res: &serde_json::Value) {
        info!("Order Callback {:?}", res);
    }
}

impl OrderManager {
    pub fn new(api_object: WebSocketApp, auth: Auth) -> OrderManager {
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
        self.api.start_websocket(&self.auth);
        self.opened = true;
        self.running = true;
    }
}
