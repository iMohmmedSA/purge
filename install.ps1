# Installs purge on Windows, from PowerShell:
#
#   powershell -ExecutionPolicy Bypass -c "irm https://github.com/iMohmmedSA/purge/releases/latest/download/install.ps1 | iex"
#
# Downloads the release archive, checks its SHA-256, puts purge.exe,
# purge-rs.exe and purged.exe into %LOCALAPPDATA%\Programs\purge, and adds
# that folder to your user PATH.
#
# Optional settings (environment variables):
#   PURGE_VERSION=v0.1.0      install this release instead of the latest
#   PURGE_INSTALL_DIR=<dir>   install here instead

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'iMohmmedSA/purge'
$downloads = if ($env:PURGE_DOWNLOAD_URL) { $env:PURGE_DOWNLOAD_URL } else { "https://github.com/$repo/releases" }
$dir = if ($env:PURGE_INSTALL_DIR) { $env:PURGE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\purge' }

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'purge: no prebuilt binaries for 32-bit Windows; install with `cargo install --locked purge`'
}
$target = 'x86_64-pc-windows-msvc'

$version = $env:PURGE_VERSION
if (-not $version) {
    $version = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
}
$name = "purge-$version-$target"
Write-Host "purge: installing $version for $target into $dir"

$tmp = Join-Path ([IO.Path]::GetTempPath()) ([Guid]::NewGuid())
New-Item -ItemType Directory $tmp | Out-Null
try {
    $zip = Join-Path $tmp "$name.zip"
    Invoke-WebRequest "$downloads/download/$version/$name.zip" -OutFile $zip
    Invoke-WebRequest "$downloads/download/$version/$name.zip.sha256" -OutFile "$zip.sha256"
    $expected = ((Get-Content -Raw "$zip.sha256") -split '\s+')[0]
    $actual = (Get-FileHash $zip -Algorithm SHA256).Hash
    if ($actual -ne $expected) {
        throw "purge: checksum mismatch for $name.zip; nothing was installed"
    }

    Expand-Archive $zip -DestinationPath $tmp
    New-Item -ItemType Directory -Force $dir | Out-Null
    foreach ($bin in 'purge', 'purge-rs', 'purged') {
        Copy-Item -Force (Join-Path $tmp "$name\$bin.exe") $dir
    }
} finally {
    Remove-Item -Recurse -Force $tmp
}
Write-Host 'purge: installed purge, purge-rs and purged'

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object { $_ })
if ($entries -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', (($entries + $dir) -join ';'), 'User')
    Write-Host "purge: added $dir to your PATH; open a new terminal to use it"
}
Write-Host 'purge: get started: purge config init $HOME\Dev'
