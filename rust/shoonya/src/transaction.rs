mod transaction {

    use std::cell::RefCell;
    use redis::{self, Commands};
    use serde_json;

    struct TransactionManager {
        pub redis_conn: RefCell<redis::Connection>,
        pub instance: String,
    }

    pub trait Transaction {
        fn on_order(&mut self, data: &serde_json::Value);
        fn on_placed(&mut self, data: &serde_json::Value);
        fn on_tick(&mut self, data: &serde_json::Value);
        fn get_pnl(&mut self) -> (f64, String);
        fn validate_self(&self, remark: String) -> bool;
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
            let json_content = serde_json::json!({
                "norenordno": norenordno,
                "utc_timestamp": utc_timestamp,
                "remarks": remarks,
                "avgprice": avgprice,
                "qty": qty,
                "buysell": buysell,
                "tradingsymbol": tradingsymbol,
                "status": status,
                "instance": self.instance,
            });
            // use redis json to store the order
            let cache_key = format!("order_tbl_{}_{}", self.instance, norenordno);
            self.redis_conn
                .borrow_mut()
                .set::<_, _, ()>(cache_key, json_content.to_string())
                .unwrap();
        }

        fn validate_self(&self, remark: String) -> bool {
            println!("Validating self");
            // if remark begins with self.instance
            if remark.starts_with(&self.instance) {
                return true;
            }
            false
        }

        fn on_placed(&mut self, data: &serde_json::Value) {
            let remarks = data["remarks"].as_str().unwrap();
            if !self.validate_self(remarks.to_string()) {
                return;
            }
            let symbolcode = data["symbolcode"].as_str().unwrap();
            let tradingsymbol = data["tradingsymbol"].as_str().unwrap();
            let exchange = data["exchange"].as_str().unwrap();
            let values = serde_json::json!({
                "symbolcode": symbolcode,
                "exchange": exchange,
                "tradingsymbol": tradingsymbol,
                "instance": self.instance,
            });
            let cache_key = format!("symb_tbl_{}_{}", self.instance, symbolcode);
            self.redis_conn
                .borrow_mut()
                .set::<_, _, ()>(cache_key, values.to_string())
                .unwrap();
        }

        fn on_tick(&mut self, data: &serde_json::Value) {
            // if "lp" in tick_data:
            if data["lp"].is_f64() {
                let lp = data["lp"].as_f64().unwrap();
                let symbolcode = data["tk"].as_str().unwrap();
                let cache_key = format!("live_tbl_{}", symbolcode);
                self.redis_conn
                    .borrow_mut()
                    .set::<_, _, ()>(cache_key, lp)
                    .unwrap();
            }
        }

        fn get_pnl(&mut self) -> (f64, String) {
            let mut pnl = 0.0;
            let pnl_str = String::new();
            let keys_order_tbl: Vec<String> =
                self.redis_conn.borrow_mut().keys("order_tbl_*").unwrap();
            let keys_symbol_tbl: Vec<String> =
                self.redis_conn.borrow_mut().keys("symb_tbl_*").unwrap();
            let keys_live_tbl: Vec<String> =
                self.redis_conn.borrow_mut().keys("live_tbl_*").unwrap();

            for key in keys_order_tbl {
                let order: String = self.redis_conn.borrow_mut().get(key).unwrap();
                let order_json: serde_json::Value = serde_json::from_str(&order).unwrap();
                let tradingsymbol = order_json["tradingsymbol"].as_str().unwrap();
                let buysell = order_json["buysell"].as_str().unwrap();
                let qty = order_json["qty"].as_i64().unwrap();
                let avgprice = order_json["avgprice"].as_f64().unwrap();
                let mut ltp = 0.0;
                for key in keys_symbol_tbl.clone() {
                    let symbol: String = self.redis_conn.borrow_mut().get(key).unwrap();
                    let symbol_json: serde_json::Value = serde_json::from_str(&symbol).unwrap();
                    if symbol_json["tradingsymbol"].as_str().unwrap() == tradingsymbol {
                        let symbolcode = symbol_json["symbolcode"].as_str().unwrap();
                        for key in keys_live_tbl.clone() {
                            let live: f64 = self.redis_conn.borrow_mut().get(key.clone()).unwrap();
                            if key.contains(symbolcode) {
                                ltp = live;
                                break;
                            }
                        }
                        break;
                    }
                }
                if buysell == "B" {
                    pnl += (ltp - avgprice) * qty as f64;
                } else if buysell == "S" {
                    pnl += (avgprice - ltp) * qty as f64;
                }
            }
            (pnl, pnl_str)
        }
    }
}
