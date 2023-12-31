use common::utils::utils::*;
use scrip_master::scrips::download_scrip;
use shoonya::auth::auth::Auth;
use shoonya::markets::markets::{get_indices, get_quote};
use shoonya::orders::orders::get_order_book;

use clap::Parser;
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

fn get_straddle_strikes(auth: &Auth, index: &str) -> serde_json::Value {
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
    let rounding = config["INDICES_ROUNDING"][index].as_f64().unwrap();
    let rounded_ltp = (index_quote / rounding).round() * rounding;

    let (ce_code, ce_symbol) = get_strike_info(&scrip_data, &expiry_date, rounded_ltp, "CE");
    let (pe_code, pe_symbol) = get_strike_info(&scrip_data, &expiry_date, rounded_ltp, "PE");

    let ce_quote = get_quote(&auth, &Exchange::NFO, &ce_code);
    let pe_quote = get_quote(&auth, &Exchange::NFO, &pe_code);

    let straddle_preimum = ce_quote + pe_quote;
    let otm_strike_ce = rounded_ltp + straddle_preimum;
    let otm_strike_pe = rounded_ltp - straddle_preimum;
    // Round the OTM strikes to the nearest strike price
    let otm_strike_ce = (otm_strike_ce / rounding).round() * rounding;
    let otm_strike_pe = (otm_strike_pe / rounding).round() * rounding;

    let (ce_code_sl, ce_symbol_sl) =
        get_strike_info(&scrip_data, &expiry_date, otm_strike_ce, "CE");
    let (pe_code_sl, pe_symbol_sl) =
        get_strike_info(&scrip_data, &expiry_date, otm_strike_pe, "PE");

    let ce_quote_sl = get_quote(&auth, &Exchange::NFO, &ce_code_sl);
    let pe_quote_sl = get_quote(&auth, &Exchange::NFO, &pe_code_sl);

    // create a json object
    let result = serde_json::json!({
        "ce_code": ce_code,
        "ce_ltp": ce_quote,
        "pe_code": pe_code,
        "pe_ltp": pe_quote,
        "ce_symbol": ce_symbol,
        "pe_symbol": pe_symbol,
        "ce_code_sl": ce_code_sl,
        "ce_ltp_sl": ce_quote_sl,
        "pe_code_sl": pe_code_sl,
        "pe_ltp_sl": pe_quote_sl,
        "ce_symbol_sl": ce_symbol_sl,
        "pe_symbol_sl": pe_symbol_sl,
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
    #[clap(long, default_value = "INFO")]
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
}

fn main() {
    let args = Cli::parse();

    let log_level = match args.log_level.as_str() {
        "INFO" => log::LevelFilter::Info,
        "DEBUG" => log::LevelFilter::Debug,
        _ => log::LevelFilter::Info,
    };

    logger::init_logger("shoonya_rust", log_level);

    let mut auth = Auth::new();

    auth.login(args.credentials_file.as_str());

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

    let straddle_strikes = get_straddle_strikes(&auth, args.index.as_str());
    info!("Straddle strikes: {}", straddle_strikes);
}
