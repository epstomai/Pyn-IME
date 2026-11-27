<#
  Pyn 拼音输入法 Windows 安装脚本（随 release 包分发）。
  用法：右键 install.cmd 运行，或 powershell -ExecutionPolicy Bypass -File install.ps1 [-Dir <目录>]
  会请求管理员权限：注册 COM/TSF 需要写 HKLM。
#>
param(
    [string]$Dir = (Join-Path $env:ProgramFiles 'Pyn'),
    [switch]$NoPause
)
$ErrorActionPreference = 'Stop'

$Clsid = '{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0101}'
$ProfileGuid = '{A94E2C1B-7D3F-4E8A-9B12-6C5F8A3D0102}'
$LangId = '0x00000804'
$TipId = "0804:$Clsid$ProfileGuid"

function Test-Admin {
    $p = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
    return $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
}

if (-not (Test-Admin)) {
    $argList = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-Dir', "`"$Dir`"")
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
    $src = $PSScriptRoot
    $files = @('ime_tsf.dll', 'ime_tsf_x86.dll', 'ime-server.exe', 'ime-settings.exe', 'models\ime_model.bin', 'models\reranker.bin')
    foreach ($f in $files) {
        if (-not (Test-Path -LiteralPath (Join-Path $src $f))) { throw "安装包缺少文件：$f" }
    }

    Write-Host "安装目录：$Dir"
    New-Item -ItemType Directory -Force -Path (Join-Path $Dir 'models') | Out-Null

    # 停止旧的后台服务（模型/词库常驻进程），让 exe 和模型可以覆盖。
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

    # 已打开的程序可能正占用旧 DLL：改名后再放新文件，旧文件重启后删除。
    $stamp = [DateTime]::Now.ToString('yyyyMMddHHmmss')
    foreach ($f in $files) {
        $from = Join-Path $src $f
        $to = Join-Path $Dir $f
        try {
            Copy-Item -LiteralPath $from -Destination $to -Force
        } catch {
            $old = "$to.old.$stamp"
            Rename-Item -LiteralPath $to -NewName (Split-Path $old -Leaf) -Force
            Copy-Item -LiteralPath $from -Destination $to -Force
        }
    }

    # 允许 UWP / 应用容器进程（开始菜单搜索、商店应用等）加载输入法 DLL。
    & icacls.exe $Dir /grant '*S-1-15-2-1:(OI)(CI)(RX)' /T /Q | Out-Null

    $reg64 = Join-Path $env:windir 'System32\regsvr32.exe'
    $reg32 = Join-Path $env:windir 'SysWOW64\regsvr32.exe'
    if (Test-Path -LiteralPath $reg32) {
        $rc = Invoke-Regsvr $reg32 @('/s', "`"$(Join-Path $Dir 'ime_tsf_x86.dll')`"")
        if ($rc -ne 0) { throw "注册 32 位 DLL 失败（regsvr32 退出码 $rc）" }
    }
    $rc = Invoke-Regsvr $reg64 @('/s', "`"$(Join-Path $Dir 'ime_tsf.dll')`"")
    if ($rc -ne 0) { throw "注册 64 位 DLL 失败（regsvr32 退出码 $rc）" }

    Add-Type -Namespace Pyn -Name Input -MemberDefinition @'
[DllImport("input.dll", CharSet = CharSet.Unicode)]
public static extern bool InstallLayoutOrTip(string psz, uint dwFlags);
'@
    if (-not [Pyn.Input]::InstallLayoutOrTip($TipId, 0)) { Write-Warning '未能自动添加到输入法列表，请在“设置 > 时间和语言 > 语言 > 中文 > 键盘”中手动添加 Pyn。' }

    $profKey = "HKCU:\Software\Microsoft\CTF\TIP\$Clsid\LanguageProfile\$LangId\$ProfileGuid"
    New-Item -Force -Path $profKey | Out-Null
    Set-ItemProperty -Path $profKey -Name Enable -Value 1 -Type DWord

    # 让当前会话立即可用（无需注销）。
    Invoke-Regsvr $reg64 @('/n', '/i:activate', '/s', "`"$(Join-Path $Dir 'ime_tsf.dll')`"") | Out-Null

    # 开机自启后台服务；输入法首次使用时也会自动拉起。
    $run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
    Set-ItemProperty -Path $run -Name PynServer -Value "`"$srv`""

    $programs = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs'
    if (Test-Path -LiteralPath $programs) {
        $settingsExe = Join-Path $Dir 'ime-settings.exe'
        $s = (New-Object -ComObject WScript.Shell).CreateShortcut((Join-Path $programs 'Pyn 设置.lnk'))
        $s.TargetPath = $settingsExe
        $s.WorkingDirectory = $Dir
        $s.IconLocation = "$settingsExe,0"
        $s.Description = 'Pyn 外观与输入设置'
        $s.Save()
    }

    Write-Host ''
    Write-Host 'Pyn 安装完成。按 Win+空格 切换到 Pyn；开始菜单中的“Pyn 设置”可调整外观与选项。' -ForegroundColor Green
    Write-Host '已经打开的程序如未出现 Pyn，重新打开该程序即可。'
    Finish 0
} catch {
    Write-Host "安装失败：$($_.Exception.Message)" -ForegroundColor Red
    Finish 1
}
