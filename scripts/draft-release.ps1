param(
    [Parameter(Mandatory)][ValidatePattern('^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')][string]$Tag,
    [Parameter(Mandatory)][ValidatePattern('^[0-9a-fA-F]{40}$')][string]$Commit,
    [Parameter(Mandatory)][ValidatePattern('^[0-9A-Za-z_.-]+/[0-9A-Za-z_.-]+$')][string]$Repository,
    [string]$ArtifactDirectory = 'dist'
)
$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false
$root = [IO.Path]::GetFullPath($ArtifactDirectory)
$expected = @('EndfieldSwitcher.exe', "EndfieldSwitcher-$Tag-windows-x64.zip", 'BUILD_INFO.json')
foreach ($name in $expected + @('SHA256SUMS.txt', 'RELEASE_NOTES.md')) {
    if (!(Test-Path -LiteralPath (Join-Path $root $name) -PathType Leaf)) { throw "Missing artifact: $name" }
}
$build = Get-Content -LiteralPath (Join-Path $root 'BUILD_INFO.json') -Raw | ConvertFrom-Json
if ($build.commit -ne $Commit -or $build.label -ne $Tag -or $build.target -ne 'x86_64-pc-windows-msvc') {
    throw 'Artifact provenance does not match requested source/tag/target'
}
$lines = @(Get-Content -LiteralPath (Join-Path $root 'SHA256SUMS.txt'))
if ($lines.Count -ne $expected.Count) { throw 'Unexpected checksum list' }
foreach ($name in $expected) {
    $hash = (Get-FileHash -LiteralPath (Join-Path $root $name) -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($lines -cnotcontains "$hash  $name") { throw "Checksum mismatch: $name" }
}

# A pre-existing tag must resolve to the source that was actually built.
$refsJson = gh api "repos/$Repository/git/matching-refs/tags/$Tag"
if ($LASTEXITCODE -ne 0) { throw 'Cannot inspect remote tags' }
$ref = @($refsJson | ConvertFrom-Json) | Where-Object { $_.ref -ceq "refs/tags/$Tag" }
if ($ref) {
    $object = $ref.object
    for ($depth = 0; $object.type -eq 'tag' -and $depth -lt 8; $depth++) {
        $tagJson = gh api "repos/$Repository/git/tags/$($object.sha)"
        if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve annotated tag' }
        $object = ($tagJson | ConvertFrom-Json).object
    }
    if ($object.type -ne 'commit' -or $object.sha -ne $Commit) {
        throw 'Tag points to another commit. Run against that tag or use a new version tag; tags are never rewritten.'
    }
}

$existingJson = gh release view $Tag --repo $Repository --json isDraft,assets 2>$null
$exists = $LASTEXITCODE -eq 0
$existing = if ($exists) { $existingJson | ConvertFrom-Json } else { $null }
if ($exists -and !$existing.isDraft) { throw 'Refusing to overwrite or unpublish an already published release' }
$pre = ($Tag -like '*-*').ToString().ToLowerInvariant()
$notes = Join-Path $root 'RELEASE_NOTES.md'
if ($exists) {
    gh release edit $Tag --repo $Repository --target $Commit --title "$Tag · Windows x64" --notes-file $notes --draft=true "--prerelease=$pre" --latest=false
} else {
    gh release create $Tag --repo $Repository --target $Commit --title "$Tag · Windows x64" --notes-file $notes --draft "--prerelease=$pre" --latest=false
}
if ($LASTEXITCODE -ne 0) { throw 'Could not prepare draft release' }
$assets = @($expected | ForEach-Object { Join-Path $root $_ }) + @((Join-Path $root 'SHA256SUMS.txt'))
gh release upload $Tag @assets --repo $Repository --clobber
if ($LASTEXITCODE -ne 0) { throw 'Could not upload release assets' }

# Only remove obsolete packages owned by this workflow, and only from a draft.
foreach ($old in $existing.assets) {
    if ($old.name -match '^EndfieldSwitcher-.+-windows-x64\.zip$' -and $expected -cnotcontains $old.name) {
        gh release delete-asset $Tag $old.name --repo $Repository --yes
        if ($LASTEXITCODE -ne 0) { throw 'Could not remove obsolete draft package' }
    }
}
$finalJson = gh release view $Tag --repo $Repository --json isDraft,tagName,targetCommitish,assets,url
if ($LASTEXITCODE -ne 0) { throw 'Cannot verify final release state' }
$final = $finalJson | ConvertFrom-Json
if (!$final.isDraft) { throw 'Release unexpectedly published; inspect the release before proceeding' }
foreach ($name in $expected + @('SHA256SUMS.txt')) {
    $asset = $final.assets | Where-Object name -CEQ $name
    if (!$asset -or $asset.state -ne 'uploaded') { throw "Release asset not uploaded: $name" }
}
Write-Output $final.url
if ($env:GITHUB_STEP_SUMMARY) {
    @("## Draft release ready", "", "[$Tag]($($final.url))", "", "Source commit: $Commit", "", 'Download EndfieldSwitcher.exe directly or the ZIP with configuration and documentation. Publish the draft manually when ready.') | Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY
}
