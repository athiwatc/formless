# Manual Windows ZIP installation

This walkthrough follows the **checked-in release workflow**, not a verified
live download or a native Windows acceptance run. It defines a Windows x64
(Intel/AMD64) asset named `formless-<tag>-x86_64-pc-windows-msvc.zip` and a
sibling `.zip.sha256`. Choose an actually available matching ZIP and checksum
from [Releases](https://github.com/athiwatc/formless/releases). Do not infer
that an asset exists from its expected name. Other architectures need a
matching supported release or the [source-build route](usage.md#install).

The ZIP contains a nested directory with the same base name, containing
`formless.exe`, `README.md`, `SPEC.md` and `LICENSE`. The checksum file contains
64 hexadecimal SHA-256 characters, two spaces, and the ZIP filename. Artifacts
are documented as unsigned. A matching colocated checksum detects a mismatched
or corrupt download; it is not an independent publisher-signature guarantee.

## Verify before extraction or execution

Download both files into the same folder without renaming them. In PowerShell,
enter the full path to the ZIP you actually downloaded, without surrounding
quote characters in the prompt response:

```powershell
$zip = Get-Item -LiteralPath (Read-Host 'Full path to the downloaded ZIP') -ErrorAction Stop
$checksumFile = $zip.FullName + '.sha256'
$line = (Get-Content -LiteralPath $checksumFile -Raw -ErrorAction Stop).Trim()
$record = [regex]::Match($line, '\A([0-9a-fA-F]{64})  (.+)\z')
if (-not $record.Success -or $record.Groups[2].Value -cne $zip.Name) {
    throw 'Checksum record is malformed or names a different ZIP. Stop.'
}
$actual = (Get-FileHash -LiteralPath $zip.FullName -Algorithm SHA256 -ErrorAction Stop).Hash
if ($actual -ine $record.Groups[1].Value) {
    throw 'SHA-256 mismatch. Stop; do not extract or run this download.'
}
```

Stop on a mismatch or any error. Obtain matching files from the intended
release before continuing; do not treat an error as successful verification.

## Extract into a user-owned directory

Continue in the same PowerShell session after verification succeeds. This
uses a version-specific extraction directory under your local application data
and refuses to merge with an existing destination:

```powershell
$installRoot = Join-Path $env:LOCALAPPDATA 'Programs\Formless\manual'
New-Item -ItemType Directory -Path $installRoot -Force -ErrorAction Stop | Out-Null
$extract = Join-Path $installRoot ($zip.BaseName + '-unpacked')
if (Test-Path -LiteralPath $extract) {
    throw 'Extraction destination already exists. Choose a new empty destination.'
}
Expand-Archive -LiteralPath $zip.FullName -DestinationPath $extract -ErrorAction Stop
$binDir = Join-Path $extract $zip.BaseName
$exe = Join-Path $binDir 'formless.exe'
if (-not (Test-Path -LiteralPath $exe -PathType Leaf)) {
    throw 'Expected nested formless.exe was not found. Inspect the archive layout.'
}
& "$exe" --version
```

`$binDir`, not `$extract`, is the directory containing the executable. The
PowerShell call operator `&` invokes the quoted path correctly when it has
spaces. `--version` is the initial local binary check. Running `& "$exe"`
without arguments starts the setup wizard. `& "$exe" doctor` is optional
configured-provider diagnostics and can contact providers; it is not just a
local version check.

Windows may display warnings for unsigned downloads. If OS or organization
policy blocks execution, follow supported Windows/organization guidance.
This guide does not change security policy or guarantee installation succeeds
on every Windows system.

## Optional per-user PATH

Direct invocation above needs no PATH change. To make this verified directory
available by name, append it to the **user** PATH while retaining the existing
value and entries:

```powershell
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $binDir) {
    if ([string]::IsNullOrEmpty($userPath)) {
        $newUserPath = $binDir
    } elseif ($userPath.EndsWith(';')) {
        $newUserPath = $userPath + $binDir
    } else {
        $newUserPath = $userPath + ';' + $binDir
    }
    [Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
}
```

Open a fresh terminal from Start, then check `Get-Command formless` and
`formless --version`. Existing terminals keep their prior environment; confirm
that the resolved executable is the intended one if several copies exist.
No administrator or machine-wide PATH change is needed.

## Manual updates

WinGet-installed binaries get the `winget upgrade` command, and Cargo
installations get the Cargo upgrade command. Build outputs are directed to
update their source checkout and rebuild with `cargo run`. Manual ZIP
installations are directed to the verified ZIP procedure below.

`formless update check` reports releases; it does not replace the installed
binary. For a manual update, choose a newer actually available matching release
and repeat checksum verification and extraction into a new directory. Close
clients and stop the relevant running daemon instances before replacing the
installed executable or switching the selected PATH entry. See the existing [daemon controls](usage.md#daemon-diagnostics-and-reset).
Use the same `--profile NAME` when managing an instance belonging to a named profile.

If PATH points to a version-specific directory, replace only that old entry
with the newly verified executable directory, preserving other entries. Open
a fresh terminal and verify its resolved path and version. Do not overwrite
configuration/data as part of replacing the binary. Winget publication is
separate; templates alone do not establish that `winget install` is available.

If Windows rejects a long profile data directory, `formless doctor` reports
the path length alongside the directory or database error. A shorter
`FORMLESS_DATA_DIR` can avoid the Windows path limit.

PowerShell reference: [Get-FileHash](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.utility/get-filehash),
[Expand-Archive](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.archive/expand-archive),
and [per-user environment variables](https://learn.microsoft.com/en-us/dotnet/api/system.environment.setenvironmentvariable).
