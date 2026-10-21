#!/usr/bin/env bash
# Install OCA on Linux — the executable form of INSTALL.md.
# Run from anywhere; it finds the repository relative to itself:
#   ./scripts/install.sh
# Safe to re-run: every step is skipped when already satisfied.

set -euo pipefail

cd "$(dirname "$0")/.."
if [[ ! -f crates/oca-cli/Cargo.toml ]]; then
    echo "error: run this script from within the OCA repository" >&2
    exit 1
fi

step() { printf '\n== %s ==\n' "$1"; }

# --- 1. Rust toolchain -------------------------------------------------------
REQUIRED_RUST=1.89

rust_version() {
    rustc --version 2>/dev/null | awk '{print $2}'
}

version_at_least() {
    # "1.89.1" >= "1.89" ?
    [[ "$(printf '%s\n' "$2" "$1" | sort -V | head -1)" == "$2" ]]
}

step "Rust toolchain (need $REQUIRED_RUST+)"
if ! command -v rustc >/dev/null 2>&1; then
    echo "rustc not found."
    if command -v rustup >/dev/null 2>&1; then
        echo "rustup exists but no toolchain — installing stable:"
        rustup update stable
        rustup default stable
    else
        echo "Installing rustup (from https://sh.rustup.rs) — press Enter to continue, Ctrl-C to abort."
        read -r
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
        # rustup just installed ~/.cargo/bin — make it visible to this run.
        export PATH="$HOME/.cargo/bin:$PATH"
    fi
elif ! version_at_least "$(rust_version)" "$REQUIRED_RUST"; then
    echo "rustc $(rust_version) is older than $REQUIRED_RUST — updating:"
    if command -v rustup >/dev/null 2>&1; then
        rustup update stable
        rustup default stable
    else
        echo "error: rustc is too old and rustup is not installed." >&2
        echo "       install rustup from https://rustup.rs, then re-run this script." >&2
        exit 1
    fi
else
    echo "ok: rustc $(rust_version)"
fi

command -v cargo >/dev/null 2>&1 || { echo "error: cargo not found after toolchain setup" >&2; exit 1; }

# --- 2. ALSA build dependencies (Linux only) ---------------------------------
step "ALSA build dependencies"
NEED_PKGS=()
pkg-config --exists alsa 2>/dev/null || NEED_PKGS+=(alsa)
command -v pkg-config >/dev/null 2>&1 || NEED_PKGS+=(pkg-config)

if [[ ${#NEED_PKGS[@]} -eq 0 ]]; then
    echo "ok: pkg-config + alsa found"
else
    echo "missing: ${NEED_PKGS[*]} — required to compile the audio crate (recording itself stays optional)"
    if command -v apt-get >/dev/null 2>&1; then
        sudo apt-get update && sudo apt-get install -y pkg-config libasound2-dev
    elif command -v dnf >/dev/null 2>&1; then
        sudo dnf install -y pkgconf alsa-lib-devel
    elif command -v pacman >/dev/null 2>&1; then
        sudo pacman -S --needed --noconfirm pkgconf alsa-lib
    elif command -v zypper >/dev/null 2>&1; then
        sudo zypper install -y pkgconf-pkg-config alsa-devel
    else
        echo "error: unknown package manager — install 'pkg-config' and the ALSA" >&2
        echo "       development headers manually (see INSTALL.md step 1), then re-run." >&2
        exit 1
    fi
fi

# --- 3. Build and install ----------------------------------------------------
step "cargo install (release build — a few minutes the first time)"
if command -v oca >/dev/null 2>&1; then
    echo "oca is already installed ($(oca --version 2>/dev/null || echo unknown)) — rebuilding over it"
    cargo install --force --path crates/oca-cli
else
    cargo install --path crates/oca-cli
fi

# --- 4. PATH check -----------------------------------------------------------
step "PATH"
if [[ ":$PATH:" == *":$HOME/.cargo/bin:"* ]] && command -v oca >/dev/null 2>&1; then
    echo "ok: $(command -v oca)"
else
    # A fresh rustup install may have it on disk but not in this shell's PATH.
    if [[ -x "$HOME/.cargo/bin/oca" ]]; then
        echo "installed at $HOME/.cargo/bin/oca, but that directory is not on your PATH."
        echo "Add this to your shell profile (~/.bashrc or ~/.zshrc), then open a new terminal:"
        echo '  export PATH="$HOME/.cargo/bin:$PATH"'
    else
        echo "error: oca not found after install — unexpected; please report it" >&2
        exit 1
    fi
fi

# --- 5. Done -----------------------------------------------------------------
step "Ready"
"$HOME/.cargo/bin/oca" --version
echo
echo "Next steps:"
echo "  oca new \"My Novel\"   # create a project"
echo "  cd my-novel && oca doctor && oca open"
echo "See README.md for the tour and INSTALL.md for optional AI setup."
