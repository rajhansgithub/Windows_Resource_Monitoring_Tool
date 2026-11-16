@echo off
cd /d "%~dp0"
echo Building Resource Monitor (release)...
cargo build --release
if errorlevel 1 goto failed
echo.
echo Exe: target\release\ResourceMonitor.exe

WHERE ISCC >nul 2>nul
IF %ERRORLEVEL% NEQ 0 (
    echo [INFO] Inno Setup 'ISCC' not on PATH - installer skipped.
) ELSE (
    ISCC setup.iss
    if errorlevel 1 goto failed
    echo Installer: dist\Setup_ResourceMonitor.exe
)
pause
exit /b 0

:failed
echo BUILD FAILED.
pause
exit /b 1
