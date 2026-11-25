//! Discord REST client (bot token) + parsing of Donatello-style donation embeds.
#![allow(dead_code)]

use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

const BASE: &str = "https://discord.com/api/v10";

#[derive(Deserialize, Clone, Default)]
pub struct EmbedField {
    #[serde(default)] pub name: String,
    #[serde(default)] pub value: String,
}

#[derive(Deserialize, Clone, Default)]
pub struct EmbedAuthor {
    #[serde(default)] pub name: String,
    #[serde(default)] pub text: String,
}

#[derive(Deserialize, Clone, Default)]
pub struct Embed {
    #[serde(default)] pub title: String,
    #[serde(default)] pub description: String,
    #[serde(default)] pub author: Option<EmbedAuthor>,
    #[serde(default)] pub fields: Vec<EmbedField>,
    #[serde(default)] pub footer: Option<EmbedAuthor>,
}

#[derive(Deserialize, Clone, Default)]
pub struct Author {
    #[serde(default)] pub username: String,
    #[serde(default)] pub global_name: Option<String>,
    #[serde(default)] pub bot: bool,
}

#[derive(Deserialize, Clone, Default)]
pub struct Message {
    #[serde(default)] pub id: String,
    #[serde(default)] pub content: String,
    #[serde(default)] pub webhook_id: Option<String>,
    #[serde(default)] pub author: Option<Author>,
    #[serde(default)] pub embeds: Vec<Embed>,
}

pub enum FetchError {
    RateLimit(u64),
    Other(String),
}

fn body_utf8(r: ureq::Response) -> Result<String, String> {
    use std::io::Read;
    let mut buf = Vec::new();
    r.into_reader().read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn request(url: &str, token: Option<&str>) -> Result<Value, FetchError> {
    let mut req = ureq::get(url)
        .set("User-Agent", "DiscordBot (http://localhost, 0.1)")
        .timeout(Duration::from_secs(20));
    if let Some(t) = token { req = req.set("Authorization", &format!("Bot {}", t)); }
    match req.call() {
        Ok(r) => {
            let text = body_utf8(r).map_err(FetchError::Other)?;
            serde_json::from_str(&text).map_err(|e| FetchError::Other(e.to_string()))
        }
        Err(ureq::Error::Status(code, r)) => {
            let body = body_utf8(r).unwrap_or_default();
            if code == 429 {
                let retry = serde_json::from_str::<Value>(&body).ok()
                    .and_then(|v| v.get("retry_after").and_then(Value::as_f64))
                    .unwrap_or(5.0);
                return Err(FetchError::RateLimit(retry.ceil() as u64 + 1));
            }
            let snippet: String = body.chars().take(160).collect();
            let hint = match code {
                401 => "invalid bot token",
                403 => "bot has no access to this channel",
                404 => "channel not found (check the channel ID)",
                _ => "",
            };
            Err(FetchError::Other(format!("HTTP {} {} {}", code, hint, snippet).trim().to_string()))
        }
        Err(e) => Err(FetchError::Other(e.to_string())),
    }
}

fn get(url: &str, token: &str) -> Result<Value, FetchError> { request(url, Some(token)) }

/// Latest messages, newest first. `after` asks for messages newer than that id.
pub fn messages(channel: &str, token: &str, after: Option<&str>) -> Result<Vec<Message>, FetchError> {
    let url = match after {
        Some(id) => format!("{}/channels/{}/messages?after={}&limit=100", BASE, channel, id),
        None => format!("{}/channels/{}/messages?limit=50", BASE, channel),
    };
    let v = get(&url, token)?;
    serde_json::from_value(v).map_err(|e| FetchError::Other(e.to_string()))
}

pub fn channel_name(channel: &str, token: &str) -> Result<String, FetchError> {
    let v = get(&format!("{}/channels/{}", BASE, channel), token)?;
    Ok(v.get("name").and_then(Value::as_str).unwrap_or("").to_string())
}

/// Incoming webhook URL -> the channel it posts into. Needs no bot auth.
pub fn webhook_channel(url: &str) -> Result<String, FetchError> {
    let (id, token) = webhook_parts(url)
        .ok_or_else(|| FetchError::Other("це не Discord webhook URL".into()))?;
    let v = request(&format!("{}/webhooks/{}/{}", BASE, id, token), None)?;
    v.get("channel_id").and_then(Value::as_str).map(|s| s.to_string())
        .ok_or_else(|| FetchError::Other("у вебхуку немає channel_id".into()))
}

fn webhook_parts(url: &str) -> Option<(String, String)> {
    let marker = "/webhooks/";
    let rest = &url[url.find(marker)? + marker.len()..];
    let (id, token) = rest.split_once('/')?;
    let token = token.split(['?', '#', '/']).next().unwrap_or("").trim();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) || token.is_empty() { return None; }
    Some((id.to_string(), token.to_string()))
}

/* ------------------------------------------------------------------ parsing */

#[derive(Clone, Debug)]
pub struct Donation {
    pub donor: String,
    pub amount: f64,
    pub message: String,
}

/// One donation = the first embed that has a readable amount.
pub fn parse_donation(m: &Message) -> Option<Donation> { parse(m, true) }

/// Same, for a channel fed by other services through a webhook: no Donatello marker required.
pub fn parse_any_donation(m: &Message) -> Option<Donation> { parse(m, false) }

/// Webhook source: other services only. Donatello messages are handled by the Donatello source.
pub fn parse_webhook_donation(m: &Message) -> Option<Donation> {
    if is_donatello(m) { return None; }
    parse_any_donation(m)
}

fn parse(m: &Message, require_donatello: bool) -> Option<Donation> {
    if require_donatello && !is_donatello(m) { return None; }
    if let Some(d) = parse_embed(m) { return Some(d); }
    // Donatello always sends an embed; other webhook services may send plain text.
    if require_donatello { return None; }
    parse_text(&m.content)
}

fn parse_embed(m: &Message) -> Option<Donation> {
    let embed = m.embeds.iter().find(|e| amount_from_embed(e).is_some())
        .or_else(|| m.embeds.first())?;
    let amount = amount_from_embed(embed)?;
    if !amount.is_finite() || amount <= 0.0 { return None; }

    let mut donor = donor_from_fields(embed);
    if donor.is_empty() {
        if let Some(a) = &embed.author {
            let n = a.name.trim();
            if !n.is_empty() && !n.to_lowercase().contains("donatello") { donor = n.to_string(); }
        }
    }
    if donor.is_empty() {
        if let Some(a) = &m.author {
            if !a.bot && m.webhook_id.is_none() {
                donor = a.global_name.clone().filter(|s| !s.trim().is_empty())
                    .unwrap_or_else(|| a.username.clone());
            }
        }
    }
    if !plausible_donor(&donor) { return None; }

    let message = message_from_fields(embed)
        .unwrap_or_else(|| message_from_description(&embed.description));
    Some(Donation { donor: donor.trim().to_string(), amount, message })
}

/// Donatello signs the message as the author, the embed title or the footer.
/// The donor's own text is not used for this check.
pub fn is_donatello(m: &Message) -> bool {
    let mut hay = String::new();
    if let Some(a) = &m.author {
        hay.push_str(&a.username);
        hay.push(' ');
        if let Some(g) = &a.global_name { hay.push_str(g); hay.push(' '); }
    }
    for e in &m.embeds {
        hay.push_str(&e.title);
        hay.push(' ');
        if let Some(a) = &e.author { hay.push_str(&a.name); hay.push(' '); }
        if let Some(f) = &e.footer { hay.push_str(if f.text.is_empty() { &f.name } else { &f.text }); hay.push(' '); }
    }
    hay.to_lowercase().contains("donatello")
}

/// A real donor name has letters; a field holding only "₴100" or "123" is not one.
fn plausible_donor(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().any(|c| c.is_alphabetic()) && !has_currency(t)
}

/* --------------------------------------------------- plain-text donations */

fn clean_md(s: &str) -> String {
    s.chars().filter(|c| !matches!(c, '*' | '_' | '~' | '`' | '>')).collect::<String>().trim().to_string()
}

fn find_currency(s: &str) -> Option<(usize, usize)> {
    let lower = s.to_lowercase();
    let mut best: Option<(usize, usize)> = None;
    for m in ["₴", "$", "€", "грн", "uah", "usd", "eur"] {
        if let Some(pos) = lower.find(m) {
            if best.is_none_or(|(p, _)| pos < p) { best = Some((pos, m.len())); }
        }
    }
    best
}

fn donor_before(s: &str) -> String {
    let s = clean_md(s);
    let segment = s.split(['–', '—', '|', ':'])
        .map(str::trim)
        .rfind(|x| !x.is_empty())
        .unwrap_or("");
    let start = segment.char_indices()
        .find(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '.')
        .map(|(i, _)| i)
        .unwrap_or(segment.len());
    segment[start..]
        .trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '.' && c != ' ')
        .trim()
        .to_string()
}

fn donor_after(s: &str) -> Option<String> {
    let lower = s.to_lowercase();
    for marker in ["від ", "from "] {
        if let Some(i) = lower.find(marker) {
            let rest = &s[i + marker.len()..];
            let name = rest.split(['–', '—', '-', '|', ':', ',']).next().unwrap_or(rest);
            let name = donor_before(name);
            if plausible_donor(&name) { return Some(name); }
        }
    }
    None
}

/// Webhook services do not always send embeds. Read labelled lines, then the common
/// "<donor> – ₴100 <goal>" one-liner (the next lines are the donor's message).
fn parse_text(content: &str) -> Option<Donation> {
    let text = content.trim();
    if text.is_empty() { return None; }

    let mut donor = String::new();
    let mut amount = None;
    let mut message = String::new();
    for line in text.lines() {
        if let Some((k, v)) = line.split_once(':') {
            let key = norm(k);
            let val = v.trim();
            if val.is_empty() { continue; }
            if is_message_label(&key) { message = clean_md(val); }
            else if is_amount_label(&key) { if let Some(a) = parse_number(val) { amount = Some(a); } }
            else if is_donor_label(&key) { donor = clean_md(val); }
        }
    }
    if let Some(a) = amount {
        if a.is_finite() && a > 0.0 && plausible_donor(&donor) {
            return Some(Donation { donor, amount: a, message });
        }
    }

    let (pos, len) = find_currency(text)?;
    let pre = &text[..pos];
    let (amount, amount_start) = if let Some(v) = leading_number(&text[pos + len..]) {
        (v, None)
    } else {
        match trailing_number_span(pre) { Some((v, st)) => (v, Some(st)), None => return None }
    };
    if !amount.is_finite() || amount <= 0.0 { return None; }

    let before_raw = match amount_start { Some(st) => pre[..st].trim(), None => pre.trim() };
    let before = before_raw.trim_end_matches(['-', '–', '—', ':', '|', '·']).trim();
    let mut donor = donor_before(before);
    if !plausible_donor(&donor) {
        donor = donor_after(&text[pos + len..]).unwrap_or_default();
    }
    if !plausible_donor(&donor) { return None; }


    let after = &text[pos + len..];
    let mut lines = after.lines();
    let first_tail = lines.next().unwrap_or("").trim();
    let rest = lines.collect::<Vec<_>>().join("\n");
    let message = if !rest.trim().is_empty() {
        clean_md(rest.trim())
    } else {
        clean_md(first_tail.trim_matches(|c: char| c.is_whitespace() || ['–', '—', '-', ':', '|', '·', ','].contains(&c)))
    };
    Some(Donation { donor: donor.trim().to_string(), amount, message })
}

fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric() || ('\u{0400}'..='\u{04ff}').contains(c)).collect()
}

fn is_amount_label(n: &str) -> bool {
    ["amount", "сума", "сумм", "ціна", "цена", "price", "total"]
        .iter().any(|k| n.contains(k))
}
fn is_donor_label(n: &str) -> bool {
    if is_message_label(n) || is_amount_label(n) { return false; }
    ["donor", "донатер", "донатвід", "від", "from", "name", "ім", "им", "user", "нік", "ник"]
        .iter().any(|k| n.contains(k))
}
fn is_message_label(n: &str) -> bool {
    ["message", "повідом", "сообщ", "comment", "коментар", "коммент", "text", "текст"]
        .iter().any(|k| n.contains(k))
}

fn has_currency(s: &str) -> bool {
    let l = s.to_lowercase();
    ["₴", "$", "€", "грн", "uah", "usd", "eur"].iter().any(|m| l.contains(m))
}

/// First currency-marked number in a string, preferring the text right after the sign.
pub fn amount_in(s: &str) -> Option<f64> {
    let (pos, len) = find_currency(s)?;
    if let Some(v) = leading_number(&s[pos + len..]) { return Some(v); }
    trailing_number(&s[..pos])
}

fn amount_from_embed(e: &Embed) -> Option<f64> {
    for f in &e.fields {
        if is_amount_label(&norm(&f.name)) {
            if let Some(v) = parse_number(&f.value) { return Some(v); }
        }
    }
    for f in &e.fields {
        if has_currency(&f.value) {
            if let Some(v) = amount_in(&f.value) { return Some(v); }
        }
    }
    for s in [&e.title, &e.description] {
        if has_currency(s) {
            if let Some(v) = amount_in(s) { return Some(v); }
        }
    }
    None
}

fn donor_from_fields(e: &Embed) -> String {
    for f in &e.fields {
        if is_donor_label(&norm(&f.name)) && !f.value.trim().is_empty() {
            return f.value.trim().to_string();
        }
    }
    for f in &e.fields {
        let n = norm(&f.name);
        if is_amount_label(&n) || has_currency(&f.value) || f.value.trim().is_empty() { continue; }
        return f.value.trim().to_string();
    }
    String::new()
}

fn message_from_fields(e: &Embed) -> Option<String> {
    for f in &e.fields {
        if is_message_label(&norm(&f.name)) && !f.value.trim().is_empty() {
            return Some(f.value.trim().to_string());
        }
    }
    None
}

/// Donatello puts the fundraiser line, a blank line, then the donor's message.
fn message_from_description(desc: &str) -> String {
    let desc = desc.trim();
    if desc.is_empty() { return String::new(); }
    let lines: Vec<&str> = desc.lines().collect();
    let start = lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(lines.len());
    if let Some(blank) = lines[start..].iter().position(|l| l.trim().is_empty()) {
        let rest = lines[start + blank + 1..].join("\n").trim().to_string();
        if !rest.is_empty() { return rest; }
    }
    let first = lines.get(start).map(|l| l.trim()).unwrap_or("");
    if first.chars().next().is_some_and(|c| ['⭕', '🎯', '🔴', '🟢', '🎁'].contains(&c)) {
        return lines[start + 1..].join("\n").trim().to_string();
    }
    desc.to_string()
}

fn leading_number(s: &str) -> Option<f64> {
    // only the same line: the next line is the donation message/lot number, not the amount
    let line = s.split(['\n', '\r']).next().unwrap_or(s);
    let t: String = line.trim_start()
        .chars().take_while(|c| c.is_ascii_digit() || *c == ',' || *c == '.' || *c == ' ' || *c == '\u{00A0}')
        .collect();
    parse_number(&t)
}

/// The numeric token(s) right before a currency mark, with the start offset.
/// Only purely numeric tokens are joined (space thousands), so a donor name ending
/// in digits ("Test500 500 UAH") yields 500, not 500500.
fn trailing_number_span(s: &str) -> Option<(f64, usize)> {
    let mut token_end = s.trim_end().len();
    let mut start = token_end;
    let mut parts: Vec<&str> = Vec::new();
    loop {
        while token_end > 0 {
            let (i, ch) = s[..token_end].char_indices().next_back().unwrap();
            if ch.is_whitespace() { token_end = i } else { break }
        }
        if token_end == 0 { break }
        let mut ts = token_end;
        while ts > 0 {
            let (i, ch) = s[..ts].char_indices().next_back().unwrap();
            if ch.is_whitespace() { break } else { ts = i }
        }
        let tok = &s[ts..token_end];
        if !tok.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.') { break }
        parts.push(tok);
        start = ts;
        token_end = ts;
    }
    if parts.is_empty() { return None; }
    let joined: String = parts.iter().rev().cloned().collect();
    parse_number(&joined).map(|v| (v, start))
}

fn trailing_number(s: &str) -> Option<f64> { trailing_number_span(s).map(|(v, _)| v) }

/// "₴100", "1 234,56", "1,234.56" -> f64.
pub fn parse_number(s: &str) -> Option<f64> {
    let cleaned: String = s.chars().filter(|c| c.is_ascii_digit() || *c == ',' || *c == '.').collect();
    if cleaned.is_empty() { return None; }
    if let Some(i) = cleaned.rfind([',', '.']) {
        let frac = &cleaned[i + 1..];
        if !frac.is_empty() && frac.len() <= 2 {
            let int: String = cleaned[..i].chars().filter(|c| c.is_ascii_digit()).collect();
            let int = if int.is_empty() { "0".to_string() } else { int };
            return format!("{}.{}", int, frac).parse().ok();
        }
    }
    let int: String = cleaned.chars().filter(|c| c.is_ascii_digit()).collect();
    if int.is_empty() { None } else { int.parse().ok() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn donatello(fields: Vec<(&str, &str)>, description: &str) -> Message {
        Message {
            id: "1".into(),
            embeds: vec![Embed {
                title: "🎉 Donatello · новий донат".into(),
                description: description.into(),
                fields: fields.into_iter().map(|(n, v)| EmbedField { name: n.into(), value: v.into() }).collect(),
                footer: Some(EmbedAuthor { name: String::new(), text: "Donatello сповіщення".into() }),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn reads_donatello_embed() {
        let m = donatello(vec![("👤", "Тест"), ("💰", "₴100")], "⭕ Збір на щось\n\nПривіт! Це тестовий донат!");
        let d = parse_donation(&m).expect("donation");
        assert_eq!(d.donor, "Тест");
        assert_eq!(d.amount, 100.0);
        assert_eq!(d.message, "Привіт! Це тестовий донат!");
    }

    #[test]
    fn reads_labelled_embed() {
        let m = donatello(vec![("Донатер", "Іван"), ("Сума", "1 234,50 ₴"), ("Повідомлення", "Лот #2")], "");
        let d = parse_donation(&m).unwrap();
        assert_eq!((d.donor.as_str(), d.amount, d.message.as_str()), ("Іван", 1234.5, "Лот #2"));
    }

    #[test]
    fn no_embed_or_no_amount_is_skipped() {
        let plain = Message { id: "2".into(), content: "Тест – 100 ₴".into(), ..Default::default() };
        assert!(parse_donation(&plain).is_none());
        let m = donatello(vec![("👤", "Тест")], "⭕ Збір");
        assert!(parse_donation(&m).is_none());
    }

    #[test]
    fn only_donatello_embeds_count() {
        let mut m = donatello(vec![("Донатер", "Іван"), ("Сума", "₴100")], "");
        m.embeds[0].title = "Other bot".into();
        m.embeds[0].footer = None;
        m.author = Some(Author { username: "OtherBot".into(), ..Default::default() });
        assert!(parse_donation(&m).is_none());
        assert_eq!(parse_any_donation(&m).unwrap().donor, "Іван");
    }

    #[test]
    fn webhook_url_parsing() {
        let url = "https://discord.com/api/webhooks/1574332686154080276/X6HbTc83O1wTT1Ae78ZGVe-3zG6ujO24mCYrc9A0Bdfwv9_1PFaiawq1XOCQjjJly2oS";
        let (id, token) = webhook_parts(url).unwrap();
        assert_eq!(id, "1574332686154080276");
        assert!(token.starts_with("X6Hb"));
        assert!(webhook_parts("https://example.com").is_none());
    }

    #[test]
    fn donor_and_amount_are_validated() {
        // amount present, donor missing
        let m = donatello(vec![("💰", "₴100")], "");
        assert!(parse_donation(&m).is_none());
        // donor is a number, not a name
        let m = donatello(vec![("👤", "123"), ("💰", "₴100")], "");
        assert!(parse_donation(&m).is_none());
    }

    #[test]
    fn plain_text_donations() {
        let m = Message {
            id: "9".into(),
            content: "🧡 **Тест** – 100 ₴ Збір на щось\n*Привіт! Це тестовий донат!*".into(),
            ..Default::default()
        };
        assert!(parse_donation(&m).is_none()); // Donatello stays embed-only
        let d = parse_any_donation(&m).expect("text donation");
        assert_eq!(d.donor, "Тест");
        assert_eq!(d.amount, 100.0);
        assert_eq!(d.message, "Привіт! Це тестовий донат!");

        let m = Message {
            id: "10".into(),
            content: "Донатер: Іван\nСума: 250 ₴\nПовідомлення: Лот 2".into(),
            ..Default::default()
        };
        let d = parse_any_donation(&m).unwrap();
        assert_eq!((d.donor.as_str(), d.amount, d.message.as_str()), ("Іван", 250.0, "Лот 2"));
    }

    #[test]
    fn webhook_source_skips_donatello() {
        let m = donatello(vec![("👤", "Тест"), ("💰", "₴100")], "");
        assert!(parse_donation(&m).is_some());
        assert!(parse_webhook_donation(&m).is_none());

        let m = Message {
            id: "11".into(),
            content: "Володимир Зеленський 5 UAH\nТестовий донат".into(),
            ..Default::default()
        };
        let d = parse_webhook_donation(&m).expect("text donation");
        assert_eq!(d.donor, "Володимир Зеленський");
        assert_eq!(d.amount, 5.0);
        assert_eq!(d.message, "Тестовий донат");
    }

    #[test]
    fn plain_text_amount_before_currency_with_lot_number_on_next_line() {
        let m = Message {
            id: "12".into(),
            content: "Володимир Зеленський 200 UAH\n1".into(),
            ..Default::default()
        };
        let d = parse_webhook_donation(&m).expect("donation");
        assert_eq!(d.donor, "Володимир Зеленський");
        assert_eq!(d.amount, 200.0);
        assert_eq!(d.message, "1");
    }

    #[test]
    fn plain_text_donor_with_digits_and_amount() {
        let m = Message { id: "13".into(), content: "Test500 500 UAH\nTest500".into(), ..Default::default() };
        let d = parse_webhook_donation(&m).expect("donation");
        assert_eq!(d.donor, "Test500");
        assert_eq!(d.amount, 500.0);
        assert_eq!(d.message, "Test500");
    }

    #[test]
    fn plain_text_space_thousands() {
        let m = Message { id: "14".into(), content: "Ivan 1 500 UAH\n-".into(), ..Default::default() };
        let d = parse_webhook_donation(&m).expect("donation");
        assert_eq!(d.donor, "Ivan");
        assert_eq!(d.amount, 1500.0);
    }
    #[test]
    fn amount_forms() {
        assert_eq!(parse_number("₴100"), Some(100.0));
        assert_eq!(parse_number("100 ₴"), Some(100.0));
        assert_eq!(parse_number("1 234,56 ₴"), Some(1234.56));
        assert_eq!(parse_number("1,234.56"), Some(1234.56));
        assert_eq!(parse_number("100,5"), Some(100.5));
        assert_eq!(amount_in("Сума: 250 ₴"), Some(250.0));
    }
}
