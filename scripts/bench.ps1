# Drives the release build through startup, scrolling, typing and search on a
# test vault and summarizes the frame timings logged via ZK_PROFILE.
#
# Usage: pwsh scripts/bench.ps1 -Vault <dir> [-Label <name>]
param(
    [Parameter(Mandatory)] [string]$Vault,
    [string]$Label = "run"
)

$ErrorActionPreference = "Stop"
$exe = Join-Path $PSScriptRoot "..\target\release\zettelkasten.exe"
$configDir = Join-Path $env:TEMP "zk-bench-config"
$log = Join-Path $env:TEMP "zk-bench-$Label.log"

Get-Process zettelkasten -ErrorAction SilentlyContinue | Stop-Process -Force
Remove-Item $configDir -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory $configDir | Out-Null
@{ last_vault = (Resolve-Path $Vault).Path; recent_vaults = @((Resolve-Path $Vault).Path) } |
    ConvertTo-Json | Set-Content (Join-Path $configDir "config.json")
Remove-Item $log -ErrorAction SilentlyContinue
Remove-Item (Join-Path $Vault ".zettelkasten\state.json") -ErrorAction SilentlyContinue

Add-Type -AssemblyName System.Windows.Forms
if (-not ("ZkBench" -as [type])) {
    Add-Type @"
using System; using System.Runtime.InteropServices;
public class ZkBench {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(int f, int x, int y, int d, int e);
  public struct RECT { public int L, T, R, B; }
}
"@
}
[ZkBench]::SetProcessDPIAware() | Out-Null

function Mark($name) { Add-Content $log "## $name" }

$env:ZETTELKASTEN_DIR = $configDir
$env:ZK_PROFILE = $log
Mark "startup"
$sw = [Diagnostics.Stopwatch]::StartNew()
$proc = Start-Process $exe -PassThru
# Wait until the window exists and frames have settled.
while ($proc.MainWindowHandle -eq 0 -and $sw.Elapsed.TotalSeconds -lt 60) { Start-Sleep -Milliseconds 50; $proc.Refresh() }
$windowMs = $sw.ElapsedMilliseconds
Start-Sleep 6
Remove-Item Env:ZK_PROFILE

$h = $proc.MainWindowHandle
[ZkBench]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 500
$r = New-Object ZkBench+RECT
[ZkBench]::GetWindowRect($h, [ref]$r) | Out-Null
$cx = [int](($r.L + $r.R) / 2); $cy = [int](($r.T + $r.B) / 2)

Mark "scroll"
[ZkBench]::SetCursorPos($cx, $cy) | Out-Null
for ($i = 0; $i -lt 40; $i++) { [ZkBench]::mouse_event(0x0800, 0, 0, -120, 0); Start-Sleep -Milliseconds 60 }
Start-Sleep 1

Mark "typing"
[System.Windows.Forms.SendKeys]::SendWait("^n"); Start-Sleep -Milliseconds 800
foreach ($ch in "Benchmark typing in a new note".ToCharArray()) {
    [System.Windows.Forms.SendKeys]::SendWait([string]$ch); Start-Sleep -Milliseconds 80
}
Start-Sleep 1
Mark "cleanup"
[System.Windows.Forms.SendKeys]::SendWait("^+{BACKSPACE}"); Start-Sleep -Milliseconds 500

Mark "search"
[System.Windows.Forms.SendKeys]::SendWait("^k"); Start-Sleep -Milliseconds 300
foreach ($ch in "rust".ToCharArray()) {
    [System.Windows.Forms.SendKeys]::SendWait([string]$ch); Start-Sleep -Milliseconds 150
}
Start-Sleep 1
Mark "end"
Stop-Process $proc -Force

# ----- summary -----
function Stats($values) {
    if ($values.Count -eq 0) { return "n=0" }
    $s = $values | Sort-Object
    $p = { param($q) $s[[Math]::Min($s.Count - 1, [int][Math]::Floor($q * $s.Count))] }
    [string]::Format([Globalization.CultureInfo]::InvariantCulture,
        "n={0,-4} median={1,7:F1} p95={2,7:F1} max={3,7:F1} ms", $s.Count, (& $p 0.5), (& $p 0.95), $s[-1])
}

$section = ""; $paint = @{}; $build = @{}; $extra = @()
foreach ($line in Get-Content $log) {
    if ($line -like "## *") { $section = $line.Substring(3); continue }
    if ($line -match "^frame build=([\d.]+) paint=([\d.]+)") {
        if (-not $paint[$section]) { $paint[$section] = @(); $build[$section] = @() }
        $paint[$section] += [double]$Matches[2]; $build[$section] += [double]$Matches[1]
    } else { $extra += $line }
}
"=== $Label ==="
"window visible after $windowMs ms"
$extra | ForEach-Object { "  $_" }
foreach ($name in "startup", "scroll", "typing", "search") {
    "{0,-8} frame total: {1}" -f $name, (Stats $paint[$name])
    "{0,-8} build only:  {1}" -f "", (Stats $build[$name])
}
