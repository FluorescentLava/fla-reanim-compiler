function Assert-NativeExecutable {
    param([string]$Path)
    $bytes = [IO.File]::ReadAllBytes($Path)
    if ($bytes.Length -lt 256 -or [Text.Encoding]::ASCII.GetString($bytes, 0, 2) -ne 'MZ') {
        throw 'The application is not a Windows PE executable.'
    }
    $pe = [BitConverter]::ToInt32($bytes, 0x3c)
    if ([BitConverter]::ToUInt32($bytes, $pe) -ne 0x00004550 -or
        [BitConverter]::ToUInt16($bytes, $pe + 4) -ne 0x8664) {
        throw 'The application must be an x64 PE executable.'
    }
    $optional = $pe + 24
    if ([BitConverter]::ToUInt16($bytes, $optional) -ne 0x20b) {
        throw 'The application must use the PE32+ format.'
    }
    # Data directory 14 is IMAGE_DIRECTORY_ENTRY_COM_DESCRIPTOR. A native Rust
    # executable has no CLR header; this check rejects a managed app accidentally
    # copied from an earlier C# build.
    if ([BitConverter]::ToUInt32($bytes, $optional + 112 + 14 * 8) -ne 0) {
        throw 'The release executable unexpectedly has a .NET CLR header.'
    }
    # A .NET apphost is itself a native PE and loads hostfxr dynamically, so its
    # imports/CLR directory alone cannot identify it. Check host payload markers
    # in both encodings used by the Windows host as well.
    foreach ($encoding in @([Text.Encoding]::ASCII, [Text.Encoding]::Unicode)) {
        $payload = $encoding.GetString($bytes)
        if ($payload -match '(?i)hostfxr\.dll|coreclr\.dll|\.runtimeconfig\.json') {
            throw 'The executable unexpectedly contains a .NET apphost/runtime payload.'
        }
    }
    $sectionCount = [BitConverter]::ToUInt16($bytes, $pe + 6)
    $sectionTable = $optional + [BitConverter]::ToUInt16($bytes, $pe + 20)
    function Convert-RvaToOffset([uint32]$Rva) {
        for ($index = 0; $index -lt $sectionCount; $index++) {
            $section = $sectionTable + 40 * $index
            $virtualSize = [BitConverter]::ToUInt32($bytes, $section + 8)
            $address = [BitConverter]::ToUInt32($bytes, $section + 12)
            $rawSize = [BitConverter]::ToUInt32($bytes, $section + 16)
            if ($Rva -ge $address -and $Rva -lt $address + [Math]::Max($virtualSize, $rawSize)) {
                return [int]($Rva - $address + [BitConverter]::ToUInt32($bytes, $section + 20))
            }
        }
        throw "Cannot map PE RVA $Rva"
    }
    $imports = @()
    $importRva = [BitConverter]::ToUInt32($bytes, $optional + 112 + 8)
    if ($importRva -ne 0) {
        $descriptor = Convert-RvaToOffset $importRva
        while ([BitConverter]::ToUInt32($bytes, $descriptor + 12) -ne 0) {
            $nameOffset = Convert-RvaToOffset ([BitConverter]::ToUInt32($bytes, $descriptor + 12))
            $end = $nameOffset
            while ($bytes[$end] -ne 0) { $end++ }
            $imports += [Text.Encoding]::ASCII.GetString($bytes, $nameOffset, $end - $nameOffset)
            $descriptor += 20
        }
    }
    foreach ($name in $imports) {
        if ($name -match '^(mscoree|coreclr|hostfxr|hostpolicy|vcruntime\d+|msvcp\d+).*\.dll$') {
            throw "The application unexpectedly imports an external language runtime: $name"
        }
    }
    return $imports
}
