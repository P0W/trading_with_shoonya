pub mod utils {

    use serde_json::json;

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

    
}