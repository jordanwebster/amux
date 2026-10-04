#!/bin/sh
# Installs amux:  curl -fsSL https://amux.sh/install | sh
#
# The stable channel's manifest is the one document that says which build
# each machine should run. This script reads it, takes this machine's
# entry, downloads the binary the entry names, and installs it only if its
# size and sha256 are the entry's. A machine installs whatever the channel
# names, whatever share of running machines a rollout has reached: the
# rollout protects machines that already run something.
#
# The manifest is signed, and an installed amux checks that signature on
# every later update with the key it was built with. This script does not:
# it and the manifest come from the same place, so a check here would
# trust the server it was checking. What it does establish is that the
# download is the file the manifest names.
#
#   AMUX_RELEASES_URL     where the manifests are (default https://amux.sh/releases)
#   AMUX_NO_MODIFY_PATH   set to leave the shell profile alone
set -eu

RELEASES_URL="${AMUX_RELEASES_URL:-https://amux.sh/releases}"
INSTALL_DIR="$HOME/.amux/bin"
BINARY_NAME="amux"
PROFILE=""

main() {
    detect_target
    read_manifest
    download_and_verify
    install_binary
    configure_path
    print_success
}

fail() {
    printf "Error: %s\n" "$1" >&2
    exit 1
}

# The target as the manifest names it: the build's Rust target triple.
detect_target() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Darwin) VENDOR_OS="apple-darwin" ;;
        Linux) VENDOR_OS="unknown-linux-gnu" ;;
        *) fail "unsupported operating system: $OS" ;;
    esac
    case "$ARCH" in
        arm64 | aarch64) CPU="aarch64" ;;
        x86_64 | amd64) CPU="x86_64" ;;
        *) fail "unsupported architecture: $ARCH" ;;
    esac
    TARGET="$CPU-$VENDOR_OS"

    printf "Detected platform: %s %s\n" "$OS" "$ARCH"
}

fetch() {
    if command -v curl > /dev/null 2>&1; then
        curl -fsSL "$1"
    elif command -v wget > /dev/null 2>&1; then
        wget -qO- "$1"
    else
        fail "curl or wget is required to download amux."
    fi
}

download() {
    if command -v curl > /dev/null 2>&1; then
        curl -fsSL -o "$2" "$1"
    else
        wget -qO "$2" "$1"
    fi
}

# Reads this target's entry out of the manifest without a JSON tool: one
# key per line, then the four fields under the target's own key. The
# fields are read as text and never evaluated.
read_manifest() {
    printf "Reading the stable channel...\n"
    MANIFEST="$(fetch "$RELEASES_URL/stable.json")" ||
        fail "could not read $RELEASES_URL/stable.json"

    ENTRY="$(printf '%s' "$MANIFEST" | tr '{},' '\n\n\n' | awk -v target="$TARGET" '
        match($0, /"[^"]*"[[:space:]]*:/) {
            name = substr($0, RSTART + 1)
            sub(/".*/, "", name)
            rest = substr($0, RSTART + RLENGTH)
            gsub(/^[[:space:]]*"?/, "", rest)
            gsub(/"?[[:space:]]*$/, "", rest)
            if (rest == "") {
                inside = (name == target)
                next
            }
            if (inside && (name == "version" || name == "url" || name == "sha256" || name == "size")) {
                print name "=" rest
            }
        }
    ')"

    VERSION="" URL="" SHA256="" SIZE=""
    while IFS='=' read -r name value; do
        case "$name" in
            version) VERSION="$value" ;;
            url) URL="$value" ;;
            sha256) SHA256="$value" ;;
            size) SIZE="$value" ;;
        esac
    done << ENTRY_END
$ENTRY
ENTRY_END

    if [ -z "$VERSION" ] || [ -z "$URL" ] || [ -z "$SHA256" ] || [ -z "$SIZE" ]; then
        fail "the stable channel has no build for $TARGET."
    fi

    printf "Stable is amux %s\n" "$VERSION"
}

download_and_verify() {
    TMP_DIR="$(mktemp -d)"
    trap 'rm -rf "$TMP_DIR"' EXIT
    DOWNLOADED="$TMP_DIR/$BINARY_NAME"

    printf "Downloading %s...\n" "$URL"
    download "$URL" "$DOWNLOADED" || fail "could not download $URL"

    ACTUAL_SIZE="$(wc -c < "$DOWNLOADED" | tr -d '[:space:]')"
    if [ "$ACTUAL_SIZE" != "$SIZE" ]; then
        fail "the download is $ACTUAL_SIZE bytes; the manifest says $SIZE."
    fi

    if command -v sha256sum > /dev/null 2>&1; then
        ACTUAL_SHA256="$(sha256sum "$DOWNLOADED" | awk '{print $1}')"
    elif command -v shasum > /dev/null 2>&1; then
        ACTUAL_SHA256="$(shasum -a 256 "$DOWNLOADED" | awk '{print $1}')"
    else
        fail "sha256sum or shasum is required to verify the download."
    fi
    if [ "$ACTUAL_SHA256" != "$SHA256" ]; then
        printf "Error: checksum verification failed.\n" >&2
        printf "  Expected: %s\n" "$SHA256" >&2
        printf "  Actual:   %s\n" "$ACTUAL_SHA256" >&2
        exit 1
    fi

    printf "Checksum verified.\n"
}

install_binary() {
    printf "Installing to %s/%s...\n" "$INSTALL_DIR" "$BINARY_NAME"

    mkdir -p "$INSTALL_DIR"
    chmod +x "$DOWNLOADED"
    mv "$DOWNLOADED" "$INSTALL_DIR/$BINARY_NAME"
}

on_path() {
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) return 0 ;;
    esac
    return 1
}

configure_path() {
    if on_path || [ -n "${AMUX_NO_MODIFY_PATH:-}" ]; then
        return
    fi

    case "$(basename "${SHELL:-sh}")" in
        zsh)
            PROFILE="$HOME/.zshrc"
            ;;
        bash)
            if [ -f "$HOME/.bashrc" ]; then
                PROFILE="$HOME/.bashrc"
            else
                PROFILE="$HOME/.profile"
            fi
            ;;
        *)
            PROFILE="$HOME/.profile"
            ;;
    esac

    if [ -f "$PROFILE" ] && grep -qF '.amux/bin' "$PROFILE" 2> /dev/null; then
        return
    fi

    printf "\n# Added by amux installer\nexport PATH=\"\$HOME/.amux/bin:\$PATH\"\n" >> "$PROFILE"
    printf "Added %s to PATH in %s\n" "$INSTALL_DIR" "$PROFILE"
}

print_success() {
    printf "\namux %s has been installed successfully!\n" "$VERSION"
    printf "\nTo get started, run:\n"

    if ! on_path; then
        if [ -n "$PROFILE" ]; then
            printf "  source %s   # or restart your shell\n" "$PROFILE"
        else
            printf "  export PATH=\"%s:\$PATH\"\n" "$INSTALL_DIR"
        fi
    fi

    printf "  amux --help\n"
}

main
