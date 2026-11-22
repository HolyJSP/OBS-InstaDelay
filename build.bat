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

echo Compiling Delay Bridge and the OBS plugin.
echo The first build downloads dependencies and takes a few minutes...
echo.
cargo build --release --workspace
if errorlevel 1 goto failed

if exist dist rmdir /s /q dist
mkdir dist
mkdir dist\obs-plugin
copy /y target\release\delay-bridge.exe dist\ >nul
copy /y README.md dist\ >nul
copy /y LICENSE dist\ >nul
copy /y config.example.json dist\ >nul
xcopy docs dist\docs\ /e /i /q /y >nul
copy /y target\release\obs_delay_bridge.dll dist\obs-plugin\obs-delay-bridge.dll >nul
copy /y target\release\delay-bridge.exe dist\obs-plugin\ >nul
copy /y obs-plugin\install\install-obs-plugin.bat dist\obs-plugin\ >nul
copy /y obs-plugin\install\uninstall-obs-plugin.bat dist\obs-plugin\ >nul

echo.
echo  Done! Built into %cd%\dist
echo.
echo    obs-plugin\       OBS plugin - Delay Bridge starts with OBS, adds hotkeys
echo                      and Tools, Delay Bridge. Recommended.
echo    delay-bridge.exe  standalone program, if you prefer to start it yourself
echo.
choice /c YN /t 15 /d N /m "Install the OBS plugin now? Close OBS first"
if errorlevel 2 goto skipinstall
call dist\obs-plugin\install-obs-plugin.bat
exit /b 0

:skipinstall
echo.
echo  You can install it later with dist\obs-plugin\install-obs-plugin.bat
echo  Setup guide: docs\OBS-SETUP.md
echo.
pause
exit /b 0

:test
echo Running tests - the end-to-end tests need ffmpeg on your PATH and take about 2 minutes...
echo.
cargo test --release --workspace -- --test-threads=1 --nocapture
if errorlevel 1 goto failed
echo.
echo  All tests passed.
pause
exit /b 0

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

:failed
echo.
echo  Build failed - see the messages above.
echo  If you are stuck, open an issue: https://github.com/HolyJSP/delay-bridge/issues
echo.
pause
exit /b 1
