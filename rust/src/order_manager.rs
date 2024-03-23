// order_manager.rs

use log::*;
use serde_json;
use shoonya::markets::markets::{WebSocketApp, Websocket};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

pub struct OrderManager {
    api: WebSocketApp,
    opened: bool,
    subscribed_symbols: HashSet<String>,
    running: bool,
    config: serde_json::Value,
}

impl OrderManager {
    pub fn new(api_object: WebSocketApp, config: serde_json::Value) -> OrderManager {
        OrderManager {
            api: api_object,
            opened: false,
            subscribed_symbols: HashSet::new(),
            running: false,
            config,
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

    pub fn subscribe(&mut self, symbols: Vec<String>) {
        // Convert HashSet to Vec<String>
        let symbols: Vec<String> = symbols.iter().cloned().collect();
        self.api.subscribe(&symbols);
        self.subscribed_symbols.extend(symbols);
        info!("Current subscribed_symbols: {:?}", self.subscribed_symbols);
    }

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

    pub fn start(&mut self, user_callback: fn(serde_json::Value)) {
        let url = "wss://api.shoonya.com/NorenWSTP/".to_owned();
        self.api.start_websocket(
            &url,
            user_callback,
            || {
                //  let mut self_clone = self_clone.lock().unwrap();
                //  self_clone._open_callback();
                
            },
            |err: String| {
                error!("Error: {}", err);
            },
        );
        self.opened = true;
        self.running = true;
    }
}
