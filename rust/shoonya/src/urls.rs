#[allow(dead_code)]

pub mod urls {
    pub const HOST: &str = "https://api.shoonya.com/NorenWClientTP/";
    pub const AUTHORIZE: &str = "/QuickAuth";
    pub const LOGOUT: &str = "/Logout";
    pub const FORGOT_PASSWORD: &str = "/ForgotPassword";
    pub const CHANGE_PASSWORD: &str = "/Changepwd";
    pub const WATCHLIST_NAMES: &str = "/MWList";
    pub const WATCHLIST: &str = "/MarketWatch";
    pub const WATCHLIST_ADD: &str = "/AddMultiScripsToMW";
    pub const WATCHLIST_DELETE: &str = "/DeleteMultiMWScrips";
    pub const PLACEORDER: &str = "/PlaceOrder";
    pub const MODIFYORDER: &str = "/ModifyOrder";
    pub const CANCELORDER: &str = "/CancelOrder";
    pub const EXITORDER: &str = "/ExitSNOOrder";
    pub const PRODUCT_CONVERSION: &str = "/ProductConversion";
    pub const ORDERBOOK: &str = "/OrderBook";
    pub const TRADEBOOK: &str = "/TradeBook";
    pub const SINGLEORDERHISTORY: &str = "/SingleOrdHist";
    pub const SEARCHSCRIP: &str = "/SearchScrip";
    pub const TPSERIES: &str = "/TPSeries";
    pub const OPTIONCHAIN: &str = "/GetOptionChain";
    pub const HOLDINGS: &str = "/Holdings";
    pub const LIMITS: &str = "/Limits";
    pub const POSITIONS: &str = "/PositionBook";
    pub const SCRIPINFO: &str = "/GetSecurityInfo";
    pub const GETQUOTES: &str = "/GetQuotes";
    pub const SPAN_CALCULATOR: &str = "/SpanCalc";
    pub const OPTION_GREEK: &str = "/GetOptionGreek";
    pub const GET_DAILY_PRICE_SERIES: &str = "/EODChartData";
    pub const WEBSOCKET_ENDPOINT: &str = "wss://wsendpoint/";
    pub const GET_INDICES_LIST: &str = "/GetIndexList";
}
