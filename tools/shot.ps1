param([string]$Proc = "disk-cleaner", [string]$Out = "shot.png", [int]$CropH = 0)
Add-Type -AssemblyName System.Drawing
Add-Type @"
using System; using System.Runtime.InteropServices;
public class W {
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
}
"@
[W]::SetProcessDPIAware() | Out-Null
$p = Get-Process -Name $Proc -ErrorAction Stop | Where-Object { $_.MainWindowHandle -ne 0 } | Sort-Object StartTime -Descending | Select-Object -First 1
$h = $p.MainWindowHandle
[W]::ShowWindow($h, 9) | Out-Null
[W]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 600
$r = New-Object W+RECT
[W]::GetWindowRect($h, [ref]$r) | Out-Null
$w = $r.R - $r.L; $hh = $r.B - $r.T
$bmp = New-Object System.Drawing.Bitmap $w, $hh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.CopyFromScreen($r.L, $r.T, 0, 0, $bmp.Size)
# 去掉視窗外框的陰影區（會拍到後面的視窗）
$b = 8
$ch = if ($CropH -gt 0 -and $CropH -lt $hh) { $CropH } else { $hh - $b }
$bmp = $bmp.Clone((New-Object System.Drawing.Rectangle $b, 0, ($w - 2 * $b), $ch), $bmp.PixelFormat)
$bmp.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "$w x $hh -> $Out"
