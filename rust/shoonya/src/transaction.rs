pub mod transaction {

    use redis::Commands;
    use serde_json;
    use std::{cell::RefCell, collections::HashMap};

    pub struct TransactionManager {
        pub redis_conn: RefCell<redis::Connection>,
        pub instance: String,
    }

    pub trait Transaction {
        fn on_order(&mut self, data: &serde_json::Value);
        fn on_placed(&mut self, data: &serde_json::Value);
        fn on_tick(&mut self, data: &serde_json::Value);
        fn get_pnl(&mut self) -> (f64, String);
    }

    impl TransactionManager {
        pub fn new() -> TransactionManager {
            const REDIS_URL: &str = "redis://127.0.0.1/";
            let redis_client = redis::Client::open(REDIS_URL).unwrap();
            let con = redis_client.get_connection().unwrap();
            let instance = std::process::id().to_string();
            let utc_timestamp = chrono::Utc::now().to_string();

            TransactionManager {
                redis_conn: RefCell::new(con),
                instance: format!("shoonya_{}_{}", instance, utc_timestamp),
            }
        }

        // craete a get_cache_key that accept multiple one or more String args , suffix them with _ and prefix with instance
        fn get_cache_key(&self, args: &[&str]) -> String {
            let args = args.join("_");
            let mut cache_key = format!("{}_{}", self.instance, args);
            cache_key = cache_key.replace(" ", "_");
            cache_key
        }

        fn validate_self(&self, remark: String) -> bool {
            println!("Validating self");
            // if remark begins with self.instance
            if remark.starts_with(&self.instance) {
                return true;
            }
            false
        }
    }

    impl Transaction for TransactionManager {
        fn on_order(&mut self, data: &serde_json::Value) {
            let mut avgprice = -1.0;
            let mut qty = -1;
            // if "fillshares" in data and "flprc" present
            if data["fillshares"].is_i64() && data["flprc"].is_f64() {
                let fillshares = data["fillshares"].as_i64().unwrap();
                let flprc = data["flprc"].as_f64().unwrap();
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

            self.redis_conn
                .borrow_mut()
                .hset_multiple::<_, _, _, ()>(
                    &cache_key,
                    &[
                        ("norenordno", norenordno),
                        ("utc_timestamp", &utc_timestamp),
                        ("remarks", remarks),
                        ("avgprice", &avgprice.to_string()),
                        ("qty", &qty.to_string()),
                        ("buysell", buysell),
                        ("tradingsymbol", tradingsymbol),
                        ("status", status),
                        ("instance", &self.instance),
                        ("symbolcode", ""),
                        ("ltp", ""),
                    ],
                )
                .unwrap();

            let cache_key = self.get_cache_key(&[tradingsymbol, "tradingsymbol_order_tbl"]);
            self.redis_conn
                .borrow_mut()
                .set::<_, _, ()>(&cache_key, &norenordno)
                .unwrap();
        }

        fn on_placed(&mut self, data: &serde_json::Value) {
            let remarks = data["remarks"].as_str().unwrap();
            if !self.validate_self(remarks.to_string()) {
                return;
            }
            let symbolcode = data["symbolcode"].as_str().unwrap();
            let tradingsymbol = data["tradingsymbol"].as_str().unwrap();

            // get the norenordno from the tradingsymbol_order_tbl
            let cache_key = self.get_cache_key(&[tradingsymbol, "tradingsymbol_order_tbl"]);
            let norenordno: String = self.redis_conn.borrow_mut().get(cache_key).unwrap();
            // use the norenordno to get the order from order_tbl and update the symbolcode
            let cache_key = self.get_cache_key(&[norenordno.as_str(), "order_tbl"]);
            // update hset in redis with symbolcode
            self.redis_conn
                .borrow_mut()
                .hset::<_, _, _, ()>(cache_key, "symbolcode", symbolcode)
                .unwrap();

            // store a mapping of symbolcode to tradingsymbol
            let cache_key = self.get_cache_key(&[symbolcode, "symb_tbl"]);
            self.redis_conn
                .borrow_mut()
                .set::<_, _, ()>(&cache_key, tradingsymbol)
                .unwrap();
        }

        fn on_tick(&mut self, tick_data: &serde_json::Value) {
            // if "lp" in tick_data:
            if tick_data["lp"].is_f64() {
                let lp = tick_data["lp"].as_f64().unwrap();
                let symbolcode = tick_data["tk"].as_str().unwrap();
                // get the tradingsymbol from symb_tbl
                let cache_key = self.get_cache_key(&[symbolcode, "symb_tbl"]);
                let tradingsymbol: String = self.redis_conn.borrow_mut().get(cache_key).unwrap();
                // get the norenordno from the tradingsymbol_order_tbl
                let cache_key =
                    self.get_cache_key(&[tradingsymbol.as_str(), "tradingsymbol_order_tbl"]);
                let norenordno: String = self.redis_conn.borrow_mut().get(cache_key).unwrap();
                // use the norenordno to get the order from order_tbl and update the ltp
                let cache_key = self.get_cache_key(&[norenordno.as_str(), "order_tbl"]);
                self.redis_conn
                    .borrow_mut()
                    .hset::<_, _, _, ()>(cache_key, "ltp", lp)
                    .unwrap();
            }
        }

        fn get_pnl(&mut self) -> (f64, String) {
            let mut pnl = 0.0;
            let mut pnl_vec: Vec<String> = Vec::new();

            // calculate the pnl when avgprice, qty and ltp are not -1 and status is "COMPLETE"
            // iterate over all the orders in order_tbl
            // if status is "COMPLETE" and avgprice, qty and ltp are not -1
            // calculate the pnl and add it to the total pnl
            // return the total pnl and a string representation of the pnl
            let cache_key = self.get_cache_key(&["*", "order_tbl"]);
            let keys: Vec<String> = self.redis_conn.borrow_mut().keys(cache_key).unwrap();
            for key in keys {
                let order: HashMap<String, String> =
                    self.redis_conn.borrow_mut().hgetall(key).unwrap();
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
