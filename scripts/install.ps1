# ARCC install script for Windows (PowerShell)
# Usage: irm https://raw.githubusercontent.com/niyongsheng/arcc/main/scripts/install.ps1 | iex

$Repo = "niyongsheng/arcc"
$Target = "x86_64-pc-windows-msvc"
$TmpDir = "$env:TEMP\arcc-install"

# Use $env:LOCALAPPDATA\arcc as install location (no admin required)
$InstallDir = "$env:LOCALAPPDATA\arcc"

Write-Host "⬇️  Downloading ARCC for Windows..." -ForegroundColor Cyan

# Clean temp dir
if (Test-Path $TmpDir) { Remove-Item -Recurse -Force $TmpDir }
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null

# Download latest release
$Url = "https://github.com/$Repo/releases/latest/download/arcc-$Target.tar.gz"
$Archive = "$TmpDir\arcc.tar.gz"

try {
    Invoke-WebRequest -Uri $Url -OutFile $Archive -UseBasicParsing
} catch {
    Write-Host "❌ Download failed: $_" -ForegroundColor Red
    exit 1
}

# Extract (tar is built into modern Windows / PowerShell 7+)
try {
    tar -xzf $Archive -C $TmpDir
} catch {
    Write-Host "❌ Extraction failed. Make sure tar is available (Windows 10 1803+ or PowerShell 7+)" -ForegroundColor Red
    exit 1
}

# Install
New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
Move-Item -Force "$TmpDir\arcc.exe" "$InstallDir\arcc.exe"

# Clean up
Remove-Item -Recurse -Force $TmpDir

# Add to PATH for current user if not already there
$UserPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($UserPath -notlike "*$InstallDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$UserPath;$InstallDir", "User")
    $env:Path = [Environment]::GetEnvironmentVariable("Path", "User") + ";" + [Environment]::GetEnvironmentVariable("Path", "Machine")
    Write-Host "✅ Added $InstallDir to PATH (you may need to restart your terminal)" -ForegroundColor Yellow
}

Write-Host "✅ ARCC installed to $InstallDir\arcc.exe" -ForegroundColor Green
Write-Host ""

# Verify
try {
    & "$InstallDir\arcc.exe" --help
} catch {
    Write-Host "⚠️  Verification failed, but the binary is installed. Check PATH settings." -ForegroundColor Yellow
}
