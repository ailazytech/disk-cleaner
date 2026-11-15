# Capture a process's main window with PrintWindow: does not steal focus and
# works even when the window is covered. A minimized window is restored with
# SW_SHOWNOACTIVATE first (no focus change).
# Usage: powershell -File tools\printwin.ps1 -ProcId 1234 -Out shot.png [-CropH 0]
param([int]$ProcId, [string]$Out = "shot.png", [int]$CropH = 0)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Text; using System.Runtime.InteropServices;
public class PW {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint p);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr h);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowTextLength(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  public static IntPtr Find(uint pid) {
    IntPtr found = IntPtr.Zero; int best = -1;
    EnumWindows((h, l) => {
      uint p; GetWindowThreadProcessId(h, out p);
      if (p == pid && IsWindowVisible(h) && GetWindowTextLength(h) > 0) {
        RECT r; GetWindowRect(h, out r); int area = (r.R - r.L) * (r.B - r.T);
        if (IsIconic(h)) area = int.MaxValue;
        if (area > best) { best = area; found = h; }
      }
      return true;
    }, IntPtr.Zero);
    return found;
  }
}
"@
[PW]::SetProcessDPIAware() | Out-Null
$h = [PW]::Find([uint32]$ProcId)
if ($h -eq [IntPtr]::Zero) { Write-Error "No window for PID $ProcId"; exit 1 }
if ([PW]::IsIconic($h)) { [PW]::ShowWindow($h, 4) | Out-Null; Start-Sleep -Milliseconds 800 }
$r = New-Object PW+RECT
[PW]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.R - $r.L; $hgt = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $hgt
$g = [System.Drawing.Graphics]::FromImage($bmp)
$hdc = $g.GetHdc()
[PW]::PrintWindow($h, $hdc, 2) | Out-Null   # PW_RENDERFULLCONTENT
$g.ReleaseHdc($hdc); $g.Dispose()
# Trim the window frame (about 8px left/right/bottom; keep the title bar)
$cx = 8; $cw = $w - 16; $ch = $hgt - 8
if ($CropH -gt 0 -and $CropH -lt $ch) { $ch = $CropH }
$crop = $bmp.Clone((New-Object System.Drawing.Rectangle $cx, 0, $cw, $ch), $bmp.PixelFormat)
$crop.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
"$cw x $ch -> $Out"
