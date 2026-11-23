//! GitHub release check + self-update for LocalAuction.
use serde_json::{json, Value};
use std::io::Write;
use std::time::Duration;

pub const REPO: &str = "terriblefeelings/LocalAuction";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn nums(v: &str) -> Vec<u64> {
    v.trim_start_matches(['v', 'V'])
        .split('.')
        .map(|p| p.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse().unwrap_or(0))
        .collect()
}

pub fn is_newer(remote: &str, local: &str) -> bool {
    let a = nums(remote);
    let b = nums(local);
    for i in 0..3 {
        let x = *a.get(i).unwrap_or(&0);
        let y = *b.get(i).unwrap_or(&0);
        if x != y {
            return x > y;
        }
    }
    false
}

/// Query the latest published release. Never panics: returns a json status object.
pub fn check() -> Value {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let req = ureq::get(&url)
        .set("User-Agent", "LocalAuction")
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(15));
    match req.call() {
        Ok(resp) => {
            let text = match resp.into_string() {
                Ok(t) => t,
                Err(e) => return json!({"ok": false, "error": e.to_string()}),
            };
            let v: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => return json!({"ok": false, "error": e.to_string()}),
            };
            let tag = v["tag_name"].as_str().unwrap_or("").to_string();
            let asset = v["assets"]
                .as_array()
                .and_then(|a| {
                    a.iter().find(|x| {
                        x["name"].as_str().unwrap_or("").to_ascii_lowercase().ends_with(".exe")
                    })
                })
                .map(|a| a["browser_download_url"].as_str().unwrap_or("").to_string());
            json!({
                "ok": true,
                "latest": tag.trim_start_matches(['v', 'V']),
                "url": v["html_url"],
                "name": v["name"],
                "published": v["published_at"],
                "asset": asset,
                "newer": is_newer(&tag, VERSION),
            })
        }
        Err(ureq::Error::Status(404, _)) => {
            json!({"ok": true, "latest": "", "newer": false, "error": "no_releases"})
        }
        Err(ureq::Error::Status(code, _)) => {
            json!({"ok": false, "error": format!("github status {code}")})
        }
        Err(e) => json!({"ok": false, "error": e.to_string()}),
    }
}

/// Download the release exe next to the running one and hand the swap to a small
/// helper script, because Windows cannot overwrite a running executable.
pub fn apply(asset_url: &str) -> Result<String, String> {
    if asset_url.is_empty() {
        return Err("немає файлу оновлення".into());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("немає теки застосунку")?.to_path_buf();
    let fresh = dir.join("LocalAuction.update.exe");
    let resp = ureq::get(asset_url)
        .set("User-Agent", "LocalAuction")
        .timeout(Duration::from_secs(120))
        .call()
        .map_err(|e| format!("завантаження: {e}"))?;
    let mut out = std::fs::File::create(&fresh).map_err(|e| format!("файл: {e}"))?;
    std::io::copy(&mut resp.into_reader(), &mut out).map_err(|e| format!("запис: {e}"))?;
    out.flush().ok();
    drop(out);

    let pid = std::process::id();
    let script = dir.join("localauction-update.cmd");
    let body = format!(
        "@echo off\r\n:wait\r\ntasklist /FI \"PID eq {pid}\" 2>nul | find \"{pid}\" >nul\r\nif not errorlevel 1 (\r\n  timeout /t 1 /nobreak >nul\r\n  goto wait\r\n)\r\nmove /y \"{}\" \"{}\" >nul\r\nstart \"\" \"{}\"\r\ndel \"%~f0\"\r\n",
        fresh.display(),
        exe.display(),
        exe.display()
    );
    std::fs::write(&script, body).map_err(|e| format!("скрипт: {e}"))?;
    std::process::Command::new("cmd")
        .args(["/C", "start", "", "/min"])
        .arg(&script)
        .spawn()
        .map_err(|e| format!("запуск: {e}"))?;
    Ok("Оновлення завантажено. Застосунок перезапуститься.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("v0.0.9", "0.1.0"));
        assert!(is_newer("v1.0", "0.9.9"));
    }
}
