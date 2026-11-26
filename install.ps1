$ErrorActionPreference = 'Stop'

[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$Repo = if ($env:AGY_T3_SYNC_REPO) { $env:AGY_T3_SYNC_REPO } else { 'samudrareggi/agy-t3-sync' }
$Version = if ($env:AGY_T3_SYNC_VERSION) { $env:AGY_T3_SYNC_VERSION } else { 'latest' }
$InstallDir = if ($env:AGY_T3_SYNC_INSTALL_DIR) { $env:AGY_T3_SYNC_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'agy-t3-sync\bin' }
$NoAutoInstall = ($env:AGY_T3_SYNC_NO_AUTO_INSTALL -eq '1')

$Archive = 'agy-t3-sync-windows-x86_64.zip'
$BaseUrl = if ($Version -eq 'latest') {
  "https://github.com/$Repo/releases/latest/download"
} else {
  "https://github.com/$Repo/releases/download/$Version"
}

$TmpDir = Join-Path ([IO.Path]::GetTempPath()) ("agy-t3-sync-" + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Force -Path $TmpDir | Out-Null

try {
  $ArchivePath = Join-Path $TmpDir $Archive
  $SumsPath = Join-Path $TmpDir 'SHA256SUMS'

  Write-Host "Downloading $Archive from $BaseUrl..."
  Invoke-WebRequest -Uri "$BaseUrl/$Archive" -OutFile $ArchivePath -UseBasicParsing
  Invoke-WebRequest -Uri "$BaseUrl/SHA256SUMS" -OutFile $SumsPath -UseBasicParsing

  $ExpectedSha = $null
  foreach ($line in Get-Content -LiteralPath $SumsPath) {
    if ($line -match '^\s*([0-9a-fA-F]{64})\s+\*?(.+?)\s*$') {
      if ($Matches[2] -eq $Archive) {
        $ExpectedSha = $Matches[1].ToLowerInvariant()
        break
      }
    }
  }
  if (-not $ExpectedSha) {
    throw "Checksum for $Archive not found in SHA256SUMS."
  }

  $ActualSha = (Get-FileHash -LiteralPath $ArchivePath -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($ActualSha -ne $ExpectedSha) {
    throw "SHA-256 mismatch for $Archive (expected $ExpectedSha, got $ActualSha)"
  }
  Write-Host "Checksum verified ($ActualSha)."

  $ExtractDir = Join-Path $TmpDir 'extract'
  Expand-Archive -LiteralPath $ArchivePath -DestinationPath $ExtractDir -Force

  New-Item -ItemType Directory -Force -Path $InstallDir | Out-Null
  $BinDest = Join-Path $InstallDir 'agy-t3-sync.exe'
  Copy-Item -LiteralPath (Join-Path $ExtractDir 'agy-t3-sync.exe') -Destination $BinDest -Force
  Write-Host "Installed $BinDest"

  $UserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
  $Parts = if ($UserPath) { $UserPath -split ';' | Where-Object { $_ } } else { @() }
  if (-not ($Parts | Where-Object { $_.TrimEnd('\') -ieq $InstallDir.TrimEnd('\') })) {
    $NewPath = if ($UserPath) { "$UserPath;$InstallDir" } else { $InstallDir }
    [Environment]::SetEnvironmentVariable('Path', $NewPath, 'User')
    $env:Path = "$env:Path;$InstallDir"
    Write-Host "Added $InstallDir to User PATH."
  }

  Write-Host ''
  & $BinDest doctor

  if (-not $NoAutoInstall) {
    Write-Host ''
    & $BinDest install
  }
} finally {
  if (Test-Path -LiteralPath $TmpDir) {
    Remove-Item -LiteralPath $TmpDir -Recurse -Force -ErrorAction SilentlyContinue
  }
}
