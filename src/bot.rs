//! Shared Discord bot identity.
//!
//! The invite link needs only the public application id, so anyone can add the
//! bot to their server without a secret. Reading channel messages does require
//! the bot token: users paste their own token in settings, or a local build can
//! compile one in `src/bot_token.rs` (git-ignored) / `LOCALAUCTION_BOT_TOKEN`.
//! Without a token the app still runs; only Discord message reading is disabled.

use std::sync::RwLock;

pub const APP_ID: &str = "1574344488456028260";
pub const NAME: &str = "MonkeBOT";
pub const PERMISSIONS: u64 = 66_560; // View Channel + Read Message History

#[path = "bot_token.rs"]
mod local;

static RUNTIME: RwLock<Option<String>> = RwLock::new(None);

/// Token from settings; empty clears it and falls back to the compiled one.
pub fn set_token(t: &str) {
    if let Ok(mut w) = RUNTIME.write() {
        let t = t.trim();
        *w = if t.is_empty() { None } else { Some(t.to_string()) };
    }
}

/// The effective token: settings first, then the compiled build secret, if any.
pub fn token() -> String {
    if let Ok(r) = RUNTIME.read() {
        if let Some(t) = r.as_ref() {
            if !t.is_empty() { return t.clone(); }
        }
    }
    if !local::TOKEN.is_empty() {
        local::TOKEN.to_string()
    } else {
        option_env!("LOCALAUCTION_BOT_TOKEN").unwrap_or("").to_string()
    }
}

pub fn invite_url() -> String {
    format!(
        "https://discord.com/oauth2/authorize?client_id={}&permissions={}&scope=bot&integration_type=0",
        APP_ID, PERMISSIONS
    )
}
