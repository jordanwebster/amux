#Requires -Version 5.1
# Installs amux:  irm https://amux.sh/install | iex
#
# The stable channel's manifest is the one document that says which build
# each machine should run. This script reads it, takes this machine's
# entry, downloads the binary the entry names, and installs it only if its
# size and sha256 are the entry's. A machine installs whatever the channel
# names, whatever share of running machines a rollout has reached: the
# rollout protects machines that already run something.
#
# The manifest is signed, and an installed amux checks that signature on
# every later update with the key it was built with. This script does not:
# it and the manifest come from the same place, so a check here would
# trust the server it was checking. What it does establish is that the
# download is the file the manifest names.
#
#   AMUX_RELEASES_URL     where the manifests are (default https://amux.sh/releases)
#   AMUX_NO_MODIFY_PATH   set to leave the user PATH alone

$ErrorActionPreference = "Stop"

$ReleasesUrl = if ($env:AMUX_RELEASES_URL) { $env:AMUX_RELEASES_URL } else { "https://amux.sh/releases" }
$InstallDir = Join-Path $env:USERPROFILE ".amux\bin"
$BinaryName = "amux.exe"

# The target as the manifest names it: the build's Rust target triple.
function Get-Target {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture

    switch ($arch) {
        "X64" { return "x86_64-pc-windows-msvc" }
        "Arm64" { return "aarch64-pc-windows-msvc" }
        default { throw "Unsupported architecture: $arch." }
    }
}

function Install-Amux {
    $target = Get-Target
    Write-Host "Detected architecture: $([System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture)"

    Write-Host "Reading the stable channel..."
    $manifest = Invoke-RestMethod -Uri "$ReleasesUrl/stable.json" -Headers @{ "User-Agent" = "amux-installer" }
    $property = $manifest.targets.PSObject.Properties[$target]
    if (-not $property) {
        throw "The stable channel has no build for $target."
    }
    $entry = $property.Value
    Write-Host "Stable is amux $($entry.version)"

    $tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) "amux-install-$([System.Guid]::NewGuid().ToString('N').Substring(0, 8))"
    New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

    try {
        $binaryPath = Join-Path $tmpDir $BinaryName

        Write-Host "Downloading $($entry.url)..."
        Invoke-WebRequest -Uri $entry.url -OutFile $binaryPath -UseBasicParsing

        $actualSize = (Get-Item $binaryPath).Length
        if ($actualSize -ne [int64]$entry.size) {
            throw "The download is $actualSize bytes; the manifest says $($entry.size)."
        }

        $actualChecksum = (Get-FileHash -Path $binaryPath -Algorithm SHA256).Hash.ToLower()
        if ($actualChecksum -ne $entry.sha256) {
            throw @"
Checksum verification failed.
  Expected: $($entry.sha256)
  Actual:   $actualChecksum
"@
        }

        Write-Host "Checksum verified."

        Write-Host "Installing to $InstallDir\$BinaryName..."
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Move-Item -Path $binaryPath -Destination (Join-Path $InstallDir $BinaryName) -Force

        if (-not $env:AMUX_NO_MODIFY_PATH) {
            $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
            if ($userPath -notlike "*$InstallDir*") {
                [Environment]::SetEnvironmentVariable("Path", "$InstallDir;$userPath", "User")
                Write-Host "Added $InstallDir to user PATH."
            }
        }

        Write-Host ""
        Write-Host "amux $($entry.version) has been installed successfully!" -ForegroundColor Green
        Write-Host ""
        Write-Host "To get started, restart your terminal and run:"
        Write-Host "  amux --help"
    }
    finally {
        Remove-Item -Path $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Install-Amux
