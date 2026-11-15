# Scroll a process's window without activating it: posts WM_MOUSEMOVE (so the
# app knows where the pointer is) and then WM_MOUSEWHEEL. Focus is not changed.
# Usage: powershell -File tools\scrollwin.ps1 -ProcId 1234 [-X 600 -Y 600] [-Notches -10]
param([int]$ProcId, [int]$X = 600, [int]$Y = 600, [int]$Notches = -10)
Add-Type @"
using System; using System.Runtime.InteropServices;
public class SW {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextLength(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint m, IntPtr w, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct POINT { public int X, Y; }
  [DllImport("user32.dll")] public static extern bool ClientToScreen(IntPtr h, ref POINT p);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  public static IntPtr Find(uint pid) {
    IntPtr found = IntPtr.Zero;
    EnumWindows((h, l) => {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p == pid && IsWindowVisible(h) && GetWindowTextLength(h) > 0) { found = h; return false; }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@
[SW]::SetProcessDPIAware() | Out-Null
$h = [SW]::Find([uint32]$ProcId)
if ($h -eq [IntPtr]::Zero) { Write-Error "No window for PID $ProcId"; exit 1 }
$client = [IntPtr](($Y -shl 16) -bor ($X -band 0xFFFF))
[SW]::PostMessage($h, 0x0200, [IntPtr]::Zero, $client) | Out-Null   # WM_MOUSEMOVE
Start-Sleep -Milliseconds 150
$pt = New-Object SW+POINT
$pt.X = $X; $pt.Y = $Y
[SW]::ClientToScreen($h, [ref]$pt) | Out-Null
$screen = [IntPtr](($pt.Y -shl 16) -bor ($pt.X -band 0xFFFF))
$step = if ($Notches -lt 0) { -120 } else { 120 }
for ($i = 0; $i -lt [Math]::Abs($Notches); $i++) {
  # The real cursor may be elsewhere (the app then sees "mouse left"), so
  # repeat the pointer position before every wheel step.
  [SW]::PostMessage($h, 0x0200, [IntPtr]::Zero, $client) | Out-Null
  $w = [IntPtr]([int]($step -shl 16))
  [SW]::PostMessage($h, 0x020A, $w, $screen) | Out-Null   # WM_MOUSEWHEEL
  Start-Sleep -Milliseconds 40
}
"scrolled $Notches notches"
