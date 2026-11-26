use serde_json::{Value, json};

pub type Body = Value;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}
impl Side {
    fn name(self) -> &'static str {
        match self {
            Self::Buy => "Buy",
            Self::Sell => "Sell",
        }
    }
    fn opposite(self) -> Self {
        match self {
            Self::Buy => Self::Sell,
            Self::Sell => Self::Buy,
        }
    }
}
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RequestError {
    #[error("invalid external reference")]
    ExternalReference,
    #[error("units must be at least 1000")]
    Units,
    #[error("invalid price")]
    Price,
    #[error("invalid order id")]
    OrderId,
}
fn validate(units: u64, reference: &str) -> Result<(), RequestError> {
    if units < 1000 {
        return Err(RequestError::Units);
    }
    if reference.is_empty()
        || reference.len() > 50
        || !reference
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(RequestError::ExternalReference);
    }
    Ok(())
}
fn stop_price(side: Side, price: f64) -> Result<f64, RequestError> {
    if !price.is_finite() || price <= 0.0 || price * 1000.0 >= i64::MAX as f64 {
        return Err(RequestError::Price);
    }
    let scaled = price * 1000.0;
    let rounded = match side {
        Side::Sell => scaled.floor(),
        Side::Buy => scaled.ceil(),
    };
    if rounded <= 0.0 {
        return Err(RequestError::Price);
    }
    Ok(rounded / 1000.0)
}
fn order(
    account_key: &str,
    side: Side,
    units: u64,
    reference: &str,
    kind: &str,
) -> Result<Body, RequestError> {
    validate(units, reference)?;
    Ok(
        json!({"AccountKey": account_key, "Uic": 42, "AssetType": "FxSpot", "BuySell": side.name(), "Amount": units, "OrderType": kind, "OrderDuration": {"DurationType": "DayOrder"}, "ManualOrder": false, "IsForceOpen": false, "ExternalReference": reference}),
    )
}
pub fn market(
    account_key: &str,
    side: Side,
    units: u64,
    external_ref: &str,
) -> Result<Body, RequestError> {
    order(account_key, side, units, external_ref, "Market")
}
pub fn open_with_stop(
    account_key: &str,
    side: Side,
    units: u64,
    price: f64,
    external_refs: (&str, &str),
) -> Result<Body, RequestError> {
    let mut entry = market(account_key, side, units, external_refs.0)?;
    let stop_side = side.opposite();
    let mut stop = order(account_key, stop_side, units, external_refs.1, "Stop")?;
    stop["OrderPrice"] = json!(stop_price(stop_side, price)?);
    entry["Orders"] = json!([stop]);
    Ok(entry)
}
pub fn move_stop(
    account_key: &str,
    order_id: &str,
    side: Side,
    units: u64,
    price: f64,
    external_ref: &str,
) -> Result<Body, RequestError> {
    check_id(order_id)?;
    let mut body = order(account_key, side, units, external_ref, "Stop")?;
    body["OrderId"] = json!(order_id);
    body["OrderPrice"] = json!(stop_price(side, price)?);
    Ok(body)
}
fn check_id(id: &str) -> Result<(), RequestError> {
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(RequestError::OrderId);
    }
    Ok(())
}
pub fn cancel_path(order_id: &str, account_key: &str) -> Result<String, RequestError> {
    check_id(order_id)?;
    let mut encoded = String::new();
    for b in account_key.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            encoded.push(char::from(b));
        } else {
            use std::fmt::Write;
            let _ = write!(encoded, "%{b:02X}");
        }
    }
    Ok(format!("/trade/v2/orders/{order_id}?AccountKey={encoded}"))
}
pub fn disable_force_open(account_key: &str) -> Body {
    json!({"AccountKey": account_key, "DisableForceOpen": true})
}
#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    #[test]
    fn bodies_and_validation() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/post_trade_v2_orders_200_with_related.json"
        ))
        .expect("fixture");
        let mut body = open_with_stop("key", Side::Buy, 1000, 146.3009, ("r79-S1-E", "r79-S1-S"))
            .expect("body");
        assert_eq!(body["Orders"][0]["OrderPrice"], json!(146.3));
        {
            let v = &mut body["Orders"][0];
            v.as_object_mut().expect("object").remove("AccountKey");
            v.as_object_mut().expect("object").remove("IsForceOpen");
        }
        body.as_object_mut().expect("object").remove("AccountKey");
        body.as_object_mut().expect("object").remove("IsForceOpen");
        assert_eq!(body, fixture["request"]);
        let patch_fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/patch_trade_v2_orders_200_single.json"
        ))
        .expect("fixture");
        let patch = move_stop("key", "1021", Side::Sell, 1000, 146.611, "r79-S5-L").expect("patch");
        let fixture_keys = patch_fixture["request"]
            .as_object()
            .expect("request")
            .keys()
            .collect::<std::collections::BTreeSet<_>>();
        let patch_keys = patch
            .as_object()
            .expect("patch")
            .keys()
            .filter(|k| *k != "AccountKey" && *k != "IsForceOpen")
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(patch_keys, fixture_keys);
        let market_fixture: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/post_trade_v2_orders_200_single.json"
        ))
        .expect("fixture");
        let market_body = market("key", Side::Sell, 1000, "ref").expect("market");
        let market_keys = market_body
            .as_object()
            .expect("market")
            .keys()
            .filter(|k| *k != "AccountKey" && *k != "IsForceOpen")
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            market_keys,
            market_fixture["request"]
                .as_object()
                .expect("request")
                .keys()
                .collect()
        );
        assert_eq!(stop_price(Side::Buy, 146.3001).expect("price"), 146.301);
        for bad in ["", "bad_", "a b", &"a".repeat(51)] {
            assert!(market("key", Side::Buy, 1000, bad).is_err());
        }
        assert!(market("key", Side::Buy, 999, "ok").is_err());
        assert_eq!(
            cancel_path("123", "a+/ 한").expect("path"),
            "/trade/v2/orders/123?AccountKey=a%2B%2F%20%ED%95%9C"
        );
        assert!(cancel_path("1/2", "x").is_err());
        assert!(include_str!("live_gate.rs").contains("LIVE_ENABLED: bool = false"));
    }
    proptest! {
        #[test]
        fn stop_does_not_narrow(p in 1.0_f64..10000.0) {
            let sell = stop_price(Side::Sell, p).expect("sell");
            let buy = stop_price(Side::Buy, p).expect("buy");
            prop_assert!(sell <= p); prop_assert!(buy >= p);
        }
        #[test]
        fn milli_round_trip(n in 0_i64..10_000_000) { prop_assert_eq!(crate::Milli::try_from(f64::from(crate::Milli(n))), Ok(crate::Milli(n))); }
    }
}
