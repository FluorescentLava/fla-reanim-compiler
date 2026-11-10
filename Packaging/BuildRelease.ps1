param([switch]$SkipBuild, [string]$DestinationDirectory)
$ErrorActionPreference = 'Stop'
$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
if ([string]::IsNullOrWhiteSpace($DestinationDirectory)) {
    $DestinationDirectory = Split-Path -Parent $projectRoot
}
elseif (-not [IO.Path]::IsPathRooted($DestinationDirectory)) {
    $DestinationDirectory = Join-Path $projectRoot $DestinationDirectory
}
$destination = [IO.Path]::GetFullPath($DestinationDirectory)
if ($destination.TrimEnd('\', '/') -eq $projectRoot.TrimEnd('\', '/')) {
    throw 'Choose a release output directory separate from the source repository root.'
}
$cargo = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
if (Get-Command cargo -ErrorAction SilentlyContinue) { $cargo = (Get-Command cargo).Source }
. (Join-Path $PSScriptRoot 'AssertNative.ps1')
$temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/')
$staging = Join-Path $temporaryRoot ('fla-reanim-release-' + [Guid]::NewGuid().ToString('N'))
Push-Location $projectRoot
try {
    if (-not $SkipBuild) {
        & $cargo build --release --locked --bins
        if ($LASTEXITCODE -ne 0) { throw 'Rust release build failed.' }
    }
    $targetDirectory = if ($env:CARGO_TARGET_DIR) { [IO.Path]::GetFullPath($env:CARGO_TARGET_DIR) } else { Join-Path $projectRoot 'target' }
    $executable = Join-Path $targetDirectory 'release\FlaReanimCompiler.exe'
    $cli = Join-Path $targetDirectory 'release\fla-reanim-cli.exe'
    $imports = @(Assert-NativeExecutable $executable)
    $cliImports = @(Assert-NativeExecutable $cli)
    # The default output contains the independent src repository. Assemble and
    # enumerate only an owned staging directory, never the output directory.
    New-Item -ItemType Directory -Path $staging | Out-Null
    Copy-Item -LiteralPath $executable -Destination $staging
    Copy-Item -LiteralPath $cli -Destination $staging
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'README-release.md') -Destination (Join-Path $staging 'README.md')
    Copy-Item -LiteralPath (Join-Path $projectRoot 'LICENSE') -Destination $staging
    & (Join-Path $PSScriptRoot 'PrepareBootstrap.ps1') -DestinationDirectory $staging | Out-Null
    $licenses = Join-Path $staging 'Licenses'
    New-Item -ItemType Directory -Path $licenses -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'naui-windows-application-MIT.txt') -Destination $licenses -Force
    $metadata = (& $cargo metadata --locked --format-version 1 --filter-platform x86_64-pc-windows-msvc | Out-String) | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'Cargo metadata failed.' }
    $notices = foreach ($package in $metadata.packages) {
        "$($package.name) $($package.version): $($package.license) $($package.repository)"
        $root = Split-Path -Parent $package.manifest_path
        $files = @(Get-ChildItem -LiteralPath $root -File | Where-Object Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)([.-]|$)')
        if ($package.license_file) { $files += Get-Item -LiteralPath (Join-Path $root $package.license_file) }
        foreach ($file in $files | Sort-Object FullName -Unique) {
            Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $licenses "$($package.name)-$($package.version)-$($file.Name)") -Force
        }
    }
    $notices | Set-Content -LiteralPath (Join-Path $licenses 'Rust-dependencies.txt') -Encoding utf8
    $files = @(Get-ChildItem -LiteralPath $staging -Recurse -File)
    foreach ($file in $files) {
        if ($file.Name -match '^(coreclr|hostfxr|hostpolicy|Microsoft\.UI\.Xaml)\.dll$' -or $file.Extension -in @('.py','.cs','.winmd','.xbf')) {
            throw "Unexpected bundled runtime or legacy source: $($file.FullName)"
        }
    }
    # ZIP cannot represent pre-1980 license timestamps from some Cargo crates.
    # Normalize only staging entries, also keeping repeated packages comparable.
    foreach ($entry in Get-ChildItem -LiteralPath $staging -Recurse) {
        $entry.LastWriteTime = [datetime]::new(1980, 1, 1)
    }
    $payload = @(Get-ChildItem -LiteralPath $staging | Select-Object -ExpandProperty FullName)
    $zipName = 'FLA-Reanim-Compiler-win-x64.zip'
    $zipPath = Join-Path $staging $zipName
    Compress-Archive -LiteralPath $payload -DestinationPath $zipPath
    $sourceRevision = $null
    $sourceDirty = $null
    if (Get-Command git -ErrorAction SilentlyContinue) {
        $gitRoot = & git -C $projectRoot rev-parse --show-toplevel 2>$null
        if ($LASTEXITCODE -eq 0 -and [IO.Path]::GetFullPath($gitRoot) -eq $projectRoot) {
            $sourceRevision = (& git -C $projectRoot rev-parse HEAD).Trim()
            if ($LASTEXITCODE -ne 0) { throw 'Cannot read source revision.' }
            $sourceChanges = @(& git -C $projectRoot status --porcelain=v1 --untracked-files=normal)
            if ($LASTEXITCODE -ne 0) { throw 'Cannot read source status.' }
            $sourceDirty = $sourceChanges.Count -gt 0
        }
    }
    $manifestPath = Join-Path $staging 'release-manifest.json'
    [pscustomobject]@{
        SourceRepository = 'https://github.com/FluorescentLava/fla-reanim-compiler'
        SourceRevision = $sourceRevision
        SourceDirty = $sourceDirty
        ExecutableBytes = (Get-Item -LiteralPath (Join-Path $staging 'FlaReanimCompiler.exe')).Length
        DistributionBytes = [int64]($files | Measure-Object Length -Sum).Sum
        ZipBytes = (Get-Item -LiteralPath $zipPath).Length
        ExecutableSha256 = (Get-FileHash -LiteralPath (Join-Path $staging 'FlaReanimCompiler.exe')).Hash
        CliBytes = (Get-Item -LiteralPath (Join-Path $staging 'fla-reanim-cli.exe')).Length
        CliSha256 = (Get-FileHash -LiteralPath (Join-Path $staging 'fla-reanim-cli.exe')).Hash
        ZipSha256 = (Get-FileHash -LiteralPath $zipPath).Hash
        Imports = $imports -join ', '
        CliImports = $cliImports -join ', '
    } | ConvertTo-Json | Set-Content -LiteralPath $manifestPath -Encoding utf8
    New-Item -ItemType Directory -Path $destination -Force | Out-Null
    foreach ($file in $files) {
        $relative = $file.FullName.Substring($staging.Length + 1)
        $target = Join-Path $destination $relative
        New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
        Copy-Item -LiteralPath $file.FullName -Destination $target -Force
    }
    Copy-Item -LiteralPath $zipPath -Destination (Join-Path $destination $zipName) -Force
    Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $destination 'release-manifest.json') -Force
    Get-Content -LiteralPath $manifestPath
}
finally {
    Pop-Location
    if (Test-Path -LiteralPath $staging -PathType Container) {
        if ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($staging)) -ne $temporaryRoot -or
            [IO.Path]::GetFileName($staging) -notmatch '^fla-reanim-release-[0-9a-f]{32}$') {
            throw 'Refusing to remove an unexpected staging directory.'
        }
        Remove-Item -LiteralPath $staging -Recurse -Force
    }
}
