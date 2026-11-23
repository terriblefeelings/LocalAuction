use serde::Deserialize;
use std::time::Duration;

const BASE: &str = "https://api.monobank.ua";

#[derive(Deserialize, Clone)]
pub struct JarInfo {
    pub id: String,
    #[serde(rename = "sendId", default)]
    pub send_id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub balance: Option<i64>,
}

#[derive(Deserialize)]
pub struct ClientInfo {
    #[serde(default)]
    pub jars: Vec<JarInfo>,
}

#[derive(Deserialize, Clone)]
pub struct StatementItem {
    pub id: String,
    pub time: i64,
    pub amount: i64,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub comment: Option<String>,
    #[serde(rename = "counterName", default)]
    pub counter_name: Option<String>,
}

fn body_utf8(r: ureq::Response) -> Result<String, String> {
    use std::io::Read;
    let mut buf = Vec::new();
    r.into_reader().read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn get_json(url: &str, token: &str) -> Result<serde_json::Value, String> {
    match ureq::get(url).set("X-Token", token).timeout(Duration::from_secs(20)).call() {
        Ok(r) => {
            let text = body_utf8(r)?;
            serde_json::from_str(&text).map_err(|e| e.to_string())
        }
        Err(ureq::Error::Status(code, r)) => {
            let body = body_utf8(r).unwrap_or_default();
            let body: String = body.chars().take(160).collect();
            let hint = match code {
                401 => "invalid Monobank token",
                403 => "token has no access",
                429 => "rate limited, retry in a minute",
                _ => "",
            };
            Err(format!("HTTP {} {} {}", code, hint, body).trim().to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

pub fn client_info(token: &str) -> Result<ClientInfo, String> {
    let v = get_json(&format!("{}/personal/client-info", BASE), token)?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

pub fn statement(account: &str, token: &str, from: i64, to: i64) -> Result<Vec<StatementItem>, String> {
    let v = get_json(&format!("{}/personal/statement/{}/{}/{}", BASE, account, from, to), token)?;
    serde_json::from_value(v).map_err(|e| e.to_string())
}

/// "jar/AbCdEfGh12" -> "AbCdEfGh12"
pub fn short_send_id(send_id: &str) -> String {
    send_id.rsplit('/').next().unwrap_or("").to_string()
}
