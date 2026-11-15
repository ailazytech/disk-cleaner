param([int]$X, [int]$Y, [int]$Delta = -1200, [string]$Proc = "disk-cleaner")
Add-Type @"
using System; using System.Runtime.InteropServices;
public class K {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern void mouse_event(int f, int dx, int dy, int d, int e);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[K]::SetProcessDPIAware() | Out-Null
$p = Get-Process -Name $Proc | Where-Object { $_.MainWindowHandle -ne 0 } | Sort-Object StartTime -Descending | Select-Object -First 1
[K]::SetForegroundWindow($p.MainWindowHandle) | Out-Null; Start-Sleep -Milliseconds 200
$r = New-Object K+RECT; [K]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
[K]::SetCursorPos($r.L + $X, $r.T + $Y) | Out-Null
for ($i = 0; $i -lt 10; $i++) { [K]::mouse_event(0x0800, 0, 0, [int]($Delta/10), 0); Start-Sleep -Milliseconds 30 }
