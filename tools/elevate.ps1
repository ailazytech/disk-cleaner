param([switch]$Admin)
$bat = Join-Path $PSScriptRoot 'mft_test.cmd'
$log = Join-Path $PSScriptRoot 'elev.log'
try {
  if ($Admin) {
    $p = Start-Process -FilePath $env:ComSpec -ArgumentList "/c `"$bat`"" -Verb RunAs -WindowStyle Hidden -PassThru -Wait
  } else {
    $p = Start-Process -FilePath $env:ComSpec -ArgumentList "/c echo test-ok > `"$PSScriptRoot\t.txt`"" -WindowStyle Hidden -PassThru -Wait
  }
  "exit: $($p.ExitCode)" | Out-File $log
} catch {
  "error: $_" | Out-File $log
}
