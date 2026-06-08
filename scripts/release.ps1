param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet("major", "minor", "patch")]
    [string] $Level
)

$ErrorActionPreference = "Stop"

function Assert-NativeSuccess([string] $Operation) {
    if ($LASTEXITCODE -ne 0) {
        throw "$Operation failed with exit code $LASTEXITCODE."
    }
}

$branch = git branch --show-current
Assert-NativeSuccess "Reading the current branch"
if ($branch -ne "main") {
    throw "Releases must be created from the main branch."
}
$status = git status --porcelain
Assert-NativeSuccess "Reading the working tree status"
if ($status) {
    throw "The working tree must be clean before creating a release."
}

git fetch origin main
Assert-NativeSuccess "Fetching origin/main"
$head = git rev-parse HEAD
Assert-NativeSuccess "Reading local HEAD"
$originHead = git rev-parse origin/main
Assert-NativeSuccess "Reading origin/main"
if ($head -ne $originHead) {
    throw "Local main must exactly match origin/main."
}

$rootManifest = Get-Content Cargo.toml -Raw
$match = [regex]::Match(
    $rootManifest,
    '(?ms)^\[workspace\.package\]\s+version = "(\d+)\.(\d+)\.(\d+)"'
)
if (-not $match.Success) {
    throw "Could not find workspace.package.version in Cargo.toml."
}

$major = [int] $match.Groups[1].Value
$minor = [int] $match.Groups[2].Value
$patch = [int] $match.Groups[3].Value
switch ($Level) {
    "major" {
        $major++
        $minor = 0
        $patch = 0
    }
    "minor" {
        $minor++
        $patch = 0
    }
    "patch" {
        $patch++
    }
}

$oldVersion = $match.Groups[1].Value + "." +
    $match.Groups[2].Value + "." +
    $match.Groups[3].Value
$newVersion = "$major.$minor.$patch"
$tag = "v$newVersion"

git show-ref --verify --quiet "refs/tags/$tag"
if ($LASTEXITCODE -eq 0) {
    throw "Tag $tag already exists."
}
if ($LASTEXITCODE -ne 1) {
    throw "Checking tag $tag failed with exit code $LASTEXITCODE."
}

$utf8NoBom = [System.Text.UTF8Encoding]::new($false)
$files = @(
    "Cargo.toml",
    "crates/fwob/Cargo.toml",
    "crates/fwob-v1/Cargo.toml",
    "crates/fwob-v2/Cargo.toml"
)
foreach ($file in $files) {
    $content = Get-Content $file -Raw
    $content = $content.Replace(
        "version = `"$oldVersion`"",
        "version = `"$newVersion`""
    )
    [System.IO.File]::WriteAllText(
        (Resolve-Path $file),
        $content,
        $utf8NoBom
    )
}

cargo update --workspace
Assert-NativeSuccess "Updating workspace dependencies"
cargo fmt --all --check
Assert-NativeSuccess "Checking formatting"
cargo test --workspace --all-features --locked
Assert-NativeSuccess "Running tests"
cargo build --workspace --release --locked
Assert-NativeSuccess "Building release binaries"

git add Cargo.toml Cargo.lock crates/fwob/Cargo.toml `
    crates/fwob-v1/Cargo.toml crates/fwob-v2/Cargo.toml
Assert-NativeSuccess "Staging the version update"
git commit -m "Release $newVersion"
Assert-NativeSuccess "Committing the version update"
git tag -a $tag -m "FWOB $newVersion"
Assert-NativeSuccess "Creating tag $tag"
git push --atomic origin main $tag
Assert-NativeSuccess "Pushing the release commit and tag"

Write-Host "Released $tag. GitHub Actions will publish crates and binaries."
