use futures::StreamExt;
use ibapi::{accounts::PositionUpdate, contracts::Contract, subscriptions::SubscriptionItem};
use nautilus_interactive_brokers::common::shared_client::{SharedClientHandle, get_or_connect};
use std::{process::ExitCode, time::Duration};

#[derive(Debug, PartialEq)]
struct Args {
    port: u16,
    client_id: i32,
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, &'static str> {
    let mut port = None;
    let mut client_id = None;
    let mut args = args.into_iter();
    while let Some(flag) = args.next() {
        let value = args.next().ok_or("missing argument value")?;
        match flag.as_str() {
            "--port" if port.is_none() => {
                let value = value.parse::<u16>().map_err(|_| "invalid port")?;
                if value == 0 {
                    return Err("invalid port");
                }
                port = Some(value);
            }
            "--client-id" if client_id.is_none() => {
                let value = value.parse::<u32>().map_err(|_| "invalid client ID")?;
                client_id =
                    Some(i32::try_from(value).map_err(|_| "client ID exceeds IB API range")?);
            }
            _ => return Err("expected unique --port or --client-id"),
        }
    }
    Ok(Args {
        port: port.unwrap_or(7497),
        client_id: client_id.unwrap_or(7),
    })
}

async fn read_summary(client: &SharedClientHandle) -> Result<String, &'static str> {
    let accounts = client
        .managed_accounts()
        .await
        .map_err(|_| "account query failed")?
        .len();
    if accounts == 0 {
        return Err("no managed accounts returned");
    }
    let mut subscription = client
        .positions()
        .await
        .map_err(|_| "position query failed")?;
    let mut positions = 0;
    loop {
        match subscription.next().await {
            Some(Ok(SubscriptionItem::Data(PositionUpdate::Position(p)))) => {
                if !p.position.is_finite() {
                    return Err("invalid position response");
                }
                positions += usize::from(p.position != 0.0);
            }
            Some(Ok(SubscriptionItem::Data(PositionUpdate::PositionEnd))) => break,
            Some(Ok(SubscriptionItem::Notice(_))) => {}
            _ => return Err("position snapshot incomplete"),
        }
    }
    subscription.cancel().await;
    let spy = Contract::stock("SPY").build();
    let details = client
        .contract_details(&spy)
        .await
        .map_err(|_| "SPY contract query failed")?;
    let [detail] = details.as_slice() else {
        return Err("expected one SPY contract");
    };
    if detail.contract.contract_id <= 0
        || detail.contract.symbol != spy.symbol
        || detail.contract.security_type != spy.security_type
        || detail.contract.currency != spy.currency
    {
        return Err("invalid SPY contract response");
    }
    Ok(format!(
        "connected server_version={} accounts={accounts} positions={positions} spy_conid={}",
        client.server_version(),
        detail.contract.contract_id
    ))
}

async fn probe(args: Args) -> Result<String, &'static str> {
    let client = get_or_connect("127.0.0.1", args.port, args.client_id, 5, Some(0))
        .await
        .map_err(|_| "connection failed")?;
    let result = read_summary(&client).await;
    client.disconnect().await;
    result
}

fn main() -> ExitCode {
    let result = (|| {
        let args = parse_args(std::env::args().skip(1))?;
        for name in ["IBAPI_RECORDING_DIR", "IBAPI_RAW_CAPTURE_DIR"] {
            if std::env::var_os(name).is_some() {
                return Err("IBAPI recording environment must be unset");
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "runtime initialization failed")?;
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(15), probe(args))
                .await
                .map_err(|_| "probe timed out")?
        })
    })();
    match result {
        Ok(summary) => {
            println!("{summary}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("ib-probe: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args, &'static str> {
        parse_args(args.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn defaults_and_overrides() {
        assert_eq!(
            parse(&[]),
            Ok(Args {
                port: 7497,
                client_id: 7
            })
        );
        assert_eq!(
            parse(&["--client-id", "42", "--port", "12345"]),
            Ok(Args {
                port: 12345,
                client_id: 42
            })
        );
        assert_eq!(
            parse(&["--port", "65535", "--client-id", "0"]),
            Ok(Args {
                port: 65535,
                client_id: 0
            })
        );
    }

    #[test]
    fn rejects_invalid_arguments() {
        for args in [
            vec!["--port"],
            vec!["--host", "example.com"],
            vec!["--port", "0"],
            vec!["--port", "65536"],
            vec!["--port", "no"],
            vec!["--client-id", "-1"],
            vec!["--client-id", "4294967296"],
            vec!["--client-id", "2147483648"],
            vec!["--port", "1", "--port", "2"],
            vec!["--client-id", "1", "--client-id", "2"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }
}
