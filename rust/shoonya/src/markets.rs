pub mod markets {

    use crate::urls::urls::{GETQUOTES, GET_INDICES_LIST, HOST};
    use serde_json::json;
    use common::utils::utils::{Exchange, get_exchange_str};

    fn _get_payload(susertoken: &str, values: &serde_json::Value) -> String {
        let payload = format!("jData={}&jKey={}", values.to_string(), susertoken);

        payload
    }

    pub fn get_indices(
        auth: &crate::auth::auth::Auth,
        exchange: &Exchange,
    ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let values = json!({
            "ordersource": "API",
            "exch": get_exchange_str(exchange),
            "uid": auth.username,
        });

        let url = format!("{}{}", HOST, GET_INDICES_LIST);
        let payload = _get_payload(&auth.susertoken, &values);

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

    pub fn get_quote(auth: &crate::auth::auth::Auth, exchange: &Exchange, token: &str) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
        let values = json!({
            "ordersource": "API",
            "exch": get_exchange_str(exchange),
            "uid": auth.username,
            "token": token,
        });

        let url = format!("{}{}", HOST, GETQUOTES);
        let payload = _get_payload(&auth.susertoken, &values);

        let client = reqwest::blocking::Client::new();
        let res: String = client
            .post(&url)
            .body(payload)
            .send()
            .unwrap()
            .text()
            .unwrap();

        let res_dict: serde_json::Value = serde_json::from_str(&res)?;
        Ok(res_dict)
    }
}