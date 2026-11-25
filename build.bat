@echo off
setlocal
title Build Delay Bridge
cd /d "%~dp0"

echo.
echo  ===============================
echo   Building Delay Bridge
echo  ===============================
echo.

where cargo >nul 2>nul
if errorlevel 1 goto norust

rem Link the C runtime statically so the .exe runs on any Windows PC without extra installs.
set "RUSTFLAGS=-C target-feature=+crt-static"

if /i "%~1"=="test" goto test

set "EXE=target\release\delay-bridge.exe"
set "DLL=target\release\obs_delay_bridge.dll"
set "LOG=%cd%\build.log"

rem Remove the previous build first, so a failed build can't leave old files that look new.
if exist "%EXE%" del /f /q "%EXE%" 2>nul
if exist "%EXE%" goto inuse
if exist "%DLL%" del /f /q "%DLL%" 2>nul
if exist "%DLL%" goto inuse

echo Compiling Delay Bridge and the OBS plugin.
echo The first build downloads dependencies and takes a few minutes...
echo.
cargo build --release --workspace
if errorlevel 1 goto failed
if not exist "%EXE%" goto failed
if not exist "%DLL%" goto failed

echo.
echo Putting the files together in dist ...
if exist dist rmdir /s /q dist 2>nul
if exist dist goto distinuse

set "PF=dist\obs-plugin\obs-delay-bridge"
mkdir "%PF%\bin\64bit" || goto copyfailed
mkdir "%PF%\data" || goto copyfailed
copy /y "%EXE%" dist\ >nul || goto copyfailed
copy /y README.md dist\ >nul || goto copyfailed
copy /y LICENSE dist\ >nul || goto copyfailed
copy /y config.example.json dist\ >nul || goto copyfailed
xcopy docs dist\docs\ /e /i /q /y >nul || goto copyfailed
rem The drop-in plugin folder (copy it into C:\ProgramData\obs-studio\plugins).
rem Top level: OBS 33 and newer.  bin\64bit: older OBS (finds delay-bridge.exe two levels up).
copy /y "%DLL%" "%PF%\obs-delay-bridge.dll" >nul || goto copyfailed
copy /y "%DLL%" "%PF%\bin\64bit\obs-delay-bridge.dll" >nul || goto copyfailed
copy /y "%EXE%" "%PF%\delay-bridge.exe" >nul || goto copyfailed
copy /y obs-plugin\install\install-obs-plugin.bat dist\obs-plugin\ >nul || goto copyfailed
copy /y obs-plugin\install\uninstall-obs-plugin.bat dist\obs-plugin\ >nul || goto copyfailed
copy /y obs-plugin\install\HOW-TO-INSTALL.txt dist\obs-plugin\ >nul || goto copyfailed
if not exist "%PF%\obs-delay-bridge.dll" goto copyfailed

echo.
echo  Done! Built into %cd%\dist
echo.
echo    dist\obs-plugin\obs-delay-bridge\   the OBS plugin folder: drag it into
echo                                        C:\ProgramData\obs-studio\plugins
echo    dist\obs-plugin\install-obs-plugin.bat   or run this to copy it there for you
echo    dist\delay-bridge.exe               standalone program, if you prefer to start it yourself
echo.
rem Show the plugin folder in File Explorer.
start "" explorer "%cd%\dist\obs-plugin"
choice /c YN /t 20 /d N /m "Install the OBS plugin now? Close OBS first"
if errorlevel 2 goto skipinstall
call dist\obs-plugin\install-obs-plugin.bat
exit /b 0

:skipinstall
echo.
echo  You can install it later: drag dist\obs-plugin\obs-delay-bridge into
echo  C:\ProgramData\obs-studio\plugins, or run dist\obs-plugin\install-obs-plugin.bat
echo  Setup guide: docs\OBS-SETUP.md
echo.
pause
exit /b 0

:test
echo Running tests - the end-to-end tests need ffmpeg on your PATH and take about 2 minutes...
echo.
cargo test --release --workspace -- --test-threads=1 --nocapture
if errorlevel 1 goto testfailed
echo.
echo  All tests passed.
pause
exit /b 0

:testfailed
echo.
echo  Some tests failed - see the messages above.
pause
exit /b 1

:norust
echo  Rust is not installed yet. To install it:
echo.
echo    1. Go to https://rustup.rs and download rustup-init.exe
echo    2. Run it and press Enter to accept the defaults.
echo       If it offers to install the Visual Studio C++ Build Tools, say yes.
echo    3. Close this window, then double-click build.bat again.
echo.
pause
exit /b 1

:inuse
echo.
echo  Can't replace the previous build: Delay Bridge is still running from this folder.
echo  Close OBS and any Delay Bridge window (or end delay-bridge.exe in Task Manager),
echo  then run build.bat again.
echo.
pause
exit /b 1

:distinuse
echo.
echo  Can't clear the old "dist" folder: something is still using it.
echo  Close OBS, any Delay Bridge window, and File Explorer windows showing "dist",
echo  then run build.bat again.
echo.
pause
exit /b 1

:copyfailed
echo.
echo  The build worked, but copying the files into dist failed.
echo  Close anything using the "dist" folder and run build.bat again.
echo.
pause
exit /b 1

:failed
rem Run it again (only the failing part rebuilds, so this is quick) to save the errors to a file.
cargo build --release --workspace > "%LOG%" 2>&1
echo.
echo  Build failed - see the messages above. They are also saved in:
echo     %LOG%
echo.
echo  Common fixes:
echo   - "link.exe not found" or "cl.exe not found": install the Visual Studio Build Tools
echo     with "Desktop development with C++" (https://visualstudio.microsoft.com/visual-cpp-build-tools/)
echo   - Update Rust: run "rustup update" (needs Rust 1.88 or newer)
echo.
echo  Still stuck? Open an issue and attach build.log:
echo  https://github.com/HolyJSP/delay-bridge/issues
echo.
pause
exit /b 1
