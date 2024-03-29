pub mod utils {

    pub enum Exchange {
        NSE = 0,
        NFO = 1,
        CDS = 2,
        MCX = 3,
        BSE = 4,
        BFO = 5,
    }

    pub fn get_exchange_str(exchange: &Exchange) -> &str {
        match exchange {
            Exchange::NSE => "NSE",
            Exchange::NFO => "NFO",
            Exchange::CDS => "CDS",
            Exchange::MCX => "MCX",
            Exchange::BSE => "BSE",
            Exchange::BFO => "BFO",
        }
    }

    pub fn get_index(trading_symbol: &str) -> String {
        let mut result = String::new();
        for (i, c) in trading_symbol.chars().enumerate() {
            if c.is_digit(10) {
                result = trading_symbol[..i].to_string();
                break;
            }
        }
        result
    }

    pub fn load_config(file_name: &str) -> serde_json::Value {
        // file is is common/config.json
        let contents = std::fs::read_to_string(file_name).unwrap();
        let config: serde_json::Value = serde_json::from_str(&contents).unwrap();
        config
    }

    //Read a txt file as a csv file
    // Header is the first line of the file
    // Exchange,Token,LotSize,Symbol,TradingSymbol,Expiry,Instrument,OptionType,StrikePrice,TickSize
    pub fn read_txt_file_as_csv(
        file_name: &str,
        config_file_name: &str,
        index: &str,
    ) -> (Vec<serde_json::Value>, String) {
        let config = load_config(config_file_name);

        let symbol_name = config["SCRIP_SYMBOL_NAME"][index].as_str().unwrap();

        let mut result: Vec<serde_json::Value> = Vec::new();
        let contents = std::fs::read_to_string(file_name).unwrap();
        let mut lines = contents.lines();
        let header = lines.next().unwrap();
        let header_fields: Vec<&str> = header.split(",").collect();
        for line in lines {
            let fields: Vec<&str> = line.split(",").collect();
            let mut obj = serde_json::json!({});
            for (i, field) in fields.iter().enumerate() {
                obj[header_fields[i]] = serde_json::Value::String(field.to_string());
            }
            result.push(obj);
        }
        let expiry_date = get_expiry_date(&result, &symbol_name);
        (result, expiry_date)
    }

    pub fn get_expiry_date(data: &Vec<serde_json::Value>, symbol: &str) -> String {
        // find the closest expiry date to today
        let mut min_diff = 100000;
        let mut expiry_date = String::new();
        for row in data.iter() {
            let sym = row["Symbol"].as_str().unwrap();
            if sym != symbol {
                continue;
            }
            let expiry = row["Expiry"].as_str().unwrap();
            let diff = chrono::NaiveDate::parse_from_str(expiry, "%d-%b-%Y")
                .unwrap()
                .signed_duration_since(chrono::Local::now().naive_local().date())
                .num_days();
            if diff < min_diff && diff >= 0 {
                min_diff = diff;
                expiry_date = expiry.to_string();
            }
        }
        expiry_date
    }

    pub fn get_strike_info(
        data: &Vec<serde_json::Value>,
        index: &str,
        expiry: &str,
        strike_price: f64,
        opt: &str,
    ) -> (String, String) {
        let mut trading_symbol = String::new();
        let mut token = String::new();
        for row in data.iter() {
            let expiry_date = row["Expiry"].as_str().unwrap();
            let sym = row["StrikePrice"].as_str().unwrap();
            let sym = sym.parse::<f64>().unwrap();
            let option_type = row["OptionType"].as_str().unwrap();
            let idx_symb = row["Symbol"].as_str().unwrap();

            if idx_symb == index
                && expiry_date == expiry
                && sym == strike_price
                && option_type == opt
            {
                token = row["Token"].as_str().unwrap().to_string();
                trading_symbol = row["TradingSymbol"].as_str().unwrap().to_string();
                break;
            }
        }
        (token, trading_symbol)
    }

    pub fn pretty_print_json(json: &serde_json::Value, indent: usize) -> String {
        let mut result = String::new();
        match json {
            serde_json::Value::Null => result.push_str("null"),
            serde_json::Value::Bool(b) => result.push_str(&b.to_string()),
            serde_json::Value::Number(n) => result.push_str(&n.to_string()),
            serde_json::Value::String(s) => result.push_str(&s.to_string()),
            serde_json::Value::Array(a) => {
                result.push_str("[\n");
                for item in a.iter() {
                    result.push_str(&pretty_print_json(item, indent + 1));
                    result.push_str(",\n");
                }
                result.push_str("]");
            }
            serde_json::Value::Object(o) => {
                result.push_str("{\n");
                for (key, value) in o.iter() {
                    result.push_str(&format!("{:indent$}{}: ", "", key, indent = indent + 1));
                    result.push_str(&pretty_print_json(value, indent + 1));
                    result.push_str(",\n");
                }
                result.push_str("}");
            }
        }
        result
    }

    pub async fn post_to_client(url: String, payload: String) -> serde_json::Value {
        let res = reqwest::Client::new()
            .post(&url)
            .body(payload)
            .send()
            .await
            .unwrap()
            .text()
            .await;

        match res {
            Ok(res) => serde_json::from_str(&res).unwrap(),
            Err(e) => serde_json::from_str(&e.to_string()).unwrap(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::utils::utils::*;

    #[test]
    fn test_get_exchange_str() {
        assert_eq!(get_exchange_str(&Exchange::NSE), "NSE");
        assert_eq!(get_exchange_str(&Exchange::NFO), "NFO");
        assert_eq!(get_exchange_str(&Exchange::CDS), "CDS");
        assert_eq!(get_exchange_str(&Exchange::MCX), "MCX");
        assert_eq!(get_exchange_str(&Exchange::BSE), "BSE");
        assert_eq!(get_exchange_str(&Exchange::BFO), "BFO");
    }

    #[test]
    fn test_load_config() {
        let config = load_config("./config.json");
        assert_eq!(config["EXCHANGE"]["NIFTY"], "NFO");
        assert_eq!(config["INDICES_ROUNDING"]["BANKEX"], 100);
        assert_eq!(config["LOT_SIZE"]["FINNIFTY"], 40);
    }

    #[test]
    fn test_read_txt_file_as_csv() {
        let (result, exipry_date) = read_txt_file_as_csv(
            "../downloads/NFO_symbols_2023-12-31.txt",
            "./config.json",
            "NIFTY",
        );
        assert_eq!(result[0]["Exchange"], "NFO");
        assert_eq!(result[0]["TickSize"], "0.05");
        assert_eq!(exipry_date, "04-JAN-2024");
    }

    #[test]
    fn test_get_expiry_date() {
        let (result, expiry_date) = read_txt_file_as_csv(
            "../downloads/NFO_symbols_2023-12-31.txt",
            "./config.json",
            "NIFTY",
        );
        assert_eq!(result[0]["Exchange"], "NFO");
        assert_eq!(expiry_date, "04-JAN-2024");
    }

    #[test]
    fn test_get_strike_info() {
        let (result, expiry_date) = read_txt_file_as_csv(
            "../downloads/NFO_symbols_2023-12-31.txt",
            "./config.json",
            "NIFTY",
        );
        assert_eq!(result[0]["Exchange"], "NFO");
        let (token, trading_symbol) =
            get_strike_info(&result, "NIFTY", &expiry_date, 21800.0, "CE");
        assert_eq!(token, "42216");
        //
        assert_eq!(trading_symbol, "NIFTY04JAN24C21800");
    }
}
