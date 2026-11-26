use crate::{Milli, wire};

#[derive(Debug, PartialEq, Eq)]
pub enum Quote {
    Firm { bid: Milli, ask: Milli },
    NotNow,
    Unusable,
}

pub fn quote(json: &str) -> Quote {
    let Ok(response) = serde_json::from_str::<wire::QuoteResponse>(json) else {
        return Quote::Unusable;
    };
    let q = response.quote;
    if q.error_code.as_deref() != Some("None") {
        return Quote::Unusable;
    }
    if q.price_type_bid.as_deref() == Some("Indicative")
        || q.price_type_ask.as_deref() == Some("Indicative")
    {
        return Quote::NotNow;
    }
    if q.delayed_by_minutes != Some(0)
        || q.price_type_bid.as_deref() != Some("Tradable")
        || q.price_type_ask.as_deref() != Some("Tradable")
        || q.market_state.as_deref() != Some("Open")
    {
        return Quote::Unusable;
    }
    let (Some(bid), Some(ask)) = (q.bid, q.ask) else {
        return Quote::Unusable;
    };
    match (Milli::try_from(bid), Milli::try_from(ask)) {
        (Ok(bid), Ok(ask)) if bid.0 > 0 && bid <= ask => Quote::Firm { bid, ask },
        _ => Quote::Unusable,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn response(s: &str) -> String {
        serde_json::from_str::<serde_json::Value>(s).expect("fixture")["response"].to_string()
    }
    #[test]
    fn quotes() {
        let tradable = response(include_str!(
            "../tests/fixtures/get_infoprices_Tradable.json"
        ));
        assert_eq!(
            quote(&tradable),
            Quote::Firm {
                bid: Milli(146799),
                ask: Milli(146819)
            }
        );
        assert_eq!(
            quote(&response(include_str!(
                "../tests/fixtures/get_infoprices_Indicative.json"
            ))),
            Quote::NotNow
        );
        let mut v: serde_json::Value = serde_json::from_str(&tradable).expect("json");
        v["Quote"]["ErrorCode"] = "Error".into();
        assert_eq!(quote(&v.to_string()), Quote::Unusable);
    }
}

#[derive(Debug, PartialEq)]
pub enum Fate {
    Working {
        filled: f64,
    },
    Done {
        filled: f64,
        avg_price: Option<Milli>,
    },
    Cancelled {
        filled: f64,
    },
    Unknown,
}

pub fn fate(json: &str, order_id: &str) -> Fate {
    let Ok(activities) = serde_json::from_str::<wire::Activities>(json) else {
        return Fate::Unknown;
    };
    if activities
        .data
        .iter()
        .any(|a| a.log_id.is_empty() || !a.log_id.bytes().all(|b| b.is_ascii_digit()))
    {
        return Fate::Unknown;
    }
    let mut latest = None;
    for activity in activities
        .data
        .into_iter()
        .filter(|a| a.order_id == order_id)
    {
        let Ok(timestamp) = time::OffsetDateTime::parse(
            &activity.activity_time,
            &time::format_description::well_known::Rfc3339,
        ) else {
            return Fate::Unknown;
        };
        let log_id = activity.log_id.trim_start_matches('0');
        if latest
            .as_ref()
            .is_none_or(|(t, previous): &(time::OffsetDateTime, wire::Activity)| {
                let previous_id = previous.log_id.trim_start_matches('0');
                (timestamp, log_id.len(), log_id) > (*t, previous_id.len(), previous_id)
            })
        {
            latest = Some((timestamp, activity));
        }
    }
    let Some((_, a)) = latest else {
        return Fate::Unknown;
    };
    let filled = a.filled_amount.unwrap_or(0.0);
    if !filled.is_finite() || filled < 0.0 {
        return Fate::Unknown;
    }
    match a.status.as_str() {
        "Placed" | "Changed" | "PartialFill" => Fate::Working { filled },
        "Cancelled" => Fate::Cancelled { filled },
        "FinalFill" => match a.average_price.map(Milli::try_from).transpose() {
            Ok(avg_price) => Fate::Done { filled, avg_price },
            Err(_) => Fate::Unknown,
        },
        _ => Fate::Unknown,
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct PlacedIds {
    pub order_id: String,
    pub related: Vec<String>,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Placement {
    Placed(PlacedIds),
    Unknown,
}
pub fn order_ids(json: &str) -> Result<PlacedIds, serde_json::Error> {
    let placed: wire::Placed = serde_json::from_str(json)?;
    Ok(PlacedIds {
        order_id: placed.order_id,
        related: placed.orders.into_iter().map(|p| p.order_id).collect(),
    })
}
pub fn placement(status: u16, json: &str) -> Result<Placement, serde_json::Error> {
    if status == 409 {
        Ok(Placement::Unknown)
    } else {
        order_ids(json).map(Placement::Placed)
    }
}
pub fn error_code(json: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()?
        .get("ErrorInfo")?
        .get("ErrorCode")?
        .as_str()
        .map(str::to_owned)
}
#[cfg(test)]
mod order_tests {
    use super::*;
    fn response(s: &str) -> String {
        serde_json::from_str::<serde_json::Value>(s).expect("fixture")["response"].to_string()
    }
    #[test]
    fn activity_fates() {
        let cases = [
            (
                include_str!("../tests/fixtures/get_orderactivities_Cancelled_Placed_Stop.json"),
                "1008",
                Fate::Cancelled { filled: 0.0 },
            ),
            (
                include_str!(
                    "../tests/fixtures/get_orderactivities_Changed_FinalFill_Placed_Limit.json"
                ),
                "1021",
                Fate::Done {
                    filled: 1000.0,
                    avg_price: Some(Milli(146594)),
                },
            ),
            (
                include_str!("../tests/fixtures/get_orderactivities_Changed_Placed_Stop.json"),
                "1034",
                Fate::Working { filled: 0.0 },
            ),
            (
                include_str!("../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"),
                "1001",
                Fate::Done {
                    filled: 1000.0,
                    avg_price: Some(Milli(146821)),
                },
            ),
            (
                include_str!(
                    "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market_Stop.json"
                ),
                "1029",
                Fate::Done {
                    filled: 1000.0,
                    avg_price: Some(Milli(146564)),
                },
            ),
            (
                include_str!("../tests/fixtures/get_orderactivities_Placed_Limit.json"),
                "1013",
                Fate::Working { filled: 0.0 },
            ),
            (
                include_str!("../tests/fixtures/get_orderactivities_Placed_Stop.json"),
                "1012",
                Fate::Working { filled: 0.0 },
            ),
        ];
        for (json, id, expected) in cases {
            let json = response(json);
            assert_eq!(fate(&json, id), expected);
            assert_eq!(fate(&json, "absent"), Fate::Unknown);
        }
    }
    #[test]
    fn same_time_uses_log_id_and_rejects_missing_or_invalid_ids() {
        let mut json: serde_json::Value = serde_json::from_str(&response(include_str!(
            "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
        )))
        .expect("fixture");
        let rows = json["Data"].as_array_mut().expect("data");
        for row in rows.iter_mut() {
            row["ActivityTime"] = "2030-01-02T14:19:35Z".into();
        }
        let expected = Fate::Done {
            filled: 1000.0,
            avg_price: Some(Milli(146821)),
        };
        assert_eq!(fate(&json.to_string(), "1001"), expected);
        json["Data"][0]["LogId"] = "9".into();
        json["Data"][1]["LogId"] = "0009".into();
        json["Data"][2]["LogId"] = "10".into();
        assert_eq!(fate(&json.to_string(), "1001"), expected);
        json["Data"].as_array_mut().expect("data").reverse();
        assert_eq!(fate(&json.to_string(), "1001"), expected);
        json["Data"][0]
            .as_object_mut()
            .expect("row")
            .remove("LogId");
        assert_eq!(fate(&json.to_string(), "1001"), Fate::Unknown);
        json["Data"][0]["LogId"] = "not-a-number".into();
        assert_eq!(fate(&json.to_string(), "1001"), Fate::Unknown);
        json["Data"][0]["OrderId"] = "other".into();
        assert_eq!(fate(&json.to_string(), "1001"), Fate::Unknown);
    }
    #[test]
    fn placements_and_errors() {
        let json = response(include_str!(
            "../tests/fixtures/post_trade_v2_orders_200_with_related.json"
        ));
        assert_eq!(
            order_ids(&json).expect("ids"),
            PlacedIds {
                order_id: "1001".into(),
                related: vec!["1002".into()]
            }
        );
        for fixture in [
            include_str!("../tests/fixtures/post_trade_v2_orders_409_single.json"),
            include_str!("../tests/fixtures/post_trade_v2_orders_409_with_related.json"),
        ] {
            let v: serde_json::Value = serde_json::from_str(fixture).expect("fixture");
            assert_eq!(
                placement(409, v["response"].as_str().unwrap_or("")).expect("placement"),
                Placement::Unknown
            );
        }
        for (fixture, code) in [
            (
                include_str!(
                    "../tests/fixtures/patch_trade_v2_orders_400_IllegalOrderTypeChange.json"
                ),
                "IllegalOrderTypeChange",
            ),
            (
                include_str!(
                    "../tests/fixtures/patch_trade_v2_orders_400_OnWrongSideOfMarket.json"
                ),
                "OnWrongSideOfMarket",
            ),
            (
                include_str!("../tests/fixtures/patch_trade_v2_orders_404_OrderNotFound.json"),
                "OrderNotFound",
            ),
        ] {
            assert_eq!(error_code(&response(fixture)).as_deref(), Some(code));
        }
    }
}
#[derive(Debug, PartialEq)]
pub struct Facts {
    pub minimum_trade_size: f64,
    pub tick_size_stop_order: Milli,
}
#[derive(Debug, thiserror::Error)]
pub enum FactsError {
    #[error("invalid response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("force open must be disabled")]
    ForceOpen,
    #[error("unsupported netting profile")]
    Netting,
    #[error("unsupported instrument")]
    Instrument,
}
pub fn facts(client_json: &str, instrument_json: &str) -> Result<Facts, FactsError> {
    let client: wire::Client = serde_json::from_str(client_json)?;
    let instrument: wire::Instrument = serde_json::from_str(instrument_json)?;
    if client.force_open_default_value {
        return Err(FactsError::ForceOpen);
    }
    if client.position_netting_profile != "FifoRealTime" {
        return Err(FactsError::Netting);
    }
    if instrument.minimum_trade_size != 1000.0
        || Milli::try_from(instrument.tick_size_stop_order) != Ok(Milli(1))
        || !["Market", "Stop"]
            .iter()
            .all(|s| instrument.supported_order_types.iter().any(|t| t == s))
    {
        return Err(FactsError::Instrument);
    }
    Ok(Facts {
        minimum_trade_size: instrument.minimum_trade_size,
        tick_size_stop_order: Milli(1),
    })
}
#[derive(Debug, PartialEq)]
pub struct Position {
    pub amount: f64,
    pub is_force_open: bool,
}
#[derive(Debug, PartialEq)]
pub struct Positions {
    pub positions: Vec<Position>,
    pub net_units: f64,
    pub count: usize,
}
pub fn positions(json: &str) -> Result<Positions, serde_json::Error> {
    let data: wire::Positions = serde_json::from_str(json)?;
    let positions: Vec<_> = data
        .data
        .into_iter()
        .map(|p| Position {
            amount: p.position_base.amount,
            is_force_open: p.position_base.is_force_open,
        })
        .collect();
    Ok(Positions {
        net_units: positions.iter().map(|p| p.amount).sum(),
        count: positions.len(),
        positions,
    })
}
#[cfg(test)]
mod facts_tests {
    use super::*;
    fn response(s: &str) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(s).expect("fixture")["response"].clone()
    }
    #[test]
    fn account_and_positions() {
        let mut client = response(include_str!("../tests/fixtures/live_client.json"));
        let instrument = response(include_str!(
            "../tests/fixtures/live_USDJPY_instrument.json"
        ))
        .to_string();
        assert!(matches!(
            facts(&client.to_string(), &instrument),
            Err(FactsError::ForceOpen)
        ));
        client["ForceOpenDefaultValue"] = false.into();
        assert!(facts(&client.to_string(), &instrument).is_ok());
        let mut p = response(include_str!(
            "../tests/fixtures/get_port_v1_positions_me_2.json"
        ));
        let parsed = positions(&p.to_string()).expect("positions");
        assert_eq!((parsed.net_units, parsed.count), (-2000.0, 2));
        p["Data"][1]["PositionBase"]["Amount"] = 1000.into();
        p["Data"][1]["PositionBase"]["IsForceOpen"] = true.into();
        let parsed = positions(&p.to_string()).expect("positions");
        assert_eq!((parsed.net_units, parsed.count), (0.0, 2));
        assert!(parsed.positions[1].is_force_open);
    }
}
