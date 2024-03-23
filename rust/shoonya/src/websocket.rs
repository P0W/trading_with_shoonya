pub mod websocket {
    use crate::auth::auth::Auth;
    use crate::urls::urls::WEBSOCKET_ENDPOINT;
    use log::*;
    use serde_json::json;
    use std::{
        net::TcpStream,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    };
    use std::{sync::Mutex, thread::JoinHandle};
    use tungstenite::{connect, protocol::WebSocket, stream::MaybeTlsStream};

    pub trait WebSocketApi {
        fn subscribe(&mut self, symbols: &Vec<String>);
        fn unsubscribe(&mut self, symbols: &Vec<String>);
        fn start_websocket(&mut self, auth: &Auth);
        fn close_websocket(&mut self);
    }

    pub trait WebSocketCallback {
        fn on_open(&self, res: &serde_json::Value);
        fn on_error(&self, res: &serde_json::Value);
        fn subscribe_callback(&self, res: &serde_json::Value);
        fn order_callback(&self, res: &serde_json::Value);
    }

    pub struct WebSocketApp {
        websocket_connected: Arc<AtomicBool>,
        // Shared Pointer to MayBeTlsStream<TcpStream>
        websocket: Option<Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>>,
        ws_thread: Option<JoinHandle<()>>,
        callback: Option<Arc<Mutex<dyn WebSocketCallback + Send>>>,
    }

    impl WebSocketApp {
        pub fn new<T: 'static + WebSocketCallback + Send>(callback: T) -> Self {
            WebSocketApp {
                websocket_connected: Arc::new(AtomicBool::new(false)),
                // dummy value to websocket
                websocket: None,
                ws_thread: None,
                callback: Some(Arc::new(Mutex::new(callback))),
            }
        }
    }

    impl WebSocketApi for WebSocketApp {
        fn subscribe(&mut self, symbols: &Vec<String>) {
            info!("Subscribing to {:?}", symbols);
            let values = json!({
                "t":"t",
                "k": symbols.join("#"),
            });
            if let Some(ws) = self.websocket.as_mut() {
                let mut ws = ws.lock().unwrap();
                ws.send(tungstenite::Message::Text(values.to_string()))
                    .unwrap();
            }
        }

        fn unsubscribe(&mut self, symbols: &Vec<String>) {
            info!("Unsubscribing from {:?}", symbols);
            let values = json!({
                "t":"u",
                "k": symbols.join("#"),
            });
            if let Some(ws) = self.websocket.as_mut() {
                let mut ws = ws.lock().unwrap();
                ws.send(tungstenite::Message::Text(values.to_string()))
                    .unwrap();
            }
        }

        fn start_websocket(&mut self, auth: &Auth) {
            let (ws_original, _) = connect(WEBSOCKET_ENDPOINT).unwrap();
            self.websocket = Some(Arc::new(Mutex::new(ws_original)));
            self.websocket_connected.store(true, Ordering::SeqCst);

            let values = json!(
                {
                    "t": "c",
                    "uid": auth.username,
                    "actid": auth.username,
                    "susertoken": auth.susertoken,
                    "source": "API",
                }
            );
            if let Some(ws) = self.websocket.as_mut() {
                let mut ws = ws.lock().unwrap();
                ws.send(tungstenite::Message::Text(values.to_string()))
                    .unwrap();
            }
            let connected_clone = self.websocket_connected.clone();
            let ws_clone = self.websocket.clone().unwrap();
            let callback_clone = self.callback.clone().unwrap();
            let ws_thread = std::thread::spawn(move || {
                let mut ws = ws_clone.lock().unwrap();
                loop {
                    let result = ws.read();
                    match result {
                        Ok(msg) => match msg {
                            tungstenite::Message::Text(text) => {
                                let res: serde_json::Value = serde_json::from_str(&text).unwrap();
                                if res["t"] == "tk"
                                    || res["t"] == "tf"
                                    || res["t"] == "dk"
                                    || res["t"] == "df"
                                {
                                    callback_clone.lock().unwrap().subscribe_callback(&res);
                                }
                                if res["t"] == "ck" && res["s"] != "OK" {
                                    callback_clone.lock().unwrap().on_error(&res);
                                }
                                if res["t"] == "om" {
                                    callback_clone.lock().unwrap().order_callback(&res);
                                }
                                if res["t"] == "ck" && res["s"] == "OK" {
                                    callback_clone.lock().unwrap().on_open(&res);
                                }
                            }
                            tungstenite::Message::Close(_) => {
                                info!("Closing Websocket");
                                connected_clone.store(false, Ordering::SeqCst);
                                break;
                            }
                            // ping message
                            tungstenite::Message::Ping(_) => {
                                let pong_msg = "{\"t\":\"h\"}".to_owned();
                                ws.write(tungstenite::Message::Pong(pong_msg.into_bytes()))
                                    .unwrap();
                            }
                            // Handle errors
                            _ => {
                                callback_clone
                                    .lock()
                                    .unwrap()
                                    .on_error(&serde_json::Value::Null);
                                break;
                            }
                        },
                        Err(e) => {
                            // get the error message as serde_json::Value
                            let error_msg = json!({"error": e.to_string()});
                            callback_clone.lock().unwrap().on_error(&error_msg);
                            break;
                        }
                    }
                }
            });

            self.ws_thread = Some(ws_thread);
        }

        fn close_websocket(&mut self) {
            if self.websocket_connected.load(Ordering::SeqCst) == false {
                return;
            }
            self.websocket_connected.store(false, Ordering::SeqCst);

            if let Some(ws) = self.websocket.as_mut() {
                let mut ws = ws.lock().unwrap();
                ws.close(None).unwrap();
            }
            if let Some(handle) = self.ws_thread.take() {
                handle.join().unwrap();
            }
        }
    }
}
