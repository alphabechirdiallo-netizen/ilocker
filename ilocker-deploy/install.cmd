@echo off
setlocal EnableDelayedExpansion
rem ============================================================
rem  ilocker - installateur pour l'invite de commande Windows
rem  (cmd.exe), distinct de PowerShell.
rem
rem  Ce script se contente de deleguer a install.ps1 (deja teste
rem  en profondeur separement) via PowerShell, present par defaut
rem  sur toute machine Windows 7 SP1+ / Windows 10 / Windows 11 -
rem  meme si votre invite de commande habituelle est cmd.exe et
rem  non PowerShell. Aucune logique d'installation n'est dupliquee
rem  ici : une seule source de verite (install.ps1).
rem
rem  Usage (depuis cmd.exe) :
rem    curl.exe -fsSL -o install.cmd https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.cmd
rem    install.cmd
rem
rem  GITHUB_TOKEN, si deja definie dans l'environnement, est
rem  automatiquement heritee par le sous-processus PowerShell
rem  (repo prive) - rien de plus a faire ici.
rem ============================================================

set "ILOC_PS="

rem 9009 est le code que cmd.exe renvoie lui-meme quand il ne trouve pas
rem le programme demande (pas besoin de where.exe, qui n'est qu'un
rem utilitaire de recherche separe, potentiellement absent sur certaines
rem installations minimales).
rem
rem IMPORTANT : %errorlevel% a l'interieur d'un bloc if (...) est
rem substitue UNE SEULE FOIS, a l'analyse du bloc entier - donc AVANT
rem que la commande pwsh ci-dessous ne s'execute et ne change reellement
rem l'errorlevel. Sans EnableDelayedExpansion + !errorlevel!, ce test
rem verrait toujours l'ancienne valeur (celle d'avant le bloc) et ne
rem detecterait donc jamais correctement pwsh.
powershell -NoProfile -Command "exit 0" >nul 2>nul
if not "%errorlevel%"=="9009" set "ILOC_PS=powershell"

if not defined ILOC_PS (
    pwsh -NoProfile -Command "exit 0" >nul 2>nul
    if not "!errorlevel!"=="9009" set "ILOC_PS=pwsh"
)

if not defined ILOC_PS (
    echo.
    echo   ilocker necessite PowerShell ^(present par defaut sur
    echo   Windows 7 SP1 et plus^), introuvable.
    echo.
    echo   Ouvrez PowerShell manuellement, puis lancez :
    echo     irm https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.ps1 ^| iex
    echo.
    exit /b 1
)

echo.
echo   ilocker - installation (delegue a %ILOC_PS%)...
echo.

call %ILOC_PS% -NoProfile -ExecutionPolicy Bypass -Command "iex (irm 'https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.ps1')"
exit /b %errorlevel%
