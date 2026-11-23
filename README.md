# LocalAuction

Standalone auction for a stream, driven by donations from Donatello, an extra Discord
webhook, and optionally a Monobank jar. No Tailscale, no Twitch, no OBS alerts — a small
desktop window for settings and a localhost page for the auction itself.

## Тестування

    cargo test
    powershell -ExecutionPolicy Bypass -File tests\smoke.ps1

`cargo test` covers message parsing, timer rules and history state; `smoke.ps1` starts a throwaway
instance on a free port and walks the API (assets, lots, approve/decline/add-to-lot, bid modes,
timer, reset, malformed input).

## Оновлення

Вкладка **Інфо** показує поточну версію застосунку і перевіряє останній реліз на GitHub
(константа `REPO` у `src/update.rs`). Кнопка **Оновити** неактивна, доки не опубліковано
новіший реліз із `.exe`-ассетом; після завантаження застосунок перезапускається сам.

## Requirements

Windows 10/11 x64. The settings window uses WebView2 (built into Windows 11; on Windows 10
install the Microsoft Edge WebView2 Runtime). The auction page opens in your normal browser.

## Discord setup

The repository build ships without a bot token — it is a local secret kept in `src/bot_token.rs`
(git-ignored) or injected at build time with `LOCALAUCTION_BOT_TOKEN`. Without it the app still
runs and the settings window works, but Discord message reading is disabled.

The app uses one shared bot (MonkeBOT) — there is no token field.

1. In the settings window press **Add bot to server**, authorize MonkeBOT in your server.
2. In Discord: channel settings → **Integrations → Webhooks → New Webhook**, then
   **Copy Webhook URL**.
3. Open [Donatello → Settings → Integrations](https://donatello.to/panel/settings?tab=integrations),
   enable **Discord Webhook**, paste the URL, enable **Embedded** messages, send a test message.
4. Enable Developer Mode, right-click the donation channel → **Copy Channel ID**, paste it
   into the settings window.
5. Press **Test connection**, then **Save**.

The invite grants **View Channel** and **Read Message History**. MonkeBOT already has the
**Message Content Intent** enabled. Only Donatello embeds with a readable donor and amount
are accepted; text-only messages are ignored.

## Optional sources

Open the **ІНТЕГРАЦІЇ** tab in the settings window and enable the optional panels at the
bottom. Both sources are off by default.

**Discord webhook** — read messages from other services in a channel. Paste that channel's
webhook URL; the app resolves the channel and reads it with MonkeBOT (the bot must be on that
server). Donatello messages are ignored by this source, so it can share the Donatello channel.
The parser accepts embeds **or plain text**: donor and amount are required.

**Monobank** — enable it, paste an `X-Token` API key from
[api.monobank.ua](https://api.monobank.ua/) and the jar link. New jar payments become bids
(amount, donor, payment comment). Monobank is polled once a minute because of its API limit.

All integration fields are blurred by default and have **show/hide** and **copy** buttons.
Source toggles apply on **Save**.

## Run

```
LocalAuction.exe
```

A **LocalAuction — Налаштування** window opens (no console). The window has three tabs:
**ІНТЕГРАЦІЇ**, **СТАВКИ**, **ТАЙМЕР**. Optional webhook/Monobank panels live inside
**ІНТЕГРАЦІЇ** and are hidden until their toggle is on. A theme button switches the whole
settings window between dark and light.

- `http://localhost:48974/` — auction page (add to OBS, or use "open auction" in the window)
- `http://localhost:48974/api/auc/...` — JSON API

There is no settings page on the web server; only the auction is served.
Closing the window stops the app and the auction page.

## How bids arrive

Discord channels are checked every 6 seconds; Monobank once a minute. An accepted message
becomes a bid: amount, donor, and donation text. The text is matched to a lot by title,
alias, or number (`#2`, exact name, fuzzy match):

- **Automatic** — the first donation matched to a lot waits in **Pending**; after you approve
  it once, later matches to that lot are applied automatically.
- **Manual** — every bid waits in **Pending**.
- Pending bids have **Прийняти / Відхилити** buttons in both modes. Approving applies the bid:
  it appears in history, changes lot totals, and runs the timer rules. Unmatched text creates a
  lot from the text (or the donor name if the text is empty).

Pending bids also appear in **Історія лотів** (marked `очікує`) until they are approved or declined.

**Timer rules** (seconds in the ТАЙМЕР tab):

- **Зміна лідера** — only when a leader already exists; the very first lot never adds time.
- **Нова позиція** — only when a new lot joins after at least one other funded lot; not for the first.
- **Додати до наявної ставки** — when the lot already has bids.
- **Не додавати час, якщо більше** — timer cap; `0` means no cap.

Donations below the minimum bid are ignored. On a fresh start only messages from the last
two minutes are considered, so old history is never replayed. Unreadable donations are
skipped and noted in `log.txt`; **Test** shows the last message's embed/text when it was not
understood. The relaxed webhook source also reads plain text messages (labelled lines or the
common `Name – 100 ₴ message` one-liner).

Recognised Donatello embed (`🎉 Donatello · новий донат`):

- donor field (`👤`, `Донатер`, `Donor`, …) or the embed author
- amount field (`💰`, `Сума`, `Amount`, …) or the first `₴`/`$`/`€`/`грн` value
- message field (`Повідомлення`, `Message`, …) or the description after the `⭕` fundraiser line

## Port and data

Default port is `48974`. Change it in `%APPDATA%\LocalAuction\config.json` or set
`LOCALAUCTION_PORT` before starting. Data:

- `%APPDATA%\LocalAuction\config.json` — channel, sources, webhook URL, Monobank key/jar, bid/timer rules, port
- `%APPDATA%\LocalAuction\state.json` — lots, bids, timer, source cursors and caches
- `%APPDATA%\LocalAuction\log.txt` — startup, connection errors and accepted bids (rotated at 1 MB)

`LOCALAUCTION_DATA` overrides the data directory.

The bot token is built into `LocalAuction.exe` (`src/bot.rs`). Anyone who has the exe can
extract it and read every channel MonkeBOT can see, so share the exe only with people you
trust. If the token leaks, reset it in the Developer Portal and rebuild with the new token.
Monobank keys and webhook URLs are stored in plain text in `config.json`.

## API

The auction page polls `GET /api/auc/state` every second. Other endpoints, all POST JSON:

`/api/auc/bid` (`username`, `amount`, `userInput`, `source`, `redemptionId`),
`/api/auc/lots`, `/api/auc/lots/delete`, `/api/auc/lots/pin`, `/api/auc/lots/adjust`,
`/api/auc/pending/approve`, `/api/auc/pending/decline`, `/api/auc/winner`,
`/api/auc/start`, `/api/auc/pause`, `/api/auc/timer`, `/api/auc/timer/reset`,
`/api/auc/reset`, `/api/auc/load`, `/api/auc/settings`,
`/api/auc/discord/test`, `/api/auc/webhook/test`, `/api/auc/monobank/test`,
`/api/auc/bot/invite`.

The API accepts cross-origin requests so the embedded settings window can use it.

## Build

```
cargo test
cargo build --release
```

The exe is `target\release\localauction.exe`. The auction page in `public/` is the
prebuilt React bundle from LocalMonoDonate (`public/auc`), served as-is.
