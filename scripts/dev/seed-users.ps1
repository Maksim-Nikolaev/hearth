<#
.SYNOPSIS
  Seed the dev users alice (admin) + bob into the backend database.

.DESCRIPTION
  Idempotent bootstrap so the live two-instance test (launch-test.ps1) can log
  in. Point it at the dev Postgres — with the containerised backend running
  (docker compose -f compose.dev.yml up -d) it is exposed on host port 5433.

      .\scripts\dev\seed-users.ps1
      .\scripts\dev\seed-users.ps1 -DatabaseUrl postgres://hearth:hearth@localhost:5433/hearth

  Creates alice/bob with passwords pw-alice / pw-bob.
#>
param(
    [string]$DatabaseUrl = $(if ($env:DATABASE_URL) { $env:DATABASE_URL } else { 'postgres://hearth:hearth@localhost:5433/hearth' })
)

$ErrorActionPreference = 'Stop'
Set-Location (Resolve-Path "$PSScriptRoot\..\..")

$env:DATABASE_URL = $DatabaseUrl
Write-Host "Seeding alice + bob into $DatabaseUrl ..." -ForegroundColor Cyan
cargo run -p hearth-backend --example seed_dev
