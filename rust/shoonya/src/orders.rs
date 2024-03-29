#[allow(dead_code)]

pub mod orders {

    use std::{cell::RefCell, rc::Rc};

    use crate::{
        auth::auth::Auth,
        urls::urls::{CANCELORDER, HOST, ORDERBOOK, PLACEORDER},
    };
    use serde_json::json;

    #[derive(Debug, Default)]
    pub struct OrderBuilder {
        pub auth: Rc<RefCell<Auth>>,
        pub orderno: String,
        pub tradingsymbol: String,
        pub exchange: String,
        pub quantity: u32,
        pub price: f64,
        pub trigger_price: f64,
        pub status: String,
        pub product_type: String,
        pub price_type: String,
        pub buy_or_sell: String,
        pub retention: String,
        pub amo: String,
        pub remarks: String,
        pub bookloss_price: f64,
        pub bookprofit_price: f64,
        pub trail_price: f64,
    }

    pub fn get_order_book(auth: &Auth) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let values = json!({
            "ordersource": "API",
            "uid": auth.username,
        });

        let url = format!("{}{}", HOST, ORDERBOOK);
        let payload = format!("jData={}&jKey={}", values.to_string(), auth.susertoken);
        let client = reqwest::blocking::Client::new();
        let res: String = client
            .post(&url)
            .body(payload)
            .send()
            .unwrap()
            .text()
            .unwrap();

        let res_dict: serde_json::Value = serde_json::from_str(&res)?;

        if res_dict["stat"] != "Ok" {
            return Err(res_dict.to_string().into());
        }

        Ok(res_dict)
    }

    pub fn cancel_order(
        auth: &Auth,
        orderno: String,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let values = json!({
            "ordersource": "API",
            "uid": auth.username,
            "norenordno": orderno,
        });

        let url = format!("{}{}", HOST, CANCELORDER);
        let payload = format!("jData={}&jKey={}", values.to_string(), auth.susertoken);
        let client = reqwest::blocking::Client::new();
        let res: String = client
            .post(&url)
            .body(payload)
            .send()
            .unwrap()
            .text()
            .unwrap();

        let res_dict: serde_json::Value = serde_json::from_str(&res).unwrap();

        if res_dict["stat"] != "Ok" {
            return Err(res_dict.to_string().into());
        }

        return Ok(res_dict);
    }

    impl OrderBuilder {
        pub fn new(auth: Rc<RefCell<Auth>>) -> OrderBuilder {
            OrderBuilder {
                auth,
                retention: "DAY".to_owned(),
                amo: "NO".to_owned(),
                product_type: "M".to_owned(),
                price_type: "MKT".to_owned(),
                ..Default::default()
            }
        }
        pub fn buy_or_sell(&mut self, buy_or_sell: String) -> &mut Self {
            self.buy_or_sell = buy_or_sell;
            self
        }
        pub fn tradingsymbol(&mut self, tradingsymbol: String) -> &mut Self {
            self.tradingsymbol = tradingsymbol;
            self
        }
        pub fn exchange(&mut self, exchange: String) -> &mut Self {
            self.exchange = exchange;
            self
        }
        pub fn quantity(&mut self, quantity: u32) -> &mut Self {
            self.quantity = quantity;
            self
        }
        pub fn price(&mut self, price: f64) -> &mut Self {
            self.price = price;
            self
        }
        pub fn trigger_price(&mut self, trigger_price: f64) -> &mut Self {
            self.trigger_price = trigger_price;
            self
        }
        pub fn status(&mut self, status: String) -> &mut Self {
            self.status = status;
            self
        }
        pub fn product_type(&mut self, product_type: String) -> &mut Self {
            self.product_type = product_type;
            self
        }
        pub fn price_type(&mut self, price_type: String) -> &mut Self {
            self.price_type = price_type;
            self
        }
        pub fn retention(&mut self, retention: String) -> &mut Self {
            self.retention = retention;
            self
        }
        pub fn amo(&mut self, amo: String) -> &mut Self {
            self.amo = amo;
            self
        }
        pub fn remarks(&mut self, remarks: String) -> &mut Self {
            self.remarks = remarks;
            self
        }
        pub fn bookloss_price(&mut self, bookloss_price: f64) -> &mut Self {
            self.bookloss_price = bookloss_price;
            self
        }
        pub fn bookprofit_price(&mut self, bookprofit_price: f64) -> &mut Self {
            self.bookprofit_price = bookprofit_price;
            self
        }
        pub fn trail_price(&mut self, trail_price: f64) -> &mut Self {
            self.trail_price = trail_price;
            self
        }
        pub fn place(&self) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            // validate prd, exch, tsym, qty, prc, ret, amo, remarks non empty
            if self.product_type.is_empty()
                || self.exchange.is_empty()
                || self.tradingsymbol.is_empty()
                || self.quantity == 0
                || self.retention.is_empty()
                || self.amo.is_empty()
                || self.remarks.is_empty()
                || self.price_type.is_empty()
                || self.buy_or_sell.is_empty()
                || self.product_type.is_empty()
            {
                return Err("prd, exch, tsym, qty, prc, ret, amo, remarks cannot be empty".into());
            }

            let mut values = json!({
                "ordersource": "API",
                "uid": self.auth.borrow().username,
                "actid": self.auth.borrow().username,
                "trantype": self.buy_or_sell,
                "prd": self.product_type,
                "exch": self.exchange,
                "tsym": self.tradingsymbol,
                "qty": self.quantity,
                "dscqty": 0,
                "prctyp": self.price_type,
                "prc": self.price,
                "trgprc": self.trigger_price,
                "ret": self.retention,
                "remarks": self.remarks,
                "amo": self.amo,
            });
            // #if cover order or high leverage order
            if self.product_type == "H" {
                // bookloss_price f64 price as string
                values["blprc"] = serde_json::Value::String(self.bookloss_price.to_string());
                // #trailing price
                if self.trail_price != 0.0 {
                    values["trailprc"] = serde_json::Value::String(self.trail_price.to_string());
                }
            }

            // #bracket order
            if self.product_type == "B" {
                values["blprc"] = serde_json::Value::String(self.bookloss_price.to_string());
                values["bpprc"] = serde_json::Value::String(self.bookprofit_price.to_string());
                // #trailing price
                if self.trail_price != 0.0 {
                    values["trailprc"] = serde_json::Value::String(self.trail_price.to_string());
                }
            }

            let url = format!("{}{}", HOST, PLACEORDER);
            let payload = format!(
                "jData={}&jKey={}",
                values.to_string(),
                self.auth.borrow().susertoken
            );
            let client = reqwest::blocking::Client::new();
            let res: String = client
                .post(&url)
                .body(payload)
                .send()
                .unwrap()
                .text()
                .unwrap();

            let res_dict: serde_json::Value = serde_json::from_str(&res).unwrap();

            if res_dict["stat"] != "Ok" {
                return Err(res_dict.to_string().into());
            }

            return Ok(res_dict);
        }
    }
}
