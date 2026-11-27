<#
  Pyn 拼音输入法 Windows 卸载脚本。
  默认保留用户词库与设置（%LOCALAPPDATA%\Pyn）；加 -RemoveUserData 一并删除。
#>
param(
    [string]$Dir = (Join-Path $env:ProgramFiles 'Pyn'),
    [switch]$RemoveUserData,
    [switch]$NoPause
)
$ErrorActionPreference = 'Stop'

$Clsid = '{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0101}'
$ProfileGuid = '{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0102}'
$TipId = "0804:$Clsid$ProfileGuid"

$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Dir', "`"$Dir`"")
    if ($RemoveUserData) { $argList += '-RemoveUserData' }
    if ($NoPause) { $argList += '-NoPause' }
    Start-Process powershell.exe -Verb RunAs -ArgumentList $argList
    exit
}

# Wait for regsvr32 itself only: `Start-Process -Wait` also waits for descendants, and activating
# the TIP spawns the long-lived ime-server.exe, which would block the script indefinitely.
function Invoke-Regsvr([string]$exe, [string[]]$arguments) {
    $p = Start-Process $exe -ArgumentList $arguments -PassThru
    $null = $p.Handle  # cache the handle so ExitCode is available after exit
    $p.WaitForExit()
    return $p.ExitCode
}

function Finish([int]$code) {
    if (-not $NoPause) { Read-Host '按回车键退出' | Out-Null }
    exit $code
}

try {
    Add-Type -Namespace Pyn -Name Native -MemberDefinition @'
[DllImport("input.dll", CharSet = CharSet.Unicode)]
public static extern bool InstallLayoutOrTip(string psz, uint dwFlags);
[DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
public static extern bool MoveFileEx(string src, string dst, int flags);
'@

    $srv = Join-Path $Dir 'ime-server.exe'
    if (Test-Path -LiteralPath $srv) {
        # Best effort: fails harmlessly when the service is not running.
        try {
            $sp = Start-Process $srv -ArgumentList '--shutdown' -WindowStyle Hidden -PassThru
            $null = $sp.WaitForExit(5000)
        } catch { }
        Start-Sleep -Milliseconds 800
    }
    Get-Process ime-server -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and ($_.Path -ieq $srv) } |
        Stop-Process -Force -ErrorAction SilentlyContinue

    [Pyn.Native]::InstallLayoutOrTip($TipId, 1) | Out-Null  # ILOT_UNINSTALL

    $reg64 = Join-Path $env:windir 'System32\regsvr32.exe'
    $reg32 = Join-Path $env:windir 'SysWOW64\regsvr32.exe'
    $dll86 = Join-Path $Dir 'ime_tsf_x86.dll'
    $dll64 = Join-Path $Dir 'ime_tsf.dll'
    if ((Test-Path -LiteralPath $reg32) -and (Test-Path -LiteralPath $dll86)) {
        Invoke-Regsvr $reg32 @('/u', '/s', "`"$dll86`"") | Out-Null
    }
    if (Test-Path -LiteralPath $dll64) {
        Invoke-Regsvr $reg64 @('/u', '/s', "`"$dll64`"") | Out-Null
    }

    Remove-Item -Recurse -Force -Path "HKCU:\Software\Microsoft\CTF\TIP\$Clsid" -ErrorAction SilentlyContinue
    Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name PynServer -ErrorAction SilentlyContinue
    Remove-Item -Force -LiteralPath (Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\Pyn 设置.lnk') -ErrorAction SilentlyContinue

    # 仍被已打开程序占用的 DLL 在重启后删除。
    $pending = 0
    if (Test-Path -LiteralPath $Dir) {
        Get-ChildItem -LiteralPath $Dir -Recurse -File | ForEach-Object {
            try { Remove-Item -LiteralPath $_.FullName -Force }
            catch { [Pyn.Native]::MoveFileEx($_.FullName, $null, 4) | Out-Null; $pending++ }
        }
        try { Remove-Item -LiteralPath $Dir -Recurse -Force } catch { [Pyn.Native]::MoveFileEx($Dir, $null, 4) | Out-Null }
    }

    if ($RemoveUserData) {
        Remove-Item -Recurse -Force -LiteralPath (Join-Path $env:LOCALAPPDATA 'Pyn') -ErrorAction SilentlyContinue
    }

    Write-Host 'Pyn 已卸载。' -ForegroundColor Green
    if ($pending -gt 0) { Write-Host "有 $pending 个文件正被已打开的程序使用，将在重启后删除。" }
    if (-not $RemoveUserData) { Write-Host "用户词库与设置保留在 $(Join-Path $env:LOCALAPPDATA 'Pyn')。" }
    Finish 0
} catch {
    Write-Host "卸载失败：$($_.Exception.Message)" -ForegroundColor Red
    Finish 1
}
