#!/usr/bin/env sh
# ============================================================
#  ilocker install.sh — Installateur universel (Linux & macOS)
#  POSIX-compliant : bash, zsh, dash, sh
#
#  ── Mode online (depuis internet) ──────────────────────────
#    # Repo public :
#    curl -fsSL https://github.com/alphabechirdiallo-netizen/ilocker/releases/latest/download/install.sh | sh
#
#    # Repo privé (avec token GitHub) :
#    curl -fsSL -H "Authorization: token $GITHUB_TOKEN" \
#      https://raw.githubusercontent.com/alphabechirdiallo-netizen/ilocker/main/ilocker-deploy/install.sh | sh
#
#  ── Mode offline (fichier reçu par Xender, USB, Bluetooth) ──
#    chmod +x ./iloc-linux-x86_64
#    ./iloc-linux-x86_64 selfinstall
#
#    # Ou via ce script avec le binaire local :
#    ./install.sh --local ./iloc-linux-x86_64
#
#  Options :
#    --version <ver>   Version spécifique (défaut: latest)
#    --dir     <path>  Répertoire d'installation
#    --local   <bin>   Installe un binaire local (mode offline)
#    --token   <tok>   GitHub Personal Access Token (repo privé)
#    --no-path         Ne pas modifier le PATH
#    --no-sentinel     Ne pas activer le Sentinel
#    --no-completion   Ne pas installer les complétions shell
#    --dry-run         Affiche sans exécuter
# ============================================================

set -eu

# ── Variables configurables ────────────────────────────────
ILOCKER_VERSION="${ILOCKER_VERSION:-latest}"
GITHUB_REPO="${GITHUB_REPO:-alphabechirdiallo-netizen/ilocker}"
GITHUB_TOKEN="${GITHUB_TOKEN:-}"
ILOCKER_BASE_URL="${ILOCKER_BASE_URL:-https://github.com/${GITHUB_REPO}/releases}"
ILOCKER_INSTALL_DIR="${ILOCKER_INSTALL_DIR:-}"
LOCAL_BINARY=""
NO_PATH_UPDATE=0
NO_SENTINEL=0
NO_COMPLETION=0
DRY_RUN=0

# ── Couleurs ───────────────────────────────────────────────
if [ -t 1 ]; then
    RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'
    CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'
else
    RED=''; GREEN=''; YELLOW=''; CYAN=''; BOLD=''; RESET=''
fi

info()    { printf "  ${CYAN}→${RESET} %s\n" "$*"; }
success() { printf "  ${GREEN}✓${RESET} %s\n" "$*"; }
warn()    { printf "  ${YELLOW}⚠${RESET} %s\n" "$*"; }
error()   { printf "  ${RED}✗ Erreur:${RESET} %s\n" "$*" >&2; exit 1; }
step()    { printf "\n${BOLD}%s${RESET}\n" "$*"; }

# ── Parse arguments ────────────────────────────────────────
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)       shift; ILOCKER_VERSION="$1" ;;
        --dir)           shift; ILOCKER_INSTALL_DIR="$1" ;;
        --local)         shift; LOCAL_BINARY="$1" ;;
        --token)         shift; GITHUB_TOKEN="$1" ;;
        --no-path)       NO_PATH_UPDATE=1 ;;
        --no-sentinel)   NO_SENTINEL=1 ;;
        --no-completion) NO_COMPLETION=1 ;;
        --dry-run)       DRY_RUN=1 ;;
        *) warn "Option inconnue: $1" ;;
    esac
    shift
done

# ── Détection plateforme ───────────────────────────────────
detect_platform() {
    OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
    ARCH="$(uname -m)"
    case "$ARCH" in
        x86_64)          ARCH="x86_64" ;;
        aarch64|arm64)   ARCH="aarch64" ;;
        *)               error "Architecture non supportée: $ARCH" ;;
    esac
    case "$OS" in
        linux)  PLATFORM="linux" ;;
        darwin) PLATFORM="macos" ;;
        *)      error "OS non supporté: $OS" ;;
    esac
    ASSET_NAME="iloc-${PLATFORM}-${ARCH}"
}

# ── Choisir le répertoire d'installation ──────────────────
choose_install_dir() {
    if [ -n "$ILOCKER_INSTALL_DIR" ]; then
        INSTALL_DIR="$ILOCKER_INSTALL_DIR"
        return
    fi
    if [ -w "/usr/local/bin" ]; then
        INSTALL_DIR="/usr/local/bin"
    else
        INSTALL_DIR="${HOME}/.local/bin"
    fi
}

# ── Helper : curl ou wget avec support token privé ────────
_http_get() {
    URL="$1"
    OUTPUT="$2"
    if command -v curl >/dev/null 2>&1; then
        if [ -n "$GITHUB_TOKEN" ]; then
            curl -fsSL --progress-bar \
                -H "Authorization: token ${GITHUB_TOKEN}" \
                -H "Accept: application/octet-stream" \
                "$URL" -o "$OUTPUT" \
                || return 1
        else
            curl -fsSL --progress-bar "$URL" -o "$OUTPUT" \
                || return 1
        fi
    elif command -v wget >/dev/null 2>&1; then
        if [ -n "$GITHUB_TOKEN" ]; then
            wget -q --show-progress \
                --header="Authorization: token ${GITHUB_TOKEN}" \
                --header="Accept: application/octet-stream" \
                "$URL" -O "$OUTPUT" \
                || return 1
        else
            wget -q --show-progress "$URL" -O "$OUTPUT" \
                || return 1
        fi
    else
        error "curl ou wget requis pour le téléchargement."
    fi
    return 0
}

# ── Télécharger le binaire (avec fallback API pour repo privé) ──
download_binary() {
    if [ "$ILOCKER_VERSION" = "latest" ]; then
        DIRECT_URL="${ILOCKER_BASE_URL}/latest/download/${ASSET_NAME}"
    else
        DIRECT_URL="${ILOCKER_BASE_URL}/download/${ILOCKER_VERSION}/${ASSET_NAME}"
    fi

    info "Téléchargement : $ASSET_NAME"
    TMP_FILE="$(mktemp /tmp/iloc-XXXXXX)"

    # Tentative directe (marche pour repo public, ou privé via browser_download_url)
    if _http_get "$DIRECT_URL" "$TMP_FILE" 2>/dev/null; then
        echo "$TMP_FILE"
        return
    fi

    # Fallback : API GitHub (nécessaire pour assets de repos privés)
    if [ -n "$GITHUB_TOKEN" ]; then
        warn "Téléchargement direct échoué — tentative via API GitHub..."
        if command -v curl >/dev/null 2>&1; then
            if [ "$ILOCKER_VERSION" = "latest" ]; then
                RELEASE_API="https://api.github.com/repos/${GITHUB_REPO}/releases/latest"
            else
                RELEASE_API="https://api.github.com/repos/${GITHUB_REPO}/releases/tags/${ILOCKER_VERSION}"
            fi
            # Récupérer l'ID de l'asset via l'API
            ASSET_ID=$(curl -fsSL \
                -H "Authorization: token ${GITHUB_TOKEN}" \
                -H "Accept: application/vnd.github.v3+json" \
                "$RELEASE_API" 2>/dev/null \
                | grep -A2 "\"name\": \"${ASSET_NAME}\"" \
                | grep '"id"' \
                | head -1 \
                | grep -o '[0-9]*')

            if [ -n "$ASSET_ID" ]; then
                ASSET_API_URL="https://api.github.com/repos/${GITHUB_REPO}/releases/assets/${ASSET_ID}"
                curl -fsSL \
                    -H "Authorization: token ${GITHUB_TOKEN}" \
                    -H "Accept: application/octet-stream" \
                    "$ASSET_API_URL" -o "$TMP_FILE" \
                    || error "Téléchargement via API GitHub échoué."
                echo "$TMP_FILE"
                return
            else
                error "Asset '${ASSET_NAME}' introuvable dans la release. Vérifiez la version et le token."
            fi
        fi
    fi

    error "Téléchargement échoué. Pour un repo privé, fournissez --token ou exportez GITHUB_TOKEN."
}

# ── Vérification d'intégrité SHA-256 ────────────────────
verify_checksum() {
    BIN_FILE="$1"

    if [ "$ILOCKER_VERSION" = "latest" ]; then
        SUMS_URL="${ILOCKER_BASE_URL}/latest/download/SHA256SUMS"
    else
        SUMS_URL="${ILOCKER_BASE_URL}/download/${ILOCKER_VERSION}/SHA256SUMS"
    fi

    SUMS_TMP="$(mktemp /tmp/iloc-sums-XXXXXX)"
    DOWNLOADED=0

    if _http_get "$SUMS_URL" "$SUMS_TMP" 2>/dev/null; then
        DOWNLOADED=1
    fi

    if [ "$DOWNLOADED" = "0" ]; then
        warn "Vérification SHA-256 ignorée (SHA256SUMS inaccessible)"
        rm -f "$SUMS_TMP"
        return
    fi

    if command -v sha256sum >/dev/null 2>&1; then
        EXPECTED=$(grep " ${ASSET_NAME}$" "$SUMS_TMP" 2>/dev/null | awk '{print $1}')
        if [ -n "$EXPECTED" ]; then
            ACTUAL=$(sha256sum "$BIN_FILE" | awk '{print $1}')
            if [ "$ACTUAL" = "$EXPECTED" ]; then
                success "SHA-256 vérifié : $ACTUAL"
            else
                rm -f "$SUMS_TMP"
                error "Checksum invalide !\n  Attendu : $EXPECTED\n  Obtenu  : $ACTUAL\nAbandonnez et re-téléchargez."
            fi
        else
            warn "Hash pour ${ASSET_NAME} introuvable dans SHA256SUMS"
        fi
    elif command -v shasum >/dev/null 2>&1; then
        EXPECTED=$(grep " ${ASSET_NAME}$" "$SUMS_TMP" 2>/dev/null | awk '{print $1}')
        if [ -n "$EXPECTED" ]; then
            ACTUAL=$(shasum -a 256 "$BIN_FILE" | awk '{print $1}')
            if [ "$ACTUAL" = "$EXPECTED" ]; then
                success "SHA-256 vérifié : $ACTUAL"
            else
                rm -f "$SUMS_TMP"
                error "Checksum invalide !\n  Attendu : $EXPECTED\n  Obtenu  : $ACTUAL"
            fi
        fi
    else
        warn "sha256sum / shasum non disponible — vérification ignorée"
    fi

    rm -f "$SUMS_TMP"
}

# ── Installer le binaire ──────────────────────────────────
install_binary() {
    SRC="$1"
    DEST="${INSTALL_DIR}/iloc"

    if [ "$DRY_RUN" = "1" ]; then
        info "[dry-run] cp $SRC $DEST"
        info "[dry-run] chmod +x $DEST"
        return
    fi

    mkdir -p "$INSTALL_DIR"
    cp "$SRC" "$DEST"
    chmod +x "$DEST"
    success "iloc installé dans $DEST"
}

# ── Configurer le PATH ─────────────────────────────────────
configure_path() {
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*)
            success "$INSTALL_DIR est déjà dans votre PATH"
            return ;;
    esac

    [ "$NO_PATH_UPDATE" = "1" ] && return
    [ "$DRY_RUN" = "1" ] && { info "[dry-run] Ajout de $INSTALL_DIR au PATH"; return; }

    SHELL_NAME="$(basename "${SHELL:-sh}")"
    case "$SHELL_NAME" in
        zsh)
            RC_FILE="${ZDOTDIR:-$HOME}/.zshrc"
            PROFILE_FILE="${ZDOTDIR:-$HOME}/.zprofile"
            ;;
        fish)
            RC_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/fish/config.fish"
            PROFILE_FILE="$RC_FILE"
            ;;
        *)
            RC_FILE="$HOME/.bashrc"
            PROFILE_FILE="$HOME/.bash_profile"
            ;;
    esac

    TARGET_RC="$RC_FILE"
    [ ! -f "$RC_FILE" ] && [ -f "$PROFILE_FILE" ] && TARGET_RC="$PROFILE_FILE"
    [ ! -f "$TARGET_RC" ] && TARGET_RC="$HOME/.profile"

    if [ -f "$TARGET_RC" ] && grep -q "$INSTALL_DIR" "$TARGET_RC" 2>/dev/null; then
        success "PATH déjà configuré dans $TARGET_RC"
        return
    fi

    if [ "$SHELL_NAME" = "fish" ]; then
        printf '\n# ilocker\nfish_add_path "%s"\n' "$INSTALL_DIR" >> "$TARGET_RC"
    else
        printf '\n# ilocker\nexport PATH="%s:$PATH"\n' "$INSTALL_DIR" >> "$TARGET_RC"
    fi

    success "PATH configuré dans $TARGET_RC"
    warn "Redémarrez votre terminal ou lancez : source $TARGET_RC"
}

# ── Activer le Sentinel ───────────────────────────────────
setup_sentinel() {
    [ "$NO_SENTINEL" = "1" ] && return
    [ "$DRY_RUN" = "1" ] && { info "[dry-run] activation Sentinel"; return; }

    ILOC_BIN="${INSTALL_DIR}/iloc"
    if [ ! -x "$ILOC_BIN" ]; then
        warn "Sentinel non activé (iloc introuvable dans $INSTALL_DIR)"
        return
    fi

    step "Activation du Sentinel (auto-save avant commandes destructrices)"
    "$ILOC_BIN" sentinel enable 2>/dev/null && \
        success "Sentinel activé — ouvrez un nouveau terminal pour l'activer" || \
        warn "Sentinel non activé (lancez 'iloc sentinel enable' manuellement)"
}

# ── Installer les complétions shell ───────────────────────
setup_completions() {
    [ "$NO_COMPLETION" = "1" ] && return
    [ "$DRY_RUN" = "1" ] && { info "[dry-run] installation complétions shell"; return; }

    ILOC_BIN="${INSTALL_DIR}/iloc"
    if [ ! -x "$ILOC_BIN" ]; then
        return
    fi

    SHELL_NAME="$(basename "${SHELL:-sh}")"
    step "Installation des complétions shell"

    case "$SHELL_NAME" in
        bash)
            BASH_COMP_DIR="$HOME/.bash_completion.d"
            mkdir -p "$BASH_COMP_DIR"
            "$ILOC_BIN" completion bash > "$BASH_COMP_DIR/iloc" 2>/dev/null && \
                success "Complétion Bash installée dans $BASH_COMP_DIR/iloc" || \
                warn "Impossible d'installer la complétion Bash"
            BASHRC="$HOME/.bashrc"
            if [ -f "$BASHRC" ] && ! grep -q "bash_completion.d" "$BASHRC" 2>/dev/null; then
                printf '\n# ilocker completions\nfor f in ~/.bash_completion.d/*; do [ -f "$f" ] && source "$f"; done\n' >> "$BASHRC"
                info "Bloc source ajouté dans $BASHRC"
            fi
            ;;
        zsh)
            ZFUNC_DIR="$HOME/.zfunc"
            mkdir -p "$ZFUNC_DIR"
            "$ILOC_BIN" completion zsh > "$ZFUNC_DIR/_iloc" 2>/dev/null && \
                success "Complétion Zsh installée dans $ZFUNC_DIR/_iloc" || \
                warn "Impossible d'installer la complétion Zsh"
            ZSHRC="${ZDOTDIR:-$HOME}/.zshrc"
            if [ -f "$ZSHRC" ] && ! grep -q "zfunc" "$ZSHRC" 2>/dev/null; then
                printf '\n# ilocker completions\nfpath=(~/.zfunc $fpath); autoload -Uz compinit && compinit\n' >> "$ZSHRC"
                info "fpath ajouté dans $ZSHRC"
            fi
            ;;
        *)
            if [ -d "/etc/bash_completion.d" ] && [ -w "/etc/bash_completion.d" ]; then
                "$ILOC_BIN" completion bash > "/etc/bash_completion.d/iloc" 2>/dev/null && \
                    success "Complétion Bash installée dans /etc/bash_completion.d/iloc" || true
            else
                info "Complétion shell : lancez 'iloc completion bash --setup' ou 'iloc completion zsh --setup'"
            fi
            ;;
    esac
}

# ── Point d'entrée ─────────────────────────────────────────

printf "\n${BOLD}ilocker — Installation${RESET}\n\n"

detect_platform
choose_install_dir

info "plateforme : ${PLATFORM}-${ARCH}"
info "destination: ${INSTALL_DIR}/iloc"
[ -n "$GITHUB_TOKEN" ] && info "mode       : repo privé (token fourni)"
echo ""

if [ -n "$LOCAL_BINARY" ]; then
    step "Mode offline — installation du binaire local"
    if [ ! -f "$LOCAL_BINARY" ]; then
        error "Fichier introuvable : $LOCAL_BINARY"
    fi
    install_binary "$LOCAL_BINARY"
else
    step "Téléchargement depuis GitHub Releases"
    TMP=$(download_binary)
    verify_checksum "$TMP"
    install_binary "$TMP"
    rm -f "$TMP"
fi

configure_path
setup_completions
setup_sentinel

echo ""
printf "${GREEN}${BOLD}  Installation terminée !${RESET}\n\n"
printf "  Premiers pas :\n"
printf "    ${CYAN}iloc init${RESET}                  initialise un projet\n"
printf "    ${CYAN}iloc save \"msg\"${RESET}             crée un snapshot\n"
printf "    ${CYAN}iloc undo${RESET}                  retour arrière\n"
printf "    ${CYAN}iloc log${RESET}                   historique\n"
printf "    ${CYAN}iloc status${RESET}                diff depuis dernier snapshot\n"
printf "\n"
printf "  Cloud BYOC (votre propre cloud) :\n"
printf "    ${CYAN}iloc config cloud add${RESET}      connecte AWS/Backblaze/Azure/GCS/R2…\n"
printf "    ${CYAN}iloc push${RESET}                  sauvegarde chiffrée vers votre bucket\n"
printf "    ${CYAN}iloc pull${RESET}                  restaure depuis votre bucket\n"
printf "\n"
printf "  Partage P2P :\n"
printf "    ${CYAN}iloc share${RESET}                 partage direct en réseau\n"
printf "    ${CYAN}iloc share --cloud${RESET}         génère un lien cloud chiffré\n"
printf "    ${CYAN}iloc clone <key>${RESET}           clone depuis un pair ou un lien\n"
printf "\n"
printf "  Providers déclaratifs (tiers, sans recompiler ilocker) :\n"
printf "    ${CYAN}iloc provider init <slug>${RESET}  crée un manifeste TOML commenté\n"
printf "    ${CYAN}iloc provider install${RESET}      installe depuis un fichier local\n"
printf "    ${CYAN}iloc connect <slug>${RESET}        puis iloc <slug> <opération>\n"
printf "\n"
printf "  Hyperscale (multi-cloud + Erasure Coding) :\n"
printf "    ${CYAN}iloc hyperscale push${RESET}       Reed-Solomon distribué sur vos clouds\n"
printf "    ${CYAN}iloc hyperscale clone <url>${RESET} reconstitution Reed-Solomon\n"
printf "\n"
printf "  Auto-gestion :\n"
printf "    ${CYAN}iloc update${RESET}                met à jour iloc\n"
printf "    ${CYAN}iloc sentinel status${RESET}       vérifie le Sentinel\n"
printf "    ${CYAN}iloc --help${RESET}                toutes les commandes\n"
printf "\n"
printf "  Centre de commandes (VS Code) :\n"
printf "    ${CYAN}iloc studio open${RESET}           parcourir + lancer les commandes visuellement\n"
echo ""
