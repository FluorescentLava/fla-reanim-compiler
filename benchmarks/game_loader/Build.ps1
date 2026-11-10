param(
    [string]$SourceRoot = '',
    [string]$OutputDirectory = (Join-Path $env:TEMP ('pvz-game-loader-' + [guid]::NewGuid().ToString('N')))
)
$ErrorActionPreference = 'Stop'
if (-not $SourceRoot) {
    $gameSourceAncestor = Get-Item -LiteralPath $PSScriptRoot
    while ($gameSourceAncestor) {
        if ((Test-Path -LiteralPath (Join-Path $gameSourceAncestor.FullName 'TodLib/Definition.cpp')) -and
            (Test-Path -LiteralPath (Join-Path $gameSourceAncestor.FullName 'TodLib/Reanimator.cpp'))) {
            $SourceRoot = $gameSourceAncestor.FullName
            break
        }
        $gameSourceAncestor = $gameSourceAncestor.Parent
    }
    if (-not $SourceRoot) { throw 'Cannot find game source above this harness; pass -SourceRoot PATH containing TodLib' }
}
$SourceRoot = (Resolve-Path -LiteralPath $SourceRoot).Path
$OutputDirectory = [System.IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
python (Join-Path $PSScriptRoot 'prepare.py') --game-source $SourceRoot --output $OutputDirectory
if ($LASTEXITCODE -ne 0) { throw 'Cannot prepare game loader harness' }
$setup = ''
if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue)) {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) { throw 'Install Visual Studio C++ build tools, or run from its developer shell' }
    $installation = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if (-not $installation) { throw 'Visual Studio C++ build tools not found' }
    $vcvars = Join-Path $installation 'VC/Auxiliary/Build/vcvars64.bat'
    $setup = 'call "' + $vcvars + '" >nul' + "`r`n"
}
$build = '@echo off' + "`r`n" + $setup + 'cl @sources.rsp' + "`r`n" + 'exit /b %errorlevel%' + "`r`n"
[System.IO.File]::WriteAllText((Join-Path $OutputDirectory 'build.cmd'), $build)
Push-Location -LiteralPath $OutputDirectory
try {
    & $env:ComSpec /d /c build.cmd
    if ($LASTEXITCODE -ne 0) { throw 'C++ game loader build failed' }
} finally {
    Pop-Location
}
Write-Output ('Loader: ' + (Join-Path $OutputDirectory 'loader.exe'))
Write-Output ('Provenance: ' + (Join-Path $OutputDirectory 'provenance.json'))
