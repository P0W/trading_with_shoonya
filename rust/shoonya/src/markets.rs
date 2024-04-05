pub mod markets {

    use crate::{
        auth::auth::Auth,
        urls::urls::{GETQUOTES, GET_INDICES_LIST, HOST, OPTIONCHAIN},
    };
    use async_trait::async_trait;
    use common::utils::utils::{get_exchange_str, post_to_client, pretty_print_json, Exchange};
    use serde_json::json;

    fn _get_payload(susertoken: &str, values: &serde_json::Value) -> String {
        let payload = format!("jData={}&jKey={}", values, susertoken);
        payload
    }

    #[async_trait]
    pub trait Markets {
        async fn get_quote(&self, _exchange: &Exchange, _token: &str) -> f64 {
            0.0
        }
        async fn get_indices(
            &self,
            _exchange: &Exchange,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            Ok(serde_json::Value::Null)
        }
        async fn get_option_chain(
            &self,
            exchange: &Exchange,
            tsym: &str,
            strike_price: f64,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error>>;
    }

    #[async_trait]
    impl Markets for Auth {
        async fn get_option_chain(
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

            let res_dict = post_to_client(url, payload).await;
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

        async fn get_indices(
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

            let res_dict = post_to_client(url, payload).await;
            if let Some(obj) = res_dict.as_object() {
                if obj.contains_key("stat") {
                    // "stat" is present in the response
                    if obj["stat"] == "Ok" {
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

        async fn get_quote(&self, exchange: &Exchange, token: &str) -> f64 {
            let values = json!({
                "ordersource": "API",
                "exch": get_exchange_str(exchange),
                "uid": self.username,
                "token": token,
            });

            let url = format!("{}{}", HOST, GETQUOTES);
            let payload = _get_payload(&self.susertoken, &values);

            let res_dict = post_to_client(url, payload).await;
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
}
