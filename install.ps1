# Installs or updates purge on Windows, from PowerShell:
#
#   powershell -ExecutionPolicy Bypass -c "irm https://github.com/iMohmmedSA/purge/releases/latest/download/install.ps1 | iex"
#
# Downloads the release archive, checks its SHA-256, puts purge.exe,
# purge-rs.exe and purged.exe into %LOCALAPPDATA%\Programs\purge, and adds
# that folder to your user PATH. Run it again to update: a running
# background service is stopped first and started again on the new version.
# A purge installed with `cargo install` is removed, so the two never
# compete.
#
# Optional settings (environment variables):
#   PURGE_VERSION=v0.1.0      install this release instead of the latest
#   PURGE_INSTALL_DIR=<dir>   install here instead
#   PURGE_FORCE=1             reinstall even if this version is installed

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$repo = 'iMohmmedSA/purge'
$downloads = if ($env:PURGE_DOWNLOAD_URL) { $env:PURGE_DOWNLOAD_URL } else { "https://github.com/$repo/releases" }
$dir = if ($env:PURGE_INSTALL_DIR) { $env:PURGE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\purge' }
$purge = Join-Path $dir 'purge.exe'

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'purge: no prebuilt binaries for 32-bit Windows; install with `cargo install --locked purge`'
}
$target = 'x86_64-pc-windows-msvc'

$version = $env:PURGE_VERSION
if (-not $version) {
    $version = (Invoke-RestMethod "https://api.github.com/repos/$repo/releases/latest").tag_name
}
$name = "purge-$version-$target"

$previous = $null
if (Test-Path $purge) {
    $previous = ((& $purge --version) -replace '^purge\s+', '').Trim()
}
# `return`, not `exit`: under `irm | iex`, `exit` would close the window.
if ($previous -and "v$previous" -eq $version -and -not $env:PURGE_FORCE) {
    Write-Host "purge: purge $previous is already installed and up to date"
    return
}
if ($previous) {
    Write-Host "purge: updating purge $previous to $($version.TrimStart('v')) in $dir"
} else {
    Write-Host "purge: installing purge $($version.TrimStart('v')) for $target into $dir"
}

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
    # Windows cannot replace a running program: stop the daemon first. From
    # 0.2.1 on, `purge service stop` waits for a run in progress to finish;
    # older versions lack it and are stopped outright.
    if ($previous -and [version]$previous -ge [version]'0.2.1') {
        & $purge service stop | Out-Null
    } elseif ($previous) {
        Get-Process -Name purged -ErrorAction SilentlyContinue | Stop-Process -Force
    }
    Start-Sleep -Milliseconds 500
    foreach ($bin in 'purge', 'purge-rs', 'purged') {
        Copy-Item -Force (Join-Path $tmp "$name\$bin.exe") $dir
    }
} finally {
    Remove-Item -Recurse -Force $tmp
}
if ($previous -and "v$previous" -ne $version) {
    Write-Host "purge: updated purge $previous to $($version.TrimStart('v'))"
} else {
    Write-Host 'purge: installed purge, purge-rs and purged'
}

# A copy Cargo installed could run instead of this one: the `purge` package,
# and `purge-cli` and `purge-daemon` from before the first release. Only
# packages Cargo itself lists are touched.
if (Get-Command cargo -ErrorAction SilentlyContinue) {
    $installed = (cargo install --list 2>$null) -join "`n"
    foreach ($package in 'purge', 'purge-cli', 'purge-daemon') {
        if ($installed -match "(?m)^$([regex]::Escape($package)) v") {
            cargo uninstall $package *> $null
            if ($LASTEXITCODE -eq 0) {
                Write-Host "purge: removed the copy Cargo installed (``$package``)"
            } else {
                Write-Host "purge: warning: could not remove the copy Cargo installed; run ``cargo uninstall $package``"
            }
        }
    }
}

# Registering the service again points it at these files and starts it.
$status = & $purge service status 2>$null
if ($status -and $status -ne 'not installed') {
    & $purge service install | Out-Null
    if ($LASTEXITCODE -eq 0) {
        Write-Host "purge: restarted the background service on $($version.TrimStart('v'))"
    } else {
        Write-Host 'purge: warning: could not restart the background service; run `purge service install`'
    }
}

# Other installations on the PATH may run instead of this one.
foreach ($other in Get-Command purged.exe -All -ErrorAction SilentlyContinue) {
    $otherDir = Split-Path $other.Source
    if ($otherDir -ne $dir) {
        Write-Host "purge: warning: another purge is installed in $otherDir and may run instead; remove it"
    }
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$entries = @($userPath -split ';' | Where-Object { $_ })
if ($entries -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', (($entries + $dir) -join ';'), 'User')
    Write-Host "purge: added $dir to your PATH; open a new terminal to use it"
}
if (-not $previous) {
    Write-Host 'purge: get started: purge config init $HOME\Dev'
}
