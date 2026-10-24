@echo off
chcp 65001 >nul
cd /d "%~dp0"
set PY=python
set PYW=pythonw
where py >nul 2>nul && (set PY=py -3& set PYW=pyw -3)
%PY% -c "import tkinter" 2>nul || (echo Не найден Python с tkinter. Установите Python с python.org ^(галочка "Add to PATH"^). & pause & exit /b 1)
%PY% -c "import psutil" 2>nul || %PY% -m pip install --user -r requirements.txt
start "" %PYW% main.py
