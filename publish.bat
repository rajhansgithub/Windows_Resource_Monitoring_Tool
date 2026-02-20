@echo off
echo Publishing Resource Monitor Tool...
dotnet publish -c Release -r win-x64 --self-contained true -p:PublishSingleFile=true -p:IncludeNativeLibrariesForSelfExtract=true -o ..\publish
if errorlevel 1 goto failed

echo.
echo Building Installer...
WHERE ISCC >nul 2>nul
IF %ERRORLEVEL% NEQ 0 (
    echo [WARNING] Inno Setup Compiler 'ISCC' not found.
    echo Please install Inno Setup 6+ and ensure ISCC is in your PATH to generate the installer.
    echo Installer generation skipped.
) ELSE (
    ISCC setup.iss
    if errorlevel 1 goto failed
    echo Installer generated successfully in ..\Releases\Installer
)

echo.
echo Publish and Installer generation completed.
pause
exit /b 0

:failed
echo.
echo BUILD FAILED.
pause
exit /b 1
