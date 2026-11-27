use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub(crate) fn fixture(s: &str) -> serde_json::Value {
    serde_json::from_str::<serde_json::Value>(s).expect("fixture")["response"].clone()
}
pub async fn serve(
    mut respond: impl FnMut(&str, &str, serde_json::Value) -> serde_json::Value + Send + 'static,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("addr"));
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = Vec::new();
            loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.expect("read");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&buf[..end]);
                    let length = header
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|x| x.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    if buf.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            let req = String::from_utf8_lossy(&buf);
            let line = req.lines().next().unwrap_or("");
            let mut parts = line.split_whitespace();
            let method = parts.next().expect("method");
            let path = parts.next().expect("path");
            let raw = req.split_once("\r\n\r\n").expect("headers").1;
            let request = if raw.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::from_str(raw).expect("order body")
            };
            let body = respond(method, path, request);
            let body = body.to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.expect("write");
        }
    });
    (base, task)
}

pub fn market_data(method: &str, path: &str) -> Option<serde_json::Value> {
    let request = format!("{method} {path} ");
    Some(if request.starts_with("GET /port/v1/clients/me ") {
        let mut v = fixture(include_str!("../tests/fixtures/live_client.json"));
        v["ForceOpenDefaultValue"] = false.into();
        v
    } else if request.starts_with("GET /ref/v1/instruments/details/42/FxSpot ") {
        fixture(include_str!(
            "../tests/fixtures/live_USDJPY_instrument.json"
        ))
    } else if request.starts_with("GET /port/v1/accounts/me ") {
        fixture(include_str!(
            "../tests/fixtures/get_port_v1_accounts_me.json"
        ))
    } else if request.starts_with("GET /port/v1/balances/me ") {
        fixture(include_str!(
            "../tests/fixtures/get_port_v1_balances_me.json"
        ))
    } else if request.starts_with("GET /port/v1/positions/me ") {
        fixture(include_str!(
            "../tests/fixtures/get_port_v1_positions_me_0.json"
        ))
    } else if request.starts_with("GET /port/v1/orders/me ") {
        fixture(include_str!(
            "../tests/fixtures/get_port_v1_orders_me_0.json"
        ))
    } else if request.starts_with("GET /trade/v1/infoprices?") {
        fixture(include_str!(
            "../tests/fixtures/get_infoprices_Tradable.json"
        ))
    } else {
        return None;
    })
}

pub async fn server() -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let posts = Arc::new(AtomicUsize::new(0));
    let count = posts.clone();
    let (base, task) = serve(move |method, path, request| {
        if let Some(body) = market_data(method, path) {
            return body;
        }
        if method == "POST" && path == "/trade/v2/orders" {
            assert_eq!(request["BuySell"], "Buy");
            assert_eq!(request["Amount"], 1000);
            assert_eq!(request["OrderType"], "Market");
            assert_eq!(request["Orders"].as_array().expect("stop list").len(), 1);
            assert_eq!(request["Orders"][0]["OrderType"], "Stop");
            count.fetch_add(1, Ordering::SeqCst);
            fixture(include_str!(
                "../tests/fixtures/post_trade_v2_orders_200_with_related.json"
            ))
        } else if method == "GET" && path.starts_with("/cs/v1/audit/orderactivities?") {
            if count.load(Ordering::SeqCst) == 0 {
                serde_json::json!({"Data":[]})
            } else {
                let mut market = fixture(include_str!(
                    "../tests/fixtures/get_orderactivities_FinalFill_Placed_Market.json"
                ));
                let mut stop = fixture(include_str!(
                    "../tests/fixtures/get_orderactivities_Placed_Stop.json"
                ));
                stop["Data"][0]["OrderId"] = "1002".into();
                market["Data"].as_array_mut().expect("history").extend(
                    stop["Data"]
                        .as_array()
                        .expect("stop history")
                        .iter()
                        .cloned(),
                );
                market
            }
        } else {
            panic!("unexpected request: {method} {path}")
        }
    })
    .await;
    (base, posts, task)
}
