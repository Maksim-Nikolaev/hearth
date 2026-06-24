@echo off
REM Drag one or more .mkv files onto this .bat to measure mic->desktop delay.
REM Also works from a terminal:  "Measure audio delay.bat" clip1.mkv clip2.mkv
setlocal
if "%~1"=="" (
  echo Drag one or more .mkv recordings onto this file, or pass them as arguments.
  echo.
  pause
  exit /b
)
python "%~dp0audio_delay.py" %*
