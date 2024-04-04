pub mod websocket {
    use crate::auth::auth::Auth;
    use crate::urls::urls::WEBSOCKET_ENDPOINT;
    use async_trait::async_trait;
    use futures_util::{SinkExt, TryStreamExt};
    use log::*;
    use serde_json::json;
    use std::borrow::BorrowMut;
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
            let ws_clone = self.websocket.borrow_mut();
            match ws_clone {
                Some(ws) => {
                    loop {
                        match ws.try_lock() {
                            Ok(mut ws) => {
                                let send_result = ws
                                    .send(tokio_tungstenite::tungstenite::Message::Text(
                                        data.to_string(),
                                    ))
                                    .await;
                                match send_result {
                                    Ok(_) => {
                                        return {
                                            info!("Data sent successfully");
                                            true
                                        }
                                    }
                                    Err(_) => {
                                        info!("Failed to send data");
                                    }
                                }
                                return true;
                            }
                            Err(_) => {
                                info!("Lock not acquired, while sending data {:?}", delay);
                                sleep(delay).await;
                                delay *= 2; // Double the delay for the next iteration
                            }
                        }
                    }
                }
                None => {
                    info!("Websocket not connected, retrying in {:?}", delay);
                    sleep(delay).await;
                    delay *= 2; // Double the delay for the next iteration
                    false
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
            loop {
                match self.callback.as_ref() {
                    Some(callback) => match callback.try_lock() {
                        Ok(mut callback) => {
                            callback.on_connect(&serde_json::Value::Null).await;
                            break;
                        }
                        Err(_) => {
                            error!("Failed to acquire lock on callback");
                            sleep(Duration::from_millis(100)).await;
                        }
                    },
                    None => {
                        error!("Callback not found");
                        sleep(Duration::from_millis(100)).await;
                    }
                }
            }

            let callback_clone = self.callback.clone().unwrap();
            let websocket_clone = self.websocket.clone().unwrap();
            let ws_thread = tokio::spawn(async move {
                loop {
                    //debug!("Waiting for message");
                    match websocket_clone.try_lock() {
                        Ok(mut ws_locked) => {
                            match ws_locked.try_next().await {
                                Ok(Some(message)) => {
                                    debug!("Message received: {:?}", message);
                                    handle_message(message, &callback_clone, ws_locked).await;
                                }
                                Ok(None) => {
                                    debug!("No more messages");
                                }
                                Err(_) => {
                                    debug!("Error while receiving message");
                                }
                            }

                            debug!("Message handled");
                        }
                        Err(_) => {
                            //  debug!("Websocket cannot be locked!");
                        }
                    }
                }
            });
            self.ws_thread = Some(ws_thread);

            Ok(())
        }
    }

    async fn handle_message(
        message: tokio_tungstenite::tungstenite::Message,
        callback_clone: &Arc<Mutex<dyn WebSocketCallback + Send>>,
        mut ws_locked: tokio::sync::MutexGuard<'_, WebSocketStream<MaybeTlsStream<TcpStream>>>,
    ) {
        debug!("Message: {:?}", message);
        match message {
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                let json: Result<serde_json::Value, _> = serde_json::from_str(text.as_str());
                //let mut callback = callback_clone.try_lock().unwrap();
                match json {
                    Ok(res) => {
                        // Use the data
                        if res["t"] == "tk"
                            || res["t"] == "tf"
                            || res["t"] == "dk"
                            || res["t"] == "df"
                        {
                            debug!("subscribe_callback {:?}", res);
                            //let _ = callback.subscribe_callback(&res).await;
                            //debug!("Sending ack");
                            loop {
                                match callback_clone.try_lock() {
                                    Ok(mut callback) => {
                                        let _ = callback.subscribe_callback(&res).await;
                                        debug!("Sending ack");
                                        break;
                                    }
                                    Err(_) => {
                                        debug!("Failed to acquire lock on callback - 1");
                                        std::thread::sleep(std::time::Duration::from_millis(100));
                                    }
                                }
                            }
                            debug!("Sending ack");
                        }
                        if res["t"] == "ck" && res["s"] != "OK" {
                            debug!("Error: {:?}", res);
                            loop {
                                match callback_clone.try_lock() {
                                    Ok(mut callback) => {
                                        let _ = callback.on_error(&res).await;
                                        break;
                                    }
                                    Err(_) => {
                                        debug!("Failed to acquire lock on callback - 2");
                                        std::thread::sleep(std::time::Duration::from_millis(100));
                                    }
                                }
                            }
                        }
                        if res["t"] == "om" {
                            debug!("Order: {:?}", res);
                            loop {
                                match callback_clone.try_lock() {
                                    Ok(mut callback) => {
                                        let _ = callback.order_callback(&res).await;
                                        break;
                                    }
                                    Err(_) => {
                                        debug!("Failed to acquire lock on callback - 3");
                                        std::thread::sleep(std::time::Duration::from_millis(100));
                                    }
                                }
                            }
                        }
                        if res["t"] == "ck" && res["s"] == "OK" {
                            debug!("Connected to websocket");
                            loop {
                                match callback_clone.try_lock() {
                                    Ok(mut callback) => {
                                        let _ = callback.on_open(&res).await;
                                        break;
                                    }
                                    Err(_) => {
                                        debug!("Failed to acquire lock on callback - 4");
                                        std::thread::sleep(std::time::Duration::from_millis(100));
                                    }
                                }
                            }
                        }
                    }
                    _ => {
                        println!("Error parsing JSON");
                    }
                }
            }
            tokio_tungstenite::tungstenite::Message::Ping(_) => {
                let pong_msg = "{\"t\":\"h\"}".to_owned();
                ws_locked
                    .send(tokio_tungstenite::tungstenite::Message::Text(pong_msg))
                    .await
                    .unwrap();
            }
            tokio_tungstenite::tungstenite::Message::Binary(bin) => {
                debug!("Binary message: {:?}", bin);
            }
            tokio_tungstenite::tungstenite::Message::Close(cl) => {
                debug!("Close message: {:?}", cl);
            }
            tokio_tungstenite::tungstenite::Message::Pong(pong) => {
                debug!("Pong message: {:?}", pong);
            }
            tokio_tungstenite::tungstenite::Message::Frame(frame) => {
                debug!("Frame message: {:?}", frame);
            }
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
