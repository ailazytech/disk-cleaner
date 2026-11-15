param([int]$X, [int]$Y, [string]$Proc = "disk-cleaner")
Add-Type @"
using System; using System.Runtime.InteropServices;
public class H {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[H]::SetProcessDPIAware() | Out-Null
$p = Get-Process -Name $Proc | Where-Object { $_.MainWindowHandle -ne 0 } | Sort-Object StartTime -Descending | Select-Object -First 1
$r = New-Object H+RECT; [H]::GetWindowRect($p.MainWindowHandle, [ref]$r) | Out-Null
[H]::SetCursorPos($r.L + $X - 2, $r.T + $Y) | Out-Null
Start-Sleep -Milliseconds 80
[H]::SetCursorPos($r.L + $X, $r.T + $Y) | Out-Null
