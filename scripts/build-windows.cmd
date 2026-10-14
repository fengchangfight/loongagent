@echo off
REM One command, from cmd.exe or Explorer: build loongagent.exe for Windows.
REM The work lives in build-windows.ps1; this only finds it and forwards the
REM arguments, so `scripts\build-windows.cmd -Target gnu` works too.
setlocal
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0build-windows.ps1" %*
exit /b %ERRORLEVEL%
