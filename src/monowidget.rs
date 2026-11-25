//! Monobank jar widget ("дружня банка"): foreign jars can't be read through the
//! personal API, so we subscribe to the same public WebSocket the widget uses and
//! turn alert items (amount + "Від: name") into bids.
use crate::{log, now_ms, Ctx};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use tungstenite::client::ClientRequestBuilder;
use tungstenite::Message;

const WS: &str = "wss://send.monobank.ua/api/web/wss";

pub fn spawn(ctx: Arc<Ctx>) {
    std::thread::spawn(move || run(ctx));
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
    let sub = serde_json::json!({"c":"subscribe","data":{"event":"/api/web/jar/statement","params":{"qrToken":token}}});
    ws.send(Message::text(sub.to_string())).map_err(|e| e.to_string())?;
    log(&format!("monobank widget: listening to jar {}", if jar.is_empty() { "?" } else { jar }));
    loop {
        match ws.read().map_err(|e| e.to_string())? {
            Message::Text(t) => handle(ctx, t.as_str()),
            Message::Close(_) => return Ok(()),
            _ => {}
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
        let uah = it["amount"].as_f64().unwrap_or(0.0) / 100.0;
        let (donor, lot_text) = split_desc(it["description"].as_str().unwrap_or(""));
        let mut st = ctx.state.lock().unwrap();
        if st.seen_widget.iter().any(|x| x == &id) { continue; }
        st.seen_widget.push(id);
        if st.seen_widget.len() > 2000 { st.seen_widget.remove(0); }
        changed = true;
        if uah < min {
            log(&format!("monobank widget: ignored {:.0} - below minimum {:.0}", uah, min));
            continue;
        }
        if st.auc.intake(&donor, uah, &lot_text, "monobank", "").is_ok() {
            log(&format!("monobank widget: {:.0} from {}", uah, donor));
        }
    }
    if changed { ctx.save_state(); }
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
