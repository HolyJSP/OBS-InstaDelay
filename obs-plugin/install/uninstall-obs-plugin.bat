@echo off
setlocal
title Uninstall Delay Bridge OBS plugin

tasklist /fi "imagename eq obs64.exe" 2>nul | find /i "obs64.exe" >nul
if not errorlevel 1 goto obsrunning

set "DEST=%ProgramData%\obs-studio\plugins\obs-delay-bridge"
set "OBSDIR="
for /f "usebackq delims=" %%A in (`powershell -NoProfile -Command "try { (Get-ItemProperty 'HKLM:\SOFTWARE\OBS Studio' -ErrorAction Stop).'(default)' } catch { '' }"`) do set "OBSDIR=%%A"
if not defined OBSDIR set "OBSDIR=%ProgramFiles%\obs-studio"
set "LEGACY=%OBSDIR%\obs-plugins\64bit"

set "ISADMIN=0"
fltmc >nul 2>&1
if not errorlevel 1 set "ISADMIN=1"

if exist "%DEST%" rmdir /s /q "%DEST%" 2>nul
if exist "%DEST%" goto needadmin
if exist "%LEGACY%\obs-delay-bridge.dll" goto needadmin_legacy
goto done

:needadmin_legacy
if "%ISADMIN%"=="0" goto needadmin
del /f /q "%LEGACY%\obs-delay-bridge.dll" 2>nul
del /f /q "%LEGACY%\delay-bridge.exe" 2>nul
goto done

:needadmin
if "%ISADMIN%"=="1" goto failed
rem Guard: if we were already relaunched and still aren't admin, stop instead of looping.
if /i "%~1"=="--elevated" goto failed
echo Asking for administrator permission...
powershell -NoProfile -ExecutionPolicy Bypass -Command "try { Start-Process -FilePath '%~f0' -ArgumentList '--elevated' -Verb RunAs -ErrorAction Stop } catch { exit 1 }"
if errorlevel 1 goto failed
exit /b 0

:done
echo.
echo  Delay Bridge OBS plugin removed.
echo  Your settings are kept in %APPDATA%\obs-studio\plugin_config\obs-delay-bridge
echo  Delete that folder too if you want to remove your saved stream keys.
echo.
pause
exit /b 0

:failed
echo.
echo  Could not remove everything. Close OBS, then delete by hand:
echo     %DEST%
echo     %LEGACY%\obs-delay-bridge.dll and delay-bridge.exe (if present)
echo.
pause
exit /b 1

:obsrunning
echo  OBS is running. Close OBS completely, then run this again.
pause
exit /b 1
