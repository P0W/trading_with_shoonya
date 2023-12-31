use common::utils::utils::{Exchange, get_exchange_str};
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
                error!("Error Occured: for {} : {}", get_exchange_str(exchange),  e);
            }
        }
    }
    result
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

    download_scrip(&Exchange::BFO);
    let quote = get_quote(&auth, &Exchange::NSE, "26000");

    log::info!("Quote: {}", quote);
}
