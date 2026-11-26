use crate::{
    config::SaxoDataClientConfig,
    http::SaxoHttp,
    parse::{self, Quote},
    providers,
};
use anyhow::Context;
use nautilus_common::{
    clients::DataClient,
    live::{runner::try_get_data_event_sender, sender::EventSender},
    messages::{
        DataEvent,
        data::{SubscribeQuotes, UnsubscribeQuotes},
    },
};
use nautilus_core::time::get_atomic_clock_realtime;
use nautilus_model::{
    data::{Data, QuoteTick},
    identifiers::{ClientId, InstrumentId, Venue},
    instruments::InstrumentAny,
    types::{Price, Quantity},
};
use std::sync::Arc;
use tokio::{sync::Mutex, task::JoinHandle};

pub struct SaxoDataClient {
    client_id: ClientId,
    config: SaxoDataClientConfig,
    http: Arc<Mutex<SaxoHttp>>,
    sender: Option<EventSender<DataEvent>>,
    connected: bool,
    task: Option<JoinHandle<()>>,
}
impl SaxoDataClient {
    pub fn new(client_id: ClientId, config: SaxoDataClientConfig) -> anyhow::Result<Self> {
        let http = SaxoHttp::new(config.env)?;
        Self::build(client_id, config, http)
    }
    fn build(
        client_id: ClientId,
        config: SaxoDataClientConfig,
        http: SaxoHttp,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !config.poll_interval.is_zero() && config.amount > 0,
            "invalid quote polling settings"
        );
        Ok(Self {
            client_id,
            config,
            http: Arc::new(Mutex::new(http)),
            sender: None,
            connected: false,
            task: None,
        })
    }
    fn cancel(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
impl Drop for SaxoDataClient {
    fn drop(&mut self) {
        self.cancel();
    }
}
fn tick(body: &str) -> Option<QuoteTick> {
    let Quote::Firm { bid, ask } = parse::quote(body) else {
        return None;
    };
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let q = &value["Quote"];
    let size = |key| {
        let n = q[key].as_f64()?;
        if !n.is_finite() || n <= 0.0 || n.fract() != 0.0 {
            return None;
        }
        Quantity::new_checked(n, 0).ok()
    };
    let now = get_atomic_clock_realtime().get_time_ns();
    let event = value["LastUpdated"]
        .as_str()
        .and_then(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
        })
        .and_then(|t| u64::try_from(t.unix_timestamp_nanos()).ok())
        .map(Into::into)?;
    QuoteTick::new_checked(
        InstrumentId::from("USD/JPY.SAXO"),
        Price::new_checked(f64::from(bid), 3).ok()?,
        Price::new_checked(f64::from(ask), 3).ok()?,
        size("BidSize")?,
        size("AskSize")?,
        event,
        now,
    )
    .ok()
}
#[async_trait::async_trait(?Send)]
impl DataClient for SaxoDataClient {
    fn client_id(&self) -> ClientId {
        self.client_id
    }
    fn venue(&self) -> Option<Venue> {
        Some(Venue::from("SAXO"))
    }
    fn start(&mut self) -> anyhow::Result<()> {
        self.sender = Some(
            try_get_data_event_sender()
                .ok_or_else(|| anyhow::anyhow!("data event sender not installed"))?,
        );
        Ok(())
    }
    fn stop(&mut self) -> anyhow::Result<()> {
        self.cancel();
        self.connected = false;
        Ok(())
    }
    fn reset(&mut self) -> anyhow::Result<()> {
        self.stop()
    }
    fn dispose(&mut self) -> anyhow::Result<()> {
        self.stop()
    }
    fn is_connected(&self) -> bool {
        self.connected
    }
    fn is_disconnected(&self) -> bool {
        !self.connected
    }
    async fn connect(&mut self) -> anyhow::Result<()> {
        if self.connected {
            return Ok(());
        }
        let sender = self
            .sender
            .as_ref()
            .context("data event sender not installed")?;
        let http = self.http.lock().await;
        let mut tokens = self.config.tokens.lock().await;
        let client = http
            .get(&mut tokens, "/port/v1/clients/me")
            .await
            .context("client request failed")?;
        anyhow::ensure!(
            client.status == 200,
            "client request returned HTTP {}",
            client.status
        );
        let instrument = http
            .get(&mut tokens, "/ref/v1/instruments/details/42/FxSpot")
            .await
            .context("instrument request failed")?;
        anyhow::ensure!(
            instrument.status == 200,
            "instrument request returned HTTP {}",
            instrument.status
        );
        let client = std::str::from_utf8(&client.body).context("client response is not UTF-8")?;
        let instrument =
            std::str::from_utf8(&instrument.body).context("instrument response is not UTF-8")?;
        let pair = providers::usd_jpy(client, instrument)
            .map_err(|e| anyhow::anyhow!("instrument validation failed: {e}"))?;
        sender
            .send(DataEvent::Data(Data::Instrument(Box::new(
                InstrumentAny::CurrencyPair(pair),
            ))))
            .map_err(|_| anyhow::anyhow!("instrument event receiver closed"))?;
        self.connected = true;
        Ok(())
    }
    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop()
    }
    fn subscribe_quotes(&mut self, cmd: SubscribeQuotes) -> anyhow::Result<()> {
        for instrument in &cmd.instrument_ids {
            if *instrument != InstrumentId::from("USD/JPY.SAXO") {
                log::warn!("unsupported quote instrument: {instrument}");
            }
        }
        if !cmd
            .instrument_ids
            .contains(&InstrumentId::from("USD/JPY.SAXO"))
        {
            return Ok(());
        }
        anyhow::ensure!(self.connected, "data client is not connected");
        if self.task.as_ref().is_some_and(|task| !task.is_finished()) {
            return Ok(());
        }
        let sender = self
            .sender
            .clone()
            .ok_or_else(|| anyhow::anyhow!("data event sender not installed"))?;
        let http = self.http.clone();
        let tokens = self.config.tokens.clone();
        let interval = self.config.poll_interval;
        let amount = self.config.amount;
        let path = format!(
            "/trade/v1/infoprices?AssetType=FxSpot&Uic=42&Amount={amount}&FieldGroups=Quote"
        );
        self.task = Some(tokio::runtime::Handle::try_current()?.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let result = http
                    .lock()
                    .await
                    .get(&mut *tokens.lock().await, &path)
                    .await;
                if let Ok(reply) = result
                    && reply.status == 200
                    && let Ok(body) = std::str::from_utf8(&reply.body)
                    && let Some(tick) = tick(body)
                    && sender.send(DataEvent::Data(Data::Quote(tick))).is_err()
                {
                    break;
                }
            }
        }));
        Ok(())
    }
    fn unsubscribe_quotes(&mut self, cmd: &UnsubscribeQuotes) -> anyhow::Result<()> {
        if cmd
            .instrument_ids
            .contains(&InstrumentId::from("USD/JPY.SAXO"))
        {
            self.cancel();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::TokenManager, env::Env, http::test_support::server};
    use nautilus_core::UUID4;
    use std::time::Duration;
    use tokio::{sync::mpsc, time::timeout};

    async fn setup(
        fixture: &str,
    ) -> (
        SaxoDataClient,
        mpsc::UnboundedReceiver<DataEvent>,
        mpsc::UnboundedReceiver<(tokio::time::Instant, String)>,
        JoinHandle<()>,
    ) {
        let body = serde_json::from_str::<serde_json::Value>(fixture).expect("fixture")["response"]
            .to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (base, requests, server_task) =
            server(Box::leak(response.into_boxed_str()), false).await;
        let mut config = SaxoDataClientConfig::new(
            Env::Sim,
            Arc::new(Mutex::new(TokenManager::for_tests("token"))),
        );
        config.poll_interval = Duration::from_millis(75);
        let mut client =
            SaxoDataClient::build(ClientId::from("SAXO"), config, SaxoHttp::test_client(&base))
                .expect("client");
        let (tx, rx) = mpsc::unbounded_channel();
        client.sender = Some(tx.into());
        client.connected = true;
        (client, rx, requests, server_task)
    }
    async fn instrument_server(
        instrument_status: u16,
    ) -> (
        SaxoDataClient,
        mpsc::UnboundedReceiver<DataEvent>,
        JoinHandle<()>,
    ) {
        use tokio::{
            io::{AsyncReadExt, AsyncWriteExt},
            net::TcpListener,
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let server_task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept");
                let mut bytes = Vec::new();
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let mut part = [0; 4096];
                    let count = stream.read(&mut part).await.expect("read");
                    assert!(count > 0, "incomplete request");
                    bytes.extend_from_slice(&part[..count]);
                }
                let request = String::from_utf8_lossy(&bytes);
                let (status, body) = if request.starts_with("GET /port/v1/clients/me ") {
                    let mut value: serde_json::Value = serde_json::from_str::<serde_json::Value>(
                        include_str!("../tests/fixtures/live_client.json"),
                    )
                    .expect("fixture")["response"]
                        .clone();
                    value["ForceOpenDefaultValue"] = false.into();
                    (200, value.to_string())
                } else if request.starts_with("GET /ref/v1/instruments/details/42/FxSpot ") {
                    let value: serde_json::Value = serde_json::from_str::<serde_json::Value>(
                        include_str!("../tests/fixtures/live_USDJPY_instrument.json"),
                    )
                    .expect("fixture")["response"]
                        .clone();
                    (instrument_status, value.to_string())
                } else {
                    (404, "{}".to_owned())
                };
                let response = format!(
                    "HTTP/1.1 {status} response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .await
                    .expect("response");
            }
        });
        let config = SaxoDataClientConfig::new(
            Env::Sim,
            Arc::new(Mutex::new(TokenManager::for_tests("token"))),
        );
        let mut client =
            SaxoDataClient::build(ClientId::from("SAXO"), config, SaxoHttp::test_client(&base))
                .expect("client");
        let (tx, events) = mpsc::unbounded_channel();
        client.sender = Some(tx.into());
        (client, events, server_task)
    }
    #[tokio::test]
    async fn connect_emits_validated_usd_jpy_instrument() {
        use nautilus_model::instruments::InstrumentAny;
        let (mut client, mut events, server) = instrument_server(200).await;
        client.connect().await.expect("connect");
        assert!(client.is_connected());
        let event = events.try_recv().expect("instrument event");
        match event {
            DataEvent::Data(Data::Instrument(instrument)) => match *instrument {
                InstrumentAny::CurrencyPair(pair) => {
                    assert_eq!(pair.id, InstrumentId::from("USD/JPY.SAXO"));
                    assert_eq!(pair.price_increment, Price::from("0.001"));
                    assert_eq!(pair.min_quantity, Some(Quantity::from(1000)));
                    println!(
                        "instrument={} price_increment={} min_quantity={}",
                        pair.id,
                        pair.price_increment,
                        pair.min_quantity.expect("min quantity")
                    );
                }
                other => panic!("unexpected instrument {other:?}"),
            },
            other => panic!("unexpected event {other:?}"),
        }
        assert!(events.try_recv().is_err());
        client.connect().await.expect("idempotent connect");
        assert!(events.try_recv().is_err());
        server.abort();
    }
    #[tokio::test]
    async fn connect_rejects_instrument_http_failure_without_event() {
        let (mut client, mut events, server) = instrument_server(500).await;
        let error = client.connect().await.expect_err("instrument failure");
        assert!(
            error
                .to_string()
                .contains("instrument request returned HTTP 500")
        );
        println!("error={error} connected={} events=0", client.is_connected());
        assert!(!client.is_connected());
        assert!(events.try_recv().is_err());
        server.abort();
    }
    #[tokio::test]
    async fn connect_fails_closed_if_sender_is_unavailable() {
        let (mut client, events, server) = instrument_server(200).await;
        drop(events);
        let error = client.connect().await.expect_err("closed receiver");
        assert!(
            error
                .to_string()
                .contains("instrument event receiver closed")
        );
        assert!(!client.is_connected());
        client.sender = None;
        let error = client.connect().await.expect_err("missing sender");
        assert!(error.to_string().contains("sender not installed"));
        assert!(!client.is_connected());
        server.abort();
    }
    fn subscribe() -> SubscribeQuotes {
        SubscribeQuotes::new(
            InstrumentId::from("USD/JPY.SAXO"),
            Some(ClientId::from("SAXO")),
            None,
            UUID4::new(),
            0.into(),
            None,
            None,
        )
    }
    fn unsubscribe() -> UnsubscribeQuotes {
        UnsubscribeQuotes::new(
            InstrumentId::from("USD/JPY.SAXO"),
            Some(ClientId::from("SAXO")),
            None,
            UUID4::new(),
            0.into(),
            None,
            None,
        )
    }
    #[tokio::test]
    async fn tradable_quote_emits_prices_and_sizes_then_unsubscribes() {
        let (mut client, mut events, mut requests, server_task) = setup(include_str!(
            "../tests/fixtures/get_infoprices_Tradable.json"
        ))
        .await;
        client.subscribe_quotes(subscribe()).expect("subscribe");
        match timeout(Duration::from_secs(2), events.recv())
            .await
            .expect("tick timeout")
            .expect("tick")
        {
            DataEvent::Data(Data::Quote(q)) => {
                assert_eq!(q.bid_price, Price::from("146.799"));
                assert_eq!(q.ask_price, Price::from("146.819"));
                assert_eq!(q.bid_size, Quantity::from(2000000));
                assert_eq!(q.ask_size, Quantity::from(3000000));
            }
            other => panic!("unexpected {other:?}"),
        }
        let request = timeout(Duration::from_secs(1), requests.recv())
            .await
            .expect("request timeout")
            .expect("request")
            .1;
        assert!(request.starts_with(
            "GET /trade/v1/infoprices?AssetType=FxSpot&Uic=42&Amount=5000&FieldGroups=Quote "
        ));
        client
            .unsubscribe_quotes(&unsubscribe())
            .expect("unsubscribe");
        while requests.try_recv().is_ok() {}
        assert!(
            timeout(Duration::from_millis(220), requests.recv())
                .await
                .is_err()
        );
        assert!(events.try_recv().is_err());
        server_task.abort();
    }
    #[tokio::test]
    async fn indicative_quote_emits_no_tick() {
        let (mut client, mut events, mut requests, server_task) = setup(include_str!(
            "../tests/fixtures/get_infoprices_Indicative.json"
        ))
        .await;
        client.subscribe_quotes(subscribe()).expect("subscribe");
        timeout(Duration::from_secs(1), requests.recv())
            .await
            .expect("request timeout")
            .expect("request");
        assert!(
            timeout(Duration::from_millis(120), events.recv())
                .await
                .is_err()
        );
        client.stop().expect("stop");
        server_task.abort();
    }
    #[tokio::test]
    async fn missing_bid_size_emits_no_tick() {
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/get_infoprices_Tradable.json"
        ))
        .expect("fixture");
        value["response"]["Quote"]
            .as_object_mut()
            .expect("quote")
            .remove("BidSize");
        let (mut client, mut events, mut requests, server_task) = setup(&value.to_string()).await;
        client.subscribe_quotes(subscribe()).expect("subscribe");
        timeout(Duration::from_secs(1), requests.recv())
            .await
            .expect("request timeout")
            .expect("request");
        assert!(
            timeout(Duration::from_millis(120), events.recv())
                .await
                .is_err()
        );
        client.disconnect().await.expect("disconnect");
        server_task.abort();
    }
}
