param(
    [Parameter(Mandatory)][ValidatePattern('^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$')][string]$Label,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{40}$')][string]$Commit,
    [string]$BinaryPath = 'target/x86_64-pc-windows-msvc/release/endfield-switcher.exe',
    [string]$OutputDirectory = 'dist'
)
$ErrorActionPreference = 'Stop'
$repo = Split-Path $PSScriptRoot -Parent
$output = [IO.Path]::GetFullPath((Join-Path $repo $OutputDirectory))
$binary = [IO.Path]::GetFullPath((Join-Path $repo $BinaryPath))
if (!(Test-Path -LiteralPath $binary -PathType Leaf)) { throw "Build executable not found: $binary" }
New-Item -ItemType Directory -Force $output | Out-Null
$package = Join-Path $output "package-$Label"
New-Item -ItemType Directory -Force $package | Out-Null
$exe = Join-Path $output 'EndfieldSwitcher.exe'
Copy-Item -LiteralPath $binary -Destination $exe -Force
Copy-Item -LiteralPath $binary -Destination (Join-Path $package 'EndfieldSwitcher.exe') -Force
foreach ($name in @('README.md', 'LICENSE', 'RELEASE_NOTES.md', 'config.example.json')) {
    Copy-Item -LiteralPath (Join-Path $repo $name) -Destination $package -Force
}
Copy-Item -LiteralPath (Join-Path $repo 'assets/endfield.ico') -Destination (Join-Path $package 'EndfieldSwitcher.ico') -Force
Copy-Item -LiteralPath (Join-Path $repo 'docs/REVIEW.md') -Destination (Join-Path $package 'REVIEW.md') -Force
$rust = rustc --version
if ($LASTEXITCODE -ne 0) { throw 'Could not read Rust version' }
$build = [ordered]@{ commit = $Commit; label = $Label; target = 'x86_64-pc-windows-msvc'; rust = "$rust" }
$build | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $output 'BUILD_INFO.json') -Encoding utf8NoBOM
Copy-Item -LiteralPath (Join-Path $output 'BUILD_INFO.json') -Destination $package -Force
Copy-Item -LiteralPath (Join-Path $repo 'RELEASE_NOTES.md') -Destination $output -Force
$zip = Join-Path $output "EndfieldSwitcher-$Label-windows-x64.zip"
# Explicit allowlist: never package a user's config, game snapshots or pending journals.
$names = @('EndfieldSwitcher.exe', 'EndfieldSwitcher.ico', 'README.md', 'LICENSE', 'RELEASE_NOTES.md', 'REVIEW.md', 'config.example.json', 'BUILD_INFO.json')
Compress-Archive -LiteralPath ($names | ForEach-Object { Join-Path $package $_ }) -DestinationPath $zip -Force
$checksums = @($exe, $zip, (Join-Path $output 'BUILD_INFO.json')) | ForEach-Object {
    (Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant() + '  ' + [IO.Path]::GetFileName($_)
}
$checksums | Set-Content -LiteralPath (Join-Path $output 'SHA256SUMS.txt') -Encoding utf8NoBOM
Write-Output "Packaged $zip from commit $Commit"
