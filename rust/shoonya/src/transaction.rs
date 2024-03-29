pub mod transaction {
    use async_trait::async_trait;
    use log::{debug, error};
    use redis_async::{
        client::{self, PairedConnection},
        resp::RespValue,
        resp_array,
    };
    use serde_json;
    use std::{cell::RefCell, collections::HashMap};

    pub struct TransactionManager {
        pub redis_conn: RefCell<PairedConnection>,
        pub instance: String,
    }

    #[async_trait]
    pub trait Transaction {
        async fn on_order(&mut self, data: &serde_json::Value);
        async fn on_placed(&mut self, data: &serde_json::Value);
        async fn on_tick(&mut self, data: &serde_json::Value);
        async fn get_pnl(&mut self) -> (f64, String);
    }

    impl TransactionManager {
        pub async fn new() -> Result<TransactionManager, Box<dyn std::error::Error>> {
            const REDIS_URL: &str = "127.0.0.1";
            let redis_client = client::paired_connect(REDIS_URL, 6379)
                .await
                .expect("Cannot connect to Redis");
            let instance = std::process::id().to_string();
            let utc_timestamp = chrono::Utc::now().timestamp_millis();
            Ok(TransactionManager {
                redis_conn: RefCell::new(redis_client),
                instance: format!("shoonya_{}_{}", instance, utc_timestamp),
            })
        }

        fn get_cache_key(&self, args: &[&str]) -> String {
            if args.is_empty() {
                return String::new();
            }

            let args = args.join("_");
            let cache_key = format!("{}_{}", self.instance, args);
            let cache_key = cache_key.replace(" ", "_");

            cache_key
        }

        fn validate_self(&self, remark: String) -> bool {
            if remark.starts_with(&self.instance) {
                return true;
            }
            false
        }

        // given a cache_key, get the value from redis
        async fn get_value(&mut self, cache_key: &str) -> String {
            let redis_conn = self.redis_conn.borrow_mut().clone();
            let value: Result<String, _> = redis_conn.send(resp_array!["GET", cache_key]).await;
            match value {
                Ok(value) => value,
                Err(e) => {
                    error!("Failed to get value: {}", e);
                    "NA".to_string()
                }
            }
        }

        // set the value in redis
        async fn set_value(&mut self, value: RespValue) -> bool {
            let redis_conn = self.redis_conn.borrow_mut().clone();
            let response: Result<String, _> = redis_conn.send(value).await;
            match response {
                Ok(_) => true,
                Err(e) => {
                    error!("Failed to set value: {}", e);
                    false
                }
            }
        }
    }

    #[async_trait]
    impl Transaction for TransactionManager {
        async fn on_order(&mut self, data: &serde_json::Value) {
            let mut avgprice = -1.0;
            let mut qty = -1;
            // if "fillshares" in data and "flprc" present
            if data["fillshares"].is_string() && data["flprc"].is_string() {
                let fillshares = data["fillshares"].as_str().unwrap().parse::<i64>().unwrap();
                let flprc = data["flprc"].as_str().unwrap().parse::<f64>().unwrap();
                avgprice = flprc;
                qty = fillshares;
            }

            let norenordno = data["norenordno"].as_str().unwrap();
            let utc_timestamp = chrono::Utc::now().to_string();
            let remarks = data["remarks"].as_str().unwrap();
            let buysell: &str = data["trantype"].as_str().unwrap();
            let tradingsymbol = data["tsym"].as_str().unwrap();
            let status = data["status"].as_str().unwrap();

            // Make an entry in the redis db for the order
            // use redis json to store the order
            let cache_key = self.get_cache_key(&[norenordno, "order_tbl"]);

            let data = resp_array![
                "HSET",
                cache_key,
                "norenordno",
                norenordno,
                "utc_timestamp",
                &utc_timestamp,
                "remarks",
                remarks,
                "avgprice",
                &avgprice.to_string(),
                "qty",
                &qty.to_string(),
                "buysell",
                buysell,
                "tradingsymbol",
                tradingsymbol,
                "status",
                status,
                "instance",
                &self.instance,
                "symbolcode",
                "",
                "ltp",
                ""
            ];
            let _response: bool = self.set_value(data).await;

            let cache_key = self.get_cache_key(&[tradingsymbol, "tradingsymbol_order_tbl"]);
            let _reponse: bool = self
                .set_value(resp_array!["SET", cache_key, norenordno])
                .await;
        }

        async fn on_placed(&mut self, data: &serde_json::Value) {
            let remarks = data["remarks"].as_str().unwrap();
            if !self.validate_self(remarks.to_string()) {
                return;
            }
            let symbolcode = data["symbolcode"].as_str().unwrap();
            let tradingsymbol = data["tradingsymbol"].as_str().unwrap();

            // get the norenordno from the tradingsymbol_order_tbl
            let cache_key = self.get_cache_key(&[tradingsymbol, "tradingsymbol_order_tbl"]);

            let norenordno: String = self.get_value(cache_key.as_str()).await;
            debug!("norenordno: {:?}", norenordno);
            // use the norenordno to get the order from order_tbl and update the symbolcode
            let cache_key = self.get_cache_key(&[norenordno.as_str(), "order_tbl"]);

            // update hset in redis with symbolcode
            let data = resp_array!["HSET", cache_key, "symbolcode", symbolcode];
            let _success: bool = self.set_value(data).await;

            // store a mapping of symbolcode to tradingsymbol
            let cache_key = self.get_cache_key(&[symbolcode, "symb_tbl"]);
            let _response: bool = self
                .set_value(resp_array!["SET", cache_key, tradingsymbol])
                .await;
        }

        async fn on_tick(&mut self, tick_data: &serde_json::Value) {
            // if "lp" in tick_data:
            debug!("tick_data: {:?}", tick_data);

            if tick_data["lp"].is_string() {
                // convert lp to f64
                let lp = tick_data["lp"].as_str().unwrap().parse::<f64>().unwrap();
                debug!("lp: {:?}", lp);

                let symbolcode = tick_data["tk"].as_str().expect("Error parsing tk");
                // get the tradingsymbol from symb_tbl
                let cache_key = self.get_cache_key(&[symbolcode, "symb_tbl"]);
                debug!("cache_key: {:?}", cache_key);
                let tradingsymbol: String = self.get_value(cache_key.as_str()).await;
                // get the norenordno from the tradingsymbol_order_tbl
                let cache_key =
                    self.get_cache_key(&[tradingsymbol.as_str(), "tradingsymbol_order_tbl"]);
                debug!("cache_key: {:?}", cache_key);
                let norenordno: String = self.get_value(cache_key.as_str()).await;
                debug!("norenordno: {:?}", norenordno);
                // use the norenordno to get the order from order_tbl and update the ltp
                let cache_key = self.get_cache_key(&[norenordno.as_str(), "order_tbl"]);
                let data = resp_array!["HSET", cache_key, "ltp", &lp.to_string()];
                let _response: bool = self.set_value(data).await;
            } else {
                debug!("No LTP in tick_data");
            }
        }

        async fn get_pnl(&mut self) -> (f64, String) {
            let mut pnl = 0.0;
            let mut pnl_vec: Vec<String> = Vec::new();

            // calculate the pnl when avgprice, qty and ltp are not -1 and status is "COMPLETE"
            // iterate over all the orders in order_tbl
            // if status is "COMPLETE" and avgprice, qty and ltp are not -1
            // calculate the pnl and add it to the total pnl
            // return the total pnl and a string representation of the pnl
            let cache_key = self.get_cache_key(&["*", "order_tbl"]);
            let redis_conn = self.redis_conn.borrow_mut().clone();

            let keys: Vec<String> = redis_conn
                .send(resp_array!["KEYS", cache_key])
                .await
                .unwrap();
            debug!("keys: {:?}", keys);
            for key in keys {
                let order: HashMap<String, String> =
                    redis_conn.send(resp_array!["HGETALL", key]).await.unwrap();
                let avgprice: f64 = order.get("avgprice").unwrap().parse().unwrap();
                let qty: i64 = order.get("qty").unwrap().parse().unwrap();
                let ltp: f64 = order.get("ltp").unwrap().parse().unwrap();
                let status: &str = order.get("status").unwrap();
                let tradingsymbol: &str = order.get("tradingsymbol").unwrap();
                let buysell: &str = order.get("buysell").unwrap();
                if status == "COMPLETE" && avgprice != -1.0 && qty != -1 && ltp != -1.0 {
                    pnl += (ltp - avgprice) * qty as f64;
                    // pnl string as buysell tradingsymbol x qty : pnl
                    let pnl_str = format!("{} {} x {} : {}", buysell, tradingsymbol, qty, pnl);
                    pnl_vec.push(pnl_str);
                }
            }
            let pnl_str = pnl_vec.join("");
            (pnl, pnl_str)
        }
    }
}
