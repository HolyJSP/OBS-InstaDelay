@echo off
setlocal
title Install Delay Bridge OBS plugin
cd /d "%~dp0"

if not exist "%~dp0obs-delay-bridge.dll" goto missing
if not exist "%~dp0delay-bridge.exe" goto missing

rem Copying into the OBS program folder needs administrator rights.
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
rem Find where OBS is installed (the OBS installer records it in the registry).
set "OBSDIR="
for /f "usebackq delims=" %%A in (`powershell -NoProfile -Command "try { (Get-ItemProperty 'HKLM:\SOFTWARE\OBS Studio' -ErrorAction Stop).'(default)' } catch { '' }"`) do set "OBSDIR=%%A"
if not defined OBSDIR set "OBSDIR=%ProgramFiles%\obs-studio"
if not exist "%OBSDIR%\bin\64bit\obs64.exe" goto noobs

tasklist /fi "imagename eq obs64.exe" 2>nul | find /i "obs64.exe" >nul
if not errorlevel 1 goto obsrunning

set "DEST=%OBSDIR%\obs-plugins\64bit"
copy /y "%~dp0obs-delay-bridge.dll" "%DEST%\" >nul
if errorlevel 1 goto copyfail
copy /y "%~dp0delay-bridge.exe" "%DEST%\" >nul
if errorlevel 1 goto copyfail

echo.
echo  Delay Bridge OBS plugin installed to:
echo     %DEST%
echo.
echo  Next:
echo   1. Start OBS. Delay Bridge starts with it in the background.
echo      The first time, the dashboard opens so you can add your stream key.
echo   2. OBS: Settings, Stream: Service Custom, Server rtmp://127.0.0.1:1935/live
echo   3. OBS: Settings, Hotkeys: search for "Delay Bridge" to set your keys.
echo   4. OBS: Tools menu, Delay Bridge opens the dashboard any time.
echo.
pause
exit /b 0

:missing
echo  obs-delay-bridge.dll and delay-bridge.exe must be in the same folder as this script.
echo  Run build.bat first, then use the copy in dist\obs-plugin.
pause
exit /b 1

:noobs
echo  Could not find OBS Studio at: %OBSDIR%
echo  Install OBS from https://obsproject.com, or copy obs-delay-bridge.dll and
echo  delay-bridge.exe into your OBS folder under obs-plugins\64bit yourself.
pause
exit /b 1

:obsrunning
echo  OBS is running. Close OBS completely, then run this installer again.
pause
exit /b 1

:copyfail
echo  Copying failed. Make sure OBS is closed and try again.
pause
exit /b 1
