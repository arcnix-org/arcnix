param(
    [Parameter(Mandatory)] [string] $Tag,
    [string] $Artifacts = 'dist',
    [ValidateSet('CurrentUser', 'AllUsers')] [string] $Scope = 'CurrentUser'
)

$ErrorActionPreference = 'Stop'
$stem = "arcnix-$Tag-windows-amd64"
$setup = Join-Path $Artifacts "$stem-setup.exe"
$portable = Join-Path $Artifacts "$stem.zip"
if ($Scope -eq 'AllUsers') {
    $installDir = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Arcnix'
    $programs = [Environment]::GetFolderPath('CommonPrograms')
    $registryRoot = 'HKLM:'
} else {
    $installDir = Join-Path ([Environment]::GetFolderPath('LocalApplicationData')) 'Programs\Arcnix'
    $programs = [Environment]::GetFolderPath('Programs')
    $registryRoot = 'HKCU:'
}
$app = Join-Path $installDir 'arcnix.exe'
$uninstaller = Join-Path $installDir 'unins000.exe'
$shortcut = Join-Path $programs 'Arcnix\Arcnix.lnk'
$uninstallKey = Join-Path $registryRoot 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{CB89D5EC-A886-4760-8578-99CCCE459EA1}_is1'

foreach ($path in @($installDir, $shortcut, $uninstallKey)) {
    if (Test-Path -LiteralPath $path) {
        throw "Test requires a Windows user without an Arcnix installation: $path"
    }
}

$expectedHash = python -c "import hashlib, sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); print(hashlib.file_digest(z.open(sys.argv[2]), 'sha256').hexdigest())" $portable "$stem/arcnix.exe"
if ($LASTEXITCODE -ne 0) { throw 'Cannot verify the portable ZIP executable.' }
$silent = @('/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART')
$version = $Tag.Substring(1)
try {
    $process = Start-Process -FilePath (Resolve-Path -LiteralPath $setup).Path `
        -ArgumentList ($silent + '/SP-' + "/$Scope") -WindowStyle Hidden -Wait -PassThru
    if ($process.ExitCode -ne 0) {
        throw "Arcnix Setup failed ($($process.ExitCode))."
    }
    if ((Get-FileHash -LiteralPath $app -Algorithm SHA256).Hash -ne $expectedHash) {
        throw 'The installed executable differs from the portable ZIP executable.'
    }
    python -c "import sys, zipfile; z = zipfile.ZipFile(sys.argv[1]); assert {'libmpv-2.dll', 'LICENSES/libmpv/LICENSE.LGPL', 'LICENSES/libmpv/SOURCE.txt'} <= set(z.namelist()) and z.testzip() is None" $app
    if ($LASTEXITCODE -ne 0) {
        throw 'The installed executable has an incomplete media runtime.'
    }
    if (-not (Test-Path -LiteralPath $shortcut)) {
        throw 'Setup did not create the Start menu shortcut.'
    }
    $registration = Get-ItemProperty -LiteralPath $uninstallKey
    if ($registration.DisplayName -ne 'Arcnix' -or $registration.DisplayVersion -ne $version) {
        throw 'Setup did not register the expected version in Windows installed apps.'
    }
    if ((& $app --version | Out-String).Trim() -ne "arcnix $version" -or $LASTEXITCODE -ne 0) {
        throw 'The installed application did not report the expected version.'
    }
    python "$PSScriptRoot/windows_webview_startup.py" $app
    if ($LASTEXITCODE -ne 0) { throw 'The installed desktop window did not start.' }
} finally {
    if (Test-Path -LiteralPath $uninstaller) {
        $process = Start-Process -FilePath $uninstaller -ArgumentList $silent -WindowStyle Hidden -Wait -PassThru
        if ($process.ExitCode -ne 0) {
            throw "Arcnix uninstall failed ($($process.ExitCode))."
        }
    }
}

foreach ($path in @($installDir, $shortcut, $uninstallKey)) {
    if (Test-Path -LiteralPath $path) {
        throw "Uninstall left behind $path"
    }
}
Write-Output "Verified Arcnix $version $Scope installation, version, shortcut and uninstall."
