param(
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [string]$ProfileDir = "target/release"
)

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
