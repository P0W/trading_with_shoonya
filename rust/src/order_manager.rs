// order_manager.rs

use async_trait::async_trait;
use log::*;
use shoonya::transaction::transaction::TransactionManager;
use shoonya::{
    auth::auth::Auth,
    transaction::transaction::Transaction,
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

pub struct WebSocketCallbackHandler {
    pub redis_transaction: TransactionManager,
    pub pnl_feed_callback: fn(f64, String),
}

impl WebSocketCallbackHandler {
    pub async fn new(
        callback: fn(f64, String),
    ) -> Result<WebSocketCallbackHandler, Box<dyn std::error::Error>> {
        let redis_transaction = TransactionManager::new().await.unwrap();
        Ok(WebSocketCallbackHandler {
            redis_transaction,
            pnl_feed_callback: callback,
        })
    }
}

#[async_trait]
impl WebSocketCallback for WebSocketCallbackHandler {
    async fn on_open(&mut self, res: &serde_json::Value) {
        info!("Websocket Opened {:?}", res);
    }

    async fn on_error(&mut self, res: &serde_json::Value) {
        info!("Websocket Error {:?}", res);
    }

    async fn subscribe_callback(&mut self, tick_data: &serde_json::Value) {
        debug!("Tick Data: {:?}", tick_data);
        let _ = self.redis_transaction.on_tick(tick_data).await;
        let (pnl, pnl_str) = self.redis_transaction.get_pnl().await;
        (self.pnl_feed_callback)(pnl, pnl_str);
        debug!("Done with subscribe_callback");
    }

    async fn order_callback(&mut self, order_data: &serde_json::Value) {
        debug!("Order Data: {:?}", order_data);
        let _ = self.redis_transaction.on_order(order_data).await;
    }

    async fn on_connect(&mut self, res: &serde_json::Value) {
        debug!("Connected to Websocket: {:?}", res);
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

    #[allow(dead_code)]
    pub async fn subscribe(&mut self, symbols: Vec<String>) {
        // Convert HashSet to Vec<String>
        let symbols: Vec<String> = symbols.to_vec();
        let _ = self.api.subscribe(&symbols).await;
        self.subscribed_symbols.extend(symbols);
        info!("Current subscribed_symbols: {:?}", self.subscribed_symbols);
    }

    #[allow(dead_code)]
    pub async fn unsubscribe(&mut self, symbols: Vec<String>) {
        let copy = self.subscribed_symbols.clone();
        for symbol in symbols {
            if self.subscribed_symbols.contains(&symbol) {
                info!("Unsubscribed from {}", symbol);
                self.subscribed_symbols.remove(&symbol);
            }
        }
        // Convert HashSet to Vec<String>
        let symbols: Vec<String> = copy.iter().cloned().collect();
        let _ = self.api.unsubscribe(&symbols).await;
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

    pub async fn start(&mut self) {
        let auth = self.auth.clone(); // Clone the auth object
        let thread = self.api.start_websocket(auth);
        self.opened = true;
        self.running = true;
        debug!("Websocket Started {:?}", self.running);
        let _ = thread.await;
    }

    pub async fn stop(&mut self) {
        let _ = self.api.close_websocket().await;
        debug!("Websocket Closed");
        self.running = false;
    }

    pub fn is_running(&self) -> bool {
        self.running
    }
}
