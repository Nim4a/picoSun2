@echo off
for %%k in (picosun2.photo picoSun.photo picosun2.raw picoSun.raw picosun2.hdr picoSun.hdr picosun2.anim picoSun.anim) do (
  reg query "HKCU\Software\Classes\%%k\shell\open\command" /ve 2>nul | findstr /i "Run" >nul
  if errorlevel 1 (
    reg query "HKCU\Software\Classes\%%k\shell\open\command" /ve 2>nul | findstr /v "error" > "%TEMP%\_q.txt" 2>nul
    findstr /i /v "" "%TEMP%\_q.txt" && echo [%%k]
  )
)
