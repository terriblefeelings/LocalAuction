//! Shared Discord bot identity.
//!
//! The invite link needs only the public application id, so anyone can add the
//! bot to their server without a secret. Reading channel messages does require
//! the bot token, which is a build secret: local builds keep it in
//! `src/bot_token.rs` (git-ignored), CI builds can set `LOCALAUCTION_BOT_TOKEN`.
//! Without a token the app still runs; only Discord message reading is disabled.

pub const APP_ID: &str = "1574344488456028260";
pub const NAME: &str = "MonkeBOT";
pub const PERMISSIONS: u64 = 66_560; // View Channel + Read Message History

#[path = "bot_token.rs"]
mod local;

/// The token compiled into this build, if any.
pub fn token() -> &'static str {
    if !local::TOKEN.is_empty() {
        local::TOKEN
    } else {
        option_env!("LOCALAUCTION_BOT_TOKEN").unwrap_or("")
    }
}

pub fn invite_url() -> String {
    format!(
        "https://discord.com/oauth2/authorize?client_id={}&permissions={}&scope=bot&integration_type=0",
        APP_ID, PERMISSIONS
    )
}
