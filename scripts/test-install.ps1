# Exercises scripts/install.ps1 end to end without touching the network, the
# real PATH or anything outside a scratch directory: the channel is a directory
# served by an HttpListener on the loopback interface, and the "binary" is a
# stand-in that prints a version.
#
#   powershell -NoProfile -File scripts\test-install.ps1     # Windows PowerShell 5.1
#   pwsh -NoProfile -File scripts/test-install.ps1           # PowerShell 7
#
# ci.yml runs it under both, because that is the split install.ps1 has to cope
# with: 5.1 is what `powershell` opens on a stock Windows 11 and is where the
# TLS default, the progress bar and the stderr-is-an-error behavior bite.
#
# It also runs on macOS and Linux under pwsh, which is how it is developed -
# there is no Windows to hand. The cases that cannot mean anything there (the
# registry PATH, above all) say so and are skipped rather than quietly passing.
# $env:OS, $env:PROCESSOR_ARCHITECTURE and $env:LOCALAPPDATA are what
# install.ps1 reads to decide where it is, so a case can set them the way
# test-install.sh shims `uname`.
#
# The installer is run the way a user runs it - the text piped into
# Invoke-Expression - in a child shell of the same version as this one, so a
# `throw` shows up here as the exit status it gives a caller.

[CmdletBinding()]
param()

Set-StrictMode -Version 3.0
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$RepoDir = Split-Path -Parent $PSScriptRoot
$Installer = Join-Path $RepoDir 'scripts/install.ps1'
if (-not (Test-Path -LiteralPath $Installer)) {
    throw "cannot find scripts/install.ps1 next to $PSCommandPath"
}

# The marker install.ps1 sends, taken from the installer rather than written
# down again: the case that asserts it asserts what the installer actually
# sends. The pattern pins the product token; the version is whatever the
# installer says, and test-install.sh is what checks install.sh agrees.
$InstallerUa = [regex]::Match(
    (Get-Content -Raw -LiteralPath $Installer),
    "\`$UserAgent = '(mapbox-cli-install/\d+)'"
).Groups[1].Value
if (-not $InstallerUa) {
    throw "cannot find the mapbox-cli-install/<n> User-Agent in $Installer"
}

$OnWindows = $env:OS -eq 'Windows_NT'
$Target = 'x86_64-pc-windows-msvc'
$Arm64Target = 'aarch64-pc-windows-msvc'
$Version = 'v9.9.9'
$PinnedVersion = 'v0.1.0-dev.abc1234'
$BasicAuth = 'channel:secret'

# The shell running this, so 5.1 tests 5.1 and 7 tests 7.
$Shell = (Get-Process -Id $PID).Path

# The cases run with a PATH of their own: a real `mapbox` in the caller's PATH
# would otherwise decide the answer to the shadowing cases.
if ($OnWindows) {
    $SafePath = @(
        (Join-Path $env:SystemRoot 'system32'),
        $env:SystemRoot,
        (Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0'),
        (Split-Path -Parent $Shell)
    ) -join ';'
} else {
    $SafePath = '/usr/bin:/bin:/usr/sbin:/sbin'
}

$script:Failures = 0
$script:Skipped = 0
$script:Output = ''
$script:Status = 0
$script:CaseDir = ''
$script:BinDir = ''
$script:Dumped = $false

$Root = Join-Path ([IO.Path]::GetTempPath()) ('mapbox-cli-tests.' + [IO.Path]::GetRandomFileName())
New-Item -ItemType Directory -Path $Root -Force | Out-Null

# --- assertions ------------------------------------------------------------

function Start-Case([string]$Name) { Write-Host ''; Write-Host $Name }
function Pass([string]$Label) { Write-Host "  ok    $Label" }
function Fail([string]$Label) {
    Write-Host "  FAIL  $Label"
    $script:Failures++
    # Once per case, and only when something has gone wrong: without it a
    # failure that is really "the child never ran" reads as every assertion
    # missing its needle, which is the same output as a wording change.
    if (-not $script:Dumped) {
        $script:Dumped = $true
        Write-Host "        --- what the installer printed (exit $($script:Status)) ---"
        if ($script:Output.Trim()) {
            foreach ($line in $script:Output -split "`r?`n") { Write-Host "        | $line" }
        } else {
            Write-Host '        | (nothing)'
        }
        Write-Host '        ---'
    }
}
function Skip([string]$Label) {
    Write-Host "  --    skipped: $Label"
    $script:Skipped++
}

function Expect-Status([int]$Want, [string]$Label) {
    if ($script:Status -eq $Want) { Pass $Label } else { Fail "$Label (exit $($script:Status))" }
}

function Expect-Out([string]$Needle, [string]$Label) {
    if ($script:Output -like "*$Needle*") { Pass $Label } else { Fail "$Label (not in output: $Needle)" }
}

function Expect-NoOut([string]$Needle, [string]$Label) {
    if ($script:Output -like "*$Needle*") { Fail "$Label (in output: $Needle)" } else { Pass $Label }
}

function Expect-File([string]$Path, [string]$Label) {
    if (Test-Path -LiteralPath $Path) { Pass $Label } else { Fail "$Label ($Path)" }
}

function Expect-NoFile([string]$Path, [string]$Label) {
    if (Test-Path -LiteralPath $Path) { Fail "$Label ($Path)" } else { Pass $Label }
}

function Expect-Equal([string]$Want, [string]$Got, [string]$Label) {
    if ($Want -eq $Got) { Pass $Label } else { Fail "$Label (wanted '$Want', got '$Got')" }
}

# --- channel fixtures ------------------------------------------------------

# A stand-in for the binary: something that prints a version when run, and on
# Windows something a Windows loader will actually load - a text file with an
# .exe name is what the "checksums out and then will not run" case is made of,
# so it cannot be what the working one is made of too. csc.exe ships with the
# .NET Framework and is on every Windows.
function New-FakeBinary([string]$Path, [string]$VersionText) {
    # csc.exe and chmod are native commands, and in Windows PowerShell a
    # native command's stderr is a terminating error while EAP is Stop.
    $ErrorActionPreference = 'Continue'
    if ($OnWindows) {
        $csc = Join-Path $env:SystemRoot 'Microsoft.NET\Framework64\v4.0.30319\csc.exe'
        if (-not (Test-Path -LiteralPath $csc)) {
            $csc = Join-Path $env:SystemRoot 'Microsoft.NET\Framework\v4.0.30319\csc.exe'
        }
        if (-not (Test-Path -LiteralPath $csc)) { throw 'no csc.exe to build the stand-in binary with' }
        $source = "$Path.cs"
        Set-Content -LiteralPath $source -Encoding ASCII -Value @"
class Program { static void Main() { System.Console.WriteLine("$VersionText"); } }
"@
        & $csc /nologo "/out:$Path" $source | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "csc.exe could not build the stand-in binary" }
        Remove-Item -LiteralPath $source -Force
    } else {
        Set-Content -LiteralPath $Path -Value "#!/bin/sh`necho '$VersionText'"
        & /bin/chmod '+x' $Path
    }
}

# Compress-Archive is what the release pipeline uses, and on Unix it drops the
# executable bit that the installer's "does it run?" check depends on - hence
# /usr/bin/zip when this is being developed away from Windows.
function New-Zip([string]$File, [string]$Zip) {
    if ($OnWindows) {
        Compress-Archive -Path $File -DestinationPath $Zip -Force
    } else {
        $dir = Split-Path -Parent $File
        Push-Location $dir
        try {
            & /usr/bin/zip -q -X $Zip (Split-Path -Leaf $File)
            if ($LASTEXITCODE -ne 0) { throw 'zip could not build the fixture archive' }
        } finally {
            Pop-Location
        }
    }
}

function Get-Sha256([string]$Path) {
    return (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash.ToLowerInvariant()
}

# One version directory of a channel: the archive, manifest.json and
# SHA256SUMS, the same three objects a real channel carries. -Dir and
# -Version differ for the same reason they differ in a real channel - `latest`
# is a copy of a version directory, and still names the release inside it.
#
#   -BadSha        the manifest promises a digest the archive does not have
#   -Broken        the archive holds a mapbox.exe that will not run
#   -Empty         the archive holds something that is not mapbox.exe
#   -ArtifactName  which target the manifest files it under
function New-ChannelVersion {
    param(
        [string]$Root,
        [string]$Dir,
        [string]$Version = $script:Version,
        [string]$VersionText = 'mapbox 9.9.9',
        [string]$ArtifactName = $Target,
        [switch]$BadSha,
        [switch]$Broken,
        [switch]$Empty
    )

    $dir = Join-Path $Root $Dir
    New-Item -ItemType Directory -Path $dir -Force | Out-Null

    $stage = Join-Path $dir 'stage'
    New-Item -ItemType Directory -Path $stage -Force | Out-Null
    $payload = Join-Path $stage 'mapbox.exe'
    if ($Empty) {
        $payload = Join-Path $stage 'README.txt'
        Set-Content -LiteralPath $payload -Value 'not a binary'
    } elseif ($Broken) {
        # Something that checksums out and then does not answer --version. On
        # Windows that is a text file with an .exe name: the loader refuses it.
        # It cannot be one here - PowerShell hands a file it cannot execute to
        # the platform's default application for the extension, and on a Mac
        # that pops open an archive utility - so on Unix it is an executable
        # that exits 1 instead. Same thing from the installer's side.
        if ($OnWindows) {
            Set-Content -LiteralPath $payload -Value 'this is not a Windows executable'
        } else {
            Set-Content -LiteralPath $payload -Value "#!/bin/sh`nexit 1"
            & /bin/chmod '+x' $payload
        }
    } else {
        New-FakeBinary $payload $VersionText
    }

    $file = "mapbox-$Version-$ArtifactName.zip"
    $zip = Join-Path $dir $file
    New-Zip $payload $zip
    Remove-Item -LiteralPath $stage -Recurse -Force

    $sha = Get-Sha256 $zip
    $claimed = $sha
    if ($BadSha) { $claimed = 'deadbeef' + $sha.Substring(8) }

    $manifest = [ordered]@{
        version   = $Version.TrimStart('v')
        commit    = '0000000000000000000000000000000000000000'
        released  = '2026-01-01T00:00:00Z'
        artifacts = [ordered]@{ $ArtifactName = [ordered]@{ file = $file; sha256 = $claimed } }
    }
    Set-Content -LiteralPath (Join-Path $dir 'manifest.json') -Value ($manifest | ConvertTo-Json -Depth 5)
    Set-Content -LiteralPath (Join-Path $dir 'SHA256SUMS') -Value "$sha  $file"
}

# --- the channel server ----------------------------------------------------

$ServerScript = {
    param([string]$Prefix, [string]$Root, [string]$BasicAuth, [string]$RequestLog)

    $listener = New-Object System.Net.HttpListener
    $listener.Prefixes.Add($Prefix)
    $listener.Start()

    $expected = ''
    if ($BasicAuth) {
        $expected = 'Basic ' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($BasicAuth))
    }

    while ($true) {
        $context = $listener.GetContext()
        $request = $context.Request
        $response = $context.Response
        try {
            # Before the gate, so a gated server can still be shut down.
            if ($request.Url.AbsolutePath -eq '/__stop__') {
                $response.StatusCode = 200
                $response.Close()
                break
            }
            # Recorded before the gate, so a request that is answered 401 is
            # still on the record - the manifest request is the only trace an
            # install leaves when it never gets as far as an artifact.
            if ($RequestLog) {
                $agent = $request.Headers['User-Agent']
                if (-not $agent) { $agent = '-' }
                [IO.File]::AppendAllText($RequestLog, "$($request.Url.AbsolutePath) $agent`n")
            }
            if ($expected -and $request.Headers['Authorization'] -ne $expected) {
                $response.StatusCode = 401
                $response.AddHeader('WWW-Authenticate', 'Basic realm="mapbox-cli"')
                $response.Close()
                continue
            }
            $path = Join-Path $Root $request.Url.AbsolutePath.TrimStart('/')
            if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
                $response.StatusCode = 404
                $response.Close()
                continue
            }
            $bytes = [IO.File]::ReadAllBytes($path)
            $response.StatusCode = 200
            if ($path.EndsWith('.json')) { $response.ContentType = 'application/json' }
            else { $response.ContentType = 'application/octet-stream' }
            $response.ContentLength64 = $bytes.Length
            $response.OutputStream.Write($bytes, 0, $bytes.Length)
            $response.Close()
        } catch {
            try { $response.Abort() } catch { }
        }
    }
    $listener.Stop()
}

function Get-FreePort {
    $probe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $probe.Start()
    $port = ([System.Net.IPEndPoint]$probe.LocalEndpoint).Port
    $probe.Stop()
    return $port
}

function Start-ChannelServer([string]$Root, [string]$BasicAuth, [string]$RequestLog = '') {
    $port = Get-FreePort
    $prefix = "http://127.0.0.1:$port/"

    $runspace = [runspacefactory]::CreateRunspace()
    $runspace.Open()
    $shell = [powershell]::Create()
    $shell.Runspace = $runspace
    [void]$shell.AddScript($ServerScript).AddArgument($prefix).AddArgument($Root).AddArgument($BasicAuth).AddArgument($RequestLog)
    $handle = $shell.BeginInvoke()

    # Up, or dead with a reason. HttpListener wants a URL reservation on
    # Windows for anything but an elevated process, and "access is denied" out
    # of a background runspace is otherwise a silent hang.
    $deadline = (Get-Date).AddSeconds(10)
    while ((Get-Date) -lt $deadline) {
        if ($handle.IsCompleted) {
            $reason = ($shell.Streams.Error | ForEach-Object { $_.ToString() }) -join '; '
            throw "the channel server stopped before it served anything: $reason"
        }
        $client = New-Object System.Net.Sockets.TcpClient
        try {
            $client.Connect('127.0.0.1', $port)
            $client.Close()
            return [pscustomobject]@{
                BaseUrl    = "http://127.0.0.1:$port"
                PowerShell = $shell
                Runspace   = $runspace
            }
        } catch {
            Start-Sleep -Milliseconds 100
        } finally {
            $client.Dispose()
        }
    }
    throw 'the channel server did not come up'
}

function Stop-ChannelServer($Server) {
    try {
        Invoke-WebRequest -Uri "$($Server.BaseUrl)/__stop__" -UseBasicParsing -TimeoutSec 5 | Out-Null
    } catch {
        # Already gone, or wedged; the runspace is torn down either way.
    }
    try { $Server.PowerShell.Stop() } catch { }
    try { $Server.PowerShell.Dispose() } catch { }
    try { $Server.Runspace.Dispose() } catch { }
}

# --- running install.ps1 ---------------------------------------------------

# One case's environment: a scratch install directory, a PATH with no mapbox in
# it, and the variables install.ps1 reads set to a known state. PATH is left
# alone by default - MAPBOX_NO_MODIFY_PATH - so a test run cannot edit the
# registry of the machine it runs on; the case that does opts in and puts it
# back.
function New-CaseEnv([string]$Name) {
    $script:CaseDir = Join-Path $Root "case-$Name"
    New-Item -ItemType Directory -Path $script:CaseDir -Force | Out-Null
    $script:BinDir = Join-Path $script:CaseDir 'bin'
    New-Item -ItemType Directory -Path $script:BinDir -Force | Out-Null
    $script:Dumped = $false

    $env:PATH = $SafePath
    $env:MAPBOX_CLI_BASE_URL = $script:BaseUrl
    $env:MAPBOX_INSTALL_DIR = $script:BinDir
    $env:MAPBOX_NO_MODIFY_PATH = '1'
    Clear-Env 'MAPBOX_CLI_VERSION'
    Clear-Env 'MAPBOX_CLI_AUTH'
    Clear-Env 'MAPBOX_TILESETS_CLI'
    Clear-Env 'MAPBOX_CLI_INSTALL_SOURCE'
    # A developer with either of these set in their own shell would otherwise
    # turn every marker case into a failure that looks like the marker broke.
    Clear-Env 'DISABLE_TELEMETRY'
    Clear-Env 'MAPBOX_CLI_NO_TELEMETRY'
    # What install.ps1 reads to decide where it is running. Set explicitly so
    # the same case means the same thing on Windows and on the machine this is
    # written on.
    $env:OS = 'Windows_NT'
    $env:PROCESSOR_ARCHITECTURE = 'AMD64'
    Clear-Env 'PROCESSOR_ARCHITEW6432'
}

# Unset, never `$env:X = ''`. Windows PowerShell deletes a variable assigned
# an empty string; PowerShell 7 keeps it, with an empty value - and an empty
# PROCESSOR_ARCHITEW6432 is worse than none, because Windows' WoW64 handling
# copies it over PROCESSOR_ARCHITECTURE in the child, which then starts with no
# architecture at all. That difference is invisible until it is a Windows
# PowerShell leg passing and a PowerShell 7 leg failing every case at once.
function Clear-Env([string]$Name) {
    Remove-Item -LiteralPath "env:$Name" -ErrorAction SilentlyContinue
}

function Invoke-Installer([string]$Path = $Installer) {
    # stderr is merged in, the way `>out 2>&1` merges it for test-install.sh -
    # and Continue rather than Stop, because a child writing to stderr is a
    # terminating NativeCommandError in Windows PowerShell otherwise.
    $ErrorActionPreference = 'Continue'
    $command = "Get-Content -Raw -LiteralPath '$Path' | Invoke-Expression"
    $output = & $Shell -NoProfile -NonInteractive -Command $command 2>&1
    $script:Status = $LASTEXITCODE
    $script:Output = ($output | Out-String)
}

# --- the cases -------------------------------------------------------------

$openRoot = Join-Path $Root 'channel'
$gatedRoot = Join-Path $Root 'gated-channel'
$brokenRoot = Join-Path $Root 'broken-channel'
$foreignRoot = Join-Path $Root 'foreign-channel'
$armRoot = Join-Path $Root 'arm-channel'
$emptyRoot = Join-Path $Root 'empty-channel'

$badShaRoot = Join-Path $Root 'bad-sha-channel'

# Beside the channels rather than in one, and nothing ever requests it: this is
# where the open server writes the path and User-Agent of every request it is
# handed, which is the only way to see a header the installer sent.
$RequestLog = Join-Path $Root 'requests.log'

New-ChannelVersion -Root $openRoot -Dir 'latest'
New-ChannelVersion -Root $openRoot -Dir $PinnedVersion -Version $PinnedVersion -VersionText 'mapbox 0.1.0-dev.abc1234'
New-ChannelVersion -Root $gatedRoot -Dir 'latest'
New-ChannelVersion -Root $brokenRoot -Dir 'latest' -Broken
New-ChannelVersion -Root $foreignRoot -Dir 'latest' -ArtifactName 'aarch64-apple-darwin'
New-ChannelVersion -Root $armRoot -Dir 'latest' -ArtifactName $Arm64Target -VersionText 'mapbox 9.9.9-arm64'
New-ChannelVersion -Root $emptyRoot -Dir 'latest' -Empty
New-ChannelVersion -Root $badShaRoot -Dir 'latest' -BadSha

$open = $null
$gated = $null
try {
    $open = Start-ChannelServer $Root '' $RequestLog
    $gated = Start-ChannelServer $gatedRoot $BasicAuth
    $script:BaseUrl = "$($open.BaseUrl)/channel"

    Write-Host "install.ps1 against $($open.BaseUrl)"
    Write-Host "  shell   $Shell ($($PSVersionTable.PSVersion))"
    Write-Host "  target  $Target"

    # --- installing the binary --------------------------------------------

    Start-Case 'installs the binary the manifest names'
    New-CaseEnv 'happy'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-File (Join-Path $script:BinDir 'mapbox.exe') 'mapbox.exe is in the install dir'
    Expect-Out 'Installed mapbox 9.9.9' 'reports the version it ran, not the one it was promised'
    Expect-Out (Join-Path $script:BinDir 'mapbox.exe') 'reports the path'
    Expect-Out 'channel  latest (9.9.9)' 'names the channel and the version it resolved'
    Expect-NoOut '.mapbox.install.' 'leaves no staging file behind'
    $staging = Get-ChildItem -LiteralPath $script:BinDir -Force | Where-Object { $_.Name -ne 'mapbox.exe' }
    Expect-Equal '' ([string]($staging | ForEach-Object { $_.Name })) 'nothing else is left in the install dir'

    Start-Case 'every request it makes says it came from the installer'
    New-CaseEnv 'user-agent'
    $env:MAPBOX_CLI_INSTALL_SOURCE = 'onboarding script; rm -rf /'
    [IO.File]::WriteAllText($RequestLog, '')
    Invoke-Installer
    Expect-Status 0 'exits 0'
    $logged = @()
    if (Test-Path -LiteralPath $RequestLog) {
        $logged = @([IO.File]::ReadAllLines($RequestLog) | Where-Object { $_ })
    }
    # Every request, not most: an unmarked one is a download that cannot be
    # attributed. Both of them are named, so a run that stopped making one
    # fails here rather than passing on the strength of the other.
    $marked = @($logged | Where-Object { $_ -like "* $InstallerUa ($Target)*" })
    $tagged = @($logged | Where-Object { $_ -like '*src/onboardingscriptrm-rf' })
    Expect-Equal '2' ([string]$logged.Count) 'made the two requests a run makes: the manifest and the archive'
    Expect-Equal '2' ([string]$marked.Count) 'both name the installer and the triple'
    Expect-Equal '2' ([string]$tagged.Count) 'both carry the source tag, sanitized for a header'
    Clear-Env 'MAPBOX_CLI_INSTALL_SOURCE'

    Start-Case 'DISABLE_TELEMETRY keeps it down to the product token'
    New-CaseEnv 'telemetry-off'
    $env:MAPBOX_CLI_INSTALL_SOURCE = 'dockerfile'
    $env:DISABLE_TELEMETRY = '1'
    [IO.File]::WriteAllText($RequestLog, '')
    Invoke-Installer
    Expect-Status 0 'exits 0 - the install is not what is being switched off'
    $logged = @([IO.File]::ReadAllLines($RequestLog) | Where-Object { $_ })
    # The whole agent, matched to its end: a request that still carried a
    # triple or a tag would not match, which is why this asserts the suffix
    # rather than searching for the token.
    $bare = @($logged | Where-Object { $_ -like "* $InstallerUa" })
    Expect-Equal '2' ([string]$logged.Count) 'still made both requests'
    Expect-Equal '2' ([string]$bare.Count) 'each carries the product token and nothing else'
    # The other half, and the reason a bare "is it set" check will not do: 0 is
    # someone declining the opt-out, not taking it.
    $env:DISABLE_TELEMETRY = '0'
    [IO.File]::WriteAllText($RequestLog, '')
    Invoke-Installer
    $logged = @([IO.File]::ReadAllLines($RequestLog) | Where-Object { $_ })
    $marked = @($logged | Where-Object { $_ -like "* $InstallerUa ($Target) src/dockerfile" })
    Expect-Equal '2' ([string]$marked.Count) 'DISABLE_TELEMETRY=0 is not an opt-out'
    Clear-Env 'DISABLE_TELEMETRY'
    Clear-Env 'MAPBOX_CLI_INSTALL_SOURCE'

    Start-Case 'MAPBOX_CLI_NO_TELEMETRY is honored, and outranks the old name'
    New-CaseEnv 'telemetry-new-name'
    $env:MAPBOX_CLI_INSTALL_SOURCE = 'dockerfile'
    # The documented name, which the binary reads and this script honors too.
    $env:MAPBOX_CLI_NO_TELEMETRY = '1'
    [IO.File]::WriteAllText($RequestLog, '')
    Invoke-Installer
    Expect-Status 0 'exits 0 - the install is not what is being switched off'
    $logged = @([IO.File]::ReadAllLines($RequestLog) | Where-Object { $_ })
    $bare = @($logged | Where-Object { $_ -like "* $InstallerUa" })
    Expect-Equal '2' ([string]$logged.Count) 'still made both requests'
    Expect-Equal '2' ([string]$bare.Count) 'each carries the product token and nothing else'
    # Both set and disagreeing. The new name is the documented one, so an
    # explicit 0 on it beats a DISABLE_TELEMETRY=1 left in an image from before
    # the rename - otherwise the old variable could never be retired.
    $env:MAPBOX_CLI_NO_TELEMETRY = '0'
    $env:DISABLE_TELEMETRY = '1'
    [IO.File]::WriteAllText($RequestLog, '')
    Invoke-Installer
    $logged = @([IO.File]::ReadAllLines($RequestLog) | Where-Object { $_ })
    $marked = @($logged | Where-Object { $_ -like "* $InstallerUa ($Target) src/dockerfile" })
    Expect-Equal '2' ([string]$marked.Count) 'the new name wins when the two disagree'
    # And the other direction, so precedence is pinned rather than implied.
    $env:MAPBOX_CLI_NO_TELEMETRY = '1'
    $env:DISABLE_TELEMETRY = '0'
    [IO.File]::WriteAllText($RequestLog, '')
    Invoke-Installer
    $logged = @([IO.File]::ReadAllLines($RequestLog) | Where-Object { $_ })
    $bare = @($logged | Where-Object { $_ -like "* $InstallerUa" })
    Expect-Equal '2' ([string]$bare.Count) 'and wins in the opt-out direction too'
    Clear-Env 'MAPBOX_CLI_NO_TELEMETRY'
    Clear-Env 'DISABLE_TELEMETRY'
    Clear-Env 'MAPBOX_CLI_INSTALL_SOURCE'

    Start-Case 'a checksum that does not match installs nothing'
    New-CaseEnv 'bad-sha'
    $env:MAPBOX_CLI_BASE_URL = "$($open.BaseUrl)/bad-sha-channel"
    Invoke-Installer
    Expect-Status 1 'exits 1'
    Expect-Out 'checksum mismatch' 'says the checksum did not match'
    Expect-Out 'deadbeef' 'shows what it expected'
    Expect-Out 'nothing was installed' 'says nothing was installed'
    Expect-NoFile (Join-Path $script:BinDir 'mapbox.exe') 'and nothing was'

    Start-Case 'an artifact that checksums out and then will not run'
    New-CaseEnv 'broken-binary'
    $env:MAPBOX_CLI_BASE_URL = "$($open.BaseUrl)/broken-channel"
    Invoke-Installer
    Expect-Status 1 'exits 1'
    Expect-Out 'it does not run here' 'checks by running --version rather than assuming'
    Expect-Out $Target 'names the target it resolved'

    Start-Case 'an archive with no mapbox.exe in it'
    New-CaseEnv 'empty-archive'
    $env:MAPBOX_CLI_BASE_URL = "$($open.BaseUrl)/empty-channel"
    Invoke-Installer
    Expect-Status 1 'exits 1'
    Expect-Out 'does not contain a mapbox.exe' 'says what the archive is missing'
    Expect-NoFile (Join-Path $script:BinDir 'mapbox.exe') 'nothing was installed'

    Start-Case 'a manifest with no artifact for this machine'
    New-CaseEnv 'no-artifact'
    $env:MAPBOX_CLI_BASE_URL = "$($open.BaseUrl)/foreign-channel"
    Invoke-Installer
    Expect-Status 1 'exits 1'
    Expect-Out "lists no artifact for $Target" 'names the target'
    Expect-NoFile (Join-Path $script:BinDir 'mapbox.exe') 'nothing was installed'

    Start-Case 'a channel that is not there'
    New-CaseEnv 'missing-channel'
    $env:MAPBOX_CLI_BASE_URL = "$($open.BaseUrl)/nowhere"
    Invoke-Installer
    Expect-Status 1 'exits 1'
    Expect-Out 'could not read' 'says which URL it could not read'
    Expect-Out 'nowhere/latest/manifest.json' 'and names it'

    Start-Case 'MAPBOX_CLI_VERSION pins a version directory'
    New-CaseEnv 'pinned'
    $env:MAPBOX_CLI_VERSION = $PinnedVersion
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'Installed mapbox 0.1.0-dev.abc1234' 'installs that exact version'
    Expect-Out "channel  $PinnedVersion" 'names the channel it resolved'

    # The channel's directories carry a leading `v`. Every place a person
    # reads a version from — `mapbox --version`, CHANGELOG.md, Cargo.toml —
    # shows it without one, so the spelling somebody copies has to work. It
    # used to 403, which reads as "not allowed" rather than "no such version".
    Start-Case 'MAPBOX_CLI_VERSION accepts a version without the leading v'
    New-CaseEnv 'pinned-bare'
    $env:MAPBOX_CLI_VERSION = $PinnedVersion -replace '^v', ''
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'Installed mapbox 0.1.0-dev.abc1234' 'installs that exact version'
    Expect-Out "channel  $PinnedVersion" 'and resolved the v-prefixed directory'

    # `latest` starts with a letter, so nothing is prepended. Getting this
    # wrong would break the default install rather than an edge case.
    Start-Case 'a channel name that is not a version is left alone'
    New-CaseEnv 'pinned-latest'
    $env:MAPBOX_CLI_VERSION = 'latest'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'channel  latest' 'asked for latest, not vlatest'

    Start-Case 'reinstalling reports the version it replaced'
    New-CaseEnv 'upgrade'
    New-FakeBinary (Join-Path $script:BinDir 'mapbox.exe') 'mapbox 0.0.1'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'replaced mapbox 0.0.1' 'reports old to new'
    Expect-Out 'Installed mapbox 9.9.9' 'reports the new version'

    Start-Case 'an install dir that does not exist yet'
    New-CaseEnv 'fresh-dir'
    $env:MAPBOX_INSTALL_DIR = Join-Path $script:BinDir 'nested/deeper'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-File (Join-Path $script:BinDir 'nested/deeper/mapbox.exe') 'created the directory and installed into it'

    Start-Case 'an install dir that exists but cannot be written'
    New-CaseEnv 'readonly'
    if ($OnWindows) {
        Skip 'making a directory unwritable on Windows needs an ACL an administrator ignores'
    } else {
        & /bin/chmod 500 $script:BinDir
        Invoke-Installer
        & /bin/chmod 700 $script:BinDir
        Expect-Status 1 'exits 1'
        Expect-Out 'is not writable' 'says the directory is not writable'
        Expect-Out 'MAPBOX_INSTALL_DIR' 'points at the override'
        Expect-Out 'never asks for administrator rights' 'does not offer to escalate'
        Expect-NoFile (Join-Path $script:BinDir 'mapbox.exe') 'nothing was installed'
    }

    # --- the gate ----------------------------------------------------------

    Start-Case 'a gated channel with no credential names the variable'
    New-CaseEnv 'gated-no-credential'
    $env:MAPBOX_CLI_BASE_URL = $gated.BaseUrl
    Invoke-Installer
    Expect-Status 1 'exits 1'
    Expect-Out 'could not read' 'says it could not read the manifest'
    Expect-Out 'MAPBOX_CLI_AUTH' 'names the variable that would let it'
    Expect-NoFile (Join-Path $script:BinDir 'mapbox.exe') 'nothing was installed'

    Start-Case 'a gated channel with the credential installs'
    New-CaseEnv 'gated'
    $env:MAPBOX_CLI_BASE_URL = $gated.BaseUrl
    $env:MAPBOX_CLI_AUTH = $BasicAuth
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'Installed mapbox 9.9.9' 'the credential reached the manifest and the artifact both'
    Expect-File (Join-Path $script:BinDir 'mapbox.exe') 'mapbox.exe is in the install dir'

    # --- where it is running ----------------------------------------------

    Start-Case 'somewhere that is not Windows'
    New-CaseEnv 'not-windows'
    $env:OS = 'Darwin'
    Invoke-Installer
    Expect-Status 1 'exits 1'
    Expect-Out 'this installer is for Windows' 'says which of the two it is'
    Expect-Out 'install.sh' 'points at the one that does work there'

    Start-Case 'the copy the pipeline publishes, with a channel substituted in'
    New-CaseEnv 'substituted'
    # What the release pipeline's sed does, before it uploads: every
    # occurrence, which is the point - a second one anywhere in the file is a
    # served installer that refuses to install.
    $published = Join-Path $script:CaseDir 'install.ps1'
    Set-Content -LiteralPath $published -NoNewline -Value `
        (Get-Content -Raw -LiteralPath $Installer).Replace('__MAPBOX_CLI_BASE_URL__', $script:BaseUrl)
    Clear-Env 'MAPBOX_CLI_BASE_URL'
    Invoke-Installer $published
    Expect-Status 0 'exits 0'
    Expect-Out 'Installed mapbox 9.9.9' 'installs from the channel baked into it, with nothing set'

    Start-Case 'the copy in the repository, with no channel substituted in'
    New-CaseEnv 'unsubstituted'
    Clear-Env 'MAPBOX_CLI_BASE_URL'
    Invoke-Installer
    Expect-Status 1 'exits 1'
    Expect-Out 'no channel to install from' 'says what is wrong'
    Expect-Out 'MAPBOX_CLI_BASE_URL' 'names the way to point it at one'

    Start-Case '32-bit Windows stops before downloading'
    New-CaseEnv 'x86'
    $env:PROCESSOR_ARCHITECTURE = 'x86'
    Invoke-Installer
    Expect-Status 1 'exits 1'
    Expect-Out 'no 32-bit Windows build' 'names what is missing'
    Expect-NoFile (Join-Path $script:BinDir 'mapbox.exe') 'nothing was installed'

    Start-Case 'a 32-bit shell on 64-bit Windows installs the 64-bit build'
    New-CaseEnv 'wow64'
    $env:PROCESSOR_ARCHITECTURE = 'x86'
    $env:PROCESSOR_ARCHITEW6432 = 'AMD64'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'Installed mapbox 9.9.9' 'reads PROCESSOR_ARCHITEW6432 rather than the shell it is in'

    Start-Case 'arm64 falls back to the x64 build, and says so'
    New-CaseEnv 'arm64-fallback'
    $env:PROCESSOR_ARCHITECTURE = 'ARM64'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'no arm64 build' 'says the build it installed is not native'
    Expect-Out 'emulation' 'says what runs it'
    Expect-Out 'Installed mapbox 9.9.9' 'and installs it'

    Start-Case 'arm64 prefers a native build when the channel has one'
    New-CaseEnv 'arm64-native'
    $env:MAPBOX_CLI_BASE_URL = "$($open.BaseUrl)/arm-channel"
    $env:PROCESSOR_ARCHITECTURE = 'ARM64'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'Installed mapbox 9.9.9-arm64' 'installs the aarch64 artifact'
    Expect-NoOut 'emulation' 'and says nothing about emulation'

    # --- PATH --------------------------------------------------------------

    Start-Case 'MAPBOX_NO_MODIFY_PATH names the directory instead of adding it'
    New-CaseEnv 'no-modify-path'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out "$script:BinDir is not on your PATH" 'names the directory'
    Expect-Out 'MAPBOX_NO_MODIFY_PATH is set' 'says why it did not add it'
    Expect-NoOut 'Added ' 'and did not'

    Start-Case 'a mapbox from somewhere else is reported, not replaced'
    New-CaseEnv 'elsewhere'
    $other = Join-Path $script:CaseDir 'other-bin'
    New-Item -ItemType Directory -Path $other -Force | Out-Null
    # `mapbox` is what install.ps1 looks up; PATHEXT is what turns that into
    # mapbox.exe, and there is no PATHEXT where this is developed.
    $otherName = 'mapbox'
    if ($OnWindows) { $otherName = 'mapbox.exe' }
    $otherBinary = Join-Path $other $otherName
    New-FakeBinary $otherBinary 'mapbox 0.0.2'
    $env:PATH = "$other$([IO.Path]::PathSeparator)$SafePath"
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out $otherBinary 'names the other binary'
    Expect-Out 'did not touch it' 'says it left the other one alone'
    Expect-Equal 'mapbox 0.0.2' (& $otherBinary --version) 'the other binary is untouched'

    Start-Case 'a mapbox further down PATH is named, with the restart it needs'
    New-CaseEnv 'behind'
    if (-not $OnWindows) {
        # The installed file is mapbox.exe, and with no PATHEXT here
        # `Get-Command mapbox` cannot match it - so the other copy would win
        # this case's PATH and it would be testing the note above instead.
        Skip 'no PATHEXT here, so the install dir cannot win the lookup'
    } else {
        $other = Join-Path $script:CaseDir 'other-bin'
        New-Item -ItemType Directory -Path $other -Force | Out-Null
        $otherBinary = Join-Path $other 'mapbox.exe'
        New-FakeBinary $otherBinary 'mapbox 0.0.2'
        # The install dir first, so ours wins and the note above stays quiet.
        # What is left is the terminal opened before the install, which is
        # still resolving the other one.
        $sep = [IO.Path]::PathSeparator
        $env:PATH = "$script:BinDir$sep$other$sep$SafePath"
        Invoke-Installer
        Expect-Status 0 'exits 0'
        Expect-Out "another mapbox at $otherBinary" 'names the other binary'
        Expect-Out 'Restart it' 'says what an already-open terminal needs'
        Expect-NoOut 'still resolves to' 'does not claim the other one wins'
        Expect-Equal 'mapbox 0.0.2' (& $otherBinary --version) 'the other binary is untouched'
    }

    Start-Case 'the install dir is added to the user PATH'
    New-CaseEnv 'modify-path'
    if (-not $OnWindows) {
        Skip 'there is no HKCU:\Environment to write to here'
    } else {
        Clear-Env 'MAPBOX_NO_MODIFY_PATH'
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
        $had = $key.GetValueNames() -contains 'Path'
        $savedValue = ''
        $savedKind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        if ($had) {
            $savedValue = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $savedKind = $key.GetValueKind('Path')
        }
        # On disk as well as in memory: everything else this script writes is
        # inside its own scratch directory, and this one case is not.
        $backup = Join-Path ([IO.Path]::GetTempPath()) 'mapbox-cli-test-install-path-backup.txt'
        Set-Content -LiteralPath $backup -Value "$($savedKind): $savedValue"
        Write-Host "  --    editing HKCU:\Environment\Path; the old value is in $backup"
        try {
            # The value install.ps1 has to preserve: REG_EXPAND_SZ, with an
            # entry that means nothing once it has been expanded.
            $key.SetValue('Path', '%USERPROFILE%\test-install-fixture', [Microsoft.Win32.RegistryValueKind]::ExpandString)
            Invoke-Installer
            $after = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            $kindAfter = $key.GetValueKind('Path')
            Expect-Status 0 'exits 0'
            Expect-Out "Added $script:BinDir to your PATH" 'says it added the directory'
            if ($after -like "*$script:BinDir*") { Pass 'the directory is in the user PATH' }
            else { Fail "the directory is in the user PATH (got '$after')" }
            if ($after -like '*%USERPROFILE%\test-install-fixture*') { Pass 'the entries that were there are still unexpanded' }
            else { Fail "the entries that were there are still unexpanded (got '$after')" }
            Expect-Equal 'ExpandString' ([string]$kindAfter) 'the value is still REG_EXPAND_SZ'

            # Twice is not twice: a second install must not append it again.
            Invoke-Installer
            $twice = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
            Expect-Equal $after $twice 'installing again leaves the PATH alone'
            Expect-NoOut 'Added ' 'and says nothing about adding it'
        } finally {
            if ($had) { $key.SetValue('Path', $savedValue, $savedKind) }
            else { $key.DeleteValue('Path', $false) }
            $key.Dispose()
            Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
        }
    }

    # --- the Tilesets CLI --------------------------------------------------

    Start-Case 'the Tilesets CLI is explained, not installed'
    New-CaseEnv 'tilesets'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'does not work natively here' 'says which command needs something else'
    Expect-Out 'WSL' 'points at where it does work'
    Expect-Out 'MAPBOX_TILESETS_CLI' 'names the override, as the CLI does'
    Expect-NoOut 'pipx' 'does not offer a pip that cannot work here'

    Start-Case 'MAPBOX_TILESETS_CLI is reported when it is already set'
    New-CaseEnv 'tilesets-override'
    $env:MAPBOX_TILESETS_CLI = 'C:\tools\tilesets.cmd'
    Invoke-Installer
    Expect-Status 0 'exits 0'
    Expect-Out 'Tilesets CLI: C:\tools\tilesets.cmd' 'reports what it is pointed at'
    Expect-NoOut 'does not work natively here' 'and does not explain it again'
} finally {
    if ($open) { Stop-ChannelServer $open }
    if ($gated) { Stop-ChannelServer $gated }
    Remove-Item -LiteralPath $Root -Recurse -Force -ErrorAction SilentlyContinue
}

# --- result ----------------------------------------------------------------

Write-Host ''
if ($script:Skipped -gt 0) { Write-Host "$($script:Skipped) case(s) skipped" }
if ($script:Failures -gt 0) {
    Write-Host "$($script:Failures) failure(s)"
    exit 1
}
Write-Host 'all good'
exit 0
