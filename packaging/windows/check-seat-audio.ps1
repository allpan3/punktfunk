<#
.SYNOPSIS
  Report whether a multi-seat host can actually give its seats audio.

.DESCRIPTION
  A seat streams desktop audio by WASAPI-loopback-capturing a render endpoint it owns. On a
  seats box there is usually no sound card, so that endpoint has to be minted: the host creates
  its own `ROOT\MEDIA` devnode per seat and binds Valve's Remote Play streaming driver to it.
  Minting therefore needs the driver package on disk - `SteamStreamingSpeakers.inf` and
  `SteamStreamingMicrophone.inf`, either already bound to a devnode or sitting in Steam's
  driver directory. Steam itself never has to run.

  Without them a seat has nothing to capture and the host retries forever, so this asserts the
  prerequisite rather than the symptom. VB-Audio Virtual Cable does NOT substitute: the wiring
  plan refuses a cable as a loopback source (capturing one re-records what is written into it),
  and `PUNKTFUNK_MIC_DEVICE` only pins the microphone. Both address the mic leg alone.

  Exit 0 = a seat can mint. Exit 1 = it cannot, and the verdict says what to install.

.PARAMETER Quiet
  Print only the verdict line.
#>
[CmdletBinding()]
param([switch]$Quiet)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

function Say([string]$m) { if (-not $Quiet) { Write-Output $m } }

# --- 1. the INF files the minting path looks for ------------------------------------------------
# Same path the host expands: %CommonProgramFiles(x86)%\Steam\drivers\Windows10\{arch}\.
$arch = if ($env:PROCESSOR_ARCHITECTURE -eq 'ARM64') { 'arm64' } else { 'x64' }
$steamDir = Join-Path ${env:CommonProgramFiles(x86)} "Steam\drivers\Windows10\$arch"
$infs = 'SteamStreamingSpeakers.inf', 'SteamStreamingMicrophone.inf'
$haveInfs = @($infs | Where-Object { Test-Path (Join-Path $steamDir $_) })
Say "steam driver dir : $steamDir"
foreach ($i in $infs) {
    Say ("  {0,-34} {1}" -f $i, $(if ($haveInfs -contains $i) { 'present' } else { 'MISSING' }))
}

# --- 2. or an already-bound devnode -------------------------------------------------------------
# The host will reuse an installed Steam streaming devnode's hardware id and INF, so a box where
# Steam was installed and later removed can still mint.
$bound = @(Get-PnpDevice -Class MEDIA -ErrorAction SilentlyContinue |
    Where-Object { $_.HardwareID -match 'SteamStreaming(Speakers|Microphone)' })
Say "bound devnodes   : $(if ($bound) { $bound.Count } else { 'none' })"

# --- 3. what the seats already minted -----------------------------------------------------------
# Minted nodes carry the seat id in their description, which is the contract's isolation claim.
$minted = @(Get-PnpDevice -Class MEDIA -ErrorAction SilentlyContinue |
    Where-Object { $_.FriendlyName -like 'Punktfunk *' })
foreach ($m in $minted) { Say ("  minted: {0} [{1}]" -f $m.FriendlyName, $m.Status) }

# --- 4. render endpoints, as the box sees them --------------------------------------------------
# `{0.0.0.*}` is the render half of an MMDevAPI endpoint id; `{0.0.1.*}` is capture. Endpoints a
# seat session sees are not always these, so run this inside a seat session for the full picture.
$renders = @(Get-PnpDevice -Class AudioEndpoint -Status OK -ErrorAction SilentlyContinue |
    Where-Object { $_.InstanceId -like '*{0.0.0.00000000}*' })
Say "render endpoints : $($renders.Count)"
foreach ($r in $renders) { Say "  $($r.FriendlyName)" }

# --- verdict ------------------------------------------------------------------------------------
if ($haveInfs.Count -eq $infs.Count -or $bound) {
    Write-Output "OK: seats can mint their own audio endpoints ($($minted.Count) minted now)."
    exit 0
}
Write-Output ("FAIL: no Steam Remote Play streaming drivers on this box, so a seat has no " +
    "render endpoint to capture and its audio will retry forever. Install Steam (it never has " +
    "to run) or stage $($infs -join ' and ') into $steamDir. A virtual cable is not a " +
    'substitute - the wiring plan refuses one as a loopback source.')
exit 1
