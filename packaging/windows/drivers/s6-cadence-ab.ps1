#requires -Version 7.0
<#
.SYNOPSIS
  Spike S6 (design/windows-video-plane-overhaul.md §3): pool vs pool-bypass, back to back, judged
  on the virtual display's compose cadence.

.DESCRIPTION
  ONE driver binary, built --features pool-bypass and deployed before this runs. The legs differ
  only by the machine knob PFVD_POOL_BYPASS, which a fresh WUDFHost reads once - so each leg
  cycles the adapter (reset-pf-vdisplay.ps1) to mint one.

    leg A  knob cleared  -> the fused pass into a pool slot, Finished at once
    leg B  knob set      -> the encoder reads the acquired surface, Finished on its completion

  Per leg: reset, start the load generator, run the loopback client for -Minutes, stop both, and
  cut the driver log the leg wrote. The instrument is the driver's own
  "[pf-vd] cadence:" line - PresentDisplayQPCTime deltas in quarter frame periods, stamped after
  FinishedProcessingFrame in BOTH modes. §5-4's bar is "never exceed 2 frame periods"; that is the
  over-2 column. Encode latency and fps come from the client's own "stats:" lines.

  The load generator is C:\Users\Public\s6-load.cmd - one line you write, pointing at whatever GPU
  load this box has. It is started and killed identically in both legs; without it the run measures
  an idle GPU and proves nothing. -NoLoad says so on purpose.

  Bypass only engages on a BGRA input kind (NVENC, SDR, 8-bit). An HDR or 10-bit session opens P010
  and the pass is mandatory - the leg B header then says mode=pool and the run is VOID. The report
  says so rather than comparing two identical legs.

.EXAMPLE
  C:\Users\Public\kick-task.ps1 -Name s6 -Script C:\Users\Public\s6-cadence-ab.ps1 -ScriptArgs "-Tag s6 -Minutes 10 -Fps 120"
#>
[CmdletBinding()]
param(
    [string]$Tag = 's6',
    [int]$Minutes = 10,
    # Nominal refresh, only for the report's period arithmetic; the driver buckets against the
    # session's own negotiated fps.
    [int]$Fps = 120,
    # Client fingerprint; empty = the sha256 of the host's own cert, which is what a loopback
    # client pins.
    [string]$Fp = '',
    [switch]$NoLoad
)
$ErrorActionPreference = 'Continue'
$PSNativeCommandUseErrorActionPreference = $false

$Pub = 'C:\Users\Public'
$Log = Join-Path $Pub "live-$Tag.log"
$Client = Join-Path $Pub 'punktfunk-native\target\release\punktfunk-session.exe'
$Reset = @(
    (Join-Path $Pub 'reset-pf-vdisplay.ps1')
    (Join-Path $Pub 'pf-phase0\packaging\windows\reset-pf-vdisplay.ps1')
) + @(Get-ChildItem (Join-Path $Pub 'pf-*\packaging\windows\reset-pf-vdisplay.ps1') -ErrorAction SilentlyContinue |
        Select-Object -ExpandProperty FullName) |
    Where-Object { Test-Path $_ } | Select-Object -First 1
$LoadCmd = Join-Path $Pub 's6-load.cmd'
$Cert = 'C:\ProgramData\punktfunk\cert.pem'

function Say($m) { "$([DateTime]::Now.ToString('HH:mm:ss')) $m" | Tee-Object -FilePath $Log -Append | Write-Output }

"=== S6 pool vs bypass $(Get-Date -Format o) ===" | Set-Content $Log
if (-not (Test-Path $Client)) { Say "FATAL no client at $Client"; exit 1 }
if (-not $Reset) { Say "FATAL no reset-pf-vdisplay.ps1 under $Pub"; exit 1 }
Say "reset script: $Reset"
if (-not $Fp) {
    if (-not (Test-Path $Cert)) { Say "FATAL no cert at $Cert and no -Fp"; exit 1 }
    $Fp = (Get-FileHash $Cert -Algorithm SHA256).Hash.ToLower()
}
if ($NoLoad) { Say 'WARN running with NO GPU load - an idle GPU cannot show contention' }
elseif (Test-Path $LoadCmd) { Say "load generator: $((Get-Content $LoadCmd) -join ' ; ')" }
else { Say "WARN no $LoadCmd - running with NO GPU load" }

# The release driver logs nothing without this; both legs need it or neither has an instrument.
[Environment]::SetEnvironmentVariable('PFVD_DEBUG_LOG', '1', 'Machine')

# WUDFHost runs as LOCAL SERVICE, so its temp dir - not C:\Windows\Temp - holds the log.
function Get-DriverLog {
    $candidates = @(
        'C:\Windows\ServiceProfiles\LocalService\AppData\Local\Temp\pfvd-driver.log',
        'C:\Windows\Temp\pfvd-driver.log'
    )
    $candidates | Where-Object { Test-Path $_ } |
        Sort-Object { (Get-Item $_).LastWriteTime } | Select-Object -Last 1
}

function Start-Leg([bool]$bypass) {
    if ($bypass) { [Environment]::SetEnvironmentVariable('PFVD_POOL_BYPASS', '1', 'Machine') }
    else { [Environment]::SetEnvironmentVariable('PFVD_POOL_BYPASS', $null, 'Machine') }
    # The adapter cycle is what mints a WUDFHost that reads the knob: bypass_enabled() resolves
    # once per process.
    & pwsh -NoProfile -File $Reset *>&1 | Add-Content $Log
    Start-Sleep 8
}

function Invoke-Leg([string]$name, [bool]$bypass) {
    Say "--- leg $name (bypass=$bypass) ---"
    Start-Leg $bypass
    $drv = Get-DriverLog
    $mark = if ($drv) { (Get-Item $drv).Length } else { 0 }
    Say "driver log $drv at $mark bytes"
    $load = $null
    if (-not $NoLoad -and (Test-Path $LoadCmd)) {
        $load = Start-Process cmd.exe -ArgumentList '/c', $LoadCmd -PassThru -WindowStyle Minimized
        Start-Sleep 10
    }
    $out = Join-Path $Pub "s6-$Tag-$name.out"
    $cli = Start-Process $Client -PassThru -RedirectStandardOutput $out `
        -ArgumentList '--connect', '127.0.0.1:9777', '--fp', $Fp, '--stats'
    Say "client pid $($cli.Id) for $Minutes min"
    Start-Sleep -Seconds ($Minutes * 60)
    Stop-Process -Id $cli.Id -Force -ErrorAction SilentlyContinue
    if ($load) {
        # cmd.exe does not take its child with it, so kill the launched image by name too. The
        # first token of s6-load.cmd is that image; anything else here is the operator's problem,
        # not a reason to lose the leg.
        Stop-Process -Id $load.Id -Force -ErrorAction SilentlyContinue
        try {
            $img = (Get-Content $LoadCmd | Select-Object -First 1).Trim().Trim('"')
            Get-Process -Name ([IO.Path]::GetFileNameWithoutExtension($img)) -ErrorAction SilentlyContinue |
                Stop-Process -Force -ErrorAction SilentlyContinue
        }
        catch { Say "WARN could not name the load image from $LoadCmd" }
    }
    Start-Sleep 3
    $tail = if ($drv -and (Test-Path $drv)) {
        $fs = [IO.File]::Open($drv, 'Open', 'Read', 'ReadWrite')
        $fs.Seek($mark, 'Begin') | Out-Null
        $sr = New-Object IO.StreamReader($fs)
        $t = $sr.ReadToEnd(); $sr.Close(); $fs.Close(); $t -split "`r?`n"
    }
    else { @() }
    $cut = Join-Path $Pub "s6-$Tag-$name.drv"
    $tail -join "`n" | Set-Content $cut
    Say "driver log cut -> $cut ($($tail.Count) lines)"
    [pscustomobject]@{ Name = $name; Drv = $tail; Out = $out }
}

# --- the report ------------------------------------------------------------------------------
# One leg's cadence lines folded into a single histogram. Bucket i covers [i/4, (i+1)/4) frame
# periods; bucket 11 is everything at or over 2.75, so buckets 8..11 are the gate's column - read
# as "at or over 2.00 periods", one bucket conservative against §5-4's "never exceed 2".
function Get-Cadence($lines) {
    $b = New-Object 'long[]' 12
    $n = 0L; $max = 0L; $wins = 0; $fps = 0
    foreach ($l in $lines) {
        if ($l -notmatch 'cadence: mode=(\w+) fps=(\d+) win_ms=(\d+) n=(\d+) max_us=(\d+) h=([\d/]+)') { continue }
        $wins++
        $fps = [int]$Matches[2]
        $n += [long]$Matches[4]
        $max = [Math]::Max($max, [long]$Matches[5])
        $h = $Matches[6] -split '/'
        for ($i = 0; $i -lt 12 -and $i -lt $h.Count; $i++) { $b[$i] += [long]$h[$i] }
    }
    [pscustomobject]@{ Windows = $wins; N = $n; MaxUs = $max; Buckets = $b; Fps = $fps }
}

function Get-Mode($lines) {
    $m = $lines | Select-String -Pattern 'encode: backend \d+ open (\d+)x(\d+) (\w+).* mode=(\w+)' |
        Select-Object -Last 1
    if ($m) { "$($m.Matches[0].Groups[1].Value)x$($m.Matches[0].Groups[2].Value) $($m.Matches[0].Groups[3].Value) mode=$($m.Matches[0].Groups[4].Value)" }
    else { 'no encoder-open line' }
}

# p50/p95 of a sorted sample; the client emits one stats line per window, so these are medians of
# medians - enough to see a leg move, not a latency claim.
function Get-Mid($v, [double]$q) {
    if (-not $v -or $v.Count -eq 0) { return 0.0 }
    $s = $v | Sort-Object
    [double]$s[[Math]::Min($s.Count - 1, [int][Math]::Floor($q * $s.Count))]
}

function Get-Client($outPath) {
    $fps = @(); $e2e = @(); $enc = @()
    if (Test-Path $outPath) {
        foreach ($l in Get-Content $outPath) {
            if ($l -notmatch '^stats: ') { continue }
            if ($l -match '(\d+) fps') { $fps += [double]$Matches[1] }
            if ($l -match 'e2e ([\d.]+)/([\d.]+) ms') { $e2e += [double]$Matches[1] }
            if ($l -match 'encode ([\d.]+)') { $enc += [double]$Matches[1] }
        }
    }
    [pscustomobject]@{ Fps = (Get-Mid $fps 0.5); E2e = (Get-Mid $e2e 0.5); Encode = (Get-Mid $enc 0.5); Samples = $fps.Count }
}

function Show-Leg($leg) {
    $c = Get-Cadence $leg.Drv
    $s = Get-Client $leg.Out
    $le1 = ($c.Buckets[0..3] | Measure-Object -Sum).Sum
    $le2 = ($c.Buckets[0..7] | Measure-Object -Sum).Sum
    $gt2 = ($c.Buckets[8..11] | Measure-Object -Sum).Sum
    $pct = { param($x) if ($c.N -gt 0) { '{0:N3}%' -f (100.0 * $x / $c.N) } else { 'n/a' } }
    $period = if ($c.Fps -gt 0) { 1000000.0 / $c.Fps } else { 1000000.0 / [Math]::Max($Fps, 1) }
    Say ''
    Say "leg $($leg.Name): $(Get-Mode $leg.Drv)"
    Say "  cadence  windows=$($c.Windows) frames=$($c.N) session_fps=$($c.Fps)"
    Say "  <=1.00 period $(& $pct $le1)   <2.00 periods $(& $pct $le2)   >=2.00 periods $gt2 $(& $pct $gt2)"
    Say "  worst delta $($c.MaxUs) us = $('{0:N2}' -f ($c.MaxUs / $period)) periods"
    Say "  histogram (quarter periods 0.00..2.75+) $($c.Buckets -join '/')"
    Say "  client   fps $($s.Fps)  e2e p50 $($s.E2e) ms  host encode $($s.Encode) ms  windows $($s.Samples)"
    [pscustomobject]@{ Name = $leg.Name; N = $c.N; Gt2 = $gt2; MaxUs = $c.MaxUs; Fps = $s.Fps; Encode = $s.Encode; Mode = (Get-Mode $leg.Drv) }
}

$a = Invoke-Leg 'pool' $false
$b = Invoke-Leg 'bypass' $true
[Environment]::SetEnvironmentVariable('PFVD_POOL_BYPASS', $null, 'Machine')

Say ''
Say '================ S6 report ================'
$ra = Show-Leg $a
$rb = Show-Leg $b
Say ''
if ($rb.Mode -notmatch 'mode=bypass') {
    Say 'VOID: leg bypass ran on the pool. Either the deployed driver was not built'
    Say '      --features pool-bypass, or the session opened a kind the bypass cannot take'
    Say '      (P010 or planar). Re-run SDR 8-bit against a pool-bypass driver.'
}
elseif ($ra.N -eq 0 -or $rb.N -eq 0) {
    Say 'VOID: a leg produced no cadence samples - check PFVD_DEBUG_LOG and the driver log path'
}
else {
    $an = 10000.0 * $ra.Gt2 / $ra.N
    $bn = 10000.0 * $rb.Gt2 / $rb.N
    Say ("frames composed                    pool {0}  bypass {1}" -f $ra.N, $rb.N)
    Say ("2-period-or-worse per 10k frames    pool {0:N2}  bypass {1:N2}" -f $an, $bn)
    Say ("worst delta us                     pool {0}  bypass {1}" -f $ra.MaxUs, $rb.MaxUs)
    Say ("client fps                         pool {0}  bypass {1}" -f $ra.Fps, $rb.Fps)
    if ($bn -le $an -and $rb.MaxUs -le ($ra.MaxUs * 1.1)) { Say 'S6 PASS: no cadence regression vs the pool' }
    else { Say 'S6 FAIL: keep the pool (design §3 fail path); G3 is met at one pass' }
}
Say 'DONE'
