@echo off
setlocal
title Uninstall Delay Bridge OBS plugin

rem Removing files from the OBS program folder needs administrator rights.
rem "fltmc" only succeeds when running as administrator (unlike "net session", it doesn't
rem depend on the Server service, which is disabled on many PCs).
fltmc >nul 2>&1
if not errorlevel 1 goto admin

rem Guard: if we were already relaunched and still aren't admin, stop instead of looping.
if /i "%~1"=="--elevated" goto notadmin

echo Asking for administrator permission...
powershell -NoProfile -ExecutionPolicy Bypass -Command "try { Start-Process -FilePath '%~f0' -ArgumentList '--elevated' -Verb RunAs -ErrorAction Stop } catch { exit 1 }"
if errorlevel 1 goto notadmin
exit /b 0

:notadmin
echo.
echo  Administrator permission was not granted.
echo  Right-click this file and choose "Run as administrator", or copy these two files
echo  by hand into your OBS folder, usually C:\Program Files\obs-studio\obs-plugins\64bit :
echo     obs-delay-bridge.dll
echo     delay-bridge.exe
echo.
pause
exit /b 1

:admin
set "OBSDIR="
for /f "usebackq delims=" %%A in (`powershell -NoProfile -Command "try { (Get-ItemProperty 'HKLM:\SOFTWARE\OBS Studio' -ErrorAction Stop).'(default)' } catch { '' }"`) do set "OBSDIR=%%A"
if not defined OBSDIR set "OBSDIR=%ProgramFiles%\obs-studio"

tasklist /fi "imagename eq obs64.exe" 2>nul | find /i "obs64.exe" >nul
if not errorlevel 1 goto obsrunning

del /f /q "%OBSDIR%\obs-plugins\64bit\obs-delay-bridge.dll" 2>nul
del /f /q "%OBSDIR%\obs-plugins\64bit\delay-bridge.exe" 2>nul
echo.
echo  Delay Bridge OBS plugin removed.
echo  Your settings are kept in %APPDATA%\obs-studio\plugin_config\obs-delay-bridge
echo  Delete that folder too if you want to remove your saved stream keys.
echo.
pause
exit /b 0

:obsrunning
echo  OBS is running. Close OBS completely, then run this again.
pause
exit /b 1
