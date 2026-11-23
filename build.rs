fn ensure_bot_token() {
    let path = std::path::Path::new("src/bot_token.rs");
    if !path.exists() {
        // repository builds have no token: the Discord sources stay disabled
        let _ = std::fs::write(path, "// generated: put a bot token here for a local build\npub const TOKEN: &str = \"\";\n");
    }
}
fn main() {
    ensure_bot_token();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        let _ = res.set_icon("icon.ico");
        res.set("ProductName", "LocalAuction");
        res.set("FileDescription", "LocalAuction");
        res.set("OriginalFilename", "LocalAuction.exe");
        let _ = res.compile();
    }
}
