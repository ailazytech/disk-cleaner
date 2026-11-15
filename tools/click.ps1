param([int]$X, [int]$Y, [string]$Proc = "disk-cleaner", [switch]$Right, [switch]$Double)
Add-Type @"
using System; using System.Runtime.InteropServices;
public class M {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(int f, int dx, int dy, int d, int e);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[M]::SetProcessDPIAware() | Out-Null
$p = Get-Process -Name $Proc | Where-Object { $_.MainWindowHandle -ne 0 } | Sort-Object StartTime -Descending | Select-Object -First 1
[M]::SetForegroundWindow($p.MainWindowHandle) | Out-Null
$r = New-Object M+RECT; [M]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
[M]::SetCursorPos($r.L + $X, $r.T + $Y) | Out-Null
Start-Sleep -Milliseconds 150
$down = 2; $up = 4; if ($Right) { $down = 8; $up = 16 }
$n = 1; if ($Double) { $n = 2 }
for ($i = 0; $i -lt $n; $i++) { [M]::mouse_event($down,0,0,0,0); Start-Sleep -Milliseconds 40; [M]::mouse_event($up,0,0,0,0); Start-Sleep -Milliseconds 60 }
