use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct QuoteResponse {
    pub quote: Quote,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Quote {
    pub bid: Option<f64>,
    pub ask: Option<f64>,
    pub error_code: Option<String>,
    pub delayed_by_minutes: Option<u32>,
    pub price_type_bid: Option<String>,
    pub price_type_ask: Option<String>,
    pub market_state: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Activities {
    pub data: Vec<Activity>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Activity {
    pub activity_time: String,
    pub log_id: String,
    pub order_id: String,
    pub status: String,
    pub filled_amount: Option<f64>,
    pub average_price: Option<f64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Placed {
    pub order_id: String,
    #[serde(default)]
    pub orders: Vec<Placed>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Client {
    pub force_open_default_value: bool,
    pub position_netting_profile: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Instrument {
    pub minimum_trade_size: f64,
    pub tick_size_stop_order: f64,
    pub supported_order_types: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Positions {
    pub data: Vec<Position>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct Position {
    pub position_base: PositionBase,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
pub(crate) struct PositionBase {
    pub amount: f64,
    pub is_force_open: bool,
}
