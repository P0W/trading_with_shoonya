use common::utils::utils::*;
use scrip_master::scrips::download_scrip;
use shoonya::auth::auth::Auth;
use shoonya::markets::markets::{get_indices, get_quote};
use shoonya::orders::orders::get_order_book;

use log::*;

mod logger;

fn build_indices_map(auth: &Auth) -> std::collections::HashMap<String, String> {
    let mut result = std::collections::HashMap::new();
    let exchanges = [
        Exchange::NSE,
        Exchange::BFO,
        Exchange::CDS,
        Exchange::NFO,
        Exchange::MCX,
    ];
    for exchange in exchanges.iter() {
        let indices = get_indices(&auth, exchange);
        match indices {
            Ok(indices) => {
                let values = indices["values"].as_array().unwrap();
                // values has {"idxname": "Nifty 50", "token": "2600"}
                // create a hashmap of idxname -> token
                for index in values {
                    let idxname = index["idxname"].as_str().unwrap().to_string();
                    let token = index["token"].as_str().unwrap().to_string();
                    result.insert(idxname, token);
                }
            }
            Err(e) => {
                error!("Error Occured: for {} : {}", get_exchange_str(exchange), e);
            }
        }
    }
    result
}

fn get_straddle_strikes(auth: &Auth, index: &str) {
    // get the config file
    let config_file = String::from("./common/config.json");
    let config = load_config(&config_file);
    let index_token: &str = config["INDICES_TOKEN"][index].as_str().unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut file_name = String::new();
    match index {
        "NIFTY" | "BANKNIFTY" | "FINNIFTY" | "MIDCPNIFTY" => {
            download_scrip(&Exchange::NFO);
            file_name = format!("./downloads/NFO_symbols_{}.txt", today);
        }
        "SENSEX" | "BANKEX" => {
            download_scrip(&Exchange::BFO);
            file_name = format!("./downloads/BFO_symbols_{}.txt", today);
        }
        "CRUDEOIL" | "GOLD" | "SILVER" => {
            download_scrip(&Exchange::MCX);
            file_name = format!("./downloads/MCX_symbols_{}.txt", today);
        }
        _ => {
            info!("Error: {}", "Unknown index");
        }
    }

    let (scrip_data, expiry_date) = read_txt_file_as_csv(&file_name, &config_file, &index);

    let index_quote = get_quote(&auth, &Exchange::NSE, index_token);
    match index_quote {
        Ok(result) if result["stat"] == "Ok" => {
            let ltp = result["lp"].as_str().unwrap();
            // convert ltp to f64
            let ltp = ltp.parse::<f64>().unwrap();
            // round to nerest config["INDICES_ROUNDING"][index]
            let rounding = config["INDICES_ROUNDING"][index].as_f64().unwrap();
            let rounded_ltp = (ltp / rounding).round() * rounding;

            info!("Index LTP: {}", ltp);

            let (token_ce, trading_symbol_ce) =
                get_strike_info(&scrip_data, &expiry_date, rounded_ltp, "CE");
            let (token_pe, trading_symbol_pe) =
                get_strike_info(&scrip_data, &expiry_date, rounded_ltp, "PE");

            info!("CE: {} {} {}", token_ce, trading_symbol_ce, rounded_ltp);
            info!("PE: {} {} {}", token_pe, trading_symbol_pe, rounded_ltp);
        }
        Err(e) => {
            info!("Error: {}", e);
        }
        _ => {
            info!("Error: {}", "Unknown error");
        }
    }
}

fn main() {
    logger::init_logger("shoonya_rust", log::LevelFilter::Debug);

    let mut auth = Auth::new();

    auth.login("../cred.yml");

    let order_book = get_order_book(&auth);

    match order_book {
        Ok(order_book) => {
            info!("Order book: {}", order_book);
        }
        Err(e) => {
            info!("Error: {}", e);
        }
    }

    let indices_map = build_indices_map(&auth);

    // log the indices_map
    for (idxname, token) in indices_map.iter() {
        info!("{}: {}", idxname, token);
    }

    download_scrip(&Exchange::NFO);

    get_straddle_strikes(&auth, "BANKNIFTY");
}
