//! Auction core (-style) — ported from the JS AUC.
//! Lots, bids, name/number/alias/fuzzy matching, pending queue, winner pick.
//! Lives in the exe so the web page only renders; all settings are exe config.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use crate::now_ms;

/* ------------------------------------------------------------------ model */

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Lot {
    pub id: String,
    pub number: i64,
    pub title: String,
    #[serde(default)] pub aliases: Vec<String>,
    #[serde(default)] pub pinned: bool,
    #[serde(default)] pub adjust: f64,
    #[serde(default)] pub first_bidder: String,
    #[serde(default)] pub total: f64,
    #[serde(default)] pub participants: usize,
    #[serde(default)] pub immune: bool,
    #[serde(default)] pub position_counted: bool,
}

#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Bid {
    pub id: String,
    #[serde(default)] pub lot_id: String,
    #[serde(default)] pub lot_title: String,
    pub username: String,
    #[serde(default)] pub source: String, // "twitch" | "discord" | "monobank" (legacy) | "manual"
    pub amount: f64,
    #[serde(default)] pub redemption_id: String,
    #[serde(default)] pub user_input: String,
    #[serde(default)] pub immune_block: bool,
    #[serde(default)] pub at: i64,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AuctionSettings {
    #[serde(default = "d_auto")] pub mode: String,      // "manual" | "auto"
    #[serde(default = "d_donations")] pub source: String, // "points" | "donations" | "both"
    #[serde(default = "d_uah")] pub currency: String,     // "UAH" | "USD"
    #[serde(default = "d_timer")] pub timer_seconds: u64,
    #[serde(default)] pub show_total_time: bool,
    #[serde(default)] pub leader_change_points: f64,
    #[serde(default)] pub leader_change_donations: f64,
    #[serde(default)] pub new_position_points: f64,
    #[serde(default)] pub new_position_donations: f64,
    #[serde(default)] pub add_existing_points: f64,
    #[serde(default)] pub add_existing_donations: f64,
    #[serde(default)] pub time_more_than_min: f64,
    #[serde(default)] pub appearance: Appearance,
}
fn d_auto() -> String { "auto".into() }
fn d_donations() -> String { "donations".into() }
fn d_uah() -> String { "UAH".into() }
fn d_timer() -> u64 { 600 }
fn d_theme() -> String { "default".into() }
fn d_accent() -> String { "#006bb9".into() }
impl Default for AuctionSettings {
    fn default() -> Self { AuctionSettings { mode: d_auto(), source: d_donations(), currency: d_uah(), timer_seconds: d_timer(), show_total_time: false, leader_change_points: 0.0, leader_change_donations: 0.0, new_position_points: 0.0, new_position_donations: 0.0, add_existing_points: 0.0, add_existing_donations: 0.0, time_more_than_min: 0.0, appearance: Appearance::default() } }
}

/// Look of the auction page; settable from the page itself.
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Appearance {
    #[serde(default = "d_theme")] pub background: String,
    #[serde(default = "d_theme")] pub geometry: String,
    #[serde(default = "d_accent")] pub primary_color: String,
    #[serde(default)] pub show_total_amount: bool,
    #[serde(default)] pub show_winning_chances: bool,
    #[serde(default = "d_true")] pub show_viewer_names: bool,
    #[serde(default)] pub auto_parse_links: bool,
    #[serde(default)] pub hide_values: bool,
}
fn d_true() -> bool { true }
impl Default for Appearance {
    fn default() -> Self { Appearance { background: d_theme(), geometry: d_theme(), primary_color: d_accent(), show_total_amount: false, show_winning_chances: false, show_viewer_names: true, auto_parse_links: false, hide_values: false } }
}

/// full auction state (persisted with the rest of the exe state)
#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AuctionState {
    #[serde(default = "d_idle")] pub status: String, // idle | open | paused | closed
    #[serde(default)] pub running: bool,
    #[serde(default)] pub ends_at: i64,      // ms while running
    #[serde(default = "d_timer_ms")] pub remaining_ms: i64,
    #[serde(default = "d_timer_ms")] pub total_ms: i64, // initial + all extensions
    #[serde(default)] pub lots: Vec<Lot>,
    #[serde(default)] pub pending: Vec<Bid>,
    #[serde(default)] pub declined: Vec<Bid>, // rejected bids kept for history
    #[serde(default)] pub log: Vec<Bid>,     // recent bids (newest first)
    #[serde(default)] pub winner: Option<Bid>,
    #[serde(default)] pub settings: AuctionSettings,
    #[serde(default)] pub seen: Vec<String>, // redemption ids (dedup)
    /// lots whose first bid was approved; auto mode trusts them from then on
    #[serde(default)] pub confirmed_lots: Vec<String>,
}
fn d_idle() -> String { "idle".into() }
fn d_timer_ms() -> i64 { 600_000 }
impl Default for AuctionState {
    fn default() -> Self {
        AuctionState { status: d_idle(), running: false, ends_at: 0, remaining_ms: d_timer_ms(), total_ms: d_timer_ms(),
            lots: Vec::new(), pending: Vec::new(), declined: Vec::new(), log: Vec::new(), winner: None, settings: AuctionSettings::default(), seen: Vec::new(),
            confirmed_lots: Vec::new() }
    }
}

fn uid() -> String { format!("{:016x}", rand::random::<u64>()) }

/* ------------------------------------------------------------------ matching */

pub fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| c.is_alphanumeric() || ('\u{0400}'..='\u{04ff}').contains(c)).collect()
}
pub fn lev(a: &str, b: &str) -> usize {
    let (m, n) = (a.chars().count(), b.chars().count());
    if m == 0 { return n; }
    if n == 0 { return m; }
    let av: Vec<char> = a.chars().collect(); let bv: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur = vec![0usize; n + 1];
    for i in 1..=m { cur[0] = i; for j in 1..=n { cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + if av[i - 1] == bv[j - 1] { 0 } else { 1 }); } std::mem::swap(&mut prev, &mut cur); }
    prev[n]
}
pub fn ratio(a: &str, b: &str) -> f64 { let l = a.chars().count().max(b.chars().count()); if l == 0 { 1.0 } else { 1.0 - lev(a, b) as f64 / l as f64 } }

/// fuzzy match threshold (Levenshtein ratio on normalised text)
pub const FUZZY_MIN: f64 = 0.82;

/// A lot number typed on its own: `2`, `#2`, `№2`, `лот 2`, `Лот2`, `n2`, `2)`, `(2)`, `02`.
/// Returns `None` when the text also contains other meaningful words.
pub fn lot_number(raw: &str) -> Option<i64> {
    const WORDS: &[&str] = &["лот", "лоту", "lot", "номер", "num", "no", "n"];
    let mut nums: Vec<i64> = Vec::new();
    let mut other = false;
    for tok in raw.split(|c: char| !c.is_alphanumeric()) {
        if tok.is_empty() { continue; }
        let t = tok.to_lowercase();
        if t.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = t.parse::<i64>() { nums.push(n); }
            continue;
        }
        if WORDS.contains(&t.as_str()) { continue; }
        let mut glued = false;
        for w in WORDS {
            if let Some(rest) = t.strip_prefix(w) {
                if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()) {
                    if let Ok(n) = rest.parse::<i64>() { nums.push(n); glued = true; }
                    break;
                }
            }
        }
        if !glued { other = true; }
    }
    if other || nums.len() != 1 { None } else { nums.pop() }
}

/// `Zbiruk`: a leading `+`/`-` (with or without a space) picks add/remove; the rest is the lot text.
pub fn split_sign(input: &str, enabled: bool) -> (i8, String) {
    if !enabled { return (1, input.to_string()); }
    let t = input.trim_start();
    for (ch, sign) in [('+', 1i8), ('-', -1i8), ('\u{2212}', -1i8)] {
        if let Some(rest) = t.strip_prefix(ch) { return (sign, rest.trim_start().to_string()); }
    }
    (1, input.to_string())
}

/// find the best lot for a raw input: exact number, exact name, alias, then fuzzy (>= FUZZY_MIN)
pub fn match_lot<'a>(lots: &'a [Lot], input: &str) -> Option<&'a Lot> {
    let raw = input.trim(); let n = norm(raw); if n.is_empty() { return None; }
    if let Some(num) = lot_number(raw) {
        if let Some(l) = lots.iter().find(|l| l.number == num) { return Some(l); }
    }
    for l in lots { if norm(&l.title) == n { return Some(l); } }
    for l in lots { if l.aliases.iter().any(|a| norm(a) == n) { return Some(l); } }
    let mut best: Option<(&Lot, f64)> = None;
    for l in lots {
        let mut s = ratio(&norm(&l.title), &n);
        for a in &l.aliases { s = s.max(ratio(&norm(a), &n)); }
        if best.is_none_or(|(_, bs)| s > bs) { best = Some((l, s)); }
    }
    best.filter(|(_, s)| *s >= FUZZY_MIN).map(|(l, _)| l)
}

/* ------------------------------------------------------------------ state ops */

impl AuctionState {
    pub fn remaining(&self) -> i64 { if self.running && self.ends_at > 0 { (self.ends_at - now_ms()).max(0) } else { self.remaining_ms.max(0) } }

    pub fn next_number(&self) -> i64 { self.lots.iter().map(|l| l.number).max().unwrap_or(0) + 1 }

    pub fn make_lot(&mut self, title: &str, by_admin: bool) -> &Lot {
        let lot = Lot { id: uid(), number: self.next_number(), title: title.to_string(), aliases: Vec::new(),
            pinned: false, adjust: 0.0, first_bidder: if by_admin { "admin".into() } else { String::new() }, total: 0.0, participants: 0, immune: false, position_counted: false };
        self.lots.push(lot); self.lots.last().unwrap()
    }

    /// recompute lot totals + participants from the bid log
    pub fn recompute(&mut self) {
        let mut acc: Vec<(String, f64, std::collections::HashSet<String>)> = self.lots.iter().map(|l| (l.id.clone(), 0.0, std::collections::HashSet::new())).collect();
        for b in &self.log {
            if let Some(e) = acc.iter_mut().find(|(id, _, _)| *id == b.lot_id) { e.1 += b.amount; e.2.insert(b.username.to_lowercase()); }
        }
        for (i, l) in self.lots.iter_mut().enumerate() { l.total = acc[i].1 + l.adjust; l.participants = acc[i].2.len(); }
    }

    pub fn add_seconds(&mut self, sec: f64) {
        if sec <= 0.0 { return; }
        let d = (sec * 1000.0) as i64;
        if self.running && self.ends_at > 0 { self.ends_at = (self.ends_at + d).max(now_ms()); } else { self.remaining_ms = (self.remaining_ms + d).max(0); }
        self.total_ms = (self.total_ms + d).max(0);
    }

    /// add a bid. `source` = twitch|discord|manual. Returns the lot title on a match, or None if queued.
    pub fn intake(&mut self, username: &str, amount: f64, user_input: &str, source: &str, redemption_id: &str) -> Result<Option<String>, String> {
        if username.is_empty() || amount == 0.0 || !amount.is_finite() { return Err("bad bid".into()); }
        let is_donation = source == "discord" || source == "monobank" || source == "webhook";
        let is_points = source == "twitch";
        if self.settings.source == "points" && is_donation { return Err("donations disabled".into()); }
        if self.settings.source == "donations" && is_points { return Err("points disabled".into()); }
        if !redemption_id.is_empty() && self.seen.iter().any(|r| r == redemption_id) { return Err("duplicate".into()); }
        if !redemption_id.is_empty() {
            self.seen.push(redemption_id.to_string());
            if self.seen.len() > 2000 { self.seen.remove(0); }
        }

        let matched = match_lot(&self.lots, user_input).map(|l| (l.id.clone(), l.title.clone()));
        let mut bid = Bid { id: uid(), lot_id: matched.as_ref().map(|m| m.0.clone()).unwrap_or_default(),
            lot_title: matched.as_ref().map(|m| m.1.clone()).unwrap_or_default(), username: username.to_string(),
            source: source.to_string(), amount, redemption_id: redemption_id.to_string(), user_input: user_input.to_string(), at: now_ms(), immune_block: false };

        if amount < 0.0 {
            let (immune, enough) = matched.as_ref()
                .and_then(|(id, _)| self.lots.iter().find(|l| &l.id == id))
                .map(|l| (l.immune, l.total + amount >= 0.0))
                .unwrap_or((false, false));
            if self.settings.mode == "manual" {
                bid.immune_block = immune;
                self.pending.insert(0, bid);
                return Ok(None);
            }
            let Some((lot_id, lot_title)) = matched else { self.pending.insert(0, bid); return Ok(None); };
            if immune || !enough {
                bid.immune_block = immune;
                self.pending.insert(0, bid);
                return Ok(None);
            }
            if self.settings.mode == "auto" && !self.confirmed_lots.contains(&lot_id) {
                self.pending.insert(0, bid); return Ok(None);
            }
            self.apply_bid(bid, &lot_id, &lot_title);
            return Ok(Some(lot_title));
        }        if self.settings.mode == "manual" { self.pending.insert(0, bid); return Ok(None); }
        if self.settings.mode == "full" {
            // full auto: apply a match, or create a lot from the text/donor; never queue
            let (lot_id, lot_title) = match matched {
                Some(m) => m,
                None => {
                    let title = if user_input.trim().is_empty() { username.to_string() } else { user_input.to_string() };
                    let id = self.make_lot(&title, false).id.clone();
                    (id, title)
                }
            };
            self.apply_bid(bid, &lot_id, &lot_title);
            if !self.confirmed_lots.contains(&lot_id) { self.confirmed_lots.push(lot_id); }
            return Ok(Some(lot_title));
        }
        if matched.is_none() { self.pending.insert(0, bid); return Ok(None); }
        let (lot_id, lot_title) = matched.unwrap();
        // auto mode: the first match for a lot is confirmed by a human once
        if !self.confirmed_lots.contains(&lot_id) { self.pending.insert(0, bid); return Ok(None); }
        self.apply_bid(bid, &lot_id, &lot_title);
        Ok(Some(lot_title))
    }

    /// apply a bid directly (used by intake + pending approval)
    pub fn apply_bid(&mut self, mut b: Bid, lot_id: &str, lot_title: &str) {
        b.lot_id = lot_id.to_string(); b.lot_title = lot_title.to_string();
        if b.amount >= 2000.0 {
            if let Some(l) = self.lots.iter_mut().find(|l| l.id == lot_id) { l.immune = true; }
        }
        let had = self.lots.iter().find(|l| l.id == lot_id).map(|l| l.total > 0.0).unwrap_or(false)
            || self.log.iter().any(|x| x.lot_id == lot_id);
        // "new position" only for a brand-new lot entering an auction that already has another funded lot
        let other_lot_exists = self.lots.iter().any(|l| l.id != lot_id && l.total > 0.0);
        let counted = self.lots.iter().find(|l| l.id == lot_id).map(|l| l.position_counted).unwrap_or(false);
        let before_top = self.top_lot().map(|l| l.id.clone());
        if let Some(l) = self.lots.iter_mut().find(|l| l.id == lot_id) { if l.first_bidder.is_empty() { l.first_bidder = b.username.clone(); } }
        self.log.insert(0, b.clone());
        self.log.truncate(1000);
        self.recompute();
        let after_top = self.top_lot().map(|l| l.id.clone());
        // a leader change needs an existing leader; the first lot never counts
        let leader_changed = before_top.is_some() && after_top.is_some() && after_top != before_top;
        let new_position = !had && other_lot_exists && !counted;
        if !had && other_lot_exists {
            if let Some(l) = self.lots.iter_mut().find(|l| l.id == lot_id) { l.position_counted = true; }
        }
        self.extend_timer(&b.source, leader_changed, new_position, had);
    }

    fn top_lot(&self) -> Option<&Lot> {
        self.lots.iter().filter(|l| l.total > 0.0).enumerate()
            .max_by(|(i, a), (j, b)| a.total.partial_cmp(&b.total).unwrap_or(std::cmp::Ordering::Equal).then_with(|| j.cmp(i)))
            .map(|(_, l)| l)
    }
    fn rank_of(&self, lot_id: &str) -> Option<usize> {
        let mut v: Vec<&Lot> = self.lots.iter().filter(|l| l.total > 0.0).collect();
        v.sort_by(|a, b| b.total.partial_cmp(&a.total).unwrap_or(std::cmp::Ordering::Equal));
        v.iter().position(|l| l.id == lot_id)
    }

    fn extend_timer(&mut self, source: &str, leader_changed: bool, new_position: bool, had: bool) {
        let s = self.settings.clone();
        if s.time_more_than_min > 0.0 && self.remaining() as f64 > s.time_more_than_min * 60_000.0 { return; }
        let pts = source == "twitch";
        let mut add = 0.0;
        if leader_changed { add += if pts { s.leader_change_points } else { s.leader_change_donations }; }
        if new_position { add += if pts { s.new_position_points } else { s.new_position_donations }; }
        if had { add += if pts { s.add_existing_points } else { s.add_existing_donations }; }
        self.add_seconds(add);
    }

    /// Admin +/- on a lot: same timer rules as a bid, using the donation settings.
    pub fn manual_adjust(&mut self, lot_id: &str, delta: f64) {
        if delta == 0.0 || !delta.is_finite() { return; }
        let Some(lot) = self.lots.iter().find(|l| l.id == lot_id) else { return };
        let had = lot.total > 0.0 || self.log.iter().any(|b| b.lot_id == lot_id);
        let counted = lot.position_counted;
        let other_lot_exists = self.lots.iter().any(|l| l.id != lot_id && l.total > 0.0);
        let before_top = self.top_lot().map(|l| l.id.clone());
        if let Some(l) = self.lots.iter_mut().find(|l| l.id == lot_id) { l.adjust += delta; }
        self.recompute();
        let after_top = self.top_lot().map(|l| l.id.clone());
        let leader_changed = before_top.is_some() && after_top.is_some() && after_top != before_top;
        let new_position = !had && other_lot_exists && !counted;
        if !had && other_lot_exists {
            if let Some(l) = self.lots.iter_mut().find(|l| l.id == lot_id) { l.position_counted = true; }
        }
        self.extend_timer("manual", leader_changed, new_position, had);
    }

    /// Admin created a lot: "new position" fires on creation, not on its first funding.
    pub fn mark_admin_new_lot(&mut self, lot_id: &str) {
        let other_lot_exists = self.lots.iter().any(|l| l.id != lot_id && l.total > 0.0);
        if !other_lot_exists { return; }
        let already = self.lots.iter().find(|l| l.id == lot_id).map(|l| l.position_counted).unwrap_or(true);
        if already { return; }
        if let Some(l) = self.lots.iter_mut().find(|l| l.id == lot_id) { l.position_counted = true; }
        self.extend_timer("manual", false, true, false);
    }

    /// pick a winner: mode = "top" (highest lot) or "wheel" (weighted random)
    pub fn pick_winner(&mut self, mode: &str, lot_id: Option<&str>) {
        let pool: Vec<Lot> = self.lots.iter().filter(|l| l.total > 0.0).cloned().collect();
        if pool.is_empty() { return; }
        let chosen = if let Some(id) = lot_id { pool.iter().find(|l| l.id == id).cloned() }
            else if mode == "top" { pool.iter().max_by(|a, b| a.total.partial_cmp(&b.total).unwrap()).cloned() }
            else { weighted_pick(&pool) };
        if let Some(lot) = chosen {
            let top = self.log.iter().filter(|b| b.lot_id == lot.id).map(|b| (b.username.clone(), b.amount)).fold(std::collections::HashMap::<String, f64>::new(), |mut m, (u, a)| { *m.entry(u).or_insert(0.0) += a; m })
                .into_iter().max_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
            let (tname, tamt) = top.map(|t| (t.0, t.1)).unwrap_or((String::new(), lot.total));
            self.winner = Some(Bid { id: uid(), lot_id: lot.id.clone(), lot_title: lot.title.clone(),
                username: tname, source: String::new(), amount: tamt,
                redemption_id: String::new(), user_input: String::new(), at: now_ms(), immune_block: false });
            self.status = "closed".into();
        }
    }
}

/// weighted-random pick over lots (chance proportional to total)
pub fn weighted_pick(lots: &[Lot]) -> Option<Lot> {
    let total: f64 = lots.iter().map(|l| l.total).sum();
    if total <= 0.0 { return lots.first().cloned(); }
    let mut r = rand::random::<f64>() * total;
    for l in lots { r -= l.total; if r <= 0.0 { return Some(l.clone()); } }
    lots.last().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn st() -> AuctionState {
        let mut s = AuctionState::default();
        s.settings.mode = "auto".into();
        s.make_lot("Alpha", true);
        s.make_lot("Beta", true);
        s.confirmed_lots = s.lots.iter().map(|l| l.id.clone()).collect();
        s
    }
    #[test] fn zbiruk_sign_variants() {
        assert_eq!(split_sign("+Alpha", true), (1i8, "Alpha".to_string()));
        assert_eq!(split_sign("+ Alpha", true), (1i8, "Alpha".to_string()));
        assert_eq!(split_sign("- 2", true), (-1i8, "2".to_string()));
        assert_eq!(split_sign("-Alpha", true), (-1i8, "Alpha".to_string()));
        assert_eq!(split_sign("Alpha", true), (1i8, "Alpha".to_string()));
        assert_eq!(split_sign("-Alpha", false), (1i8, "-Alpha".to_string()));
    }

    #[test] fn minus_bids_and_immunity() {
        let mut s = AuctionState::default();
        s.settings.mode = "full".into();
        s.make_lot("Alpha", true);
        s.intake("u", 100.0, "Alpha", "discord", "").unwrap();
        assert_eq!(s.lots[0].total, 100.0);
        s.intake("v", -40.0, "Alpha", "discord", "").unwrap();
        assert_eq!(s.lots[0].total, 60.0);
        // more than the current total: waits for the admin
        assert_eq!(s.intake("w", -100.0, "Alpha", "discord", "").unwrap(), None);
        assert_eq!(s.pending.len(), 1);
        assert_eq!(s.lots[0].total, 60.0);
        // manual approval still applies
        let b = s.pending.remove(0);
        let lid = s.lots[0].id.clone();
        s.apply_bid(b, &lid, "Alpha");
        assert_eq!(s.lots[0].total, -40.0);
    }

    #[test] fn manual_immune_minus_is_flagged() {
        let mut s = AuctionState::default();
        s.settings.mode = "full".into();
        s.make_lot("Big", true);
        s.intake("u", 2000.0, "Big", "discord", "").unwrap();
        s.settings.mode = "manual".into();
        assert_eq!(s.intake("v", -100.0, "Big", "discord", "").unwrap(), None);
        assert!(s.pending[0].immune_block);
        assert_eq!(s.lots[0].total, 2000.0);
    }
    #[test] fn immune_minus_pending_is_flagged() {
        let mut s = AuctionState::default();
        s.settings.mode = "full".into();
        s.make_lot("Big", true);
        s.intake("u", 2000.0, "Big", "discord", "").unwrap();
        assert_eq!(s.intake("v", -100.0, "Big", "discord", "").unwrap(), None);
        assert!(s.pending[0].immune_block);
        let mut t = AuctionState::default();
        t.settings.mode = "full".into();
        t.make_lot("Small", true);
        t.intake("u", 50.0, "Small", "discord", "").unwrap();
        assert_eq!(t.intake("v", -100.0, "Small", "discord", "").unwrap(), None);
        assert!(!t.pending[0].immune_block);
    }
    #[test] fn single_2000_donation_gives_immunity() {
        let mut s = AuctionState::default();
        s.settings.mode = "full".into();
        s.make_lot("Big", true);
        s.intake("u", 2000.0, "Big", "discord", "").unwrap();
        assert!(s.lots[0].immune);
        assert_eq!(s.intake("v", -100.0, "Big", "discord", "").unwrap(), None);
        assert_eq!(s.pending.len(), 1);
        let b = s.pending.remove(0);
        let lid = s.lots[0].id.clone();
        s.apply_bid(b, &lid, "Big");
        assert_eq!(s.lots[0].total, 1900.0);
    }
    #[test] fn zbiruk_symbol_accepts_numbers_and_fuzzy() {
        let mut s = st(); // Alpha(1), Beta(2), auto mode, confirmed
        let (sign, text) = split_sign("+#2", true);
        assert_eq!(sign, 1i8);
        assert_eq!(s.intake("u", 10.0 * sign as f64, &text, "discord", "").unwrap().as_deref(), Some("Beta"));
        let (sign, text) = split_sign("- #2", true);
        assert_eq!(sign, -1i8);
        s.intake("v", 5.0 * sign as f64, &text, "discord", "").unwrap();
        assert_eq!(s.lots[1].total, 5.0);
        let (sign, text) = split_sign("+Alphha", true);
        assert_eq!(sign, 1i8);
        s.intake("w", 10.0 * sign as f64, &text, "discord", "").unwrap();
        assert_eq!(s.lots[0].total, 10.0);
        let (sign, text) = split_sign("- Alphha", true);
        assert_eq!(sign, -1i8);
        s.intake("x", 4.0 * sign as f64, &text, "discord", "").unwrap();
        assert_eq!(s.lots[0].total, 6.0);
    }
    #[test] fn number_forms_match_lot() {
        let mut s = st();
        for v in ["2", "#2", "№2", "# 2", "лот 2", "Лот2", "лот №2", "n2", "no2", "2)", "(2)", "02", " 2. "] {
            assert_eq!(s.intake("num", 5.0, v, "manual", "").unwrap().as_deref(), Some("Beta"), "input {v}");
        }
        assert_eq!(lot_number("Nintendo 2"), None);   // real words block the number shortcut
        assert_eq!(lot_number("лот 2 Nintendo"), None);
        assert_eq!(lot_number("2"), Some(2));
        assert_eq!(lot_number("№ 12"), Some(12));
    }

    #[test] fn fuzzy_threshold_is_82_percent() {
        assert!(ratio("nintendo switch", "nintendo swich") >= FUZZY_MIN);
        let mut s = AuctionState::default();
        s.settings.mode = "auto".into();
        s.make_lot("Nintendo Switch", true);
        s.confirmed_lots = s.lots.iter().map(|l| l.id.clone()).collect();
        let got = s.intake("u", 5.0, "nintendo swich", "discord", "").unwrap();
        assert_eq!(got.as_deref(), Some("Nintendo Switch"));
        // far enough to miss: waits for the admin instead of matching
        let got2 = s.intake("v", 5.0, "nintendo", "discord", "").unwrap();
        assert_eq!(got2, None);
        assert_eq!(s.lots.len(), 1);
        assert_eq!(s.pending.len(), 1);
    }

    #[test] fn exact_and_number_and_fuzzy() {
        let mut s = st();
        assert_eq!(s.intake("u1", 10.0, "Alpha", "manual", "").unwrap().as_deref(), Some("Alpha"));
        assert_eq!(s.intake("u2", 5.0, "#2", "manual", "").unwrap().as_deref(), Some("Beta"));
        assert_eq!(s.intake("u3", 5.0, "Alphha", "manual", "").unwrap().as_deref(), Some("Alpha"));
        assert_eq!(s.intake("u4", 5.0, "zzz", "manual", "").unwrap(), None); // pending
    }
    #[test] fn auto_first_match_needs_approval_then_autoadds() {
        let mut s = AuctionState::default();
        s.settings.mode = "auto".into();
        s.make_lot("Alpha", true);
        let lot_id = s.lots[0].id.clone();

        assert_eq!(s.intake("u", 10.0, "Alpha", "discord", "").unwrap(), None);
        assert_eq!(s.pending.len(), 1);
        assert_eq!(s.pending[0].lot_id, lot_id); // suggestion is prefilled

        let p = s.pending.remove(0);
        s.apply_bid(p, &lot_id, "Alpha");
        s.confirmed_lots.push(lot_id);

        assert_eq!(s.intake("u", 5.0, "Alpha", "discord", "").unwrap().as_deref(), Some("Alpha"));
        assert_eq!(s.lots[0].total, 15.0);
    }
    #[test] fn manual_mode_always_pending() {
        let mut s = st();
        s.settings.mode = "manual".into();
        assert_eq!(s.intake("u", 10.0, "Alpha", "discord", "").unwrap(), None);
        assert_eq!(s.pending.len(), 1);
        assert_eq!(s.lots[0].total, 0.0);
    }
    #[test] fn full_auto_applies_and_creates_without_pending() {
        let mut s = AuctionState::default();
        s.settings.mode = "full".into();
        s.make_lot("Alpha", true);

        assert_eq!(s.intake("u", 10.0, "Alpha", "discord", "").unwrap().as_deref(), Some("Alpha"));
        assert!(s.pending.is_empty());
        assert_eq!(s.intake("v", 5.0, "brand new", "discord", "").unwrap().as_deref(), Some("brand new"));
        assert!(s.pending.is_empty());
        assert_eq!(s.lots.len(), 2);
        assert_eq!(s.log.len(), 2);
        assert_eq!(s.confirmed_lots.len(), 2);
    }
    #[test] fn applying_a_bid_extends_timer_by_rules() {
        let mut s = AuctionState::default();
        s.settings.mode = "auto".into();
        s.settings.new_position_donations = 30.0;
        s.make_lot("First", true);
        let first_id = s.lots[0].id.clone();
        s.confirmed_lots.push(first_id);
        s.intake("u", 10.0, "First", "discord", "").unwrap();
        assert_eq!(s.remaining_ms, 600_000); // first lot never extends

        s.make_lot("Second", true);
        let second_id = s.lots[1].id.clone();
        s.confirmed_lots.push(second_id);
        s.intake("u", 10.0, "Second", "discord", "").unwrap();
        assert_eq!(s.remaining_ms, 630_000); // second unique lot: +30s
    }
    #[test] fn first_lot_gets_no_timer_bonus() {
        let mut s = AuctionState::default();
        s.settings.mode = "auto".into();
        s.settings.leader_change_donations = 20.0;
        s.settings.new_position_donations = 30.0;
        s.settings.add_existing_donations = 40.0;
        s.make_lot("Only", true);
        let id = s.lots[0].id.clone();
        s.confirmed_lots.push(id);
        s.intake("u", 10.0, "Only", "discord", "").unwrap();
        assert_eq!(s.remaining_ms, 600_000);
    }
    #[test] fn manual_adjust_extends_timer_by_rules() {
        let mut s = AuctionState::default();
        s.settings.leader_change_donations = 20.0;
        s.settings.new_position_donations = 30.0;
        s.settings.add_existing_donations = 10.0;
        s.make_lot("Alpha", true);
        let a = s.lots[0].id.clone();
        s.manual_adjust(&a, 100.0);
        assert_eq!(s.remaining_ms, 600_000); // first money, no other funded lot
        s.make_lot("Beta", true);
        let b = s.lots[1].id.clone();
        s.manual_adjust(&b, 100.0);
        assert_eq!(s.remaining_ms, 630_000); // new position
        s.manual_adjust(&b, 50.0);
        assert_eq!(s.remaining_ms, 660_000); // takes the lead + add existing
        s.manual_adjust(&a, 200.0);
        assert_eq!(s.remaining_ms, 690_000); // leader change + add existing
    }
    #[test] fn add_existing_applies_to_later_bids() {
        let mut s = AuctionState::default();
        s.settings.mode = "auto".into();
        s.settings.add_existing_donations = 10.0;
        s.make_lot("Alpha", true);
        let id = s.lots[0].id.clone();
        s.confirmed_lots.push(id);
        s.intake("u", 10.0, "Alpha", "discord", "").unwrap();
        assert_eq!(s.remaining_ms, 600_000); // no existing bid yet
        s.intake("u", 10.0, "Alpha", "discord", "").unwrap();
        assert_eq!(s.remaining_ms, 610_000); // lot already had a bid
    }
    #[test] fn admin_new_lot_extends_once() {
        let mut s = AuctionState::default();
        s.settings.new_position_donations = 30.0;
        s.make_lot("A", true);
        let a = s.lots[0].id.clone();
        s.manual_adjust(&a, 100.0);
        let b = s.make_lot("B", true).id.clone();
        s.mark_admin_new_lot(&b);
        assert_eq!(s.remaining_ms, 630_000);
        s.manual_adjust(&b, 100.0);
        assert_eq!(s.remaining_ms, 630_000); // funding must not count the position twice
    }
    #[test] fn manual_money_then_bid_counts_as_existing() {
        let mut s = AuctionState::default();
        s.settings.mode = "auto".into();
        s.settings.add_existing_donations = 10.0;
        s.settings.new_position_donations = 30.0;
        s.make_lot("Alpha", true);
        s.make_lot("Beta", true);
        let a = s.lots[0].id.clone();
        let b = s.lots[1].id.clone();
        s.manual_adjust(&a, 100.0);
        s.manual_adjust(&b, 100.0);
        let base = s.remaining_ms;
        s.confirmed_lots.push(a);
        s.intake("u", 10.0, "Alpha", "discord", "").unwrap();
        assert_eq!(s.remaining_ms, base + 10_000); // add existing, not a new position
    }
    #[test] fn points_source_uses_points_values() {
        let mut s = AuctionState::default();
        s.settings.mode = "auto".into();
        s.settings.source = "both".into();
        s.settings.new_position_points = 7.0;
        s.settings.new_position_donations = 30.0;
        s.make_lot("A", true);
        let a = s.lots[0].id.clone();
        s.confirmed_lots.push(a);
        s.intake("t", 10.0, "A", "twitch", "r1").unwrap();
        assert_eq!(s.remaining_ms, 600_000);
        s.make_lot("B", true);
        let b = s.lots[1].id.clone();
        s.confirmed_lots.push(b);
        s.intake("t", 10.0, "B", "twitch", "r2").unwrap();
        assert_eq!(s.remaining_ms, 607_000); // points values, not donations
    }
    #[test] fn time_more_than_min_blocks_extensions() {
        let mut s = AuctionState::default();
        s.settings.new_position_donations = 30.0;
        s.settings.add_existing_donations = 10.0;
        s.settings.time_more_than_min = 5.0;
        s.make_lot("A", true);
        s.make_lot("B", true);
        let a = s.lots[0].id.clone();
        let b = s.lots[1].id.clone();
        s.manual_adjust(&a, 100.0);
        s.remaining_ms = 600_000; // above the cap
        s.manual_adjust(&b, 100.0);
        assert_eq!(s.remaining_ms, 600_000);
        s.remaining_ms = 200_000; // below the cap
        s.manual_adjust(&b, 10.0);
        assert_eq!(s.remaining_ms, 210_000);
    }
    #[test] fn filters_dedup_totals() {
        let mut s = st();
        s.settings.source = "points".into();
        assert!(s.intake("m", 5.0, "Alpha", "discord", "").is_err());
        assert!(s.intake("t", 5.0, "Alpha", "twitch", "R1").is_ok());
        assert!(s.intake("t", 5.0, "Alpha", "twitch", "R1").is_err()); // dedup
        s.intake("t", 5.0, "Alpha", "twitch", "R2").unwrap();
        s.intake("u", 20.0, "Alpha", "manual", "").unwrap();
        let a = s.lots.iter().find(|l| l.title == "Alpha").unwrap();
        assert_eq!(a.total, 30.0);
        assert_eq!(a.participants, 2);
        assert_eq!(a.first_bidder, "admin");
    }
    #[test] fn winner_top() { let mut s = st(); s.intake("a", 10.0, "Alpha", "manual", "").unwrap(); s.intake("b", 40.0, "Beta", "manual", "").unwrap(); s.pick_winner("top", None); assert_eq!(s.winner.unwrap().lot_title, "Beta"); }
}
