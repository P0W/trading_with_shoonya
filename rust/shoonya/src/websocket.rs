pub mod websocket {
    use crate::auth::auth::Auth;
    use crate::urls::urls::WEBSOCKET_ENDPOINT;
    use async_trait::async_trait;
    use futures_util::{SinkExt, TryStreamExt};
    use log::*;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::net::TcpStream;
    use tokio::sync::Mutex;
    use tokio::time::{sleep, Duration};
    use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

    #[async_trait]
    pub trait WebSocketApi {
        async fn subscribe(&mut self, symbols: &Vec<String>);
        async fn unsubscribe(&mut self, symbols: &Vec<String>);
    }

    #[async_trait]
    pub trait WebSocketCallback {
        async fn on_open(&mut self, res: &serde_json::Value);
        async fn on_error(&mut self, res: &serde_json::Value);
        async fn subscribe_callback(&mut self, res: &serde_json::Value);
        async fn order_callback(&mut self, res: &serde_json::Value);
        async fn on_connect(&mut self, res: &serde_json::Value);
    }

    pub struct WebSocketApp {
        websocket: Option<Arc<Mutex<WebSocketStream<MaybeTlsStream<TcpStream>>>>>,
        ws_thread: Option<tokio::task::JoinHandle<()>>,
        callback: Option<Arc<Mutex<dyn WebSocketCallback + Send>>>,
    }

    impl WebSocketApp {
        pub fn new<T: 'static + WebSocketCallback + Send>(callback: T) -> Self {
            WebSocketApp {
                websocket: None,
                ws_thread: None,
                callback: Some(Arc::new(Mutex::new(callback))),
            }
        }

        async fn send_data(&mut self, data: serde_json::Value) -> bool {
            let mut delay = Duration::from_millis(10);
            loop {
                match self.websocket.as_mut() {
                    Some(ws) => {
                        let ws = ws.try_lock();
                        match ws {
                            Ok(mut ws) => {
                                let send_result = ws
                                    .send(tokio_tungstenite::tungstenite::Message::Text(
                                        data.to_string(),
                                    ))
                                    .await;
                                match send_result {
                                    Ok(_) => return true,
                                    Err(_) => {
                                        info!("Failed to send data, retrying in {:?}", delay);
                                        sleep(delay).await;
                                        delay *= 2; // Double the delay for the next iteration
                                    }
                                }
                            }
                            Err(_) => {
                                info!("Lock not acquired, while sending data");
                                return false;
                            }
                        }
                    }
                    None => {
                        error!("Websocket not connected");
                        return false;
                    }
                }
            }
        }

        pub async fn close_websocket(&mut self) -> Result<(), Box<dyn std::error::Error>> {
            if let Some(ws) = self.websocket.as_mut() {
                let ws_lock = ws.try_lock();
                match ws_lock {
                    Ok(mut ws) => {
                        ws.close(None).await?;
                    }
                    Err(e) => {
                        error!("Failed to acquire lock on websocket: {}", e);
                        return Err(Box::new(e));
                    }
                }
            }
            if let Some(ws_thread) = self.ws_thread.as_mut() {
                ws_thread.await?;
            }
            info!("Websocket closed successfully");
            Ok(())
        }
        pub async fn start_websocket(
            &mut self,
            auth: &Auth,
        ) -> Result<(), Box<dyn std::error::Error>> {
            let (ws_original, _) = connect_async(WEBSOCKET_ENDPOINT).await?;
            debug!("Connected to websocket");
            self.websocket = Some(Arc::new(Mutex::new(ws_original)));

            let values = json!(
                {
                    "t": "c",
                    "uid": auth.username,
                    "actid": auth.username,
                    "susertoken": auth.susertoken,
                    "source": "API",
                }
            );
            let success = self.send_data(values).await;
            if success {
                info!("Websocket connected");
            } else {
                error!("Failed to connect websocket");
                return Err(Box::new(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Failed to connect websocket",
                )));
            }

            if let Some(callback) = &self.callback {
                let mut callback = callback.try_lock().map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::Other, "Failed to lock callback")
                })?;
                callback.on_connect(&serde_json::Value::Null).await;
            }

            let callback_clone = self.callback.clone().unwrap();
            let websocket_clone = self.websocket.clone().unwrap();
            let ws_thread = tokio::spawn(async move {
                loop {
                    match websocket_clone.try_lock() {
                        Ok(mut ws_locked) => {
                            let message = ws_locked.try_next().await.unwrap();
                            handle_message(message, &callback_clone, ws_locked).await;
                        }
                        Err(_) => {
                            debug!("Websocket cannot be locked!");
                        }
                    }
                }
            });
            self.ws_thread = Some(ws_thread);

            Ok(())
        }
    }

    async fn handle_message(
        message: Option<tokio_tungstenite::tungstenite::Message>,
        callback_clone: &Arc<Mutex<dyn WebSocketCallback + Send>>,
        mut ws_locked: tokio::sync::MutexGuard<'_, WebSocketStream<MaybeTlsStream<TcpStream>>>,
    ) {
        match message {
            Some(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                let json: Result<serde_json::Value, _> = serde_json::from_str(text.as_str());
                let mut callback = callback_clone.try_lock().unwrap();
                match json {
                    Ok(res) => {
                        // Use the data
                        if res["t"] == "tk"
                            || res["t"] == "tf"
                            || res["t"] == "dk"
                            || res["t"] == "df"
                        {
                            debug!("Unknown message --1: {:?}", res);
                            let _ = callback.subscribe_callback(&res).await;
                        }
                        if res["t"] == "ck" && res["s"] != "OK" {
                            let _ = callback.on_error(&res).await;
                        }
                        if res["t"] == "om" {
                            let _ = callback.order_callback(&res).await;
                        }
                        if res["t"] == "ck" && res["s"] == "OK" {
                            let _ = callback.on_open(&res).await;
                        } else {
                            debug!("Unknown message: {:?}", res);
                        }
                    }
                    _ => {
                        println!("Error parsing JSON");
                    }
                }
            }
            Some(tokio_tungstenite::tungstenite::Message::Ping(_)) => {
                let pong_msg = "{\"t\":\"h\"}".to_owned();
                ws_locked
                    .send(tokio_tungstenite::tungstenite::Message::Text(pong_msg))
                    .await
                    .unwrap();
            }
            Some(tokio_tungstenite::tungstenite::Message::Binary(bin)) => {
                debug!("Binary message: {:?}", bin);
            }
            Some(tokio_tungstenite::tungstenite::Message::Close(cl)) => {
                debug!("Close message: {:?}", cl);
            }
            Some(tokio_tungstenite::tungstenite::Message::Pong(pong)) => {
                debug!("Pong message: {:?}", pong);
            }
            Some(tokio_tungstenite::tungstenite::Message::Frame(frame)) => {
                debug!("Frame message: {:?}", frame);
            }
            None => {}
        }
    }

    #[async_trait]
    impl WebSocketApi for WebSocketApp {
        async fn subscribe(&mut self, symbols: &Vec<String>) {
            info!("Subscribing to {:?}", symbols);
            let values = json!({
                "t": "t",
                "k": symbols.join("#"),
            });

            debug!("Subscribing json: {:?}", values);

            if self.send_data(values).await {
                info!("Subscribed to {:?}", symbols);
            } else {
                error!("Failed to subscribe to {:?}", symbols);
            }
        }

        async fn unsubscribe(&mut self, symbols: &Vec<String>) {
            info!("Unsubscribing from {:?}", symbols);
            let values = json!({
                "t":"u",
                "k": symbols.join("#"),
            });
            if self.send_data(values).await {
                info!("Unsubscribed from {:?}", symbols);
            } else {
                error!("Failed to unsubscribe from {:?}", symbols);
            }
        }
    }
}
