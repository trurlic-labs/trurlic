#!/usr/bin/env bash
# Trurlic installer — downloads and installs the trurlic binary.
#
# Usage (recommended — verify before running):
#
#   curl -fsSL https://raw.githubusercontent.com/trurlic-labs/trurlic/master/install.sh -o install.sh
#   sha256sum install.sh            # compare against install.sh.sha256 from the GitHub Release
#   bash install.sh
#
# Usage (convenience one-liner — not recommended for production):
#
#   curl -fsSL https://raw.githubusercontent.com/trurlic-labs/trurlic/master/install.sh | bash
#
# Options (via environment):
#   TRURLIC_VERSION          Pin to a specific version (default: latest)
#   TRURLIC_INSTALL          Install directory (default: /usr/local/bin or ~/.local/bin)
#   TRURLIC_TARGET           Override target triple (e.g. aarch64-unknown-linux-gnu)
#   TRURLIC_SKIP_SIGNATURE_CHECK  Set to 1 to skip minisign signature verification (NOT recommended)
#
# This script:
#   1. Detects OS and architecture (or uses TRURLIC_TARGET).
#   2. Downloads the release archive from GitHub Releases.
#   3. Verifies the SHA-256 checksum.
#   4. Verifies the minisign signature (requires minisign unless TRURLIC_SKIP_SIGNATURE_CHECK=1).
#   5. Verifies build provenance (if gh CLI is available).
#   6. Extracts the binary to the install directory.
#   7. Verifies the installed binary runs.
#
# Requirements: curl (or wget), sha256sum (or shasum), tar, uname, jq
set -euo pipefail

# ---------------------------------------------------------------------------
# Formatting
# ---------------------------------------------------------------------------

RED='\033[0;31m'
GREEN='\033[0;32m'
BOLD='\033[1m'
DIM='\033[2m'
RESET='\033[0m'

info()  { printf "${BOLD}%s${RESET}\n" "$*"; }
ok()    { printf "  ${GREEN}✓${RESET} %s\n" "$*"; }
warn()  { printf "  ${DIM}! %s${RESET}\n" "$*"; }
fail()  { printf "  ${RED}✗ %s${RESET}\n" "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# OS / arch detection
# ---------------------------------------------------------------------------

detect_target() {
    # Allow explicit override via environment.
    if [ -n "${TRURLIC_TARGET:-}" ]; then
        case "$TRURLIC_TARGET" in
            x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu \
            |x86_64-apple-darwin|aarch64-apple-darwin)
                echo "$TRURLIC_TARGET"
                return ;;
            *) fail "Unknown TRURLIC_TARGET: $TRURLIC_TARGET" ;;
        esac
    fi

    local os arch

    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Linux)  os="unknown-linux-gnu"  ;;
        Darwin) os="apple-darwin"       ;;
        MINGW*|MSYS*|CYGWIN*)
            fail "This installer does not support Windows natively.
  Download the .zip from https://github.com/trurlic-labs/trurlic/releases
  Or run this script inside WSL." ;;
        *)      fail "Unsupported operating system: $os" ;;
    esac

    case "$arch" in
        x86_64|amd64)   arch="x86_64"  ;;
        aarch64|arm64)  arch="aarch64" ;;
        *)              fail "Unsupported architecture: $arch" ;;
    esac

    echo "${arch}-${os}"
}

# ---------------------------------------------------------------------------
# Version resolution
# ---------------------------------------------------------------------------

resolve_version() {
    local version="${TRURLIC_VERSION:-latest}"

    if [ "$version" = "latest" ]; then
        command -v jq >/dev/null 2>&1 \
            || fail "jq is required to resolve the latest version. Install jq or set TRURLIC_VERSION explicitly."

        version=$(curl -fsSL "https://api.github.com/repos/trurlic-labs/trurlic/releases/latest" \
            | jq -r '.tag_name // empty' \
            | sed 's/^v//')

        if [ -z "$version" ]; then
            fail "Could not determine latest version. Set TRURLIC_VERSION explicitly."
        fi
    fi

    # Strip leading 'v' if present.
    version="${version#v}"
    echo "$version"
}

# ---------------------------------------------------------------------------
# Download helpers
# ---------------------------------------------------------------------------

download() {
    local url="$1" dest="$2"

    if command -v curl >/dev/null 2>&1; then
        curl -fsSL -o "$dest" "$url"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$dest" "$url"
    else
        fail "Neither curl nor wget found. Install one and try again."
    fi
}

# ---------------------------------------------------------------------------
# Checksum verification
# ---------------------------------------------------------------------------

verify_checksum() {
    local file="$1" expected="$2"

    local actual
    if command -v sha256sum >/dev/null 2>&1; then
        actual="$(sha256sum "$file" | awk '{print $1}')"
    elif command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$file" | awk '{print $1}')"
    else
        fail "Neither sha256sum nor shasum found. Cannot verify download integrity."
    fi

    if [ "$actual" != "$expected" ]; then
        fail "Checksum mismatch!\n  Expected: $expected\n  Got:      $actual\n  The download may be corrupted or tampered with."
    fi
}

# ---------------------------------------------------------------------------
# Build provenance verification
# ---------------------------------------------------------------------------

verify_attestation() {
    local file="$1"

    if ! command -v gh >/dev/null 2>&1; then
        warn "gh (GitHub CLI) not found — skipping build provenance verification."
        warn "Install gh to verify artifacts were built by the official CI pipeline."
        return 0
    fi

    info "  Verifying build provenance..."
    if gh attestation verify "$file" --repo "trurlic-labs/trurlic" 2>&1; then
        ok "Build provenance verified"
    else
        fail "Build provenance verification failed. The artifact may have been tampered with."
    fi
}

# ---------------------------------------------------------------------------
# Minisign signature verification
# ---------------------------------------------------------------------------

# Embedded public key — must match keys/release.minisign.pub in the repo.
TRURLIC_MINISIGN_PUBKEY="RWRIH8Kh8EblJnNzGzseyluNI3vqSrLnDhkS0rR7+PWvKLAOSpwI9J1R"

verify_signature() {
    local file="$1" sig_url="$2"

    if [ "${TRURLIC_SKIP_SIGNATURE_CHECK:-0}" = "1" ]; then
        warn "TRURLIC_SKIP_SIGNATURE_CHECK is set — skipping signature verification."
        warn "This is NOT recommended. Signatures prove the artifact was signed by the Trurlic release key."
        return 0
    fi

    if ! command -v minisign >/dev/null 2>&1; then
        echo ""
        fail "minisign is not installed. Signature verification is required.

  Install minisign:
    macOS:  brew install minisign
    Ubuntu: apt install minisign
    Arch:   pacman -S minisign
    Other:  https://jedisct1.github.io/minisign/

  Or (NOT recommended) bypass with:
    TRURLIC_SKIP_SIGNATURE_CHECK=1"
    fi

    local sig_file="${file}.minisig"
    info "  Downloading signature..."
    download "$sig_url" "$sig_file" \
        || fail "Signature download failed."

    info "  Verifying minisign signature..."
    if minisign -V -P "$TRURLIC_MINISIGN_PUBKEY" -m "$file" 2>&1; then
        ok "Minisign signature verified"
    else
        fail "Signature verification failed. The artifact may have been tampered with."
    fi
}

# ---------------------------------------------------------------------------
# Install directory
# ---------------------------------------------------------------------------

resolve_install_dir() {
    local dir="${TRURLIC_INSTALL:-}"

    if [ -n "$dir" ]; then
        echo "$dir"
        return
    fi

    # Prefer /usr/local/bin if writable (or if we can sudo).
    if [ -w "/usr/local/bin" ]; then
        echo "/usr/local/bin"
    elif [ "$(id -u)" = "0" ]; then
        echo "/usr/local/bin"
    else
        # Fallback to user-local directory.
        local user_bin="${HOME}/.local/bin"
        mkdir -p "$user_bin"
        echo "$user_bin"
    fi
}

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

main() {
    info ""
    info "  Trurlic Installer"
    info ""

    local target version install_dir
    target="$(detect_target)"
    version="$(resolve_version)"
    install_dir="$(resolve_install_dir)"

    local base_url="https://github.com/trurlic-labs/trurlic/releases/download/v${version}"
    local archive="trurlic-v${version}-${target}.tar.gz"
    local checksum_file="trurlic-v${version}-checksums.sha256"

    ok "OS/arch: ${target}"
    ok "Version: ${version}"
    ok "Install: ${install_dir}"
    echo ""

    # --- Download ---

    tmpdir="$(mktemp -d)"
    trap 'rm -rf "$tmpdir"' EXIT

    info "  Downloading ${archive}..."
    download "${base_url}/${archive}" "${tmpdir}/${archive}" \
        || fail "Download failed. Check https://github.com/trurlic-labs/trurlic/releases"

    info "  Downloading checksums..."
    download "${base_url}/${checksum_file}" "${tmpdir}/${checksum_file}" \
        || fail "Checksum file download failed."

    # --- Verify ---

    local expected_checksum
    expected_checksum="$(grep "${archive}" "${tmpdir}/${checksum_file}" | awk '{print $1}')"

    if [ -z "$expected_checksum" ]; then
        fail "Archive not found in checksum file. Release may be incomplete."
    fi

    verify_checksum "${tmpdir}/${archive}" "$expected_checksum"
    ok "Checksum verified (SHA-256)"

    verify_signature "${tmpdir}/${archive}" "${base_url}/${archive}.minisig"

    verify_attestation "${tmpdir}/${archive}"

    # --- Extract ---

    tar xzf "${tmpdir}/${archive}" -C "${tmpdir}"

    local extracted_dir="${tmpdir}/trurlic-v${version}-${target}"
    if [ ! -d "$extracted_dir" ]; then
        fail "Expected directory ${extracted_dir} not found in archive. Release may be malformed."
    fi

    install -m 0755 "${extracted_dir}/trurlic" "${install_dir}/trurlic"

    ok "Installed to ${install_dir}/trurlic"

    # --- Post-install verification ---

    if ! command -v trurlic >/dev/null 2>&1; then
        echo ""
        printf "  ${DIM}Add to your PATH:${RESET}\n"
        echo "    export PATH=\"${install_dir}:\$PATH\""
        echo ""
    fi

    if command -v trurlic >/dev/null 2>&1; then
        local installed_version
        installed_version="$(trurlic --version 2>/dev/null || echo 'unknown')"
        ok "${installed_version}"
    fi

    # --- Done ---

    echo ""
    info "  Next steps:"
    echo ""
    echo "    cd your-project"
    echo "    trurlic init"
    echo "    trurlic serve        # start the MCP server"
    echo ""
}

main "$@"
