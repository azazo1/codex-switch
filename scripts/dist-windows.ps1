param(
    [string]$ProfileDir = "target/release"
)

$metadata = cargo metadata --locked --no-deps --format-version 1 | Out-String | ConvertFrom-Json
$Version = $metadata.packages |
    Where-Object { $_.name -eq "codex-switch" } |
    Select-Object -First 1 -ExpandProperty version
if (-not $Version) {
    throw "failed to resolve codex-switch version from cargo metadata"
}

$binary = Join-Path $ProfileDir "codex-switch.exe"
if (-not (Test-Path -LiteralPath $binary -PathType Leaf)) {
    throw "missing Windows binary: $binary"
}

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    "AMD64" { "x86_64" }
    "ARM64" { "aarch64" }
    default { throw "unsupported Windows architecture: $env:PROCESSOR_ARCHITECTURE" }
}

$output = "dist/codex-switch-$Version-windows-$arch.zip"
New-Item -ItemType Directory -Force -Path dist | Out-Null
Compress-Archive -LiteralPath $binary -DestinationPath $output -Force
Write-Output "created $output"
