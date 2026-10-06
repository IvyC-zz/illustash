@echo off
setlocal
rem Installed setup is prepended by build.py. Diagnostics run only on explicit request.
if "%~1"=="--check" (
  "%ILLUSTASH_RESEARCH_WORKER%" --check
  if errorlevel 1 exit /b 1
  "%ILLUSTASH_INDEX_WORKER%" --check
  if errorlevel 1 exit /b 1
  "%ILLUSTASH_PROVENANCE_WORKER%" --check
  if errorlevel 1 exit /b 1
  "%ILLUSTASH_RECOGNITION_WORKER%" --check
  if errorlevel 1 exit /b 1
  exit /b 0
)
if exist "%~dp0bin\illustash-desktop.exe" (
  call "%~dp0desktop.cmd" %*
) else (
  call "%~dp0start.cmd" %*
)
exit /b %errorlevel%
