#!/usr/bin/env bash
# Project Loom - Friction-Free POSIX Installer
# Usage: curl -fsSL https://raw.githubusercontent.com/gsuquet/loom/main/install.sh | bash

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

echo -e "${BLUE}==>${NC} Installing Loom..."

OS="$(uname -s | tr '[:upper:]' '[:lower:]')"
ARCH="$(uname -m)"

case "$ARCH" in
    x86_64|amd64)
        ARCH="x86_64"
        ;;
    arm64|aarch64)
        ARCH="aarch64"
        ;;
    *)
        echo -e "${RED}Error: Unsupported architecture $ARCH${NC}"
        exit 1
        ;;
esac

case "$OS" in
    darwin|linux)
        ;;
    *)
        echo -e "${RED}Error: Unsupported operating system $OS${NC}"
        exit 1
        ;;
esac

INSTALL_DIR="${HOME}/.loom/bin"
mkdir -p "${INSTALL_DIR}"

echo -e "${BLUE}==>${NC} Detected platform: ${OS}-${ARCH}"
echo -e "${BLUE}==>${NC} Target directory: ${INSTALL_DIR}"

# Export PATH to shell profile if not already present
PROFILE_ENTRY='export PATH="$HOME/.loom/bin:$PATH"'
SHELL_NAME="$(basename "${SHELL:-bash}")"
RC_FILE=""

case "$SHELL_NAME" in
    zsh)
        RC_FILE="${HOME}/.zshrc"
        ;;
    bash)
        if [[ -f "${HOME}/.bash_profile" ]]; then
            RC_FILE="${HOME}/.bash_profile"
        else
            RC_FILE="${HOME}/.bashrc"
        fi
        ;;
    fish)
        RC_FILE="${HOME}/.config/fish/config.fish"
        PROFILE_ENTRY='set -gx PATH $HOME/.loom/bin $PATH'
        ;;
    *)
        RC_FILE="${HOME}/.profile"
        ;;
esac

if [[ -n "$RC_FILE" ]]; then
    if ! grep -q ".loom/bin" "$RC_FILE" 2>/dev/null; then
        echo -e "\n# >>> loom path >>>\n${PROFILE_ENTRY}\n# <<< loom path <<<" >> "$RC_FILE"
        echo -e "${GREEN}==>${NC} Added ~/.loom/bin to ${RC_FILE}"
    fi
fi

echo -e "${GREEN}==>${NC} Loom installation complete! Run 'loom --version' to verify."
