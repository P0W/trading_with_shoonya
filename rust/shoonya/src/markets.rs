pub mod markets {

    use crate::{
        auth::auth::Auth,
        urls::urls::{GETQUOTES, GET_INDICES_LIST, HOST, OPTIONCHAIN},
    };
    use common::utils::utils::{get_exchange_str, pretty_print_json, Exchange};
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
    use url::Url;

    fn _get_payload(susertoken: &str, values: &serde_json::Value) -> String {
        let payload = format!("jData={}&jKey={}", values.to_string(), susertoken);

        payload
    }

    pub trait Markets {
        fn get_quote(&self, _exchange: &Exchange, _token: &str) -> f64 {
            0.0
        }
        fn get_indices(
            &self,
            _exchange: &Exchange,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            Ok(serde_json::Value::Null)
        }
        fn get_option_chain(
            &self,
            exchange: &Exchange,
            tsym: &str,
            strike_price: f64,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error>>;
    }

    pub trait Websocket {
        fn subscribe(&mut self, symbols: &Vec<String>);
        fn unsubscribe(&mut self, symbols: &Vec<String>);
        fn _event_handler_feed_update(&self, tick_data: serde_json::Value);
        fn _event_handler_order_update(&self, order_data: serde_json::Value);
        fn open_callback(&self);
        fn socket_error_callback(&self);
        fn socket_close_callback(&self);
        fn start_websocket<F, G, H>(
            &mut self,
            url: &str,
            user_callback: F,
            open_callback: G,
            error_callback: H,
        ) where
            F: Fn(serde_json::Value) + Send + 'static,

            G: Fn() + Send + 'static,
            H: Fn(String) + Send + 'static;
        fn close_websocket(&mut self);
    }

    impl Markets for Auth {
        fn get_option_chain(
            &self,
            exchange: &Exchange,
            tsym: &str,
            strike_price: f64,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            let values = json!({
                "ordersource": "API",
                "exch": get_exchange_str(exchange),
                "uid": self.username,
                "strprc": format!("{}", strike_price),
                "cnt": "5",
                "tsym": tsym
            });

            let url = format!("{}{}", HOST, OPTIONCHAIN);
            let payload = _get_payload(&self.susertoken, &values);

            let client = reqwest::blocking::Client::new();
            let res: String = client.post(&url).body(payload).send()?.text()?;

            let res_dict: serde_json::Value = serde_json::from_str(&res)?;
            if let Some(obj) = res_dict.as_object() {
                if obj.contains_key("stat") {
                    // "stat" is present in the response
                    if obj["stat"] == "Ok" {
                        // "stat" is "Ok"
                        return Ok(res_dict);
                    } else {
                        // "stat" is not "Ok"
                        return Err(res_dict.to_string().into());
                    }
                } else {
                    // "stat" is not present in the response
                    return Ok(res_dict);
                }
            }

            Ok(res_dict)
        }

        fn get_indices(
            &self,
            exchange: &Exchange,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            let values = json!({
                "ordersource": "API",
                "exch": get_exchange_str(exchange),
                "uid": self.username,
            });

            let url = format!("{}{}", HOST, GET_INDICES_LIST);
            let payload = _get_payload(&self.susertoken, &values);

            let client = reqwest::blocking::Client::new();
            let res: String = client.post(&url).body(payload).send()?.text()?;

            let res_dict: serde_json::Value = serde_json::from_str(&res)?;
            if let Some(obj) = res_dict.as_object() {
                if obj.contains_key("stat") {
                    // "stat" is present in the response
                    if obj["stat"] == "Ok" {
                        // "stat" is "Ok"
                        return Ok(res_dict);
                    } else {
                        // "stat" is not "Ok"
                        return Err(res_dict.to_string().into());
                    }
                } else {
                    // "stat" is not present in the response
                    return Ok(res_dict);
                }
            }

            Ok(res_dict)
        }

        fn get_quote(&self, exchange: &Exchange, token: &str) -> f64 {
            let values = json!({
                "ordersource": "API",
                "exch": get_exchange_str(exchange),
                "uid": self.username,
                "token": token,
            });

            let url = format!("{}{}", HOST, GETQUOTES);
            let payload = _get_payload(&self.susertoken, &values);

            let client = reqwest::blocking::Client::new();
            let res: String = client
                .post(&url)
                .body(payload)
                .send()
                .unwrap()
                .text()
                .unwrap();

            let res_dict: serde_json::Value = serde_json::from_str(&res).unwrap();
            if let Some(obj) = res_dict.as_object() {
                if obj.contains_key("stat") {
                    // "stat" is present in the response
                    if obj["stat"] == "Ok" {
                        // "stat" is "Ok"
                        let lp: f64 = obj["lp"].as_str().unwrap().parse().unwrap_or_else(|_| {
                            log::error!("Error: {}", pretty_print_json(&res_dict, 2));
                            -9999.0
                        });
                        return lp;
                    } else {
                        // "stat" is not "Ok"
                        return -9999.0;
                    }
                } else {
                    // "stat" is not present in the response
                    return -9999.0;
                }
            }
            -9999.0
        }
    }

    pub struct WebSocketApp {
        websocket_connected: Arc<AtomicBool>,
        // Shared Pointer to MayBeTlsStream<TcpStream>
        websocket: Option<Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>>,
        ws_thread: Option<JoinHandle<()>>,
    }

    impl WebSocketApp {
        pub fn new() -> Self {
            WebSocketApp {
                websocket_connected: Arc::new(AtomicBool::new(false)),
                // dummy value to websocket
                websocket: None,
                ws_thread: None,
            }
        }
    }

    impl Websocket for WebSocketApp {
        fn subscribe(&mut self, symbols: &Vec<String>) {
            info!("Subscribing to {:?}", symbols);
            let values = json!({
                "t":"t",
                "k": symbols.join("#"),
            });
            // get the websocket
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
            // ws.send(tungstenite::Message::Text(values.to_string()))
            //     .unwrap();
        }

        fn _event_handler_feed_update(&self, tick_data: serde_json::Value) {
            info!("Tick Data: {}", pretty_print_json(&tick_data, 2));
        }

        fn _event_handler_order_update(&self, order_data: serde_json::Value) {
            info!("Order Data: {}", pretty_print_json(&order_data, 2));
        }

        fn open_callback(&self) {
            todo!()
        }

        fn socket_error_callback(&self) {
            todo!()
        }

        fn socket_close_callback(&self) {
            todo!()
        }

        fn start_websocket<F, G, H>(
            &mut self,
            url: &str,
            user_callback: F,
            open_callback: G,
            error_callback: H,
        ) where
            F: Fn(serde_json::Value) + Send + 'static,
            G: Fn() + Send + 'static,
            H: Fn(String) + Send + 'static,
        {
            let (ws_original, _) = connect(Url::parse(url).unwrap()).unwrap();
            self.websocket = Some(Arc::new(Mutex::new(ws_original)));
            self.websocket_connected.store(true, Ordering::SeqCst);

            open_callback();
            let connected_clone = self.websocket_connected.clone();
            let ws_clone = self.websocket.clone().unwrap();
            let ws_thread = std::thread::spawn(move || {
                let mut ws = ws_clone.lock().unwrap();
                loop {
                    let result = ws.read();
                    match result {
                        Ok(msg) => match msg {
                            tungstenite::Message::Text(text) => {
                                let data: serde_json::Value = serde_json::from_str(&text).unwrap();
                                user_callback(data);
                            }
                            tungstenite::Message::Close(_) => {
                                info!("Closing Websocket");
                                connected_clone.store(false, Ordering::SeqCst);
                                break;
                            }
                            // Handle errors
                            _ => {
                                // get the error message
                                error_callback("Got an error".to_owned());
                                //close_callback();
                                break;
                            }
                        },
                        Err(e) => {
                            // get the error message
                            let error = e.to_string();
                            error_callback(error);
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
