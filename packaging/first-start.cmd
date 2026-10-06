@echo off
setlocal
set "SOURCE=%~1"
set "LIBRARY=%~2"
if not defined SOURCE set /p "SOURCE=Image folder (absolute path): "
if not defined LIBRARY set /p "LIBRARY=New library directory (for example D:\Illustash\test.pilib): "
if not defined SOURCE exit /b 1
if not defined LIBRARY exit /b 1
"%~dp0bin\illustash-terms.exe" library-create --folder "%SOURCE%" --dest "%LIBRARY%" --name "Test library"
if errorlevel 1 (pause & exit /b 1)
call "%~dp0research.cmd" --library "%LIBRARY%"
if errorlevel 1 pause
