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
    use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

    #[async_trait]
    pub trait WebSocketApi {
        async fn subscribe(&mut self, symbols: &Vec<String>);
        async fn unsubscribe(&mut self, symbols: &Vec<String>);
    }

    pub trait WebSocketCallback {
        fn on_open(&mut self, res: &serde_json::Value);
        fn on_error(&mut self, res: &serde_json::Value);
        fn subscribe_callback(&mut self, res: &serde_json::Value);
        fn order_callback(&mut self, res: &serde_json::Value);
        fn on_connect(&mut self, res: &serde_json::Value);
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
            match self.websocket.as_mut() {
                Some(ws) => {
                    let ws = ws.try_lock();
                    match ws {
                        Ok(mut ws) => {
                            let _ = ws
                                .send(tokio_tungstenite::tungstenite::Message::Text(
                                    data.to_string(),
                                ))
                                .await;
                            true
                        }
                        Err(_) => {
                            info!("Lock not acquired, while sending data");
                            false
                        }
                    }
                }
                None => {
                    error!("Websocket not connected");
                    false
                }
            }
        }

        pub async fn close_websocket(&mut self) {
            if let Some(ws) = self.websocket.as_mut() {
                let mut ws = ws.try_lock().unwrap();
                ws.close(None).await.unwrap();
            }
            if let Some(ws_thread) = self.ws_thread.as_mut() {
                ws_thread.await.unwrap();
            }
        }

        pub async fn start_websocket(&mut self, auth: &Auth) {
            let (ws_original, _) = connect_async(WEBSOCKET_ENDPOINT)
                .await
                .expect("Failed to connect");
            debug!("Connected to websocket");
            self.websocket = Some(Arc::new(Mutex::new(ws_original)));
            {
                let values = json!(
                    {
                        "t": "c",
                        "uid": auth.username,
                        "actid": auth.username,
                        "susertoken": auth.susertoken,
                        "source": "API",
                    }
                );
                let sucess = self.send_data(values).await;
                if sucess {
                    info!("Websocket connected");
                } else {
                    error!("Failed to connect websocket");
                }
                self.callback
                    .as_mut()
                    .unwrap()
                    .try_lock()
                    .unwrap()
                    .on_connect(&serde_json::Value::Null);
            }

            let callback_clone = self.callback.clone().unwrap();
            let websocket_clone = self.websocket.clone().unwrap();
            let ws_thread = tokio::spawn(async move {
                loop {
                    //debug!("Waiting for message");
                    match websocket_clone.try_lock() {
                        Ok(mut ws_locked) => {
                            //debug!("Locked websocket");
                            //assert!(ws_locked.next().await.is_some());
                            let message = ws_locked.try_next().await.unwrap();
                            //debug!("Received message");
                            match message {
                                Some(tokio_tungstenite::tungstenite::Message::Text(text)) => {
                                    let json: Result<serde_json::Value, _> =
                                        serde_json::from_str(text.as_str());
                                    match json {
                                        Ok(res) => {
                                            // Use the data
                                            if res["t"] == "tk"
                                                || res["t"] == "tf"
                                                || res["t"] == "dk"
                                                || res["t"] == "df"
                                            {
                                                callback_clone
                                                    .try_lock()
                                                    .unwrap()
                                                    .subscribe_callback(&res);
                                            }
                                            if res["t"] == "ck" && res["s"] != "OK" {
                                                callback_clone.try_lock().unwrap().on_error(&res);
                                            }
                                            if res["t"] == "om" {
                                                callback_clone
                                                    .try_lock()
                                                    .unwrap()
                                                    .order_callback(&res);
                                            }
                                            if res["t"] == "ck" && res["s"] == "OK" {
                                                callback_clone.try_lock().unwrap().on_open(&res);
                                            } else {
                                                debug!("Unknown message: {:?}", res);
                                            }
                                        }
                                        _ => {
                                            println!("Error parsing JSON");
                                        }
                                    }
                                }
                                // Handle Ping messages
                                Some(tokio_tungstenite::tungstenite::Message::Ping(_)) => {
                                    let pong_msg = "{\"t\":\"h\"}".to_owned();
                                    ws_locked
                                        .send(tokio_tungstenite::tungstenite::Message::Text(
                                            pong_msg,
                                        ))
                                        .await
                                        .unwrap();
                                }
                                Some(tokio_tungstenite::tungstenite::Message::Binary(bin)) => {
                                    debug!("Binary message: {:?}", bin);
                                    callback_clone
                                        .try_lock()
                                        .unwrap()
                                        .on_error(&serde_json::Value::Null);
                                }
                                // Handle Close, Frame, Pong messages
                                Some(tokio_tungstenite::tungstenite::Message::Close(cl)) => {
                                    debug!("Close message: {:?}", cl);
                                    // callback_clone
                                    //     .try_lock()
                                    //     .unwrap()
                                    //     .on_error(&serde_json::Value::Null);
                                }
                                Some(tokio_tungstenite::tungstenite::Message::Pong(pong)) => {
                                    debug!("Pong message: {:?}", pong);
                                    // callback_clone
                                    //     .try_lock()
                                    //     .unwrap()
                                    //     .on_error(&serde_json::Value::Null);
                                }
                                Some(tokio_tungstenite::tungstenite::Message::Frame(frame)) => {
                                    debug!("Frame message: {:?}", frame);
                                    // callback_clone
                                    //     .try_lock()
                                    //     .unwrap()
                                    //     .on_error(&serde_json::Value::Null);
                                }
                                None => {
                                    // let json_msg = json!(
                                    //     {
                                    //         "msg": "No message received",
                                    //     }
                                    // );
                                    //callback_clone.try_lock().unwrap().on_error(&json_msg);
                                }
                            }
                        }
                        Err(_) => {
                            debug!("Websocket cannot be locked!");
                        }
                    }
                }
            });
            self.ws_thread = Some(ws_thread);
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
