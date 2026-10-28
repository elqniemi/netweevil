@echo off
rem Double-click to start NetWeevil (builds once, then opens the console).
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0start.ps1"
if errorlevel 1 pause
