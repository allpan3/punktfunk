<# Build the audited TermWrap payload from immutable source archives. #>
[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$OutDir)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$PSNativeCommandUseErrorActionPreference = $false

$term = @{
    Name = 'termwrap'
    Url = 'https://codeload.github.com/llccd/TermWrap/tar.gz/5d425e6e1584a82e4811c2ecc8529fd4a51e93df'
    Sha256 = '163237da6ed1f89a1576edc06d823b55d1023319a4071219e1d3b858f4d5a55a'
}
$zydis = @{
    Name = 'zydis'
    Url = 'https://codeload.github.com/zyantific/zydis/tar.gz/938b5158fd7db5043f88285b23470c8b3b02108a'
    Sha256 = '5150de4d2b52e319985edf142a00a3c052498b8356d810c68ac26290e36771d2'
}
$zycore = @{
    Name = 'zycore'
    Url = 'https://codeload.github.com/zyantific/zycore-c/tar.gz/75a36c45ae1ad382b0f4e0ede0af84c11ee69928'
    Sha256 = 'c24ebd402d0ce83cd54f08e68189e68d54e3b5938f65ca972dfdbac89757139b'
}

function Get-PinnedSource([hashtable]$Source, [string]$Work) {
    $archive = Join-Path $Work "$($Source.Name).tar.gz"
    Invoke-WebRequest -Uri $Source.Url -OutFile $archive -UseBasicParsing
    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $Source.Sha256) {
        throw "$($Source.Name) archive hash mismatch: expected $($Source.Sha256), got $actual"
    }
    $dest = Join-Path $Work $Source.Name
    New-Item -ItemType Directory -Path $dest | Out-Null
    & tar.exe -xzf $archive -C $dest --strip-components=1
    if ($LASTEXITCODE -ne 0) { throw "tar failed for $($Source.Name) ($LASTEXITCODE)" }
    $dest
}

function Find-MsBuild {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { throw 'vswhere.exe not found; install Visual Studio Build Tools' }
    $path = & $vswhere -latest -products '*' -requires Microsoft.Component.MSBuild -find 'MSBuild\**\Bin\MSBuild.exe' |
        Select-Object -First 1
    if (-not $path) { throw 'MSBuild.exe not found; install the Desktop development with C++ workload' }
    $path
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$work = Join-Path ([IO.Path]::GetTempPath()) "punktfunk-termwrap-$PID-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $work | Out-Null
try {
    $termRoot = Get-PinnedSource $term $work
    $zydisRoot = Get-PinnedSource $zydis $work
    $zycoreRoot = Get-PinnedSource $zycore $work

    $termZydis = Join-Path $termRoot 'zydis'
    New-Item -ItemType Directory -Force -Path $termZydis | Out-Null
    Copy-Item (Join-Path $zydisRoot '*') $termZydis -Recurse -Force
    $termZycore = Join-Path $termZydis 'dependencies\zycore'
    New-Item -ItemType Directory -Force -Path $termZycore | Out-Null
    Copy-Item (Join-Path $zycoreRoot '*') $termZycore -Recurse -Force

    $msbuild = Find-MsBuild
    & $msbuild (Join-Path $termZydis 'msvc\Zydis.sln') /m /t:Zydis /p:Configuration=Release /p:Platform=x64
    if ($LASTEXITCODE -ne 0) { throw "Zydis build failed ($LASTEXITCODE)" }
    & $msbuild (Join-Path $termRoot 'TermWrap.sln') /m /t:TermWrap /p:Configuration=Release /p:Platform=x64
    if ($LASTEXITCODE -ne 0) { throw "TermWrap build failed ($LASTEXITCODE)" }

    $termDll = Join-Path $termRoot 'x64\Release\TermWrap.dll'
    $zydisDll = Join-Path $termZydis 'msvc\bin\ReleaseX64\Zydis.dll'
    foreach ($file in @($termDll, $zydisDll)) {
        if (-not (Test-Path -LiteralPath $file)) { throw "expected build output missing: $file" }
        Copy-Item -LiteralPath $file -Destination $OutDir -Force
    }
    Copy-Item -LiteralPath (Join-Path $termRoot 'LICENSE') -Destination (Join-Path $OutDir 'LICENSE-TermWrap.txt') -Force
    Copy-Item -LiteralPath (Join-Path $termZydis 'LICENSE') -Destination (Join-Path $OutDir 'LICENSE-Zydis.txt') -Force
    Copy-Item -LiteralPath (Join-Path $termZycore 'LICENSE') -Destination (Join-Path $OutDir 'LICENSE-Zycore.txt') -Force
} finally {
    Remove-Item -LiteralPath $work -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output "TermWrap payload built from $($term.Url)"
