use crate::{env::Env, parse};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Accounts {
    data: Vec<Account>,
}
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Account {
    account_key: String,
    active: bool,
    legal_asset_types: Vec<String>,
    is_trial_account: bool,
}

pub fn check(
    accounts_json: &str,
    account_key: &str,
    env: Env,
    client_json: &str,
    instrument_json: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    parse::facts(client_json, instrument_json)?;
    let accounts: Accounts = serde_json::from_str(accounts_json)?;
    let matching: Vec<_> = accounts
        .data
        .iter()
        .filter(|a| a.account_key == account_key)
        .collect();
    if account_key.is_empty()
        || matching.len() != 1
        || !matching[0].active
        || !matching[0].legal_asset_types.iter().any(|s| s == "FxSpot")
        || matching[0].is_trial_account != (env == Env::Sim)
    {
        return Err("account does not match required settings".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn response(s: &str) -> serde_json::Value {
        serde_json::from_str::<serde_json::Value>(s).expect("fixture")["response"].clone()
    }
    #[test]
    fn checks_account_and_facts() {
        let mut accounts = response(include_str!(
            "../tests/fixtures/get_port_v1_accounts_me.json"
        ));
        let mut client = response(include_str!("../tests/fixtures/live_client.json"));
        client["ForceOpenDefaultValue"] = false.into();
        let instrument = response(include_str!(
            "../tests/fixtures/live_USDJPY_instrument.json"
        ));
        let check = |a: &serde_json::Value, c: &serde_json::Value, env| {
            super::check(
                &a.to_string(),
                "TestAccountKey1==",
                env,
                &c.to_string(),
                &instrument.to_string(),
            )
        };
        assert!(check(&accounts, &client, Env::Sim).is_ok());
        assert!(check(&accounts, &client, Env::Live).is_err());
        let original = accounts.clone();
        accounts["Data"][0]["AccountKey"] = "other".into();
        assert!(check(&accounts, &client, Env::Sim).is_err());
        accounts = original.clone();
        accounts["Data"]
            .as_array_mut()
            .expect("array")
            .push(original["Data"][0].clone());
        assert!(check(&accounts, &client, Env::Sim).is_err());
        accounts = original.clone();
        accounts["Data"][0]["LegalAssetTypes"] = serde_json::json!([]);
        assert!(check(&accounts, &client, Env::Sim).is_err());
        accounts = original.clone();
        client["ForceOpenDefaultValue"] = true.into();
        assert!(check(&accounts, &client, Env::Sim).is_err());
    }
}
