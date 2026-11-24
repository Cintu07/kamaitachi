# Installs kamaitachi for the current user: no admin rights, nothing outside your
# own profile and HKCU.
#
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1            # install or update
#   powershell -ExecutionPolicy Bypass -File scripts\install.ps1 -Uninstall # take it all out
#
# It copies kamaitachi.exe to %LOCALAPPDATA%\Programs\kamaitachi, adds a Start menu
# entry, and tells Windows it is a browser, so it shows up under Settings > Apps >
# Default apps. It never makes itself the default; that stays your choice.
# Uninstalling leaves %APPDATA%\hayate alone (your stamps, skin and filter lists).

param(
    [string]$Exe = "",
    [switch]$Uninstall
)
$ErrorActionPreference = "Stop"

$dir = Join-Path $env:LOCALAPPDATA "Programs\kamaitachi"
$target = Join-Path $dir "kamaitachi.exe"
$link = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\kamaitachi.lnk"
$client = "HKCU:\Software\Clients\StartMenuInternet\kamaitachi"
$progId = "HKCU:\Software\Classes\kamaitachiURL"

if ($Uninstall) {
    foreach ($p in @($client, $progId)) {
        if (Test-Path $p) { Remove-Item $p -Recurse -Force }
    }
    Remove-ItemProperty "HKCU:\Software\RegisteredApplications" -Name "kamaitachi" -ErrorAction SilentlyContinue
    if (Test-Path $link) { Remove-Item $link -Force }
    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
    Write-Output "kamaitachi removed. Your data in $env:APPDATA\hayate was kept."
    exit 0
}

# the exe next to this script (a downloaded release), else the one cargo built
if ($Exe -eq "") {
    $here = Join-Path $PSScriptRoot "kamaitachi.exe"
    $built = Join-Path $PSScriptRoot "..\target\release\kamaitachi.exe"
    if (Test-Path $here) { $Exe = $here } elseif (Test-Path $built) { $Exe = $built }
}
if ($Exe -eq "" -or -not (Test-Path $Exe)) {
    throw "no kamaitachi.exe found; build it with 'cargo build --release' or pass -Exe <path>"
}

New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item $Exe $target -Force

$shell = New-Object -ComObject WScript.Shell
$lnk = $shell.CreateShortcut($link)
$lnk.TargetPath = $target
$lnk.WorkingDirectory = $dir
$lnk.Description = "kamaitachi, a light browser"
$lnk.Save()

# what Windows needs to list it as a browser. New-Item -Force on a registry key that
# already exists wipes its values, and RegisteredApplications holds every other
# app's entry, so only keys that are missing get made
function Ensure($key) { if (-not (Test-Path $key)) { New-Item -Path $key -Force | Out-Null } }
$open = "`"$target`" `"%1`""
Ensure "$progId\shell\open\command"
Set-Item "$progId" "kamaitachi page"
Set-Item "$progId\shell\open\command" $open
Ensure "$progId\DefaultIcon"
Set-Item "$progId\DefaultIcon" "$target,0"

Ensure "$client\Capabilities\URLAssociations"
Ensure "$client\shell\open\command"
Ensure "$client\DefaultIcon"
Set-Item "$client" "kamaitachi"
Set-Item "$client\shell\open\command" "`"$target`""
Set-Item "$client\DefaultIcon" "$target,0"
Set-ItemProperty "$client\Capabilities" -Name ApplicationName -Value "kamaitachi"
Set-ItemProperty "$client\Capabilities" -Name ApplicationDescription -Value "A browser that stays light"
Set-ItemProperty "$client\Capabilities" -Name ApplicationIcon -Value "$target,0"
Set-ItemProperty "$client\Capabilities\URLAssociations" -Name http -Value "kamaitachiURL"
Set-ItemProperty "$client\Capabilities\URLAssociations" -Name https -Value "kamaitachiURL"
Ensure "HKCU:\Software\RegisteredApplications"
Set-ItemProperty "HKCU:\Software\RegisteredApplications" -Name kamaitachi -Value "Software\Clients\StartMenuInternet\kamaitachi\Capabilities"

$mb = [math]::Round((Get-Item $target).Length / 1MB, 2)
Write-Output "kamaitachi installed to $target ($mb MB)."
Write-Output "Start it from the Start menu. To make it your default browser: Settings > Apps > Default apps > kamaitachi."
