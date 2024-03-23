mod logger;
mod order_manager;

use common::utils::utils::*;
use scrip_master::scrips::download_scrip;
use shoonya::auth::auth::Auth;
use shoonya::markets::markets::Markets;
use shoonya::websocket::websocket::WebSocketApp;

use clap::Parser;
use log::*;

use crate::order_manager::WebSocketCallbackHandler;

#[allow(dead_code)]
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
        let indices = auth.get_indices(exchange);
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

fn get_straddle_strikes(auth: &Auth, index: &str, closest_price: f64) -> serde_json::Value {
    // get the config file
    let config_file = String::from("./common/config.json");
    let config = load_config(&config_file);
    let index_token: &str = config["INDICES_TOKEN"][index].as_str().unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let file_name;
    let exchange: Exchange;
    let index_exchange;
    match index {
        "NIFTY" | "BANKNIFTY" | "FINNIFTY" | "MIDCPNIFTY" => {
            exchange = Exchange::NFO;
            index_exchange = Exchange::NSE;
            file_name = format!("./downloads/NFO_symbols_{}.txt", today);
        }
        "SENSEX" | "BANKEX" => {
            exchange = Exchange::BFO;
            index_exchange = Exchange::BSE;
            file_name = format!("./downloads/BFO_symbols_{}.txt", today);
        }
        "CRUDEOIL" | "GOLD" | "SILVER" => {
            exchange = Exchange::MCX;
            index_exchange = Exchange::MCX;
            file_name = format!("./downloads/MCX_symbols_{}.txt", today);
        }
        _ => {
            info!("Error: {}", "Unknown index");
            std::process::exit(-1);
        }
    }
    download_scrip(&exchange);
    let (scrip_data, expiry_date) = read_txt_file_as_csv(&file_name, &config_file, &index);
    info!("Expiry date: {}", expiry_date);

    let index_quote = auth.get_quote(&index_exchange, index_token);
    let rounding = config["INDICES_ROUNDING"][index].as_f64().unwrap();
    let rounded_strike = (index_quote / rounding).round() * rounding;

    let (ce_code, ce_symbol) =
        get_strike_info(&scrip_data, &index, &expiry_date, rounded_strike, "CE");
    let (pe_code, pe_symbol) =
        get_strike_info(&scrip_data, &index, &expiry_date, rounded_strike, "PE");

    let ce_quote = auth.get_quote(&exchange, &ce_code);
    let pe_quote = auth.get_quote(&exchange, &pe_code);

    let straddle_preimum = ce_quote + pe_quote;
    let otm_strike_ce = rounded_strike + straddle_preimum;
    let otm_strike_pe = rounded_strike - straddle_preimum;
    // Round the OTM strikes to the nearest strike price
    let otm_strike_ce = (otm_strike_ce / rounding).round() * rounding;
    let otm_strike_pe = (otm_strike_pe / rounding).round() * rounding;

    // check if the OTM strikes are same as the rounded_strike
    if otm_strike_ce == rounded_strike || otm_strike_pe == rounded_strike {
        error!("Cannot do the iron fly strategy, exiting!");
        std::process::exit(-1);
    }

    let (ce_code_sl, ce_symbol_sl) =
        get_strike_info(&scrip_data, &index, &expiry_date, otm_strike_ce, "CE");
    let (pe_code_sl, pe_symbol_sl) =
        get_strike_info(&scrip_data, &index, &expiry_date, otm_strike_pe, "PE");

    let ce_quote_sl = auth.get_quote(&exchange, &ce_code_sl);
    let pe_quote_sl = auth.get_quote(&exchange, &pe_code_sl);

    // max diff between ce_strike and otm_strike_ce and pe_strike and otm_strike_pe
    let max_diff = (otm_strike_ce - rounded_strike)
        .abs()
        .max((otm_strike_pe - rounded_strike).abs());

    let opt_chain = auth.get_option_chain(&exchange, &ce_symbol, rounded_strike);
    let mut stangle_data = serde_json::Value::Null;
    match opt_chain {
        Ok(opt_chain) => {
            let data = opt_chain["values"].as_array().unwrap();
            let mut strikes = Vec::new();
            for item in data.iter() {
                let token = item["token"].as_str().unwrap();
                let tsym = item["tsym"].as_str().unwrap();
                let ltp = auth.get_quote(&exchange, &tsym);
                let opttype = item["optt"].as_str().unwrap();
                strikes.push((ltp, tsym, opttype, token));
            }

            debug!("Strikes: {:?}", strikes);
            // find the nearest strike ltp and strike tsym closest to NEAREST_LTP for each option type,
            // minimize the difference
            let mut nearest_ce_strike = 0.0;
            let mut nearest_ce_strike_tsym = String::new();
            let mut nearest_ce_token: String = String::new();
            let mut nearest_pe_strike = 0.0;
            let mut nearest_pe_strike_tsym = String::new();
            let mut nearest_pe_token: String = String::new();
            let mut min_diff_ce = f64::MAX;
            let mut min_diff_pe = f64::MAX;
            for strike in strikes.iter() {
                let ltp = strike.0;
                let tsym = strike.1;
                let opttype = strike.2;
                let token = strike.3;
                let diff = (ltp - closest_price).abs();
                if opttype == "CE" && diff < min_diff_ce {
                    min_diff_ce = diff;
                    nearest_ce_strike = ltp;
                    nearest_ce_strike_tsym = tsym.to_string();
                    nearest_ce_token = token.to_string();
                } else if opttype == "PE" && diff < min_diff_pe {
                    min_diff_pe = diff;
                    nearest_pe_strike = ltp;
                    nearest_pe_strike_tsym = tsym.to_string();
                    nearest_pe_token = token.to_string();
                }
            }
            debug!(
                "CE: {} {} {}",
                nearest_ce_strike, nearest_ce_strike_tsym, nearest_ce_token
            );
            debug!(
                "PE: {} {} {}",
                nearest_pe_strike, nearest_pe_strike_tsym, nearest_pe_token
            );

            stangle_data = serde_json::json!({
                "ce_code": nearest_ce_token,
                "pe_code": nearest_pe_token,
                "ce_symbol": nearest_ce_strike_tsym,
                "pe_symbol": nearest_pe_strike_tsym,
                "ce_ltp": nearest_ce_strike,
                "pe_ltp": nearest_pe_strike,
            });
        }
        Err(e) => {
            info!("Error for Option chain: {}", e);
        }
    }

    // create a json object
    let result = serde_json::json!({
        "ce_code": ce_code,
        "pe_code": pe_code,
        "ce_symbol": ce_symbol,
        "pe_symbol": pe_symbol,
        "ce_ltp": ce_quote,
        "pe_ltp": pe_quote,
        "ce_code_sl": ce_code_sl,
        "pe_code_sl": pe_code_sl,
        "ce_symbol_sl": ce_symbol_sl,
        "pe_symbol_sl": pe_symbol_sl,
        "ce_ltp_sl": ce_quote_sl,
        "pe_ltp_sl": pe_quote_sl,
        "max_diff": max_diff,
        "strangle": stangle_data
    });

    result
}

/// Shoonya Trading Bot
#[derive(clap::Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Force login
    #[clap(short, long)]
    force: bool,

    /// Index to trade
    #[clap(short, long, default_value = "BANKNIFTY")]
    index: String,

    /// Quantity to trade
    #[clap(short, long, default_value = "1")]
    qty: u32,

    /// Stop loss factor
    #[clap(long, default_value = "30")]
    sl_factor: u32,

    /// Target profit
    #[clap(long, default_value = "35")]
    target: u32,

    /// Log level
    #[clap(long, default_value = "DEBUG")]
    log_level: String,

    /// Show strikes only and exit
    #[clap(short, long)]
    show_strikes: bool,

    /// PnL display interval in seconds
    #[clap(long, default_value = "5")]
    pnl_display_interval: u32,

    /// Target MTM profit
    #[clap(long, default_value = "1000")]
    target_mtm: u32,

    /// Book profit percent of premium left
    #[clap(long, default_value = "50")]
    book_profit: u32,

    /// Credentials file
    #[clap(short, long, default_value = "../cred.yml")]
    credentials_file: String,

    /// Closest to ltp
    #[clap(long, default_value = "25.0")]
    closest_ltp: f64,
}

fn main() {
    let args = Cli::parse();

    let log_level = match args.log_level.as_str() {
        "INFO" => log::LevelFilter::Info,
        "DEBUG" => log::LevelFilter::Debug,
        "WARN" => log::LevelFilter::Warn,
        "ERROR" => log::LevelFilter::Error,
        "NONE" => log::LevelFilter::Off,
        _ => log::LevelFilter::Info,
    };

    logger::init_logger("shoonya_rust", log_level);

    let mut auth = Auth::new();

    auth.login(args.credentials_file.as_str(), args.force);

    // let order_book = get_order_book(&auth);

    // match order_book {
    //     Ok(order_book) => {
    //         info!("Order book: {}", order_book);
    //     }
    //     Err(e) => {
    //         info!("Error: {}", e);
    //     }
    // }

    // let straddle_strikes = get_straddle_strikes(&auth, args.index.as_str(), args.closest_ltp);
    // info!(
    //     "Straddle strikes: {}",
    //     pretty_print_json(&straddle_strikes, 3)
    // );

    let websocket = WebSocketApp::new(WebSocketCallbackHandler);

    let mut order_manager = order_manager::OrderManager::new(websocket, auth);

    order_manager.start();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if order_manager.day_over() {
            break;
        }
    }
}
