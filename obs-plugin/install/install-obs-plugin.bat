@echo off
setlocal
title Install Delay Bridge OBS plugin
cd /d "%~dp0"

rem Installs the ready-made "obs-delay-bridge" folder next to this script into
rem   %ProgramData%\obs-studio\plugins\obs-delay-bridge
rem which is where OBS looks for plugins (the only place OBS 34 and newer will).
rem You can also just copy that folder there yourself - see HOW-TO-INSTALL.txt.

set "SRC=%~dp0obs-delay-bridge"
set "DEST=%ProgramData%\obs-studio\plugins\obs-delay-bridge"
if not exist "%SRC%\obs-delay-bridge.dll" goto missing
if not exist "%SRC%\delay-bridge.exe" goto missing

tasklist /fi "imagename eq obs64.exe" 2>nul | find /i "obs64.exe" >nul
if not errorlevel 1 goto obsrunning

rem Older versions of this installer put the plugin in the OBS program folder. Removing that
rem copy needs administrator rights; nothing else does.
set "OBSDIR="
for /f "usebackq delims=" %%A in (`powershell -NoProfile -Command "try { (Get-ItemProperty 'HKLM:\SOFTWARE\OBS Studio' -ErrorAction Stop).'(default)' } catch { '' }"`) do set "OBSDIR=%%A"
if not defined OBSDIR set "OBSDIR=%ProgramFiles%\obs-studio"
set "LEGACY=%OBSDIR%\obs-plugins\64bit"

set "ISADMIN=0"
fltmc >nul 2>&1
if not errorlevel 1 set "ISADMIN=1"

if exist "%LEGACY%\obs-delay-bridge.dll" if "%ISADMIN%"=="0" goto elevate

if not exist "%ProgramData%\obs-studio\plugins" mkdir "%ProgramData%\obs-studio\plugins" 2>nul
robocopy "%SRC%" "%DEST%" /MIR /R:1 /W:1 /NJH /NJS /NFL /NDL /NP >nul
rem robocopy: exit codes 0-7 mean success.
if errorlevel 8 goto copyfail

if exist "%LEGACY%\obs-delay-bridge.dll" del /f /q "%LEGACY%\obs-delay-bridge.dll" 2>nul
if exist "%LEGACY%\delay-bridge.exe" del /f /q "%LEGACY%\delay-bridge.exe" 2>nul

echo.
echo  Delay Bridge OBS plugin installed to:
echo     %DEST%
echo.
echo  Next:
echo   1. Start OBS. Delay Bridge starts with it in the background, and the
echo      Delay Bridge dock appears (first time: it walks you through setup).
echo   2. In the dock's OBS card, click "Use Delay Bridge with my ... settings".
echo   3. OBS: Settings, Hotkeys: search for "Delay Bridge" to set your keys.
echo.
pause
exit /b 0

:copyfail
if "%ISADMIN%"=="1" goto copyfail2
goto elevate

:elevate
rem Guard: if we were already relaunched and still aren't admin, stop instead of looping.
if /i "%~1"=="--elevated" goto notadmin
echo Asking for administrator permission...
powershell -NoProfile -ExecutionPolicy Bypass -Command "try { Start-Process -FilePath '%~f0' -ArgumentList '--elevated' -Verb RunAs -ErrorAction Stop } catch { exit 1 }"
if errorlevel 1 goto notadmin
exit /b 0

:notadmin
echo.
echo  Administrator permission was not granted.
echo  You can install by hand instead: copy the "obs-delay-bridge" folder into
echo     %ProgramData%\obs-studio\plugins
echo  (paste that path into the File Explorer address bar; create "plugins" if missing).
echo.
pause
exit /b 1

:copyfail2
echo.
echo  Could not copy the plugin to %DEST%
echo  Close OBS and try again, or copy the "obs-delay-bridge" folder there by hand.
echo.
pause
exit /b 1

:missing
echo  The "obs-delay-bridge" folder must be next to this script.
echo  Run build.bat first, then use the copy in dist\obs-plugin.
pause
exit /b 1

:obsrunning
echo  OBS is running. Close OBS completely, then run this installer again.
pause
exit /b 1
