# ============================================================
#  ilocker install.ps1 — Installateur Windows (PowerShell 5.1+)
#
#  ── Mode online (repo public) ────────────────────────────────
#    irm https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.ps1 | iex
#
#  ── Mode online (repo privé avec token) ─────────────────────
#    $env:GITHUB_TOKEN = "ghp_xxxxx"
#    irm https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.ps1 | iex
#
#    # Ou en passant le token en paramètre :
#    .\install.ps1 -GithubToken "ghp_xxxxx"
#
#  ── Mode offline (reçu par Xender, USB, Bluetooth, email) ───
#    .\iloc-windows-x86_64.exe selfinstall
#    # Ou via ce script avec un binaire local :
#    .\install.ps1 -LocalBinary .\iloc-windows-x86_64.exe
#
#  Variables d'environnement :
#    $env:ILOCKER_VERSION     Version spécifique (défaut: latest)
#    $env:GITHUB_REPO         Repo GitHub (défaut: alphabechirdiallo-netizen/ilocker)
#    $env:GITHUB_TOKEN        Token GitHub (requis pour repo privé)
# ============================================================

#Requires -Version 5.1
[CmdletBinding()]
param(
    [string]$Version      = $env:ILOCKER_VERSION,
    [string]$InstallDir   = $env:ILOCKER_INSTALL_DIR,
    [string]$LocalBinary  = "",
    [string]$GithubToken  = $env:GITHUB_TOKEN,
    [switch]$DryRun,
    [switch]$NoPath,
    [switch]$NoCompletion,
    [switch]$SkipChecksum
)

Set-StrictMode -Version Latest
$ErrorActionPreference = "Stop"
$ProgressPreference    = "SilentlyContinue"

# ── Configuration ─────────────────────────────────────────
$GithubRepo = if ($env:GITHUB_REPO) { $env:GITHUB_REPO } else { "alphabechirdiallo-netizen/ilocker" }
$BaseUrl    = "https://github.com/$GithubRepo/releases"
$ApiBase    = "https://api.github.com/repos/$GithubRepo"
$DefaultDir = "$env:LOCALAPPDATA\ilocker\bin"
$BinName    = "iloc.exe"

if (-not $Version)    { $Version    = "latest" }
if (-not $InstallDir) { $InstallDir = $DefaultDir }

# ── Détection d'architecture ──────────────────────────────
$Arch = $env:PROCESSOR_ARCHITECTURE
$AssetName = switch ($Arch) {
    "AMD64"   { "iloc-windows-x86_64.exe" }
    "ARM64"   { "iloc-windows-x86_64.exe" }   # pas de build ARM64 Windows — x64 tourne en WoW64
    "x86"     { "iloc-windows-x86_64.exe" }
    default   { "iloc-windows-x86_64.exe" }
}

# ── Helpers ────────────────────────────────────────────────
function Write-Step  { param([string]$m) Write-Host "`n$m" -ForegroundColor Cyan }
function Write-Info  { param([string]$m) Write-Host "  -> $m" -ForegroundColor Gray }
function Write-Ok    { param([string]$m) Write-Host "  v $m" -ForegroundColor Green }
function Write-Warn  { param([string]$m) Write-Host "  ! $m" -ForegroundColor Yellow }
function Write-Err   { param([string]$m) Write-Host "  x $m" -ForegroundColor Red; exit 1 }

# ── Builder les headers HTTP (avec token si dispo) ────────
function Get-HttpHeaders {
    param([switch]$ApiJson)
    $headers = @{}
    if ($GithubToken) {
        $headers["Authorization"] = "token $GithubToken"
    }
    if ($ApiJson) {
        $headers["Accept"] = "application/vnd.github.v3+json"
    } else {
        $headers["Accept"] = "application/octet-stream"
    }
    return $headers
}

# ── Téléchargement (direct + fallback API pour repo privé) ─
function Get-IlocBinary {
    $tmpFile = [System.IO.Path]::GetTempFileName() + ".exe"

    if ($Version -eq "latest") {
        $directUrl = "$BaseUrl/latest/download/$AssetName"
    } else {
        $directUrl = "$BaseUrl/download/$Version/$AssetName"
    }

    Write-Info "Téléchargement : $AssetName"

    # Tentative directe (repo public, ou privé si les cookies/redirect fonctionnent)
    try {
        $headers = Get-HttpHeaders
        Invoke-WebRequest -Uri $directUrl -OutFile $tmpFile -Headers $headers -UseBasicParsing
        return $tmpFile
    } catch {
        # Silence — on essaie le fallback API
    }

    # Fallback : API GitHub (asset privé)
    if ($GithubToken) {
        Write-Warn "Téléchargement direct échoué — tentative via API GitHub..."
        try {
            $apiHeaders = Get-HttpHeaders -ApiJson
            if ($Version -eq "latest") {
                $releaseUrl = "$ApiBase/releases/latest"
            } else {
                $releaseUrl = "$ApiBase/releases/tags/$Version"
            }

            $release = Invoke-RestMethod -Uri $releaseUrl -Headers $apiHeaders -UseBasicParsing
            $asset   = $release.assets | Where-Object { $_.name -eq $AssetName } | Select-Object -First 1

            if (-not $asset) {
                Write-Err "Asset '$AssetName' introuvable dans la release '$Version'. Vérifiez la version et le token."
            }

            $dlHeaders = Get-HttpHeaders   # Accept: application/octet-stream
            Invoke-WebRequest -Uri $asset.url -OutFile $tmpFile -Headers $dlHeaders -UseBasicParsing
            return $tmpFile
        } catch {
            Write-Err "Téléchargement via API GitHub échoué : $_"
        }
    }

    Write-Err "Téléchargement échoué.`nPour un repo privé, fournissez -GithubToken ou exportez `$env:GITHUB_TOKEN."
}

# ── Vérification SHA-256 ───────────────────────────────────
function Test-Checksum {
    param([string]$BinPath)
    if ($SkipChecksum) { return }

    if ($Version -eq "latest") {
        $sumsUrl = "$BaseUrl/latest/download/SHA256SUMS"
    } else {
        $sumsUrl = "$BaseUrl/download/$Version/SHA256SUMS"
    }

    try {
        $headers     = Get-HttpHeaders
        $rawContent  = (Invoke-WebRequest -Uri $sumsUrl -Headers $headers -UseBasicParsing).Content
        # GitHub sert SHA256SUMS en content-type: application/octet-stream ->
        # Invoke-WebRequest renvoie alors .Content en byte[] et non en string ;
        # -split sur un byte[] ne donne jamais les vraies lignes du fichier.
        $sumsContent = if ($rawContent -is [byte[]]) {
            [System.Text.Encoding]::UTF8.GetString($rawContent)
        } else {
            $rawContent
        }
        $lines       = $sumsContent -split "`r?`n"
        $expected    = $null

        foreach ($line in $lines) {
            if ($line -match "^\s*([a-fA-F0-9]{64})\s+$AssetName\s*$") {
                $expected = $Matches[1].ToLower()
                break
            }
        }

        if ($expected) {
            $actual = (Get-FileHash $BinPath -Algorithm SHA256).Hash.ToLower()
            if ($actual -eq $expected) {
                Write-Ok "SHA-256 vérifié : $actual"
            } else {
                Remove-Item $BinPath -Force -ErrorAction SilentlyContinue
                Write-Err "Checksum invalide !`n  Attendu : $expected`n  Obtenu  : $actual`nAbandonnez et re-téléchargez."
            }
        } else {
            Write-Warn "Hash pour $AssetName introuvable dans SHA256SUMS — vérification ignorée"
        }
    } catch {
        Write-Warn "Vérification SHA-256 ignorée (SHA256SUMS inaccessible)"
    }
}

# ── Installation ───────────────────────────────────────────
function Install-IlocBinary {
    param([string]$SourcePath)
    $dest = Join-Path $InstallDir $BinName

    if ($DryRun) {
        Write-Info "[dry-run] Copy-Item '$SourcePath' -> '$dest'"
        return
    }

    if (-not (Test-Path $InstallDir)) {
        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        Write-Info "Dossier créé : $InstallDir"
    }

    Copy-Item -Path $SourcePath -Destination $dest -Force
    Write-Ok "iloc installé dans $dest"
}

# ── Configurer le PATH utilisateur ────────────────────────
function Add-IlocToPath {
    param([string]$Dir)
    $currentPath = [Environment]::GetEnvironmentVariable("PATH", "User")
    if ($null -eq $currentPath) { $currentPath = "" }

    if ($currentPath -like "*$Dir*") {
        Write-Ok "$Dir est déjà dans votre PATH utilisateur"
        return
    }

    if ($NoPath -or $DryRun) {
        if ($DryRun) { Write-Info "[dry-run] Ajout de $Dir au PATH" }
        return
    }

    $newPath = if ($currentPath) { "$Dir;$currentPath" } else { $Dir }
    [Environment]::SetEnvironmentVariable("PATH", $newPath, "User")
    Write-Ok "PATH utilisateur mis à jour"
    Write-Warn "Redémarrez PowerShell / votre terminal pour activer iloc"

    # Notifier Windows du changement sans redémarrer
    try {
        $HWND_BROADCAST = [IntPtr]0xffff
        $WM_SETTINGCHANGE = 0x001A
        Add-Type -TypeDefinition @"
            using System;
            using System.Runtime.InteropServices;
            public class Win32Env {
                [DllImport("user32.dll", SetLastError=true)]
                public static extern IntPtr SendMessageTimeout(
                    IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam,
                    uint fuFlags, uint uTimeout, out UIntPtr lpdwResult);
            }
"@ -ErrorAction SilentlyContinue
        $result = [UIntPtr]::Zero
        [Win32Env]::SendMessageTimeout($HWND_BROADCAST, $WM_SETTINGCHANGE,
            [UIntPtr]::Zero, "Environment", 2, 5000, [ref]$result) | Out-Null
    } catch { <# silencieux #> }
}

# ── Complétions shell ──────────────────────────────────────
function Install-Completions {
    if ($NoCompletion -or $DryRun) { return }

    $ilocBin = Join-Path $InstallDir $BinName
    if (-not (Test-Path $ilocBin)) { return }

    Write-Step "Complétions shell"
    Write-Warn "La complétion PowerShell native n'est pas encore générée par iloc."
    Write-Info "Pour activer la complétion Bash sous WSL, lancez dans WSL :"
    Write-Info "  iloc completion bash >> ~/.bash_completion"
    Write-Info "  source ~/.bash_completion"
}

# ── Note Sentinel ──────────────────────────────────────────
function Show-SentinelNote {
    Write-Host ""
    Write-Host "  Note Sentinel :" -ForegroundColor Yellow
    Write-Host "  Le Sentinel fonctionne uniquement sous Bash et Zsh." -ForegroundColor Gray
    Write-Host "  Sur Windows natif, il n'est pas disponible." -ForegroundColor Gray
    Write-Host "  Si vous utilisez WSL, lancez-y : iloc sentinel enable" -ForegroundColor Gray
}

# ── Point d'entrée ─────────────────────────────────────────

Write-Host ""
Write-Host "ilocker — Installation Windows" -ForegroundColor Cyan
Write-Host ""
Write-Info "architecture : $Arch -> $AssetName"
Write-Info "destination  : $InstallDir\$BinName"
if ($GithubToken) { Write-Info "mode         : repo privé (token fourni)" }
Write-Host ""

if ($LocalBinary -ne "") {
    Write-Step "Mode offline — installation du binaire local"
    if (-not (Test-Path $LocalBinary)) {
        Write-Err "Fichier introuvable : $LocalBinary"
    }
    Install-IlocBinary -SourcePath $LocalBinary
} else {
    Write-Step "Téléchargement depuis GitHub Releases"
    $tmp = Get-IlocBinary
    Test-Checksum -BinPath $tmp
    Install-IlocBinary -SourcePath $tmp
    Remove-Item $tmp -Force -ErrorAction SilentlyContinue
}

Add-IlocToPath -Dir $InstallDir
Install-Completions
Show-SentinelNote

Write-Host ""
Write-Host "  Installation terminée !" -ForegroundColor Green
Write-Host ""
Write-Host "  Premiers pas :" -ForegroundColor Gray
Write-Host "    iloc init                  initialise un projet" -ForegroundColor Cyan
Write-Host "    iloc save ""msg""             cree un snapshot" -ForegroundColor Cyan
Write-Host "    iloc undo                  retour arriere" -ForegroundColor Cyan
Write-Host "    iloc log                   historique" -ForegroundColor Cyan
Write-Host "    iloc status                diff depuis dernier snapshot" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Cloud BYOC (votre propre cloud) :" -ForegroundColor Gray
Write-Host "    iloc config cloud add      connecte AWS/Backblaze/Azure/GCS/R2..." -ForegroundColor Cyan
Write-Host "    iloc push                  sauvegarde chiffree vers votre bucket" -ForegroundColor Cyan
Write-Host "    iloc pull                  restaure depuis votre bucket" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Partage P2P :" -ForegroundColor Gray
Write-Host "    iloc share                 partage direct en reseau" -ForegroundColor Cyan
Write-Host "    iloc share --cloud         genere un lien cloud chiffre" -ForegroundColor Cyan
Write-Host "    iloc clone <key>           clone depuis un pair ou un lien" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Providers declaratifs (tiers, sans recompiler iloc) :" -ForegroundColor Gray
Write-Host "    iloc provider init <slug>  cree un manifeste TOML commente" -ForegroundColor Cyan
Write-Host "    iloc provider install      installe depuis un fichier local" -ForegroundColor Cyan
Write-Host "    iloc connect <slug>        puis iloc <slug> <operation>" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Hyperscale (multi-cloud + Erasure Coding) :" -ForegroundColor Gray
Write-Host "    iloc hyperscale push       Reed-Solomon distribue sur vos clouds" -ForegroundColor Cyan
Write-Host "    iloc hyperscale clone      reconstitution Reed-Solomon" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Auto-gestion :" -ForegroundColor Gray
Write-Host "    iloc update                met a jour iloc" -ForegroundColor Cyan
Write-Host "    iloc --help                toutes les commandes" -ForegroundColor Cyan
Write-Host ""
Write-Host "  Centre de commandes (VS Code) :" -ForegroundColor Gray
Write-Host "    iloc studio open           parcourir + lancer les commandes visuellement" -ForegroundColor Cyan
Write-Host ""
