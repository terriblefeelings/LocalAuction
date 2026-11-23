# LocalAuction end-to-end smoke test (API level).
# Usage: powershell -ExecutionPolicy Bypass -File tests\smoke.ps1 [-Port 49050]
param([int]$Port = 49050, [string]$Exe = '')

$ErrorActionPreference = 'Stop'
$exe = if ($Exe) { $Exe } else { Join-Path (Split-Path -Parent $PSScriptRoot) 'target\release\localauction.exe' }
if (-not (Test-Path $exe)) { $exe = Join-Path (Split-Path -Parent $PSScriptRoot) 'LocalAuction.exe' }
$data = Join-Path $env:TEMP ('la-smoke-' + [guid]::NewGuid().ToString('N').Substring(0, 8))
$env:LOCALAUCTION_DATA = $data
$env:LOCALAUCTION_PORT = "$Port"
$base = "http://127.0.0.1:$Port"
$fail = 0
$pass = 0

function ok($name, $cond, $detail = '') {
    if ($cond) { $script:pass++; Write-Host ("PASS  " + $name) -ForegroundColor Green }
    else { $script:fail++; Write-Host ("FAIL  " + $name + " " + $detail) -ForegroundColor Red }
}
function post($path, $body) {
    $json = if ($null -eq $body) { '{}' } else { $body | ConvertTo-Json -Compress -Depth 8 }
    Invoke-RestMethod -Method Post -Uri ($base + $path) -ContentType 'application/json' -Body $json
}
function state { Invoke-RestMethod ($base + '/api/auc/state') }
function lotByNumber($s, $n) { $s.lots | Where-Object { $_.number -eq $n } | Select-Object -First 1 }

$proc = Start-Process -FilePath $exe -PassThru
try {
    $up = $false
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Milliseconds 250
        try { $s0 = state; $up = $true; break } catch { }
    }
    ok 'server starts' $up
    if (-not $up) { throw 'server did not start' }

    # static assets
    foreach ($a in '/', '/auc.js', '/auc.css', '/auction.svg', '/roulette.svg', '/lumasik.png', '/info.jpg', '/luma/luma1.mp3') {
        try { $r = Invoke-WebRequest ($base + $a) -UseBasicParsing; ok "asset $a" ($r.StatusCode -eq 200 -and $r.RawContentLength -gt 0) } catch { ok "asset $a" $false $_.Exception.Message }
    }
    try { Invoke-WebRequest ($base + '/api/nope') -UseBasicParsing | Out-Null; ok 'unknown api returns error' $false } catch { ok 'unknown api returns error' ($_.Exception.Response.StatusCode.value__ -eq 404) }

    # settings round-trip
    $null = post '/api/auc/settings' @{ mode = 'auto'; minBid = 10; timerSeconds = 300; currency = 'UAH' }
    $s = state
    $cfg = Invoke-RestMethod ($base + '/api/auc/settings')
    ok 'settings applied' ($s.settings.mode -eq 'auto' -and $s.settings.timerSeconds -eq 300 -and $cfg.minBid -eq 10) ("mode=" + $s.settings.mode)

    # lots
    $null = post '/api/auc/lots' @{ title = 'Alpha' }
    $null = post '/api/auc/lots' @{ title = 'Beta' }
    $null = post '/api/auc/lots' @{ title = 'Gamma' }
    $s = state
    ok 'three lots created' ($s.lots.Count -eq 3 -and (lotByNumber $s 1).title -eq 'Alpha') ("lots=" + $s.lots.Count)

    # auto mode: the first bid on a lot still needs one manual approval
    $null = post '/api/auc/bid' @{ username = 'Ivan'; amount = 100; userInput = 'Alpha'; source = 'discord' }
    $s = state
    ok 'first bid on a lot waits (auto mode)' ($s.pending.Count -eq 1)
    $null = post '/api/auc/pending/approve' @{ id = $s.pending[0].id }
    $s = state
    ok 'approval applies amount' ((lotByNumber $s 1).total -eq 100 -and $s.pending.Count -eq 0) ("total=" + (lotByNumber $s 1).total)

    $null = post '/api/auc/bid' @{ username = 'Petro'; amount = 50; userInput = 'alpha'; source = 'webhook' }
    $s = state
    ok 'later bids auto-approve' ((lotByNumber $s 1).total -eq 150 -and $s.pending.Count -eq 0) ("total=" + (lotByNumber $s 1).total)
    ok 'history has approved rows' (($s.history | Where-Object { $_.status -eq 'approved' }).Count -eq 2) ("history=" + $s.history.Count)

    # decline keeps history and does not fund the lot
    $null = post '/api/auc/bid' @{ username = 'Nope'; amount = 70; userInput = 'Beta'; source = 'discord' }
    $s = state
    $null = post '/api/auc/pending/decline' @{ id = $s.pending[0].id }
    $s = state
    ok 'decline clears pending' ($s.pending.Count -eq 0)
    $rej = @($s.history | Where-Object { $_.status -eq 'rejected' }).Count
    ok 'declined bid stays in history' ($rej -eq 1) ("rejected=" + $rej + " history=" + $s.history.Count)
    ok 'declined bid does not fund lot' ((lotByNumber $s 2).total -eq 0)

    # pin + adjust
    $id = (lotByNumber $s 2).id
    $null = post '/api/auc/lots/pin' @{ lotId = $id }
    $s = state
    ok 'lot pin toggles' ([bool](lotByNumber $s 2).pinned)
    $null = post '/api/auc/lots/adjust' @{ lotId = (lotByNumber $s 3).id; delta = 75 }
    $s = state
    ok 'lot adjust adds cost' ((lotByNumber $s 3).total -eq 75)

    # manual mode: pending approve creates a lot from the bid text
    $null = post '/api/auc/settings' @{ mode = 'manual' }
    $null = post '/api/auc/bid' @{ username = 'Olia'; amount = 200; userInput = 'NewLot'; source = 'monobank' }
    $s = state
    ok 'manual bid waits for approval' ($s.pending.Count -eq 1)
    $null = post '/api/auc/pending/approve' @{ id = $s.pending[0].id }
    $s = state
    $new = $s.lots | Where-Object { $_.title -eq 'NewLot' }
    ok 'approve creates lot' ($s.pending.Count -eq 0 -and $new -and $new.total -eq 200)

    # pending add to an existing lot by number
    $before = (lotByNumber (state) 3).total
    $null = post '/api/auc/bid' @{ username = 'Random'; amount = 300; userInput = 'random'; source = 'webhook' }
    $s = state
    $null = post '/api/auc/pending/add' @{ id = $s.pending[0].id; number = 3 }
    $s = state
    ok 'pending add merges into existing lot' ((lotByNumber $s 3).total -eq ($before + 300) -and $s.pending.Count -eq 0) ("total=" + (lotByNumber $s 3).total)
    ok 'merged bid is approved in history' (($s.history | Where-Object { $_.status -eq 'approved' }).Count -ge 4)

    # full auto creates lots with no approval
    $null = post '/api/auc/settings' @{ mode = 'full' }
    $null = post '/api/auc/bid' @{ username = 'Vova'; amount = 400; userInput = 'BrandNewLot'; source = 'discord' }
    $s = state
    $b = $s.lots | Where-Object { $_.title -eq 'BrandNewLot' }
    ok 'full auto creates lot immediately' ($s.pending.Count -eq 0 -and $b -and $b.total -eq 400)

    # timer
    $null = post '/api/auc/timer' @{ seconds = 60 }
    $s = state
    ok 'timer add' ($s.remainingMs -ge 60000)
    $before = $s.remainingMs
    $null = post '/api/auc/timer' @{ seconds = -30 }
    $s = state
    ok 'timer subtract' ($s.remainingMs -le $before)
    $null = post '/api/auc/timer/reset' @{}
    $s = state
    ok 'timer reset' ($s.remainingMs -eq ($s.settings.timerSeconds * 1000)) ("remaining=" + $s.remainingMs)
    $null = post '/api/auc/start' @{}
    ok 'timer starts' ((state).running)
    $null = post '/api/auc/pause' @{}
    ok 'timer pauses' (-not (state).running)

    # app info + update check (network optional)
    $info = Invoke-RestMethod ($base + '/api/app/info')
    ok 'app info version' ($info.version -eq '0.1.0' -and $info.repo -ne '')
    try {
        $u = post '/api/app/update/check' $null
        ok 'update check answers' ($null -ne $u.ok) ("ok=" + $u.ok)
    } catch { ok 'update check answers' $false $_.Exception.Message }

    # malformed body must not kill the server
    try { Invoke-RestMethod -Method Post -Uri ($base + '/api/auc/bid') -ContentType 'application/json' -Body 'not json' | Out-Null } catch { }
    try { $s = state; ok 'server survives malformed json' ($null -ne $s.lots) } catch { ok 'server survives malformed json' $false }

    # reset
    $null = post '/api/auc/reset' @{}
    $s = state
    ok 'reset clears auction' ($s.lots.Count -eq 0 -and $s.history.Count -eq 0 -and $s.pending.Count -eq 0)
}
catch {
    $fail++
    Write-Host ("ERROR " + $_.Exception.Message) -ForegroundColor Red
}
finally {
    if ($proc -and -not $proc.HasExited) { Stop-Process -Id $proc.Id -Force }
    Start-Sleep -Milliseconds 300
    Remove-Item -Recurse -Force $data -ErrorAction SilentlyContinue
}

Write-Host ""
Write-Host ("passed: " + $pass + "  failed: " + $fail) -ForegroundColor ($(if ($fail) { 'Red' } else { 'Green' }))
if ($fail) { exit 1 } else { exit 0 }
