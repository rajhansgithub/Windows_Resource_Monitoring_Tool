@echo off
cd /d "%~dp0"
echo ==========================================
echo      QUICK SAVE: UPDATING GITHUB
echo ==========================================
echo.

:: 1. Stage all changes
echo [1/4] Staging changes...
git add .

:: 2. Commit with timestamp
echo [2/4] Committing changes...
:: Get formatted date and time for the commit message
set "datetime=%date% %time%"
git commit -m "Quick Save: %datetime%"

:: 3. Pull from GitHub
echo [3/4] Pulling remote changes to prevent push errors...
git pull --rebase origin main

:: 4. Push to GitHub
echo [4/4] Pushing to remote...
git push origin main

echo.
echo ==========================================
echo        SUCCESSFULLY SAVED!
echo ==========================================
echo.
echo You can close this window.
timeout /t 5
