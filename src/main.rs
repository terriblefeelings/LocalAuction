//! Standalone local auction driven by donation embeds in a private Discord channel.
//! The auction page is served on localhost; settings live in a small desktop window.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod auction;
mod bot;
mod discord;
mod icon;
mod mono;
mod monowidget;
mod update;

use auction::{AuctionSettings, AuctionState, Bid, Lot};
use discord::FetchError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tiny_http::{Header, Method, Request, Response, Server};

const INDEX_HTML: &str = include_str!("../public/index.html");
const AUC_JS: &str = include_str!("../public/auc.js");
const AUC_CSS: &str = include_str!("../public/auc.css");
const SETTINGS_HTML: &str = include_str!("../public/settings.html");
const RAT_SVG: &str = include_str!("../public/rat.svg");
const AUCTION_SVG: &str = include_str!("../public/auction.svg");
const ROULETTE_SVG: &str = include_str!("../public/roulette.svg");
const LUMASIK_PNG: &[u8] = include_bytes!("../public/lumasik.png");
const INFO_JPG: &[u8] = include_bytes!("../public/info.jpg");
const NERD_WOFF2: &[u8] = include_bytes!("../public/nerd.woff2");
const SOTOCHKA_JPG: &[u8] = include_bytes!("../public/sotochka.jpg");
const LUMA: [&[u8]; 17] = [
    include_bytes!("../public/luma/luma1.mp3"),
    include_bytes!("../public/luma/luma2.mp3"),
    include_bytes!("../public/luma/luma3.mp3"),
    include_bytes!("../public/luma/luma4.mp3"),
    include_bytes!("../public/luma/luma5.mp3"),
    include_bytes!("../public/luma/luma6.mp3"),
    include_bytes!("../public/luma/luma7.mp3"),
    include_bytes!("../public/luma/luma8.mp3"),
    include_bytes!("../public/luma/luma9.mp3"),
    include_bytes!("../public/luma/luma10.mp3"),
    include_bytes!("../public/luma/luma11.mp3"),
    include_bytes!("../public/luma/luma12.mp3"),
    include_bytes!("../public/luma/luma13.mp3"),
    include_bytes!("../public/luma/luma14.mp3"),
    include_bytes!("../public/luma/luma15.mp3"),
    include_bytes!("../public/luma/luma16.mp3"),
    include_bytes!("../public/luma/luma17.mp3"),
];

const DEFAULT_PORT: u16 = 48974;
const FIRST_RUN_WINDOW_MS: i64 = 120_000;

static LOG_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static EMPTY_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn set_log_path(path: PathBuf) {
    if std::fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    *LOG_PATH.lock().unwrap() = Some(path);
}

fn log(msg: &str) {
    use std::io::Write;
    let line = format!("[{}] {}\n", now_ms() / 1000, msg);
    print!("{}", line);
    if let Some(path) = LOG_PATH.lock().unwrap().as_ref() {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/* ------------------------------------------------------------------ store */

#[derive(Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
struct Config {
    #[serde(default = "d_true")] donatello_enabled: bool,
    channel: String,
    min_bid: f64,
    zbiruk: bool,
    poll_seconds: u64,
    port: u16,
    webhook_enabled: bool,
    webhook_url: String,
    monobank_enabled: bool,
    monobank_token: String,
    monobank_jar: String,
    mono_widget_enabled: bool,
    mono_widget_jar: String,
    mono_widget_token: String,
}
fn d_true() -> bool { true }
impl Default for Config {
    fn default() -> Self {
        Config {
            donatello_enabled: true,
            channel: String::new(),
            min_bid: 10.0,
            zbiruk: false,
            poll_seconds: 6,
            port: DEFAULT_PORT,
            webhook_enabled: false,
            webhook_url: String::new(),
            monobank_enabled: false,
            monobank_token: String::new(),
            monobank_jar: String::new(),
            mono_widget_enabled: false,
            mono_widget_jar: String::new(),
            mono_widget_token: String::new(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(default, rename_all = "camelCase")]
struct State {
    auc: AuctionState,
    last_msg_id: String,
    webhook_last_msg_id: String,
    webhook_channel: String,
    mono_account: String,
    mono_account_jar: String,
    mono_last_ts: i64,
    seen_widget: Vec<String>,
    widget_jar: String,
    widget_seen: u8,
    widget_balance: i64,
    widget_ws_sum: i64,
    seen_tx: Vec<String>,
    seen_webhook: Vec<String>,
    seen_mono: Vec<String>,
}

struct Ctx { cfg: Mutex<Config>, state: Mutex<State>, base: PathBuf, upd: Mutex<serde_json::Value> }

impl Ctx {
    fn load() -> Self {
        let base = std::env::var_os("LOCALAUCTION_DATA").map(PathBuf::from)
            .or_else(|| std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("LocalAuction")))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let _ = std::fs::create_dir_all(&base);
        let cfg = std::fs::read_to_string(base.join("config.json")).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let mut state: State = std::fs::read_to_string(base.join("state.json")).ok()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        // no points source exists in this app
        if state.auc.settings.source == "points" { state.auc.settings.source = "donations".into(); }
        // redemption-id dedup is legacy (Twitch); our sources dedup their own message ids
        state.auc.seen.clear();
        Ctx { cfg: Mutex::new(cfg), state: Mutex::new(state), base, upd: Mutex::new(json!({"checked": false})) }
    }
    fn save_cfg(&self) { save_json(&self.base.join("config.json"), &*self.cfg.lock().unwrap()); }
    fn save_state(&self) { save_json(&self.base.join("state.json"), &*self.state.lock().unwrap()); }
}

fn save_json<T: Serialize>(path: &std::path::Path, data: &T) {
    if let Ok(text) = serde_json::to_string_pretty(data) {
        let _ = std::fs::write(path, text);
    }
}

/* ------------------------------------------------------------------ discord */

fn clean_channel(s: &str) -> String {
    let s = s.trim().trim_end_matches('/');
    let last = s.rsplit('/').next().unwrap_or(s);
    last.chars().filter(|c| c.is_ascii_digit()).collect()
}

fn clean_jar(s: &str) -> String {
    let s = s.trim();
    let s = s.rsplit('/').next().unwrap_or(s);
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}

fn now_secs() -> i64 { now_ms() / 1000 }

fn id_num(id: &str) -> u64 {
    id.trim().parse().unwrap_or(0)
}

/// Discord snowflake -> unix ms.
fn id_ms(id: &str) -> i64 {
    ((id_num(id) >> 22) as i64) + 1_420_070_400_000
}

fn reset_cursor(st: &mut State) {
    st.last_msg_id.clear();
}

fn reset_webhook(st: &mut State) {
    st.webhook_channel.clear();
    st.webhook_last_msg_id.clear();
}

fn reset_mono(st: &mut State) {
    st.mono_account.clear();
    st.mono_account_jar.clear();
    st.mono_last_ts = 0;
}

/// New embed messages become bids. One donation = one message; text-only messages are ignored.
#[allow(clippy::too_many_arguments)]
fn ingest_messages(
    auc: &mut AuctionState,
    seen: &mut Vec<String>,
    cursor: &mut String,
    msgs: Vec<discord::Message>,
    min_bid: f64,
    zbiruk: bool,
    first_run: bool,
    strict: bool,
    source: &str,
) -> Vec<String> {
    let mut msgs = msgs;
    msgs.sort_by_key(|m| id_num(&m.id));
    let cutoff = now_ms() - FIRST_RUN_WINDOW_MS;
    let mut added = Vec::new();

    for m in msgs {
        if id_num(&m.id) == 0 { continue; }
        let id = m.id.clone();
        let within_window = !first_run || id_ms(&id) >= cutoff;

        if within_window && !seen.iter().any(|x| x == &id) {
            seen.push(id.clone());
            if seen.len() > 2000 { seen.remove(0); }

            let parsed = if strict {
                if m.embeds.is_empty() { None } else { discord::parse_donation(&m) }
            } else if source == "webhook" {
                discord::parse_webhook_donation(&m)
            } else {
                discord::parse_any_donation(&m)
            };
            match parsed {
                Some(d) => {
                    if d.amount >= min_bid {
                        let (sign, text) = auction::split_sign(&d.message, zbiruk);
                        let amount = d.amount * sign as f64;
                        if auc.intake(&d.donor, amount, &text, source, "").is_ok() {
                            added.push(format!("{}: {:.0} from {} ({})", source, amount, d.donor, text.trim()));
                        }
                    } else {
                        log(&format!(
                            "{}: ignored {:.0} from {} — below minimum {:.0}",
                            source, d.amount, d.donor, min_bid
                        ));
                    }
                }
                None => {
                    if strict && !m.embeds.is_empty() {
                        log(&format!("{}: could not read a donation from message {}", source, id));
                    }
                }
            }
        }

        if id_num(&id) > id_num(cursor) { *cursor = id; }
    }
    added
}

fn warn_empty(msgs: &[discord::Message]) {
    if !msgs.is_empty()
        && msgs.iter().all(|m| m.content.is_empty() && m.embeds.is_empty())
        && !EMPTY_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        log("discord: channel messages are empty - enable the Message Content intent for the bot");
    }
}

fn poll_donatello(ctx: &Ctx, cfg: &Config) -> Result<Vec<String>, FetchError> {
    let channel = clean_channel(&cfg.channel);
    let (after, first_run) = {
        let st = ctx.state.lock().unwrap();
        (if st.last_msg_id.is_empty() { None } else { Some(st.last_msg_id.clone()) }, st.last_msg_id.is_empty())
    };

    let msgs = discord::messages(&channel, bot::token(), after.as_deref())?;
    warn_empty(&msgs);
    let (added, moved) = {
        let mut st = ctx.state.lock().unwrap();
        let State { auc, seen_tx, last_msg_id, .. } = &mut *st;
        let before = last_msg_id.clone();
        let added = ingest_messages(auc, seen_tx, last_msg_id, msgs, cfg.min_bid, cfg.zbiruk, first_run, true, "discord");
        (added, *last_msg_id != before)
    };
    if moved { ctx.save_state(); }
    Ok(added)
}

fn poll_webhook(ctx: &Ctx, cfg: &Config) -> Result<Vec<String>, FetchError> {
    let url = cfg.webhook_url.trim().to_string();
    let cached = { let st = ctx.state.lock().unwrap(); st.webhook_channel.clone() };
    let channel = if cached.is_empty() {
        let ch = discord::webhook_channel(&url)?;
        let mut st = ctx.state.lock().unwrap();
        st.webhook_channel = ch.clone();
        st.webhook_last_msg_id.clear();
        ch
    } else { cached };

    // the webhook source can share a channel with Donatello: it skips Donatello messages
    let (after, first_run) = {
        let st = ctx.state.lock().unwrap();
        (if st.webhook_last_msg_id.is_empty() { None } else { Some(st.webhook_last_msg_id.clone()) }, st.webhook_last_msg_id.is_empty())
    };
    let msgs = discord::messages(&channel, bot::token(), after.as_deref())?;
    warn_empty(&msgs);
    let (added, moved) = {
        let mut st = ctx.state.lock().unwrap();
        let State { auc, seen_webhook, webhook_last_msg_id, .. } = &mut *st;
        let before = webhook_last_msg_id.clone();
        let added = ingest_messages(auc, seen_webhook, webhook_last_msg_id, msgs, cfg.min_bid, cfg.zbiruk, first_run, false, "webhook");
        (added, *webhook_last_msg_id != before)
    };
    if moved { ctx.save_state(); }
    Ok(added)
}

fn ingest_mono(st: &mut State, items: Vec<mono::StatementItem>, min_bid: f64, zbiruk: bool) -> Vec<String> {
    let mut items = items;
    items.sort_by_key(|t| t.time);
    let mut added = Vec::new();
    let State { auc, seen_mono, .. } = st;
    for tx in items {
        if tx.amount <= 0 || seen_mono.iter().any(|x| x == &tx.id) { continue; }
        seen_mono.push(tx.id.clone());
        if seen_mono.len() > 2000 { seen_mono.remove(0); }
        let uah = tx.amount as f64 / 100.0;
        if uah < min_bid {
            let who = tx.counter_name.as_deref().filter(|s| !s.trim().is_empty())
                .or(tx.description.as_deref()).unwrap_or("-");
            log(&format!("mono: ignored {:.0} from {} - below minimum {:.0}", uah, who, min_bid));
            continue;
        }
        let donor = tx.counter_name.clone().filter(|s| !s.trim().is_empty())
            .or_else(|| tx.description.clone().filter(|s| !s.trim().is_empty()))
            .unwrap_or_else(|| "Анонім".into());
        let msg = tx.comment.clone().unwrap_or_default();
        let (sign, text) = crate::auction::split_sign(&msg, zbiruk);
        let signed = uah * sign as f64;
        if auc.intake(&donor, signed, &text, "monobank", "").is_ok() {
            added.push(format!("monobank: {:.0} from {} ({})", signed, donor, text.trim()));
        }
    }
    added
}

fn poll_monobank(ctx: &Ctx, cfg: &Config) -> Result<Vec<String>, String> {
    let token = cfg.monobank_token.trim();
    let jar = clean_jar(&cfg.monobank_jar);
    let (account, account_jar) = {
        let st = ctx.state.lock().unwrap();
        (st.mono_account.clone(), st.mono_account_jar.clone())
    };
    let account = if !account.is_empty() && account_jar == jar {
        account
    } else {
        let info = mono::client_info(token)?;
        let found = info.jars.iter()
            .find(|j| j.id == jar || j.send_id.as_deref().map(mono::short_send_id) == Some(jar.clone()))
            .ok_or_else(|| format!("jar {} is not on this token (use the widget mode for foreign jars)", jar))?;
        let mut st = ctx.state.lock().unwrap();
        st.mono_account = found.id.clone();
        st.mono_account_jar = jar.clone();
        st.mono_last_ts = 0;
        let id = found.id.clone();
        drop(st);
        id
    };
    let to = now_secs();
    let from = {
        let st = ctx.state.lock().unwrap();
        if st.mono_last_ts > 0 { (st.mono_last_ts - 5).min(to - 1) } else { to - 120 }
    };
    if to <= from { return Ok(Vec::new()); }

    let items = mono::statement(&account, token, from, to)?;
    let added = {
        let mut st = ctx.state.lock().unwrap();
        let a = ingest_mono(&mut st, items, cfg.min_bid, cfg.zbiruk);
        st.mono_last_ts = to;
        a
    };
    ctx.save_state();
    Ok(added)
}

fn err_text(e: &FetchError) -> String {
    match e {
        FetchError::RateLimit(s) => format!("перевищено ліміт, повтор через {}с", s),
        FetchError::Other(m) => m.clone(),
    }
}

/// Open a URL in the user's default browser (reliable, unlike explorer.exe with URLs).
fn open_url(url: &str) {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    let op: Vec<u16> = "open\0".encode_utf16().collect();
    let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { ShellExecuteW(std::ptr::null_mut(), op.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1); }
}

fn run_poller(ctx: Arc<Ctx>) {
    if bot::token().is_empty() { log("discord: no bot token in this build - channel sources disabled"); }
    let mut err_discord = String::new();
    let mut err_webhook = String::new();
    let mut err_mono = String::new();
    let mut last_mono = 0i64;

    loop {
        let cfg = ctx.cfg.lock().unwrap().clone();
        let mut wait = cfg.poll_seconds.clamp(3, 300);

        if cfg.donatello_enabled && !clean_channel(&cfg.channel).is_empty() && !bot::token().is_empty() {
            match poll_donatello(&ctx, &cfg) {
                Ok(added) => {
                    for line in added { log(&format!("bid: {}", line)); }
                    if !err_discord.is_empty() { err_discord.clear(); log("discord: ok"); }
                }
                Err(FetchError::RateLimit(secs)) => {
                    wait = wait.max(secs);
                    let e = err_text(&FetchError::RateLimit(secs));
                    if e != err_discord { log(&format!("discord: {}", e)); err_discord = e; }
                }
                Err(e) => {
                    let text = err_text(&e);
                    if text != err_discord { log(&format!("discord: {}", text)); err_discord = text; }
                }
            }
        }

        if cfg.webhook_enabled && !cfg.webhook_url.trim().is_empty() && !bot::token().is_empty() {
            match poll_webhook(&ctx, &cfg) {
                Ok(added) => {
                    for line in added { log(&format!("bid: {}", line)); }
                    if !err_webhook.is_empty() { err_webhook.clear(); log("webhook: ok"); }
                }
                Err(FetchError::RateLimit(secs)) => {
                    wait = wait.max(secs);
                    let e = err_text(&FetchError::RateLimit(secs));
                    if e != err_webhook { log(&format!("webhook: {}", e)); err_webhook = e; }
                }
                Err(e) => {
                    let text = err_text(&e);
                    if text != err_webhook { log(&format!("webhook: {}", text)); err_webhook = text; }
                }
            }
        }

        let now = now_secs();
        if cfg.monobank_enabled && !cfg.monobank_token.trim().is_empty()
            && !clean_jar(&cfg.monobank_jar).is_empty() && now - last_mono >= 60
        {
            last_mono = now;
            match poll_monobank(&ctx, &cfg) {
                Ok(added) => {
                    for line in added { log(&format!("bid: {}", line)); }
                    if !err_mono.is_empty() { err_mono.clear(); log("monobank: ok"); }
                }
                Err(e) => {
                    if e != err_mono { log(&format!("monobank: {}", e)); err_mono = e; }
                }
            }
        }

        std::thread::sleep(Duration::from_secs(wait));
    }
}

/* ------------------------------------------------------------------ json */

fn state_value(st: &State) -> Value {
    let s = &st.auc.settings;
    let mut v = serde_json::to_value(&st.auc).unwrap_or(json!({}));
    if let Some(o) = v.get_mut("settings").and_then(Value::as_object_mut) {
        o.insert("timer".into(), json!({
            "showTotalTime": s.show_total_time,
            "leaderChangePoints": s.leader_change_points, "leaderChangeDonations": s.leader_change_donations,
            "newPositionPoints": s.new_position_points, "newPositionDonations": s.new_position_donations,
            "addExistingPoints": s.add_existing_points, "addExistingDonations": s.add_existing_donations,
            "timeMoreThanMin": s.time_more_than_min,
        }));
    }
    if let Some(o) = v.as_object_mut() {
        o.remove("seen");            // internal dedup, never needed by the page
        o.remove("confirmedLots");   // internal trust list

        // history = unapproved/rejected bids (marked) + applied bids, newest first
        let mut hist: Vec<Value> = Vec::new();
        for (items, status) in [
            (&st.auc.pending, "pending"),
            (&st.auc.log, "approved"),
            (&st.auc.declined, "rejected"),
        ] {
            for b in items {
                let mut val = serde_json::to_value(b).unwrap_or(json!({}));
                if let Some(obj) = val.as_object_mut() {
                    obj.insert("status".into(), json!(status));
                }
                hist.push(val);
            }
        }
        hist.sort_by(|a, b| {
            let at = |v: &Value| v.get("at").and_then(Value::as_i64).unwrap_or(0);
            at(b).cmp(&at(a))
        });
        o.insert("history".into(), Value::Array(hist));
    }
    v
}

fn settings_json(ctx: &Ctx) -> Value {
    let cfg = ctx.cfg.lock().unwrap();
    let st = ctx.state.lock().unwrap();
    let mut v = serde_json::to_value(&st.auc.settings).unwrap_or(json!({}));
    if let Some(o) = v.as_object_mut() {
        o.insert("donatelloEnabled".into(), json!(cfg.donatello_enabled));
        o.insert("channel".into(), json!(cfg.channel));
        o.insert("minBid".into(), json!(cfg.min_bid));
        o.insert("zbiruk".into(), json!(cfg.zbiruk));
        o.insert("pollSeconds".into(), json!(cfg.poll_seconds));
        o.insert("webhookEnabled".into(), json!(cfg.webhook_enabled));
        o.insert("webhookUrl".into(), json!(cfg.webhook_url));
        o.insert("monobankEnabled".into(), json!(cfg.monobank_enabled));
        o.insert("monobankToken".into(), json!(cfg.monobank_token));
        o.insert("monobankJar".into(), json!(cfg.monobank_jar));
        o.insert("monoWidgetEnabled".into(), json!(cfg.mono_widget_enabled));
        o.insert("monoWidgetJar".into(), json!(cfg.mono_widget_jar));
        o.insert("monoWidgetToken".into(), json!(cfg.mono_widget_token));
    }
    v
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoadBody {
    #[serde(default)] settings: Option<AuctionSettings>,
    #[serde(default)] lots: Option<Vec<Lot>>,
    #[serde(default)] bids: Option<Vec<Bid>>,
    #[serde(default)] remaining_ms: Option<i64>,
}

fn post_settings(ctx: &Ctx, v: &Value) {
    {
        let mut cfg = ctx.cfg.lock().unwrap();
        let mut st = ctx.state.lock().unwrap();
        if let Some(x) = v.get("donatelloEnabled").and_then(Value::as_bool) { cfg.donatello_enabled = x; }
        if let Some(c) = v.get("channel").and_then(Value::as_str) {
            let c = clean_channel(c);
            if c != cfg.channel {
                cfg.channel = c;
                reset_cursor(&mut st);
            }
        }
        if let Some(x) = v.get("zbiruk").and_then(Value::as_bool) { cfg.zbiruk = x; }
        if let Some(x) = v.get("minBid").and_then(Value::as_f64) {
            if x >= 0.0 { cfg.min_bid = x; }
        }
        if let Some(x) = v.get("webhookEnabled").and_then(Value::as_bool) { cfg.webhook_enabled = x; }
        if let Some(u) = v.get("webhookUrl").and_then(Value::as_str) {
            let u = u.trim().to_string();
            if u != cfg.webhook_url {
                cfg.webhook_url = u;
                reset_webhook(&mut st);
            }
        }
        if let Some(x) = v.get("monobankEnabled").and_then(Value::as_bool) { cfg.monobank_enabled = x; }
        if let Some(t) = v.get("monobankToken").and_then(Value::as_str) {
            let t = t.trim().to_string();
            if t != cfg.monobank_token {
                cfg.monobank_token = t;
                reset_mono(&mut st);
            }
        }
        if let Some(j) = v.get("monobankJar").and_then(Value::as_str) {
            let j = clean_jar(j);
            if j != cfg.monobank_jar {
                cfg.monobank_jar = j;
                reset_mono(&mut st);
            }
        }        if let Some(x) = v.get("monoWidgetEnabled").and_then(Value::as_bool) { cfg.mono_widget_enabled = x; }
        if let Some(x) = v.get("monoWidgetJar").and_then(Value::as_str) { cfg.mono_widget_jar = x.trim().to_string(); }
        if let Some(x) = v.get("monoWidgetToken").and_then(Value::as_str) { cfg.mono_widget_token = x.trim().to_string(); }

        let s = &mut st.auc.settings;
        if let Some(x) = v.get("mode").and_then(Value::as_str) { s.mode = x.into(); }
        if let Some(x) = v.get("source").and_then(Value::as_str) { s.source = x.into(); }
        if let Some(x) = v.get("currency").and_then(Value::as_str) { s.currency = x.into(); }
        if let Some(x) = v.get("timerSeconds").and_then(Value::as_u64) { s.timer_seconds = x; }
        if let Some(x) = v.get("showTotalTime").and_then(Value::as_bool) { s.show_total_time = x; }
        if let Some(x) = v.get("leaderChangeDonations").and_then(Value::as_f64) { s.leader_change_donations = x; }
        if let Some(x) = v.get("newPositionDonations").and_then(Value::as_f64) { s.new_position_donations = x; }
        if let Some(x) = v.get("addExistingDonations").and_then(Value::as_f64) { s.add_existing_donations = x; }
        if let Some(x) = v.get("timeMoreThanMin").and_then(Value::as_f64) { s.time_more_than_min = x; }

        if let Some(a) = v.get("appearance").and_then(Value::as_object) {
            let ap = &mut s.appearance;
            if let Some(x) = a.get("background").and_then(Value::as_str) { ap.background = x.into(); }
            if let Some(x) = a.get("geometry").and_then(Value::as_str) { ap.geometry = x.into(); }
            if let Some(x) = a.get("primaryColor").and_then(Value::as_str) { ap.primary_color = x.into(); }
            if let Some(x) = a.get("showTotalAmount").and_then(Value::as_bool) { ap.show_total_amount = x; }
            if let Some(x) = a.get("showWinningChances").and_then(Value::as_bool) { ap.show_winning_chances = x; }
            if let Some(x) = a.get("showViewerNames").and_then(Value::as_bool) { ap.show_viewer_names = x; }
            if let Some(x) = a.get("autoParseLinks").and_then(Value::as_bool) { ap.auto_parse_links = x; }
            if let Some(x) = a.get("hideValues").and_then(Value::as_bool) { ap.hide_values = x; }
        }
    }
    ctx.save_cfg();
    ctx.save_state();
}

fn trunc(s: &str, n: usize) -> String { s.chars().take(n).collect() }

fn embed_debug(m: &discord::Message) -> Value {
    let Some(e) = m.embeds.first() else { return Value::Null; };
    json!({
        "title": trunc(&e.title, 200),
        "description": trunc(&e.description, 300),
        "author": e.author.as_ref().map(|a| trunc(&a.name, 100)).unwrap_or_default(),
        "fields": e.fields.iter().take(12)
            .map(|f| json!({ "name": trunc(&f.name, 60), "value": trunc(&f.value, 120) }))
            .collect::<Vec<_>>(),
    })
}

/// Used by the settings window's "Test" button: prove the bot can read the channel
/// and show how the last donation embed was understood.
fn discord_test(ctx: &Ctx, v: &Value) -> Value {
    if bot::token().is_empty() { return json!({ "ok": false, "error": "немає токена бота у цій збірці" }); }
    let stored_channel = ctx.cfg.lock().unwrap().channel.clone();
    let typed_channel = v.get("channel").and_then(Value::as_str).map(clean_channel).unwrap_or_default();
    let channel = if typed_channel.is_empty() { stored_channel } else { typed_channel };

    if channel.is_empty() { return json!({ "ok": false, "error": "немає ID каналу" }); }

    match discord::messages(&channel, bot::token(), None) {
        Ok(msgs) => {
            let name = discord::channel_name(&channel, bot::token()).unwrap_or_default();
            let empty = !msgs.is_empty() && msgs.iter().all(|m| m.content.is_empty() && m.embeds.is_empty());
            let sample = msgs.iter().find(|m| discord::parse_donation(m).is_some()).cloned()
                .or_else(|| msgs.into_iter().find(|m| !m.embeds.is_empty()));
            let parsed = sample.as_ref().and_then(discord::parse_donation)
                .map(|d| json!({ "donor": d.donor, "amount": d.amount, "message": d.message }));
            let embed = sample.as_ref().map(embed_debug).unwrap_or(Value::Null);
            let hint = if sample.is_none() && empty {
                "повідомлення порожні — увімкніть Message Content Intent для бота"
            } else if sample.is_some() && parsed.is_none() {
                "embed не схожий на донат Donatello — немає донора, суми або позначки Donatello"
            } else { "" };
            json!({ "ok": true, "channel": name, "parsed": parsed, "embed": embed, "hint": hint })
        }
        Err(e) => json!({ "ok": false, "error": err_text(&e) }),
    }
}

/// Test a webhook source: resolve its channel, read it with the bot, parse the latest embed
/// with the relaxed parser (other services may not say Donatello).
fn webhook_test(ctx: &Ctx, v: &Value) -> Value {
    let stored = ctx.cfg.lock().unwrap().webhook_url.clone();
    let typed = v.get("webhook").and_then(Value::as_str).map(|s| s.trim().to_string()).unwrap_or_default();
    let url = if typed.is_empty() { stored } else { typed };
    if url.is_empty() { return json!({ "ok": false, "error": "немає URL вебхука" }); }

    let channel = match discord::webhook_channel(&url) {
        Ok(c) => c,
        Err(e) => return json!({ "ok": false, "error": err_text(&e) }),
    };
    let name = discord::channel_name(&channel, bot::token()).unwrap_or_default();

    match discord::messages(&channel, bot::token(), None) {
        Ok(msgs) => {
            let empty = !msgs.is_empty() && msgs.iter().all(|m| m.content.is_empty() && m.embeds.is_empty());
            let sample = msgs.iter().find(|m| discord::parse_webhook_donation(m).is_some()).cloned()
                .or_else(|| msgs.into_iter().find(|m| !discord::is_donatello(m) && (!m.embeds.is_empty() || !m.content.trim().is_empty())));
            let parsed = sample.as_ref().and_then(discord::parse_webhook_donation)
                .map(|d| json!({ "donor": d.donor, "amount": d.amount, "message": d.message }));
            let embed = sample.as_ref().map(embed_debug).unwrap_or(Value::Null);
            let text = sample.as_ref().map(|m| trunc(&m.content, 300)).unwrap_or_default();
            let hint = if sample.is_none() && empty {
                "повідомлення порожні — увімкніть Message Content Intent для бота"
            } else if sample.is_some() && parsed.is_none() {
                "embed або текст не схожі на донат — немає донора або суми"
            } else { "" };
            json!({ "ok": true, "channel": name, "channelId": channel, "parsed": parsed, "embed": embed, "text": text, "hint": hint })
        }
        Err(e) => json!({ "ok": false, "error": err_text(&e) }),
    }
}

/// Test the Monobank token + jar pair without touching the statement endpoint.
fn monobank_test(ctx: &Ctx, v: &Value) -> Value {
    let (stored_token, stored_jar) = {
        let c = ctx.cfg.lock().unwrap();
        (c.monobank_token.clone(), c.monobank_jar.clone())
    };
    let typed_token = v.get("token").and_then(Value::as_str).map(|s| s.trim().to_string()).unwrap_or_default();
    let token = if typed_token.is_empty() { stored_token } else { typed_token };
    let typed_jar = v.get("jar").and_then(Value::as_str).map(clean_jar).unwrap_or_default();
    let jar = if typed_jar.is_empty() { clean_jar(&stored_jar) } else { typed_jar };

    if token.is_empty() { return json!({ "ok": false, "error": "немає API-ключа Monobank" }); }
    if jar.is_empty() { return json!({ "ok": false, "error": "немає посилання на банку" }); }

    match mono::client_info(&token) {
        Ok(info) => match info.jars.iter().find(|j| {
            j.id == jar || j.send_id.as_deref().map(mono::short_send_id) == Some(jar.clone())
        }) {
            Some(j) => json!({
                "ok": true,
                "jar": j.title.clone().unwrap_or_default(),
                "balance": j.balance.unwrap_or(0) as f64 / 100.0,
            }),
            None => json!({ "ok": false, "error": "банку не знайдено на цьому токені" }),
        },
        Err(e) => json!({ "ok": false, "error": e }),
    }
}

/* ------------------------------------------------------------------ http */

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
}

fn respond_json(request: Request, code: u16, v: Value) {
    let body = serde_json::to_vec(&v).unwrap_or_else(|_| b"{}".to_vec());
    let resp = Response::from_data(body)
        .with_status_code(code)
        .with_header(header("Content-Type", "application/json; charset=utf-8"))
        .with_header(header("Cache-Control", "no-store"))
        .with_header(header("Access-Control-Allow-Origin", "*"))
        .with_header(header("Access-Control-Allow-Methods", "GET, POST, OPTIONS"))
        .with_header(header("Access-Control-Allow-Headers", "content-type"));
    let _ = request.respond(resp);
}

fn respond_text(request: Request, mime: &str, body: &str) {
    let resp = Response::from_string(body.to_owned())
        .with_header(header("Content-Type", mime))
        .with_header(header("Cache-Control", "no-store"));
    let _ = request.respond(resp);
}

fn respond_bytes(request: Request, mime: &str, body: &[u8]) {
    let resp = Response::from_data(body.to_vec())
        .with_header(header("Content-Type", mime))
        .with_header(header("Cache-Control", "no-store"));
    let _ = request.respond(resp);
}

fn read_json(request: &mut Request) -> Value {
    let mut body = String::new();
    let _ = request.as_reader().take(4 << 20).read_to_string(&mut body);
    serde_json::from_str(&body).unwrap_or(json!({}))
}

fn g<'a>(v: &'a Value, key: &str) -> &'a str { v.get(key).and_then(Value::as_str).unwrap_or("") }
fn gf(v: &Value, key: &str) -> f64 { v.get(key).and_then(Value::as_f64).unwrap_or(0.0) }

fn handle(ctx: &Arc<Ctx>, mut request: Request) {
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("/");

    if *request.method() == Method::Options && path.starts_with("/api/") {
        let resp = Response::empty(204)
            .with_header(header("Access-Control-Allow-Origin", "*"))
            .with_header(header("Access-Control-Allow-Methods", "GET, POST, OPTIONS"))
            .with_header(header("Access-Control-Allow-Headers", "content-type"));
        let _ = request.respond(resp);
        return;
    }

    if *request.method() == Method::Get {
        match path {
            "/" | "/index.html" => return respond_text(request, "text/html; charset=utf-8", INDEX_HTML),
            "/auc.js" => return respond_text(request, "application/javascript; charset=utf-8", AUC_JS),
            "/auc.css" => return respond_text(request, "text/css; charset=utf-8", AUC_CSS),
            "/auction.svg" => return respond_text(request, "image/svg+xml", AUCTION_SVG),
            "/roulette.svg" => return respond_text(request, "image/svg+xml", ROULETTE_SVG),
            "/lumasik.png" => return respond_bytes(request, "image/png", LUMASIK_PNG),
            "/info.jpg" => return respond_bytes(request, "image/jpeg", INFO_JPG),
            "/nerd.woff2" => return respond_bytes(request, "font/woff2", NERD_WOFF2),
            "/sotochka.jpg" => return respond_bytes(request, "image/jpeg", SOTOCHKA_JPG),
            p if p.starts_with("/luma/") => {
                let name = p.trim_start_matches("/luma/").trim_end_matches(".mp3").trim_start_matches("luma");
                if let Ok(n) = name.parse::<usize>() {
                    if n >= 1 && n <= LUMA.len() { return respond_bytes(request, "audio/mpeg", LUMA[n - 1]); }
                }
                let _ = request.respond(Response::empty(404)); return;
            }
            "/favicon.ico" => { let _ = request.respond(Response::empty(204)); return; }
            "/api/auc/state" => { let st = ctx.state.lock().unwrap(); return respond_json(request, 200, state_value(&st)); }
            "/api/auc/bids" => {
                let st = ctx.state.lock().unwrap();
                let v = serde_json::to_value(&st.auc.log).unwrap_or(json!([]));
                return respond_json(request, 200, v);
            }
            "/api/auc/emotes" => return respond_json(request, 200, json!({ "emotes": [] })),
            "/api/app/info" => {
                let upd = ctx.upd.lock().unwrap().clone();
                return respond_json(request, 200, json!({ "version": update::VERSION, "repo": update::REPO, "check": upd }));
            }
            "/api/auc/settings" => return respond_json(request, 200, settings_json(ctx)),
            _ => {}
        }
    }

    if *request.method() == Method::Post && path.starts_with("/api/app/") {
        match path {
            "/api/app/update/check" => {
                let res = update::check();
                *ctx.upd.lock().unwrap() = json!({ "at": now_secs(), "result": res.clone() });
                return respond_json(request, 200, res);
            }
            "/api/app/update/apply" => {
                let v = read_json(&mut request);
                let url = g(&v, "asset").to_string();
                let (ok, msg) = match update::apply(&url) { Ok(m) => (true, m), Err(e) => (false, e) };
                return respond_json(request, 200, json!({ "ok": ok, "message": msg }));
            }
            _ => {}
        }
    }

    if *request.method() == Method::Post && path.starts_with("/api/auc/") {
        let v = read_json(&mut request);

        if path == "/api/auc/discord/test" {
            return respond_json(request, 200, discord_test(ctx, &v));
        }
        if path == "/api/auc/webhook/test" {
            return respond_json(request, 200, webhook_test(ctx, &v));
        }
        if path == "/api/auc/monobank/test" {
            return respond_json(request, 200, monobank_test(ctx, &v));
        }
        if path == "/api/auc/bot/invite" {
            open_url(&bot::invite_url());
            return respond_json(request, 200, json!({ "ok": true }));
        }

        match path {
            "/api/auc/lots" => {
                let title = g(&v, "title").trim().to_string();
                if !title.is_empty() {
                    let mut st = ctx.state.lock().unwrap();
                    let id = st.auc.make_lot(&title, true).id.clone();
                    st.auc.mark_admin_new_lot(&id);
                    drop(st);
                    ctx.save_state();
                }
            }
            "/api/auc/lots/delete" => {
                let id = g(&v, "lotId").to_string();
                let mut st = ctx.state.lock().unwrap();
                st.auc.lots.retain(|l| l.id != id);
                st.auc.log.retain(|b| b.lot_id != id);
                st.auc.confirmed_lots.retain(|x| x != &id);
                st.auc.recompute();
                drop(st);
                ctx.save_state();
            }
            "/api/auc/lots/pin" => {
                let id = g(&v, "lotId").to_string();
                let mut st = ctx.state.lock().unwrap();
                if let Some(l) = st.auc.lots.iter_mut().find(|l| l.id == id) { l.pinned = !l.pinned; }
                drop(st);
                ctx.save_state();
            }
            "/api/auc/lots/adjust" => {
                let id = g(&v, "lotId").to_string();
                let d = gf(&v, "delta");
                let mut st = ctx.state.lock().unwrap();
                st.auc.manual_adjust(&id, d);
                drop(st);
                ctx.save_state();
            }
            "/api/auc/bid" => {
                let (u, ui, s, rid) = (g(&v, "username").to_string(), g(&v, "userInput").to_string(), g(&v, "source").to_string(), g(&v, "redemptionId").to_string());
                let amount = gf(&v, "amount");
                let src = if s.is_empty() { "manual".to_string() } else { s };
                let mut st = ctx.state.lock().unwrap();
                let _ = st.auc.intake(&u, amount, &ui, &src, &rid);
                drop(st);
                ctx.save_state();
            }
            "/api/auc/pending/approve" => {
                let id = g(&v, "id").to_string();
                let lid = g(&v, "lotId").to_string();
                let mut st = ctx.state.lock().unwrap();
                let a = &mut st.auc;
                if let Some(i) = a.pending.iter().position(|b| b.id == id) {
                    let b = a.pending.remove(i);
                    let (lid2, lt) = if !lid.is_empty() {
                        let t = a.lots.iter().find(|l| l.id == lid).map(|l| l.title.clone()).unwrap_or_default();
                        (lid.clone(), t)
                    } else if !b.lot_id.is_empty() { (b.lot_id.clone(), b.lot_title.clone()) }
                    else {
                        let t = if b.user_input.trim().is_empty() { b.username.clone() } else { b.user_input.clone() };
                        let nid = a.make_lot(&t, false).id.clone();
                        (nid, t)
                    };
                    a.apply_bid(b, &lid2, &lt);
                    if !lid2.is_empty() && !a.confirmed_lots.contains(&lid2) { a.confirmed_lots.push(lid2); }
                }
                drop(st);
                ctx.save_state();
            }
            "/api/auc/pending/add" => {
                let id = g(&v, "id").to_string();
                let number = gf(&v, "number");
                let title = g(&v, "title").trim().to_string();
                let sign = gf(&v, "sign");
                let mut st = ctx.state.lock().unwrap();
                let a = &mut st.auc;
                let existing = if number > 0.0 {
                    a.lots.iter().find(|l| l.number as f64 == number).map(|l| (l.id.clone(), l.title.clone()))
                } else { None };
                let target = match existing {
                    Some(t) => Some(t),
                    None if sign > 0.0 && !title.is_empty() => {
                        let l = a.make_lot(&title, true);
                        Some((l.id.clone(), l.title.clone()))
                    }
                    None => None,
                };
                if let (Some((lid, lt)), Some(i)) = (target, a.pending.iter().position(|b| b.id == id)) {
                    let mut b = a.pending.remove(i);
                    if sign < 0.0 { b.amount = -b.amount.abs(); }
                    a.apply_bid(b, &lid, &lt);
                    if !lid.is_empty() && !a.confirmed_lots.contains(&lid) { a.confirmed_lots.push(lid); }
                }
                drop(st);
                ctx.save_state();
            }            "/api/auc/pending/decline" => {
                let id = g(&v, "id").to_string();
                let mut st = ctx.state.lock().unwrap();
                if let Some(i) = st.auc.pending.iter().position(|b| b.id == id) {
                    let b = st.auc.pending.remove(i);
                    st.auc.declined.insert(0, b);
                    st.auc.declined.truncate(200);
                }
                drop(st);
                ctx.save_state();
            }
            "/api/auc/winner" => {
                let mode = if g(&v, "mode").is_empty() { "top" } else { g(&v, "mode") };
                let lid = g(&v, "lotId").to_string();
                let mut st = ctx.state.lock().unwrap();
                st.auc.pick_winner(mode, if lid.is_empty() { None } else { Some(&lid) });
                drop(st);
                ctx.save_state();
            }
            "/api/auc/start" => {
                let mut st = ctx.state.lock().unwrap();
                let a = &mut st.auc;
                a.status = "open".into();
                a.running = true;
                a.winner = None;
                a.ends_at = now_ms() + a.remaining_ms.max(0);
                if a.total_ms <= 0 { a.total_ms = a.remaining_ms; }
                drop(st);
                ctx.save_state();
            }
            "/api/auc/pause" => {
                let mut st = ctx.state.lock().unwrap();
                let a = &mut st.auc;
                a.remaining_ms = a.remaining();
                a.running = false;
                a.ends_at = 0;
                a.status = "paused".into();
                drop(st);
                ctx.save_state();
            }
            "/api/auc/timer/reset" => {
                let mut st = ctx.state.lock().unwrap();
                let sec = st.auc.settings.timer_seconds as i64;
                let a = &mut st.auc;
                a.running = false;
                a.ends_at = 0;
                a.remaining_ms = sec * 1000;
                a.total_ms = sec * 1000;
                a.status = "idle".into();
                drop(st);
                ctx.save_state();
            }
            "/api/auc/timer" => {
                let s = gf(&v, "seconds");
                let mut st = ctx.state.lock().unwrap();
                let d = (s * 1000.0) as i64;
                let a = &mut st.auc;
                if a.running && a.ends_at > 0 { a.ends_at = (a.ends_at + d).max(now_ms()); }
                else { a.remaining_ms = (a.remaining_ms + d).max(0); }
                a.total_ms = (a.total_ms + d).max(0);
                drop(st);
                ctx.save_state();
            }
            "/api/auc/reset" => {
                let mut st = ctx.state.lock().unwrap();
                let a = &mut st.auc;
                a.lots.clear();
                a.pending.clear();
                a.declined.clear();
                a.log.clear();
                a.winner = None;
                a.confirmed_lots.clear();
                a.running = false;
                a.ends_at = 0;
                a.status = "idle".into();
                drop(st);
                ctx.save_state();
            }
            "/api/auc/load" => {
                if let Ok(body) = serde_json::from_value::<LoadBody>(v) {
                    let mut st = ctx.state.lock().unwrap();
                    if let Some(s) = body.settings { st.auc.settings = s; }
                    if let Some(l) = body.lots { st.auc.lots = l; }
                    if let Some(b) = body.bids { st.auc.log = b; }
                    if let Some(ms) = body.remaining_ms { st.auc.remaining_ms = ms; }
                    let a = &mut st.auc;
                    a.pending.clear();
                    a.declined.clear();
                    a.confirmed_lots.clear();
                    a.winner = None;
                    a.running = false;
                    a.ends_at = 0;
                    a.status = "idle".into();
                    a.recompute();
                    drop(st);
                    ctx.save_state();
                }
            }
            "/api/auc/settings" => post_settings(ctx, &v),
            _ => { let _ = request.respond(Response::empty(404)); return; }
        }
        let st = ctx.state.lock().unwrap();
        return respond_json(request, 200, state_value(&st));
    }

    let _ = request.respond(Response::empty(404));
}

fn main() {
    let ctx = Arc::new(Ctx::load());
    set_log_path(ctx.base.join("log.txt"));
    let mut port = std::env::var("LOCALAUCTION_PORT").ok().and_then(|p| p.parse::<u16>().ok())
        .unwrap_or(ctx.cfg.lock().unwrap().port);
    log(&format!("start: port {} data {}", port, ctx.base.display()));
    {
        let c = ctx.cfg.lock().unwrap();
        log(&format!("discord: bot {} channel '{}' every {}s", bot::NAME, c.channel, c.poll_seconds.max(1)));
    }

    let bind = |p: u16| Server::http(&format!("127.0.0.1:{}", p));
    let mut server = bind(port);
    if server.is_err() {
        let mut next = None;
        let mut last_err = None;
        let last = port.saturating_add(25).min(65535);
        for p in port.saturating_add(1)..=last {
            match bind(p) {
                Ok(s) => { next = Some((p, s)); break; }
                Err(e) => last_err = Some(e),
            }
        }
        match next {
            Some((p, s)) => {
                log(&format!("port {} busy - using {}", port, p));
                port = p;
                server = Ok(s);
            }
            None => { if let Some(e) = last_err { server = Err(e); } }
        }
    }
    let addr = format!("127.0.0.1:{}", port);
    match server {
        Ok(server) => {
            let http_ctx = ctx.clone();
            std::thread::spawn(move || {
                for request in server.incoming_requests() { handle(&http_ctx, request); }
            });
            let poller = ctx.clone();
            std::thread::spawn(move || run_poller(poller));
            let widget = ctx.clone();
            monowidget::spawn(widget);
            log(&format!("auction: http://localhost:{}/", port));
            let api = format!("http://127.0.0.1:{}", port);
            let html = SETTINGS_HTML
                .replace("__API_URL__", &api)
                .replace("__AUCTION_URL__", &format!("{}/", api))
                .replace("<!--RAT-->", &RAT_SVG.replace("fill=\"#000000\"", "fill=\"currentColor\""));
            if let Err(e) = run_window(&html, &ctx.base.join("WebView2")) { log(&format!("window error: {}", e)); }
        }
        Err(e) => {
            log(&format!("cannot listen on {}: {}", addr, e));
            let html = format!(
                "<!doctype html><meta charset=\"utf-8\"><body style=\"margin:0;background:#10151b;color:#e8eef5;font:15px/1.6 system-ui;padding:32px\">\
                 <h1 style=\"font-size:20px\">LocalAuction не може запуститися</h1>\
                 <p>Порт <b>{}</b> уже зайнятий.</p>\
                 <p style=\"color:#8fa3b8\">Закрийте інший застосунок, який його використовує, або задайте <code>LOCALAUCTION_PORT</code> перед запуском.</p>\
                 <p style=\"color:#8fa3b8;font-size:13px\">{}</p></body>",
                port, e
            );
            if let Err(e) = run_window(&html, &ctx.base.join("WebView2")) { log(&format!("window error: {}", e)); }
        }
    }
}

/// Settings live in a small native window; the browser only gets the auction page.
fn run_window(html: &str, data_dir: &std::path::Path) -> wry::Result<()> {
    use tao::dpi::LogicalSize;
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tao::window::{Icon, WindowBuilder};
    use wry::WebViewBuilder;

    let event_loop = EventLoopBuilder::new().build();
    let mut icon_rgba = icon::rgba(false);
    for p in icon_rgba.chunks_mut(4) { p[0] = 0xd0; p[1] = 0xd0; p[2] = 0xd0; } // muted white app icon
    let icon = Icon::from_rgba(icon_rgba, icon::ICON_W, icon::ICON_H).ok();
    let window = WindowBuilder::new()
        .with_title("LocalAuction")
        .with_window_icon(icon)
        .with_inner_size(LogicalSize::new(760.0, 720.0))
        .with_min_inner_size(LogicalSize::new(520.0, 480.0))
        .build(&event_loop)
        .expect("failed to build window");

    let mut web_context = wry::WebContext::new(Some(data_dir.to_path_buf()));
    let _webview = WebViewBuilder::with_web_context(&mut web_context)
        .with_html(html)
        .with_background_color((16, 21, 27, 255))
        // links open in the user's default browser, not inside this window
        .with_new_window_req_handler(|url| {
            open_url(&url);
            false
        })
        .build(&window)?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent { event: WindowEvent::CloseRequested, .. } = event {
            *control_flow = ControlFlow::Exit;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snowflake(ms: i64) -> String {
        (((ms.max(1_420_070_400_000) as u64 - 1_420_070_400_000) << 22) | 0x123).to_string()
    }

    fn msg(id: &str, amount: &str, message: &str) -> discord::Message {
        discord::Message {
            id: id.into(),
            embeds: vec![discord::Embed {
                title: "Donatello".into(),
                description: message.into(),
                fields: vec![
                    discord::EmbedField { name: "👤".into(), value: "Тест".into() },
                    discord::EmbedField { name: "💰".into(), value: amount.into() },
                ],
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn ingest_filters_min_and_dedups() {
        let mut st = State::default();
        let now = now_ms();
        let old = snowflake(now - 1_000);
        let fresh = snowflake(now - 500);
        let m_old = msg(&old, "₴5", "мало");
        let m_fresh = msg(&fresh, "₴100", "Лот 1");

        // newest first on purpose, like the Discord API
        let added = {
            let State { auc, seen_tx, last_msg_id, .. } = &mut st;
            ingest_messages(auc, seen_tx, last_msg_id, vec![m_fresh.clone(), m_old], 10.0, false, true, true, "discord")
        };
        assert_eq!(added.len(), 1);
        assert_eq!(st.last_msg_id, fresh);
        assert_eq!(st.auc.pending.len(), 1);

        let added = {
            let State { auc, seen_tx, last_msg_id, .. } = &mut st;
            ingest_messages(auc, seen_tx, last_msg_id, vec![m_fresh], 10.0, false, false, true, "discord")
        };
        assert!(added.is_empty());
        assert_eq!(st.auc.pending.len(), 1);
    }
    #[test]
    fn zbiruk_minus_and_plus_messages() {
        let mut st = State::default();
        st.auc.settings.mode = "full".into();
        st.auc.make_lot("Alpha", true);
        st.auc.intake("seed", 500.0, "Alpha", "discord", "").unwrap();
        let now = now_ms();
        let minus = snowflake(now - 400);
        let added = {
            let State { auc, seen_tx, last_msg_id, .. } = &mut st;
            ingest_messages(auc, seen_tx, last_msg_id, vec![msg(&minus, "\u{20B4}200", "- Alpha")], 10.0, true, true, true, "discord")
        };
        assert_eq!(added.len(), 1);
        assert_eq!(st.auc.lots[0].total, 300.0);
        let plus = snowflake(now - 300);
        let added = {
            let State { auc, seen_tx, last_msg_id, .. } = &mut st;
            ingest_messages(auc, seen_tx, last_msg_id, vec![msg(&plus, "\u{20B4}100", "+ Alpha")], 10.0, true, false, true, "discord")
        };
        assert_eq!(added.len(), 1);
        assert_eq!(st.auc.lots[0].total, 400.0);
    }

    #[test]
    fn first_run_skips_old_backlog() {
        let mut st = State::default();
        let old = msg(&snowflake(now_ms() - 600_000), "₴100", "старий донат");
        let added = {
            let State { auc, seen_tx, last_msg_id, .. } = &mut st;
            ingest_messages(auc, seen_tx, last_msg_id, vec![old.clone()], 10.0, false, true, true, "discord")
        };
        assert!(added.is_empty());
        assert_eq!(st.last_msg_id, old.id);
        assert!(st.auc.pending.is_empty());
    }

    #[test]
    fn state_json_exposes_timer_and_appearance() {
        let v = state_value(&State::default());
        assert!(v["settings"]["timer"].is_object());
        assert_eq!(v["settings"]["appearance"]["primaryColor"], "#006bb9");
        assert!(v.get("seen").is_none());
        assert!(v.get("confirmedLots").is_none());
    }

    #[test]
    fn history_keeps_all_bid_statuses() {
        let mut st = State::default();
        st.auc.log.push(Bid { id: "a".into(), username: "Ivan".into(), amount: 100.0, at: 3, ..Default::default() });
        st.auc.pending.push(Bid { id: "p".into(), username: "Petro".into(), amount: 25.0, user_input: "wait".into(), at: 2, ..Default::default() });
        st.auc.declined.push(Bid { id: "d".into(), username: "Olia".into(), amount: 5.0, user_input: "nope".into(), at: 1, ..Default::default() });
        let v = state_value(&st);
        let h = v["history"].as_array().expect("history");
        assert_eq!(h.len(), 3);
        assert_eq!(h[0]["status"], "approved");
        assert_eq!(h[1]["status"], "pending");
        assert_eq!(h[2]["status"], "rejected");
    }

    #[test]
    fn auto_created_lots_and_history_survive_restart() {
        let mut st = State::default();
        st.auc.settings.mode = "full".into();
        st.auc.make_lot("Alpha", true);
        st.auc.intake("u", 10.0, "Alpha", "discord", "").unwrap();
        st.auc.intake("v", 20.0, "auto lot", "discord", "").unwrap();
        assert_eq!(st.auc.lots.len(), 2);

        let json = serde_json::to_string(&st).expect("serialize");
        let back: State = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.auc.lots.len(), 2);
        let v = state_value(&back);
        let h = v["history"].as_array().expect("history");
        assert_eq!(h.len(), 2);
        assert_eq!(h[0]["status"], "approved");
        assert_eq!(h[1]["status"], "approved");
    }
}
