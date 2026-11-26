use crate::parse;
use nautilus_model::{
    identifiers::{InstrumentId, Symbol},
    instruments::CurrencyPair,
    types::{Currency, Price, Quantity},
};

pub fn usd_jpy(
    client_json: &str,
    instrument_json: &str,
) -> Result<CurrencyPair, Box<dyn std::error::Error>> {
    parse::facts(client_json, instrument_json)?;
    let instrument: serde_json::Value = serde_json::from_str(instrument_json)?;
    if instrument["AssetType"] != "FxSpot" || instrument["TickSize"].as_f64() != Some(0.001) {
        return Err("unsupported instrument".into());
    }
    Ok(CurrencyPair::builder()
        .instrument_id(InstrumentId::from("USD/JPY.SAXO"))
        .raw_symbol(Symbol::from("USD/JPY"))
        .base_currency(Currency::USD())
        .quote_currency(Currency::JPY())
        .price_precision(3)
        .size_precision(0)
        .price_increment(Price::from("0.001"))
        .size_increment(Quantity::from("1"))
        .min_quantity(Quantity::from("1000"))
        .ts_event(0.into())
        .ts_init(0.into())
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn instrument_matches_facts() {
        let mut client: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/live_client.json"))
                .expect("fixture");
        client["response"]["ForceOpenDefaultValue"] = false.into();
        let instrument: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/live_USDJPY_instrument.json"
        ))
        .expect("fixture");
        let pair = usd_jpy(
            &client["response"].to_string(),
            &instrument["response"].to_string(),
        )
        .expect("pair");
        assert_eq!(pair.id.to_string(), "USD/JPY.SAXO");
        assert_eq!(pair.price_precision, 3);
        assert_eq!(pair.size_precision, 0);
        assert_eq!(pair.min_quantity, Some(Quantity::from("1000")));
    }
}
