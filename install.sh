#!/bin/sh
# Install the Oplire bridge and optional Linux utilities.
set -eu

PREFIX=/usr/local
WITH_SERVICE=0
WITH_BINARY=1
DRY=0

usage() {
    cat <<EOF
Usage: $0 [options]

  --prefix DIR      installation prefix (default: /usr/local)
  --with-service    install and enable the OpenRC/systemd watcher
  --no-service      do not install a service (default)
  --no-binary       install legacy scripts only; skip the bridge binary
  --dry-run         show actions without running them
  -h, --help        show this help
EOF
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix)
            [ "$#" -ge 2 ] || { echo "--prefix requires a path" >&2; exit 2; }
            PREFIX=$2
            shift 2
            ;;
        --with-service) WITH_SERVICE=1; shift ;;
        --no-service) WITH_SERVICE=0; shift ;;
        --no-binary) WITH_BINARY=0; shift ;;
        --dry-run|-n) DRY=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "Unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BINDIR="$PREFIX/bin"

run() {
    if [ "$DRY" -eq 1 ]; then
        printf '  [dry-run]'
        printf ' %s' "$@"
        printf '\n'
    else
        "$@"
    fi
}

say() { printf '\n== %s\n' "$*"; }

if [ "$(id -u)" -ne 0 ]; then
    echo "Run this script as root (or with doas/sudo)." >&2
    exit 1
fi

if [ "$WITH_BINARY" -eq 1 ] && [ "$DRY" -eq 0 ] && ! command -v cargo >/dev/null 2>&1; then
    echo "Cargo is required to install the bridge (or pass --no-binary)." >&2
    exit 1
fi

if [ "$WITH_BINARY" -eq 1 ]; then
    say "Building and installing the Rust bridge"
    run cargo install --path "$HERE" --root "$PREFIX" --locked --force
fi

say "Installing scripts into $BINDIR"
run install -d -m 755 "$BINDIR"
for file in reset-opencode reset-opencode-default proxy-rotate proxy-hunt; do
    run install -m 755 "$HERE/bin/$file" "$BINDIR/$file"
done

if [ "$WITH_SERVICE" -eq 1 ]; then
    say "Installing the watcher (no rotation on HTTP 429)"
    if [ -f /sbin/openrc-run ]; then
        run install -m 755 "$HERE/init.d/proxy-rotate" /etc/init.d/proxy-rotate
        run rc-update add proxy-rotate default
        run rc-service proxy-rotate start
    elif command -v systemctl >/dev/null 2>&1; then
        run install -m 644 "$HERE/systemd/proxy-rotate.service" \
            /etc/systemd/system/proxy-rotate.service
        run systemctl daemon-reload
        run systemctl enable --now proxy-rotate
    else
        echo "No supported init system found; service not installed."
    fi
fi

say "Done"
cat <<EOF
  Initialize the legacy pool: proxy-rotate init (optional)
  proxy-hunt is disabled; HTTP 429 never triggers a WARP/proxy reset.
EOF
if [ "$WITH_BINARY" -eq 1 ]; then
    cat <<EOF
  Oplire listens on 127.0.0.1:8080 by default.
  Start the bridge: $BINDIR/oplire proxy
  Check an endpoint: $BINDIR/oplire check --url http://127.0.0.1:3000/health
EOF
else
    echo "  Rust bridge binary was not installed (--no-binary)."
fi
