@echo off
rem 以系統管理員身分執行（tools\elevate.ps1 -Admin），測試 MFT 完整讀取與 USN 增量更新。
rem Run elevated (tools\elevate.ps1 -Admin) to benchmark full MFT reads and USN incremental updates.
chcp 65001 >nul
set EXE=%~dp0..\target\release\disk-cleaner.exe
set OUT=%~dp0mft_out.txt
echo === %SystemDrive% full MFT === > "%OUT%"
"%EXE%" --bench %SystemDrive%\ >> "%OUT%" 2>&1
echo === %SystemDrive% incremental (USN) === >> "%OUT%"
"%EXE%" --bench %SystemDrive%\ >> "%OUT%" 2>&1
echo === subfolder via drive index === >> "%OUT%"
"%EXE%" --bench "%LOCALAPPDATA%" >> "%OUT%" 2>&1
echo === DONE === >> "%OUT%"
