#[allow(dead_code)]

pub mod orders {

    use crate::{
        auth::auth::Auth,
        urls::urls::{CANCELORDER, HOST, ORDERBOOK, PLACEORDER},
    };
    use serde_json::json;

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

    pub fn place_order(
        auth: &Auth,
        buy_or_sell: String,
        product_type: String,
        exchange: String,
        tradingsymbol: String,
        quantity: i32,
        discloseqty: i32,
        price_type: String,
        price: f64,
        trigger_price: Option<f64>,
        retention: String,
        amo: String,
        remarks: Option<String>,
        bookloss_price: f64,
        bookprofit_price: f64,
        trail_price: f64,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let mut values = json!({
            "ordersource": "API",
            "uid": auth.username,
            "actid": auth.username,
            "trantype": buy_or_sell,
            "prd": product_type,
            "exch": exchange,
            "tsym": tradingsymbol,
            "qty": quantity,
            "dscqty": discloseqty,
            "prctyp": price_type,
            "prc": price,
            "trgprc": trigger_price,
            "ret": retention,
            "remarks": remarks,
            "amo": amo,
        });

        // #if cover order or high leverage order
        if product_type == "H" {
            // bookloss_price f64 price as string
            values["blprc"] = serde_json::Value::String(bookloss_price.to_string());
            // #trailing price
            if trail_price != 0.0 {
                values["trailprc"] = serde_json::Value::String(trail_price.to_string());
            }
        }

        // #bracket order
        if product_type == "B" {
            values["blprc"] = serde_json::Value::String(bookloss_price.to_string());
            values["bpprc"] = serde_json::Value::String(bookprofit_price.to_string());
            // #trailing price
            if trail_price != 0.0 {
                values["trailprc"] = serde_json::Value::String(trail_price.to_string());
            }
        }

        let url = format!("{}{}", HOST, PLACEORDER);
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
}
