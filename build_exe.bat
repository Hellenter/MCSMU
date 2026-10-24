@echo off
chcp 65001 >nul
cd /d "%~dp0"
where py >nul 2>nul && (set PY=py -3) || (set PY=python)
%PY% -m pip install --upgrade pyinstaller psutil
%PY% -m PyInstaller --noconfirm --onefile --windowed --name MCServerManager main.py
echo.
echo Готово: dist\MCServerManager.exe
pause
