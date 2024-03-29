pub mod auth {

    use crate::urls::urls::{AUTHORIZE, HOST};
    use log::*;
    use redis::Commands;
    use sha2::{Digest, Sha256};
    use totp_rs::{Rfc6238, Secret, TOTP};

    #[derive(Debug, Default)]
    pub struct Auth {
        pub username: String,
        pub accountid: String,
        pub password: String,
        pub susertoken: String,
    }

    impl Auth {
        pub async fn login(&mut self, file_name: &str, force_login: bool) {
            const REDIS_URL: &str = "redis://127.0.0.1/";
            const TOKEN: &str = "access_token_shoonya";

            let redis_client = redis::Client::open(REDIS_URL).unwrap();
            let mut con = redis_client.get_connection().unwrap();

            let super_token: Result<String, redis::RedisError> = con.get(TOKEN);
            let file = std::fs::File::open(file_name).unwrap();
            let creds: serde_json::Value = serde_yaml::from_reader(file).unwrap();
            match super_token {
                Ok(token) if force_login == false => {
                    debug!("Token found in cache");
                    let userid = creds["user"].as_str().unwrap();
                    let password = creds["pwd"].as_str().unwrap();
                    self.set_session(userid, password, token.as_str());
                }
                _ => {
                    debug!("Token not found in cache");
                    // login and get the token
                    let creds = self.get_creds(creds).await.unwrap();
                    let token = creds["susertoken"].as_str().unwrap().to_string();
                    // set the token in redis with expiry of 2 hours
                    let _: () = con.set_ex(TOKEN, token, 7200).unwrap();
                }
            }
        }

        pub fn new() -> Auth {
            Auth {
                username: "".to_string(),
                accountid: "".to_string(),
                password: "".to_string(),
                susertoken: "".to_string(),
            }
        }

        // read from a yml file provided by the user
        async fn get_creds(
            &mut self,
            creds: serde_json::Value,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            // convert to string creds["totp_pin"]
            let totp_pin = creds["totp_pin"].as_str().unwrap();

            let rfc =
                Rfc6238::with_defaults(Secret::Encoded(totp_pin.to_string()).to_bytes().unwrap())
                    .unwrap();

            // create a TOTP from rfc
            let totp = TOTP::from_rfc6238(rfc).unwrap();
            let two_fa = totp.generate_current().unwrap();

            let result = self
                ._login(
                    creds["user"].as_str().unwrap(),
                    creds["pwd"].as_str().unwrap(),
                    &two_fa,
                    creds["vc"].as_str().unwrap(),
                    creds["apikey"].as_str().unwrap(),
                    creds["imei"].as_str().unwrap(),
                )
                .await;

            result
        }

        async fn _login(
            &mut self,
            userid: &str,
            password: &str,
            two_fa: &str,
            vendor_code: &str,
            api_secret: &str,
            imei: &str,
        ) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
            let url = format!("{}{}", HOST, AUTHORIZE);

            let mut hasher = Sha256::new();
            hasher.update(password);
            let pwd = format!("{:x}", hasher.finalize());

            hasher = Sha256::new();
            hasher.update(format!("{}|{}", userid, api_secret));
            let app_key = format!("{:x}", hasher.finalize());

            let values = serde_json::json!({
                "source": "API",
                "apkversion": "1.0.0",
                "uid": userid,
                "pwd": pwd,
                "factor2": two_fa,
                "vc": vendor_code,
                "appkey": app_key,
                "imei": imei,
            });

            let client = reqwest::Client::new();
            // let res: String = client
            //     .post(&url)
            //     .body(format!("jData={}", values.to_string()))
            //     .send()?
            //     .await?.
            //     text()?;
            // await the response
            let res = client
                .post(&url)
                .body(format!("jData={}", values.to_string()))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();

            let res_dict: serde_json::Value = serde_json::from_str(&res)?;

            if res_dict["stat"] != "Ok" {
                return Err(res_dict.to_string().into());
            }

            self.username = userid.to_string();
            self.accountid = userid.to_string();
            self.password = password.to_string();
            self.susertoken = res_dict["susertoken"].as_str().unwrap().to_string();

            Ok(res_dict)
        }

        fn set_session(&mut self, userid: &str, password: &str, usertoken: &str) -> bool {
            self.username = userid.to_string();
            self.accountid = userid.to_string();
            self.password = password.to_string();
            self.susertoken = usertoken.to_string();

            true
        }
    }
}
