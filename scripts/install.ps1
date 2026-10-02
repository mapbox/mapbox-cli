# Installs the `mapbox` CLI on Windows: resolve the target from the processor
# architecture, read the channel manifest, download the .zip it names, verify
# its SHA-256, put mapbox.exe in place - and put its directory on PATH.
#
#     irm https://cli.mapbox.com/install.ps1 | iex
#
# This is the Windows half of scripts/install.sh, which is POSIX sh and stops
# with instructions when it recognizes Git Bash, MSYS2 or Cygwin. The two are
# the same script in two languages: same environment variables, same order of
# operations, and the same things said about a checksum that does not match, an
# install directory that cannot be written, and a `mapbox` that came from
# somewhere else. Change one and change the other.
#
# Six constraints that are easy to break:
#
#   * Windows PowerShell 5.1 is a target, not just PowerShell 7. It is what
#     `powershell` still opens on a stock Windows 11, so: no `$IsWindows`, no
#     `?:`, no `?.`, and `-UseBasicParsing` on the web calls.
#   * It is executed, not run. `irm ... | iex` runs this text in the caller's own
#     session, where `exit` would close their console - every failure path
#     `throw`s instead. Everything lives inside one `& { ... }` block so that no
#     variable or function of ours outlives the install, and so the preference
#     variables set below are undone when it ends.
#   * Nothing here may read stdin, for the reason install.sh may not: piped
#     into `iex` there is no prompt to answer. Nothing is asked - the one
#     question install.sh has, the Tilesets CLI, has no Windows answer.
#   * It is served, not run from the repo. Mapbox's release pipeline
#     substitutes __MAPBOX_CLI_BASE_URL__ for the channel's own URL before
#     uploading this file, so anything new that differs per channel has to go
#     through the same substitution. A copy with nothing substituted in stops
#     at the check below rather than trying to download from a placeholder.
#   * The install directory goes on the *user's* PATH, which lives in the
#     registry and is usually REG_EXPAND_SZ. Writing it back as REG_SZ turns
#     every %USERPROFILE%-style entry in it into a literal. See Add-ToUserPath.
#   * Every byte of it is ASCII, and PSUseBOMForUnicodeEncodedFile in CI is
#     what keeps it that way. Windows PowerShell reads a .ps1 with no byte
#     order mark as the machine's ANSI code page, so an em dash in a message
#     would arrive as mojibake there - and a byte order mark is not the way
#     out, because it rides along in the string `irm` hands to `iex`, where it
#     turns `& { ... }` into a background job that installs nothing.
#
# scripts/test-install.ps1 is the test, and runs on Windows PowerShell 5.1 and
# PowerShell 7 both.

& {
    Set-StrictMode -Version 3.0
    $ErrorActionPreference = 'Stop'
    # Windows PowerShell renders a progress bar per chunk of an
    # Invoke-WebRequest download, which costs more wall time than the transfer.
    $ProgressPreference = 'SilentlyContinue'

    $BaseUrl = '__MAPBOX_CLI_BASE_URL__'
    if ($env:MAPBOX_CLI_BASE_URL) { $BaseUrl = $env:MAPBOX_CLI_BASE_URL }
    $BaseUrl = $BaseUrl.TrimEnd('/')

    $Version = 'latest'
    if ($env:MAPBOX_CLI_VERSION) { $Version = $env:MAPBOX_CLI_VERSION }

    # Accepted with or without the leading `v`, same as install.sh. The
    # channel names its directories `v0.2.1`, but `mapbox --version`,
    # CHANGELOG.md and Cargo.toml all show the version without one, so the
    # spelling a person copies is the one that used to 403. `latest` and any
    # other non-numeric channel name is left alone.
    if ($Version -match '^[0-9]') { $Version = "v$Version" }

    $InstallDir = $env:MAPBOX_INSTALL_DIR

    # Only set if you need a non-production channel. Set MAPBOX_CLI_AUTH to
    # user:password to authenticate to it - the same credential the outer
    # `irm -Headers` used to fetch this script, which is why it has to be in
    # the environment rather than on that call alone: this script makes two
    # more requests of its own.
    $Auth = $env:MAPBOX_CLI_AUTH

    # Both requests below go out under this name rather than whatever
    # Invoke-WebRequest sends, so a download that came from the installer can
    # be told apart from a hand-written irm, a mirror, or CI pulling the same
    # archive - the User-Agent is what the access logs carry either way. It
    # says nothing about who is installing: the channel and the artifact are
    # in the request path already, and the triple appended below is what
    # PROCESSOR_ARCHITECTURE reports.
    #
    # This string and install.sh's are the only two copies of it, because each
    # script is downloaded and run on its own and can read nothing else. They
    # are not left to agree by hand: test-install.sh reads both and fails when
    # they differ, and this suite takes what it asserts from the line below
    # rather than writing it down a third time.
    $UserAgent = 'mapbox-cli-install/1'

    # Set MAPBOX_CLI_INSTALL_SOURCE to name what is doing the installing - a
    # provisioning script, CI - and it rides along as `src/<value>`.
    # Everything but letters, digits, dot, dash and underscore is dropped,
    # because this value comes from the environment and ends up in a header.
    #
    # Convention for a coding agent invoking this script on someone's behalf:
    # agent-<name> - agent-claude-code, agent-cursor - so an access-log query
    # can tell an agent-driven install from a human or CI one without a second
    # reporting mechanism. A dash, not a slash: / is one of the characters the
    # sanitizer below strips, so agent/claude-code would silently become
    # agentclaude-code and lose the separator that makes the convention
    # legible. Freeform otherwise, the same as every other value here - no
    # fixed list of agent names to keep in sync.
    $InstallSource = $env:MAPBOX_CLI_INSTALL_SOURCE

    # The switch src/telemetry.rs honors for the CLI's own User-Agent, read
    # here the same way, because someone who put it in a Dockerfile and then
    # runs `irm ... | iex` in the same file has already said which way they
    # want it. The product token above survives it - the equivalent of
    # `mapbox-cli/<version>` going out either way - and the triple and the
    # source tag are what it drops.
    #
    # Two names, and only the binary dropped the old one.
    # MAPBOX_CLI_NO_TELEMETRY is the documented switch; DISABLE_TELEMETRY is
    # what it was called before, and this script still honors it. The rename
    # was announced as breaking for the binary; nothing announced it for the
    # installers, which are fetched and run in one line with no release notes
    # in front of the reader - and breaking an opt-out is the one change that
    # must not happen quietly. So both work here, and the new name wins when
    # both are set.
    #
    # Unset, empty or whitespace is a cleared variable. `0`, `f`, `false`, `n`,
    # `no` and `off` are clap's false spellings, the same reading this script
    # already gives MAPBOX_NO_MODIFY_PATH, so a `0` is someone declining the
    # opt-out rather than taking it. Anything else opts out, including a
    # spelling nobody planned for: the safe reading of a value we do not know,
    # on a variable by that name, is the one that sends less.
    $TelemetryAllowed = $true
    $disableTelemetry = $env:MAPBOX_CLI_NO_TELEMETRY
    if ($null -eq $disableTelemetry) {
        $disableTelemetry = $env:DISABLE_TELEMETRY
    }
    if ($null -ne $disableTelemetry) {
        $disableTelemetry = $disableTelemetry.Trim().ToLowerInvariant()
        $TelemetryAllowed = (
            $disableTelemetry.Length -eq 0 -or
            @('0', 'f', 'false', 'n', 'no', 'off') -contains $disableTelemetry
        )
    }

    # Set MAPBOX_NO_MODIFY_PATH to keep this out of the registry; the directory
    # is then named, with the line to add it, the way install.sh names a shell
    # profile.
    $ModifyPath = $true
    if ($env:MAPBOX_NO_MODIFY_PATH -and $env:MAPBOX_NO_MODIFY_PATH -notin @('0', 'no', 'false')) {
        $ModifyPath = $false
    }

    $Repo = 'https://github.com/mapbox/mapbox-cli'

    function Write-Err([string]$Text) { [Console]::Error.WriteLine($Text) }

    # --- presentation ------------------------------------------------------
    #
    # Color through Write-Host's own -ForegroundColor rather than ANSI codes:
    # the console renders it under 5.1 and conhost alike, and a redirected run
    # drops it by itself. NO_COLOR is honored the way the CLI honors it.
    $UseColor = -not $env:NO_COLOR

    # The check mark only where the font is known to carry it: Windows
    # Terminal's Cascadia does, conhost's default Consolas does not. Built
    # from its code point because this file has to stay ASCII.
    $MarkOk = '*'
    if ($env:WT_SESSION) { $MarkOk = [string][char]0x2713 }

    function Write-Part([string]$Text, [string]$Color = '') {
        if ($UseColor -and $Color) { Write-Host $Text -ForegroundColor $Color -NoNewline }
        else { Write-Host $Text -NoNewline }
    }
    # Where a step line starts. A question's answers are its children, so a
    # step that asked one sets this two deeper while it reports, then puts it
    # back. A hashtable so the functions below see the change.
    $Layout = @{ Indent = '  ' }
    function Write-Step([string]$Mark, [string]$Color, [string]$Text, [string]$Detail = '') {
        Write-Part $Layout.Indent
        Write-Part $Mark $Color
        Write-Part " $Text"
        if ($Detail) { Write-Part " $Detail" 'DarkGray' }
        Write-Host ''
    }
    function Write-Ok([string]$Text, [string]$Detail = '') { Write-Step $MarkOk 'Green' $Text $Detail }
    function Write-Skip([string]$Text) { Write-Step '-' 'DarkGray' $Text }
    function Write-Warn([string]$Text) { Write-Step '!' 'Yellow' $Text }
    # A line of detail under the step line before it, aligned with its text.
    function Write-Detail([string]$Text) { Write-Host "$($Layout.Indent)  $Text" }

    # A question reads in three layers, as in install.sh: the question in the
    # console's own text color, what answering does in gray under it, and the
    # command it runs in cyan, so the eye lands on the question and the
    # command. Not White: on a light console theme it all but disappears.
    function Write-AskTitle([string]$Text) {
        Write-Part '  '
        Write-Part '?' 'Cyan'
        Write-Part " $Text"
        Write-Host ''
    }
    # -Parts alternates plain note text and a command: note, command, note...
    function Write-AskNote([string[]]$Parts) {
        Write-Part '    '
        for ($i = 0; $i -lt $Parts.Count; $i++) {
            if ($i % 2) { Write-Part $Parts[$i] 'Cyan' } else { Write-Part $Parts[$i] 'DarkGray' }
        }
        Write-Host ''
    }
    function Read-AskAnswer([string]$Question) {
        Write-Part "    $Question "
        Write-Part '[y/N] ' 'DarkGray'
        return Read-Host
    }

    # Every failure path ends here. The message is written out in full and then
    # thrown, so the summary line appears twice: once as we wrote it, and once
    # more in whatever the host makes of a terminating error. That is the price
    # of `throw` being the only way to end with a non-zero status that does not
    # also close an interactive console - `exit` under `iex` does exactly that.
    function Fail {
        param([Parameter(Mandatory = $true)][string]$Message, [string]$Detail)
        Write-Err "mapbox-cli: $Message"
        if ($Detail) {
            Write-Err ''
            Write-Err $Detail
        }
        throw "mapbox-cli: $Message"
    }

    # 401 or 0 for anything that never got a response. Written defensively
    # because the exception type differs between the two PowerShells -
    # WebException in 5.1, HttpResponseException in 7 - and StrictMode makes a
    # missing property an error rather than $null.
    function Get-HttpStatus($ErrorRecord) {
        try { return [int]$ErrorRecord.Exception.Response.StatusCode } catch { return 0 }
    }

    # What `<path> --version` says, or '' if it will not run. Used both for the
    # copy being replaced and for the one just installed, which is what makes
    # an artifact built for another platform fail here rather than the first
    # time the user runs it.
    function Get-BinaryVersion([string]$Path) {
        # Local to this function: with EAP=Stop, Windows PowerShell turns a
        # native command's stderr into a terminating NativeCommandError, so a
        # binary that fails *and explains why* would throw out of the try below
        # rather than return ''.
        $ErrorActionPreference = 'Continue'
        try {
            $output = @(& $Path --version 2>$null)
            if ($LASTEXITCODE -eq 0 -and $output.Count -gt 0) { return ([string]$output[0]).Trim() }
        } catch {
            # Not an executable at all - a text file with an .exe name, an
            # artifact for another architecture. '' says the same thing.
        }
        return ''
    }

    function Test-OnPath([string]$PathValue, [string]$Dir) {
        $want = $Dir.TrimEnd('\')
        foreach ($entry in $PathValue -split ';') {
            $expanded = [Environment]::ExpandEnvironmentVariables($entry).Trim().TrimEnd('\')
            if ($expanded -and $expanded -eq $want) { return $true }
        }
        return $false
    }

    # The user's PATH is HKCU:\Environment\Path, and on most machines that
    # value is REG_EXPAND_SZ holding entries like
    # %USERPROFILE%\AppData\Local\Microsoft\WindowsApps.
    # [Environment]::SetEnvironmentVariable(..., 'User') would rewrite the whole
    # value as REG_SZ and turn those into literal percent signs, so read and
    # write it through the registry instead, keeping the kind it already had.
    # Returns $true if it added the directory, $false if it was already there.
    function Add-ToUserPath([string]$Dir) {
        $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
        if (-not $key) { $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment') }
        try {
            $raw = ''
            $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
            if ($key.GetValueNames() -contains 'Path') {
                $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
                $kind = $key.GetValueKind('Path')
            }
            if (Test-OnPath $raw $Dir) { return $false }
            $trimmed = $raw.Trim().TrimEnd(';')
            if ($trimmed) { $key.SetValue('Path', "$trimmed;$Dir", $kind) }
            else { $key.SetValue('Path', $Dir, $kind) }
        } finally {
            $key.Dispose()
        }
        return $true
    }

    # Explorer captured its environment at logon and refreshes it only on
    # WM_SETTINGCHANGE; without this broadcast even a terminal opened *after*
    # the install inherits the old PATH, because Explorer is what starts it.
    # Best effort: a host that cannot compile the P/Invoke (Constrained
    # Language Mode, say) still installed fine, and a new session picks the
    # change up anyway.
    function Send-EnvironmentChange {
        try {
            if (-not ('MapboxCli.NativeMethods' -as [type])) {
                Add-Type -Namespace 'MapboxCli' -Name 'NativeMethods' -MemberDefinition @'
[System.Runtime.InteropServices.DllImport("user32.dll", SetLastError = true, CharSet = System.Runtime.InteropServices.CharSet.Auto)]
public static extern System.IntPtr SendMessageTimeout(System.IntPtr hWnd, uint Msg, System.IntPtr wParam, string lParam, uint fuFlags, uint uTimeout, out System.UIntPtr lpdwResult);
'@
            }
            $result = [UIntPtr]::Zero
            # HWND_BROADCAST, WM_SETTINGCHANGE, SMTO_ABORTIFHUNG, 5s.
            [void][MapboxCli.NativeMethods]::SendMessageTimeout(
                [IntPtr]0xffff, 0x1A, [IntPtr]::Zero, 'Environment', 0x2, 5000, [ref]$result)
        } catch {
            # Nothing to say: the install succeeded either way.
        }
    }

    # --- where we are ------------------------------------------------------

    # A channel to name in the messages below, even when this copy has none
    # baked in - the placeholder is not a URL to hand anybody.
    $exampleUrl = $BaseUrl
    if ($exampleUrl -notlike 'http*') { $exampleUrl = 'https://cli.mapbox.com' }

    if ($env:OS -ne 'Windows_NT') {
        # $IsWindows would be the obvious test and does not exist in Windows
        # PowerShell 5.1, where this most needs to work.
        Fail 'this installer is for Windows.' @"
On macOS and Linux, install.sh is the one to use:

    curl -fsSL $exampleUrl/install.sh | sh
"@
    }

    # Not a comparison against the placeholder itself: the release pipeline
    # substitutes every occurrence of it in this file, which would rewrite the
    # comparison too and make the served copy refuse to install from the
    # channel baked into it. Anything that is not a URL is the same problem
    # anyway.
    if ($BaseUrl -notlike 'http*') {
        Fail 'no channel to install from.' @"
This is the copy of the installer in the repository, and it has no URL baked
in - the served copies get one substituted at publish time. Install from a
channel:

    irm $exampleUrl/install.ps1 | iex

Or point this copy at one:

    `$env:MAPBOX_CLI_BASE_URL = '$exampleUrl'
"@
    }

    # A 32-bit PowerShell on 64-bit Windows reports x86 in
    # PROCESSOR_ARCHITECTURE and the real architecture in PROCESSOR_ARCHITEW6432
    # - read that first, or a WoW64 shell installs nothing on a machine that
    # has a build.
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    if (-not $arch) { $arch = 'an unnamed architecture' }

    # In preference order. Arm64 Windows has a native aarch64-pc-windows-msvc
    # build; the x64 one is the fallback for a manifest that hasn't published
    # it yet (an older pinned version, say), which runs under emulation.
    $candidates = @()
    switch ($arch) {
        'AMD64' { $candidates = @('x86_64-pc-windows-msvc') }
        'ARM64' { $candidates = @('aarch64-pc-windows-msvc', 'x86_64-pc-windows-msvc') }
        'x86' {
            Fail 'there is no 32-bit Windows build.' "Build from source: $Repo"
        }
        default {
            Fail "no prebuilt binary for $arch." "Build from source: $Repo"
        }
    }

    # The machine's own triple, whichever build ends up on it. An ARM64
    # Windows has no native build yet and installs the x64 one, so this and
    # the artifact name in the same log line are what say an install was
    # emulated rather than native.
    if ($TelemetryAllowed) {
        $UserAgent = "$UserAgent ($($candidates[0]))"
        if ($InstallSource) {
            $tag = $InstallSource -replace '[^A-Za-z0-9._-]', ''
            if ($tag) { $UserAgent = "$UserAgent src/$tag" }
        }
    }

    # --- the manifest ------------------------------------------------------

    # Windows PowerShell takes its TLS versions from whatever the .NET
    # Framework was configured with, which on an un-patched Windows 10 is still
    # TLS 1.0. CloudFront answers nothing below 1.2, and the failure is an
    # unhelpful "underlying connection was closed".
    if ($PSVersionTable.PSVersion.Major -lt 6) {
        [Net.ServicePointManager]::SecurityProtocol =
            [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    }

    # Basic auth by hand rather than -Credential, which waits to be challenged
    # and so never sends anything the CloudFront function will accept.
    $headers = @{}
    if ($Auth) {
        $headers['Authorization'] = 'Basic ' + [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($Auth))
    }

    $manifestUrl = "$BaseUrl/$Version/manifest.json"
    $manifest = $null
    try {
        $manifest = Invoke-RestMethod -Uri $manifestUrl -Headers $headers -UserAgent $UserAgent -UseBasicParsing -TimeoutSec 60
    } catch {
        $detail = ''
        if ((Get-HttpStatus $_) -eq 401 -and -not $Auth) {
            $detail = @"
This channel is private. Set the credential and run it again:

    `$env:MAPBOX_CLI_AUTH = 'user:password'
"@
        }
        Fail "could not read $manifestUrl" $detail
    }

    # A channel served with the wrong content type hands back a string rather
    # than an object.
    if ($manifest -is [string]) { $manifest = $manifest | ConvertFrom-Json }
    if (-not $manifest.PSObject.Properties['artifacts']) {
        Fail "$manifestUrl is not a channel manifest - it lists no artifacts."
    }

    $artifacts = $manifest.artifacts
    $resolvedVersion = ''
    if ($manifest.PSObject.Properties['version']) { $resolvedVersion = [string]$manifest.version }

    $target = ''
    foreach ($candidate in $candidates) {
        if ($artifacts.PSObject.Properties[$candidate]) {
            $target = $candidate
            break
        }
    }
    if (-not $target) {
        Fail "$manifestUrl lists no artifact for $($candidates -join ' or ')."
    }
    $emulated = $arch -eq 'ARM64' -and $target -eq 'x86_64-pc-windows-msvc'

    $entry = $artifacts.$target
    $file = ''
    if ($entry.PSObject.Properties['file']) { $file = [string]$entry.file }
    $sha256 = ''
    if ($entry.PSObject.Properties['sha256']) { $sha256 = [string]$entry.sha256 }

    if (-not $file) {
        Fail "$manifestUrl lists no artifact for $target."
    }
    # The manifest is the only checksum this reads. SHA256SUMS sits beside it
    # and carries the same digests, so fetching both would prove nothing extra:
    # they come from one origin over one TLS session. It stays published for
    # people verifying a download by hand.
    if (-not $sha256) {
        Fail "$manifestUrl lists $file for $target with no sha256; refusing to install unverified bytes."
    }

    # --- download and verify ----------------------------------------------

    $workDir = Join-Path ([IO.Path]::GetTempPath()) ('mapbox-cli.' + [IO.Path]::GetRandomFileName())
    $staged = ''
    New-Item -ItemType Directory -Path $workDir -Force | Out-Null
    try {
        # The manifest names the artifact relative to the channel directory.
        # Keep only the leaf for the local copy, so a manifest that ever
        # carries a path prefix still writes somewhere that exists - and give
        # it a .zip name, because Expand-Archive reads the extension rather
        # than the bytes.
        $localName = $file.Split('/')[-1]
        if (-not $localName.EndsWith('.zip', 'OrdinalIgnoreCase')) { $localName += '.zip' }
        $archive = Join-Path $workDir $localName
        $artifactUrl = "$BaseUrl/$Version/$file"

        $platformName = 'Windows (x64)'
        if ($target -like 'aarch64-*') { $platformName = 'Windows (arm64)' }
        $shownVersion = $resolvedVersion
        if (-not $shownVersion) { $shownVersion = $Version }
        Write-Host ''
        Write-Part "Installing Mapbox CLI $shownVersion"
        Write-Part " for $platformName" 'DarkGray'
        Write-Host ''
        Write-Host ''
        if ($emulated) {
            Write-Warn 'This channel has no arm64 build; installing the x64 one, which Windows 11 on Arm runs under emulation.'
        }

        try {
            Invoke-WebRequest -Uri $artifactUrl -OutFile $archive -Headers $headers -UserAgent $UserAgent -UseBasicParsing -TimeoutSec 600
        } catch {
            Fail "could not download $artifactUrl"
        }

        # Before unpacking, not after: an artifact that fails here never gets
        # the chance to write anything, anywhere.
        $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
        if ($actual -ne $sha256.ToLowerInvariant()) {
            Fail "checksum mismatch for $file - nothing was installed." @"
  expected  $($sha256.ToLowerInvariant())
  actual    $actual

Either the download was corrupted or the artifact is not the one the manifest
describes. Try again; if it repeats, do not install it - report it at
$Repo/issues
"@
        }

        $sizeBytes = (Get-Item -LiteralPath $archive).Length
        $invariant = [Globalization.CultureInfo]::InvariantCulture
        if ($sizeBytes -ge 1MB) { $size = [string]::Format($invariant, '{0:0.0} MB', $sizeBytes / 1MB) }
        else { $size = [string]::Format($invariant, '{0} KB', [math]::Ceiling($sizeBytes / 1KB)) }
        Write-Ok "Downloaded $($file.Split('/')[-1])" "($size, SHA-256 verified)"

        $unpacked = Join-Path $workDir 'unpacked'
        try {
            Expand-Archive -LiteralPath $archive -DestinationPath $unpacked -Force
        } catch {
            Fail "could not unpack $file"
        }

        $unpackedExe = Join-Path $unpacked 'mapbox.exe'
        if (-not (Test-Path -LiteralPath $unpackedExe)) {
            Fail "$file does not contain a mapbox.exe at its root."
        }

        # --- install ------------------------------------------------------

        if (-not $InstallDir) {
            if (-not $env:LOCALAPPDATA) {
                Fail 'LOCALAPPDATA is not set, so there is no default install directory.' `
                    'Set MAPBOX_INSTALL_DIR to where mapbox.exe should go.'
            }
            $InstallDir = Join-Path $env:LOCALAPPDATA 'Programs\mapbox'
        }
        if (-not (Test-Path -LiteralPath $InstallDir)) {
            try {
                New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
            } catch {
                Fail "could not create $InstallDir"
            }
        }
        $InstallDir = (Resolve-Path -LiteralPath $InstallDir).ProviderPath.TrimEnd('\')

        # Ask before anything is moved, so an install into a directory someone
        # else owns fails with the override spelled out rather than an
        # exception halfway through.
        $probe = Join-Path $InstallDir (".mapbox.write-test.$PID")
        try {
            [IO.File]::WriteAllText($probe, '')
            Remove-Item -LiteralPath $probe -Force
        } catch {
            Fail "$InstallDir exists but is not writable." @"
Install somewhere you own instead:

    `$env:MAPBOX_INSTALL_DIR = "`$env:LOCALAPPDATA\Programs\mapbox"
    irm $BaseUrl/install.ps1 | iex

This script never asks for administrator rights on your behalf. If that
directory really is where you want mapbox.exe, run it again from a shell that
can write there.
"@
        }

        $destination = Join-Path $InstallDir 'mapbox.exe'

        # What is already here, before anything is replaced. A `mapbox` from
        # `cargo install` or another install directory lives somewhere else
        # entirely; that one is reported after the install rather than touched.
        $previousVersion = ''
        if (Test-Path -LiteralPath $destination) {
            $previousVersion = Get-BinaryVersion $destination
        }

        # Left behind by an earlier install that replaced a running mapbox.exe;
        # nothing holds them now, and if something still does, next time.
        Get-ChildItem -LiteralPath $InstallDir -Filter 'mapbox.exe.old.*' -ErrorAction SilentlyContinue |
            ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }

        # Copy into the install directory under a temp name, then rename: a
        # rename within one directory is atomic, so a concurrent or interrupted
        # install can never leave a half-written mapbox.exe in its place.
        $staged = Join-Path $InstallDir ".mapbox.install.$PID.exe"
        try {
            Copy-Item -LiteralPath $unpackedExe -Destination $staged -Force
        } catch {
            Fail "could not write to $InstallDir"
        }

        $asideNote = ''
        try {
            Move-Item -LiteralPath $staged -Destination $destination -Force
        } catch {
            # Windows refuses to replace a running .exe, but it does allow it
            # to be renamed out of the way - which is what makes an upgrade
            # possible while a `mapbox` somewhere is mid-request.
            $aside = Join-Path $InstallDir ('mapbox.exe.old.' + [DateTime]::Now.ToString('yyyyMMddHHmmss'))
            try {
                Move-Item -LiteralPath $destination -Destination $aside -Force
                Move-Item -LiteralPath $staged -Destination $destination -Force
            } catch {
                Fail "could not replace $destination" @"
Close anything using mapbox.exe - a shell that is running it, an editor
watching the folder - and run this again.
"@
            }
            try {
                Remove-Item -LiteralPath $aside -Force
            } catch {
                $asideNote = $aside
            }
        }
        $staged = ''

        # Report what the binary says about itself rather than what the
        # manifest claimed: an artifact built for another platform fails here
        # and nowhere earlier.
        $installedVersion = Get-BinaryVersion $destination
        if (-not $installedVersion) {
            $detail = "Build from source: $Repo"
            if ($emulated) {
                $detail = @"
This is the x64 build running on arm64, which needs the x64 emulation that
Windows 11 on Arm has and Windows 10 on Arm does not.
"@
            }
            Fail "installed $destination, but it does not run here. The artifact may be built for a platform other than $target." $detail
        }

        if ($previousVersion -and $previousVersion -ne $installedVersion) {
            Write-Ok "Installed $installedVersion to $destination" "(replaced $previousVersion)"
        } else {
            Write-Ok "Installed $installedVersion to $destination"
        }
        if ($asideNote) {
            Write-Warn "The copy it replaced is still running; $asideNote is deleted next time."
        }

        # --- PATH ----------------------------------------------------------

        # Resolved before this session's PATH is touched below, so what it
        # finds is what the user's shells find. -CommandType Application
        # because under `iex` a `mapbox` function or alias of theirs is in
        # scope too, and is not what would run a binary.
        $shadowing = ''
        $onPath = Get-Command 'mapbox' -CommandType Application -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($onPath -and $onPath.Source -ne $destination) { $shadowing = $onPath.Source }

        # -All lists every match in PATH order rather than the winner alone. A
        # second mapbox behind ours is invisible to the check above, and it is
        # the one a terminal opened before this install can still be resolving
        # - so it is worth naming even though nothing here is wrong yet.
        $behind = ''
        if (-not $shadowing) {
            $rest = @(Get-Command 'mapbox' -CommandType Application -All -ErrorAction SilentlyContinue |
                Where-Object { $_.Source -ne $destination })
            if ($rest.Count -gt 0) { $behind = $rest[0].Source }
        }

        $alreadyOnPath = Test-OnPath $env:Path $InstallDir
        if ($ModifyPath) {
            $added = $false
            try {
                $added = Add-ToUserPath $InstallDir
            } catch {
                Write-Warn "Could not add $InstallDir to your PATH: $($_.Exception.Message)"
                Write-Host '    Add it in Settings > "Edit environment variables for your account".'
            }
            if ($added) {
                Send-EnvironmentChange
                Write-Ok "Added $InstallDir to your PATH" '(other open terminals need a restart)'
            }
            if (-not $alreadyOnPath) {
                # Whether it was just added or was already in the registry, the
                # session that ran this does not have it until now. Under
                # `irm | iex` that session is the user's own shell, so mapbox
                # works there immediately.
                $env:Path = $env:Path.TrimEnd(';') + ";$InstallDir"
            }
        } elseif (-not $alreadyOnPath) {
            Write-Warn "$InstallDir is not on your PATH, and MAPBOX_NO_MODIFY_PATH is set."
            Write-Host '    Add it in Settings > "Edit environment variables for your account",'
            Write-Host '    or for this session only:'
            Write-Host ''
            Write-Host "        `$env:Path += `";$InstallDir`""
            Write-Host ''
        }

        if ($shadowing) {
            Write-Warn "mapbox on your PATH still resolves to $shadowing"
            Write-Host '    That one came from somewhere else, and this script did not touch it. To use'
            Write-Host "    the copy just installed, remove it, or put $InstallDir ahead of it on PATH -"
            Write-Host '    entries from your user PATH come after the machine-wide ones.'
        } elseif ($behind) {
            Write-Warn "There is another mapbox at $behind"
            Write-Host "    $InstallDir comes first on your PATH, so a new terminal runs the copy just"
            Write-Host '    installed. A terminal that was already open can still resolve the old one:'
            Write-Host '    Restart it, and Get-Command mapbox says which file it would run. This script'
            Write-Host '    did not touch the other copy.'
        }

        # --- the Tilesets CLI ----------------------------------------------
        #
        # install.sh offers to install it here. There is nothing to offer on
        # Windows: `mapbox tilesets-cli` forwards to the Python package
        # mapbox-tilesets, which is macOS and Linux only. This says what
        # `launch_failed` in src/tilesets_cli.rs says when it is reached on
        # Windows; keep the two in step.

        if ($env:MAPBOX_TILESETS_CLI) {
            Write-Ok "Found Tilesets CLI: $env:MAPBOX_TILESETS_CLI (MAPBOX_TILESETS_CLI)"
        } else {
            Write-Skip 'Tilesets CLI skipped: only the mapbox tilesets-cli command needs it, and it does not work natively here.'
            Write-Host '    Run that command from WSL, or point the CLI at a tilesets of your own,'
            Write-Host '    such as a wrapper that shells into WSL:'
            Write-Host ''
            Write-Host "        `$env:MAPBOX_TILESETS_CLI = 'C:\path\to\tilesets.cmd'"
        }

        # --- Coding agent skill ----------------------------------------------
        #
        # Most machines running this script have no coding agent installed at
        # all, and generate-skills answers "nowhere to write" as
        # no_agent_detected rather than writing anything when that is the
        # case (src/skill_dest.rs) - checked first, silently, so nothing is
        # offered when there is nothing to offer. -Global, not the
        # project-scoped default: this script runs in whatever directory the
        # shell happened to be in, which has no relation to a project.
        #
        # Past that point this asks: writing into a directory this CLI does
        # not own and downloading a whole separate library is a bigger ask
        # than a one-line telemetry notice, and an install nobody consented
        # to is not a feature. [Console]::IsInputRedirected is false for a
        # real interactive session even under `irm ... | iex` - that pipes an
        # object through PowerShell's own pipeline into Invoke-Expression,
        # which runs in this same process rather than redirecting a child
        # process's stdin the way `curl ... | sh` does - so a person typing
        # that at a real prompt still gets asked; only a session with no
        # console at all (a scheduled task, an image build) does not.
        function Get-AgentName([string]$Json) {
            ([regex]::Matches($Json, '"source":"([^"]*)"') | ForEach-Object {
                    $_.Groups[1].Value -replace ', all projects$', ''
                }) -join ', '
        }

        $AgentSetupAllowed = $true
        $disableAgentSetup = $env:MAPBOX_CLI_NO_AGENT_SETUP
        if ($null -ne $disableAgentSetup) {
            $disableAgentSetup = $disableAgentSetup.Trim().ToLowerInvariant()
            $AgentSetupAllowed = (
                $disableAgentSetup.Length -eq 0 -or
                @('0', 'f', 'false', 'n', 'no', 'off') -contains $disableAgentSetup
            )
        }

        if ($AgentSetupAllowed) {
            # A child writing to stderr is a terminating NativeCommandError in
            # Windows PowerShell under $ErrorActionPreference = 'Stop' (set
            # once, at the top of this script) - the same reason
            # Invoke-Installer in test-install.ps1 has to switch to
            # 'Continue' around a call that does the same. Every command
            # below writes its failures to stderr, on purpose, so this has to
            # hold for all of them; restored in the finally below regardless
            # of which path is reached.
            $previousErrorAction = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            try {
                $checkOut = (& $destination generate-skills --global --dry-run -o json 2>$null) -join ''
                if ($LASTEXITCODE -eq 0) {
                    $agentNames = Get-AgentName $checkOut
                    Write-Ok "Coding agents found: $agentNames."
                    $doAgentSetup = $false
                    if (-not [Console]::IsInputRedirected) {
                        Write-AskTitle 'Set up the mapbox CLI skill and the Mapbox Agent Skills library for them?'
                        Write-AskNote @('This runs ', 'mapbox generate-skills --global', ' and ', 'mapbox agent-skills install --global', ',')
                        Write-AskNote @('now or any time later.')
                        $answer = Read-AskAnswer 'Set them up now?'
                        $Layout.Indent = '    '
                        if ($answer -match '^(?i:y|yes)$') { $doAgentSetup = $true }
                    }
                    if ($doAgentSetup) {
                        # Offline and safe to re-run: it replaces its own
                        # generated directory wholesale and refuses only if
                        # something else already lives there, which -Global
                        # keeps out of this script's way.
                        & $destination generate-skills --global *> $null
                        if ($LASTEXITCODE -eq 0) {
                            Write-Ok "Wrote the mapbox CLI skill for: $agentNames."
                        }

                        # `install` errors with already_installed on a
                        # reinstall, since the skill directory is already
                        # there from a previous run of this same script -
                        # `update` is the one safe to repeat, but only once
                        # checked first: some of what changed since the last
                        # run might be a local edit, not just an upstream
                        # refresh, and that is never overwritten without
                        # being named.
                        $agentStderr = [System.IO.Path]::GetTempFileName()
                        try {
                            & $destination agent-skills install --global -o json 1>$null 2>$agentStderr
                            if ($LASTEXITCODE -eq 0) {
                                Write-Ok "Installed the Mapbox Agent Skills library for: $agentNames."
                            } else {
                                $agentOut = Get-Content -LiteralPath $agentStderr -Raw -ErrorAction SilentlyContinue
                                if ($agentOut -and $agentOut.Contains('"code":"already_installed"')) {
                                    $updateStderr = [System.IO.Path]::GetTempFileName()
                                    try {
                                        $updateOut = (& $destination agent-skills update --global --dry-run -o json 2>$updateStderr) -join ''
                                        if ($LASTEXITCODE -eq 0) {
                                            if ($updateOut.Contains('"updated":[]')) {
                                                & $destination agent-skills update --global *> $null
                                                Write-Ok "Updated the Mapbox Agent Skills library for: $agentNames."
                                            } else {
                                                Write-Warn 'Some Mapbox Agent Skills have local changes and were left alone.'
                                                Write-Detail "Run 'mapbox agent-skills update --global' to review and replace them."
                                            }
                                        }
                                    } finally {
                                        Remove-Item -LiteralPath $updateStderr -Force -ErrorAction SilentlyContinue
                                    }
                                }
                            }
                        } finally {
                            Remove-Item -LiteralPath $agentStderr -Force -ErrorAction SilentlyContinue
                        }
                    } else {
                        Write-Skip 'Not set up. Run these any time:'
                        Write-Detail 'mapbox generate-skills --global'
                        Write-Detail 'mapbox agent-skills install --global'
                    }
                    $Layout.Indent = '  '
                }
            } finally {
                $ErrorActionPreference = $previousErrorAction
            }
        }

        # --- Mapbox MCP servers ----------------------------------------------
        #
        # A separate question from the skill above: this registers a hosted
        # server in the coding agent's own config, next to servers the user
        # added, rather than writing a directory this CLI owns. `mapbox mcp
        # install` never replaces an entry that is already there (src/mcp.rs),
        # so on a reinstall it is only offered when something is left to
        # register. Same opt-out and same no-console rule as the skill.
        if ($AgentSetupAllowed) {
            $previousErrorAction = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            try {
                # Fails with mcp_client_not_found when no supported client is
                # installed, which is most machines: nothing to offer, so
                # nothing is said.
                $mcpCheck = (& $destination mcp install --global --dry-run -o json 2>$null) -join ''
                if ($LASTEXITCODE -eq 0) {
                    $clientLabels = @{
                        'claude-code' = 'Claude Code'; 'codex' = 'Codex'
                        'vscode' = 'VS Code'; 'cursor' = 'Cursor'
                    }
                    $mcpClients = @(($mcpCheck | ConvertFrom-Json).results |
                            Where-Object { $_.status -eq 'would_install' } |
                            ForEach-Object { $_.client } | Select-Object -Unique |
                            ForEach-Object { if ($clientLabels.ContainsKey($_)) { $clientLabels[$_] } else { $_ } }) -join ', '
                    if ($mcpClients) {
                        $doMcpSetup = $false
                        if (-not [Console]::IsInputRedirected) {
                            Write-AskTitle "Add the Mapbox MCP servers to ${mcpClients}?"
                            Write-AskNote @('This runs ', 'mapbox mcp install --global', ', now or any time later.')
                            $answer = Read-AskAnswer 'Add them now?'
                            $Layout.Indent = '    '
                            if ($answer -match '^(?i:y|yes)$') { $doMcpSetup = $true }
                        }
                        if ($doMcpSetup) {
                            # Each client's own CLI does the registering, and
                            # Codex's can stop to print a sign-in URL, so
                            # stderr - where mapbox forwards what they print -
                            # is left on the console, live. No spinner: a
                            # second writer to the console is not worth it
                            # here without a terminal to test it on.
                            Write-Part $Layout.Indent
                            Write-Part '...' 'Cyan'
                            Write-Host ' Adding the Mapbox MCP servers'
                            $mcpOut = (& $destination mcp install --global -o json) -join ''
                            $mcpOk = $LASTEXITCODE -eq 0
                            # One line per server and outcome, clients joined,
                            # so a server added just now reads differently from
                            # one that was already there.
                            $serverLabels = @{ 'mapbox' = 'Mapbox MCP'; 'mapbox-devkit' = 'Mapbox DevKit MCP' }
                            $rows = @()
                            try { $rows = @(($mcpOut | ConvertFrom-Json).results) } catch { $rows = @() }
                            $groups = [ordered]@{}
                            foreach ($row in $rows) {
                                $what = if ($serverLabels.ContainsKey($row.server)) { $serverLabels[$row.server] } else { $row.server }
                                $who = if ($clientLabels.ContainsKey($row.client)) { $clientLabels[$row.client] } else { $row.client }
                                $key = "$($row.status)|$what"
                                if (-not $groups.Contains($key)) { $groups[$key] = @() }
                                $groups[$key] += $who
                            }
                            foreach ($key in $groups.Keys) {
                                $status, $what = $key -split '\|', 2
                                $who = $groups[$key] -join ', '
                                switch ($status) {
                                    'installed' { Write-Ok "Added $what to $who" }
                                    'already_installed' { Write-Skip "$what was already in $who" }
                                    'installed_login_incomplete' { Write-Warn "Added $what to $who, but sign-in did not finish" }
                                    'client_not_found' { Write-Warn "Skipped $what for ${who}: CLI not on PATH" }
                                    'config_unreadable' { Write-Warn "Skipped $what for ${who}: config could not be read" }
                                    default { Write-Warn "Could not add $what to $who" }
                                }
                            }
                            if (-not $mcpOk) {
                                if ($rows.Count -eq 0) { Write-Warn 'Could not add the Mapbox MCP servers.' }
                                Write-Detail "Run 'mapbox mcp install --global' to see why and try again."
                            }
                        } else {
                            Write-Skip "Mapbox MCP servers not added to $mcpClients. Run this any time:"
                            Write-Detail 'mapbox mcp install --global'
                        }
                        $Layout.Indent = '  '
                    }
                }
            } finally {
                $ErrorActionPreference = $previousErrorAction
            }
        }

        # --- summary -------------------------------------------------------
        #
        # The last thing on screen, because it is what the reader acts on. The
        # PATH of the session that ran this was updated above, so under
        # `irm | iex` mapbox already works in it.

        Write-Host ''
        Write-Part "$installedVersion is ready." 'Green'
        Write-Host ''
        Write-Host ''
        Write-Part 'Get started'
        Write-Host ''
        foreach ($pair in @(
                @('mapbox auth login', 'Sign in to your Mapbox account'),
                @('mapbox --help', 'See every command'))) {
            Write-Part ('  {0,-20}' -f $pair[0]) 'Cyan'
            Write-Host " $($pair[1])"
        }
        Write-Host ''
        Write-Host 'Docs: https://cli.mapbox.com'

        # Said once, here, rather than on every future command: someone piping
        # this into `iex` is not going to read the man page before their first
        # run. Skipped when telemetry is already off, since there's nothing to
        # opt out of.
        if ($TelemetryAllowed) {
            Write-Host ''
            Write-Host 'Mapbox CLI collects telemetry by default. To disable it, set'
            Write-Host 'MAPBOX_CLI_NO_TELEMETRY=1. Learn more: https://github.com/mapbox/mapbox-cli#privacy'
        }
        Write-Host ''
    } finally {
        if ($staged -and (Test-Path -LiteralPath $staged)) {
            Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        }
        Remove-Item -LiteralPath $workDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}
