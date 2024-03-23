#[allow(dead_code)]

pub mod orders {

    use crate::urls::urls::{HOST, ORDERBOOK};
    use serde_json::json;

    pub fn get_order_book(
        auth: &crate::auth::auth::Auth,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
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
}
