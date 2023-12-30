#[allow(dead_code)]
// main.rs
mod auth;
mod logger;
mod markets;
mod orders;
mod scrips;
mod urls;

fn main() {
    logger::init_logger("shhonya_rust", log::LevelFilter::Debug);

    let mut auth = auth::auth::Auth::new();

    auth.login("../cred.yml");

    // let order_book = orders::orders::get_order_book(&auth);

    // match order_book {
    //     Ok(order_book) => {
    //         info!("Order book: {}", order_book);
    //     }
    //     Err(e) => {
    //         info!("Error: {}", e);
    //     }
    // }

    // let indices = markets::markets::get_indices(&auth, "NSE");

    // match indices {
    //     Ok(indices) => {
    //         info!("Indices: {}", indices);
    //     }
    //     Err(e) => {
    //         info!("Error Occured: {}", e);
    //     }
    // }

    // let scrips = scrips::scrips::download_scrip(&scrips::scrips::Exchange::BFO);
    let quote = markets::markets::get_quote(&auth, "NSE", "22");

    log::info!("Quote: {}", quote);
}
