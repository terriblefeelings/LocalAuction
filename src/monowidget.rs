//! Monobank jar widget ("дружня банка"): foreign jars can't be read through the
//! personal API. We subscribe to the same public WebSocket the widget uses and also
//! reconcile the public balance every 5 minutes, so a missed alert still becomes a bid.
use crate::{log, now_ms, Ctx};
use serde_json::Value;
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tungstenite::client::ClientRequestBuilder;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error as WsError, Message};

const WS: &str = "wss://send.monobank.ua/api/web/wss";
/// The widget page re-sends `subscribe` every 3 minutes: the server stops pushing
/// alerts once the subscription lease expires. A read timeout gives us a timer.
const TICK: Duration = Duration::from_secs(30);
const RESUB: Duration = Duration::from_secs(120);

pub fn spawn(ctx: Arc<Ctx>) {
    let ws = ctx.clone();
    std::thread::spawn(move || run(ws));
    let bal = ctx.clone();
    std::thread::spawn(move || balance_loop(bal));
}

fn run(ctx: Arc<Ctx>) {
    loop {
        let (on, token, jar) = {
            let c = ctx.cfg.lock().unwrap();
            (c.mono_widget_enabled, c.mono_widget_token.trim().to_string(), c.mono_widget_jar.trim().to_string())
        };
        if !on || token.is_empty() {
            std::thread::sleep(Duration::from_secs(3));
            continue;
        }
        match session(&ctx, &token, &jar) {
            Ok(()) => std::thread::sleep(Duration::from_secs(3)),
            Err(e) => {
                log(&format!("monobank widget: {}", e));
                std::thread::sleep(Duration::from_secs(10));
            }
        }
    }
}

fn session(ctx: &Arc<Ctx>, token: &str, jar: &str) -> Result<(), String> {
    let params = serde_json::json!({"lang":"uk","traceId":format!("{:x}", now_ms()),"qrToken":token});
    let proto = b64(params.to_string().as_bytes());
    let req = ClientRequestBuilder::new(WS.parse().map_err(|e| format!("url: {e}"))?)
        .with_sub_protocol("mono")
        .with_sub_protocol(&proto)
        .with_header("Origin", "https://send.monobank.ua")
        .with_header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36");
    let (mut ws, _) = tungstenite::connect(req).map_err(|e| e.to_string())?;
    if let MaybeTlsStream::Rustls(s) = ws.get_ref() {
        let _ = s.sock.set_read_timeout(Some(TICK));
    }
    // alert ids may restart per connection, so a fresh socket starts a fresh dedup list
    ctx.state.lock().unwrap().seen_widget.clear();
    let sub = serde_json::json!({"c":"subscribe","data":{"event":"/api/web/jar/statement","params":{"qrToken":token}}});
    let sub_text = sub.to_string();
    ws.send(Message::text(sub_text.clone())).map_err(|e| e.to_string())?;
    let mut sub_at = Instant::now();
    log(&format!("monobank widget: listening to jar {}", if jar.is_empty() { "?" } else { jar }));
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => handle(ctx, t.as_str()),
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(WsError::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.to_string()),
        }
        let (on, token_now, jar_now) = {
            let c = ctx.cfg.lock().unwrap();
            (c.mono_widget_enabled, c.mono_widget_token.trim().to_string(), c.mono_widget_jar.trim().to_string())
        };
        if !on || token_now != token || jar_now != jar { return Ok(()); }
        if sub_at.elapsed() >= RESUB {
            ws.send(Message::text(sub_text.clone())).map_err(|e| e.to_string())?;
            sub_at = Instant::now();
            log("monobank widget: subscription renewed");
        }
    }
}

fn handle(ctx: &Arc<Ctx>, text: &str) {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return };
    if v["c"].as_str() != Some("/api/web/jar/statement") { return; }
    let data = &v["data"];
    if data["authorized"].as_bool() == Some(false) {
        log("monobank widget: token expired - update jarToken in settings");
        return;
    }
    let min = ctx.cfg.lock().unwrap().min_bid;
    let Some(items) = data["items"].as_array() else { return };
    let mut changed = false;
    for it in items {
        let id = it["id"].as_str().map(str::to_string)
            .or_else(|| it["id"].as_i64().map(|n| n.to_string()))
            .unwrap_or_default();
        if id.is_empty() { continue; }
        let kop = it["amount"].as_f64().unwrap_or(0.0);
        let uah = kop / 100.0;
        let (donor, lot_text) = split_desc(it["description"].as_str().unwrap_or(""));
        let mut st = ctx.state.lock().unwrap();
        if st.seen_widget.iter().any(|x| x == &id) { continue; }
        st.seen_widget.push(id.clone());
        if st.seen_widget.len() > 2000 { st.seen_widget.remove(0); }
        changed = true;
        if uah < min {
            log(&format!("monobank widget: ignored {:.0} - below minimum {:.0}", uah, min));
            continue;
        }
        match st.auc.intake(&donor, uah, &lot_text, "monobank", "") {
            Ok(_) => {
                st.widget_ws_sum += kop.round() as i64;
                log(&format!("monobank widget: {:.0} from {} (id {})", uah, donor, id));
            }
            Err(e) => log(&format!("monobank widget: {:.0} from {} skipped - {}", uah, donor, e)),
        }
    }
    if changed { ctx.save_state(); }
}

/// Balance reconciliation: catches alerts the socket missed (offline, competing
/// widget page). WS items are added to `widget_ws_sum` first, so nothing counts twice.
fn balance_loop(ctx: Arc<Ctx>) {
    loop {
        // /bank/jar is rate limited to roughly one request per five minutes per IP
        std::thread::sleep(Duration::from_secs(300));
        let (on, jar) = {
            let c = ctx.cfg.lock().unwrap();
            (c.mono_widget_enabled, c.mono_widget_jar.trim().to_string())
        };
        if !on || jar.is_empty() { continue; }
        let Ok((_title, cur)) = crate::mono::public_jar(&jar) else { continue };
        let min = ctx.cfg.lock().unwrap().min_bid;
        let mut st = ctx.state.lock().unwrap();
        if st.widget_jar != jar {
            st.widget_jar = jar.clone();
            st.widget_seen = 0;
            st.widget_balance = cur;
            st.widget_ws_sum = 0;
            drop(st);
            log(&format!("monobank widget: balance baseline {:.0} for {}", cur as f64 / 100.0, jar));
            continue;
        }
        if st.widget_seen == 0 {
            st.widget_seen = 1;
            st.widget_balance = cur;
            st.widget_ws_sum = 0;
            drop(st);
            log(&format!("monobank widget: balance baseline {:.0} for {}", cur as f64 / 100.0, jar));
            continue;
        }
        let expected = st.widget_balance + st.widget_ws_sum;
        let mut changed = false;
        if cur > expected {
            let uah = (cur - expected) as f64 / 100.0;
            if uah >= min && st.auc.intake("Monobank", uah, "", "monobank", "").is_ok() {
                log(&format!("monobank widget: +{:.0} by balance (no alert)", uah));
                changed = true;
            }
        }
        st.widget_balance = cur;
        st.widget_ws_sum = 0;
        drop(st);
        if changed { ctx.save_state(); }
    }
}

/// The widget alert shows "Від: Ілля Г." — that is the donor, not the lot text.
fn split_desc(s: &str) -> (String, String) {
    let t = s.trim();
    let low = t.to_lowercase();
    for p in ["від:", "від ", "from:", "from "] {
        if low.starts_with(p) {
            let name = t[p.len()..].trim().to_string();
            if !name.is_empty() { return (name, String::new()); }
        }
    }
    ("Monobank".to_string(), t.to_string())
}

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((chunk[0] as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 { out.push(T[(n >> 6) as usize & 63] as char); }
        if chunk.len() > 2 { out.push(T[n as usize & 63] as char); }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{b64, split_desc};
    #[test]
    fn base64_without_padding() {
        assert_eq!(b64(b"hello"), "aGVsbG8");
        assert_eq!(b64(b"{}"), "e30");
        assert_eq!(b64(b"a"), "YQ");
    }
    #[test]
    fn desc_splits_donor() {
        assert_eq!(split_desc("Від: Ілля Г."), ("Ілля Г.".to_string(), String::new()));
        assert_eq!(split_desc("From: Ivan"), ("Ivan".to_string(), String::new()));
        assert_eq!(split_desc("на піцу"), ("Monobank".to_string(), "на піцу".to_string()));
    }
}
