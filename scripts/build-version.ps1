$ErrorActionPreference = "Stop"
$PSNativeCommandUseErrorActionPreference = $false

$root = Split-Path -Parent $PSScriptRoot
Push-Location -LiteralPath $root
try {

function Get-GitOutput {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$GitArgs
    )

    $output = & git @GitArgs 2>$null
    if ($LASTEXITCODE -ne 0) {
        return $null
    }

    $text = (($output | Out-String) -replace "`r", "").Trim()
    if ([string]::IsNullOrWhiteSpace($text)) {
        return $null
    }

    return $text
}

function Select-VersionTag {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Tags
    )

    $lines = $Tags -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { $_ -ne "" }
    $prefixed = $lines | Where-Object { $_.StartsWith("v") } | Select-Object -First 1
    if ($prefixed) {
        return $prefixed
    }

    return $lines | Select-Object -First 1
}

$metadata = cargo metadata --locked --no-deps --format-version 1 | Out-String | ConvertFrom-Json
$packageVersion = $metadata.packages |
    Where-Object { $_.name -eq "codex-switch" } |
    Select-Object -First 1 -ExpandProperty version
if (-not $packageVersion) {
    throw "failed to resolve codex-switch version from cargo metadata"
}

$fallbackTag = "v$packageVersion"
$exactTag = $null
$tags = Get-GitOutput -GitArgs @("tag", "--points-at", "HEAD")
if ($tags) {
    $exactTag = Select-VersionTag -Tags $tags
}

if ($exactTag) {
    $tag = $exactTag
} else {
    $described = Get-GitOutput -GitArgs @("describe", "--tags", "--abbrev=0", "HEAD")
    if ($described) {
        $tag = $described
    } else {
        $tag = $fallbackTag
    }
}

$commit = Get-GitOutput -GitArgs @("rev-parse", "--short=7", "HEAD")
$dirty = $false
if ($commit) {
    & git diff-index --quiet HEAD -- | Out-Null
    if ($LASTEXITCODE -eq 1) {
        $dirty = $true
    }
}

if (-not $commit) {
    $display = $tag
} elseif ($dirty) {
    $display = "$tag^$commit"
} elseif ($exactTag) {
    $display = $tag
} else {
    $display = "$tag-$commit"
}

$artifact = $display
if ($artifact.StartsWith("v")) {
    $artifact = $artifact.Substring(1)
}

Write-Output $artifact
} finally {
    Pop-Location
}
