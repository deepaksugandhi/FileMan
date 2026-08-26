@echo off
rem Builds FileMan (release) and generates the Inno Setup installer.
rem Output: installer\FileMan-<version>-setup.exe
setlocal

set "ISCC=%ProgramFiles%\Inno Setup 7\ISCC.exe"
if not exist "%ISCC%" set "ISCC=%ProgramFiles(x86)%\Inno Setup 6\ISCC.exe"
if not exist "%ISCC%" (
    echo ERROR: Inno Setup compiler ^(ISCC.exe^) not found.
    exit /b 1
)

echo === cargo build --release ===
cargo build --release || goto :fail

echo === Inno Setup ===
"%ISCC%" installer.iss || goto :fail

echo.
echo Done. Installer: installer\FileMan-*-setup.exe
exit /b 0

:fail
echo BUILD FAILED.
exit /b 1
