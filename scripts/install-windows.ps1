# Install SynkMart Host as a Windows service.
# Run from an elevated PowerShell prompt.
param(
  [Parameter(Mandatory = $true)]
  [string]$BinaryPath,
  [string]$ServiceName = "SynkMartHost",
  [string]$DisplayName = "SynkMart Store Host",
  [string]$Bind = "0.0.0.0:8787",
  [string]$DataDir = "$env:LOCALAPPDATA\SynkMart\host"
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $BinaryPath)) {
  throw "Binary not found: $BinaryPath"
}

$resolved = (Resolve-Path $BinaryPath).Path
New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
$db = Join-Path $DataDir "store.db"

$existing = Get-Service -Name $ServiceName -ErrorAction SilentlyContinue
if ($existing) {
  Write-Host "Stopping existing service $ServiceName..."
  Stop-Service -Name $ServiceName -Force -ErrorAction SilentlyContinue
  sc.exe delete $ServiceName | Out-Null
  Start-Sleep -Seconds 2
}

$binPath = "`"$resolved`" --bind $Bind --database `"$db`""
Write-Host "Creating service $ServiceName"
sc.exe create $ServiceName binPath= $binPath start= auto DisplayName= $DisplayName | Out-Null
sc.exe description $ServiceName "Local SynkMart store host (LAN POS API + SQLite)" | Out-Null
Start-Service -Name $ServiceName
Write-Host "Started $ServiceName on $Bind"
Write-Host "Database: $db"
Write-Host "Next: open SynkMart POS setup wizard (Host + Client) or curl /api/v1/setup/local"
