[CmdletBinding()]
param(
    [Parameter(Position = 0)]
    [string]$OutZip
)

$ErrorActionPreference = "Stop"

$ScriptDir = $PSScriptRoot
$RepoRoot = (Resolve-Path (Join-Path $ScriptDir "..")).Path

if ([string]::IsNullOrWhiteSpace($OutZip)) {
    $OutZip = Join-Path $ScriptDir "A2_submission.zip"
} elseif (-not [System.IO.Path]::IsPathRooted($OutZip)) {
    $OutZip = [System.IO.Path]::GetFullPath((Join-Path (Get-Location) $OutZip))
}

$StageDir = Join-Path ([System.IO.Path]::GetTempPath()) ("a2_stage_" + [System.IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $StageDir -Force | Out-Null

try {
    # 1. Copy rwetac, architecture.md, Cargo.lock, and original_repo subset
    Copy-Item -Path (Join-Path $RepoRoot "rwetac") -Destination (Join-Path $StageDir "rwetac") -Recurse -Force
    Copy-Item -Path (Join-Path $RepoRoot "architecture.md") -Destination (Join-Path $StageDir "architecture.md") -Force
    Copy-Item -Path (Join-Path $RepoRoot "Cargo.lock") -Destination (Join-Path $StageDir "Cargo.lock") -Force

    $StageOriginalRepoDir = Join-Path $StageDir "original_repo"
    New-Item -ItemType Directory -Path $StageOriginalRepoDir -Force | Out-Null
    Copy-Item -Path (Join-Path $ScriptDir "original_repo\Cargo.toml") -Destination (Join-Path $StageOriginalRepoDir "Cargo.toml") -Force
    Copy-Item -Path (Join-Path $ScriptDir "original_repo\Cargo.lock") -Destination (Join-Path $StageOriginalRepoDir "Cargo.lock") -Force
    Copy-Item -Path (Join-Path $ScriptDir "original_repo\architecture.md") -Destination (Join-Path $StageOriginalRepoDir "architecture.md") -Force
    Copy-Item -Path (Join-Path $ScriptDir "original_repo\rwetac") -Destination (Join-Path $StageOriginalRepoDir "rwetac") -Recurse -Force

    # 2. Create root Cargo.toml with a2_diagnostics as the root binary package
    $CargoToml = @"
[package]
name = "a2_diagnostics"
version = "0.1.0"
edition = "2024"
license.workspace = true

[dependencies]
anyhow = "1.0.99"
rwetac = { path = "rwetac" }

[workspace]
members = [
    "rwetac"
]
resolver = "3"

[workspace.package]
license = "MIT"
"@

    Set-Content -Path (Join-Path $StageDir "Cargo.toml") -Value $CargoToml -Encoding utf8

    # 3. Copy A2/src/main.rs to top-level src/main.rs and A2/README.md to README.md
    $StageSrcDir = Join-Path $StageDir "src"
    New-Item -ItemType Directory -Path $StageSrcDir -Force | Out-Null
    Copy-Item -Path (Join-Path $ScriptDir "src\main.rs") -Destination (Join-Path $StageSrcDir "main.rs") -Force
    Copy-Item -Path (Join-Path $ScriptDir "README.md") -Destination (Join-Path $StageDir "README.md") -Force

    # Copy *.eta examples, resolving any Windows git text symlinks (raw relative paths) or filesystem symlinks
    $StageExamplesDir = Join-Path $StageDir "examples"
    New-Item -ItemType Directory -Path $StageExamplesDir -Force | Out-Null

    Get-ChildItem -Path (Join-Path $ScriptDir "examples") -Filter "*.eta" | ForEach-Object {
        $destFile = Join-Path $StageExamplesDir $_.Name
        $copied = $false

        if (-not $_.PSIsContainer -and $_.Length -lt 256) {
            $firstLine = (Get-Content $_.FullName -TotalCount 1).Trim()
            if ($firstLine -match '^\.\.[\\/]' -or ($firstLine.EndsWith('.eta') -and -not ($firstLine -match '[\{\};]'))) {
                $possibleTarget = Join-Path $_.DirectoryName $firstLine
                if (Test-Path $possibleTarget -PathType Leaf) {
                    Copy-Item -Path $possibleTarget -Destination $destFile -Force
                    $copied = $true
                }
            }
        }

        if (-not $copied) {
            Copy-Item -Path $_.FullName -Destination $destFile -Recurse -Force
        }
    }

    # 4. Build the zip archive
    $OutZipDir = Split-Path -Path $OutZip -Parent
    if (-not [string]::IsNullOrEmpty($OutZipDir) -and -not (Test-Path $OutZipDir)) {
        New-Item -ItemType Directory -Path $OutZipDir -Force | Out-Null
    }
    if (Test-Path $OutZip) {
        Remove-Item -Path $OutZip -Force
    }

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $StageDir,
        $OutZip,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $false
    )

    Write-Host "Created zip archive at: $OutZip"
} finally {
    if (Test-Path $StageDir) {
        Remove-Item -Path $StageDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
