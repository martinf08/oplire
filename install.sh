#!/bin/sh
# Installation de oplire-toolkit (WARP + pool de proxies OpenCode).
set -eu

PREFIX="/usr/local"
WITH_SERVICE=1
DRY=0

usage() {
    cat <<EOF
Usage: $0 [options]

  --prefix DIR     préfixe d'installation (défaut : /usr/local)
  --no-service     ne pas activer le service au boot
  --dry-run        affiche les actions sans les exécuter
  -h, --help       cette aide
EOF
}

for arg in "$@"; do
    case "$arg" in
        --prefix) PREFIX="$2"; shift 2 ;;
        --no-service) WITH_SERVICE=0; shift ;;
        --dry-run|-n) DRY=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "option inconnue : $arg" >&2; usage; exit 2 ;;
    esac
done

HERE=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BINDIR="$PREFIX/bin"

run() {
    if [ "$DRY" -eq 1 ]; then
        printf '  [dry-run] %s\n' "$*"
    else
        "$@"
    fi
}

say() { printf '\n== %s\n' "$*"; }

[ "$(id -u)" -eq 0 ] || {
    echo "Ce script doit être lancé en root (ou avec doas/sudo)." >&2
    exit 1
}

say "Installation des scripts dans $BINDIR"
run mkdir -p "$BINDIR"
for f in reset-opencode reset-opencode-default proxy-rotate proxy-hunt; do
    run install -m 755 "$HERE/bin/$f" "$BINDIR/$f"
done

say "Initialisation de la base du pool"
if [ "$DRY" -eq 0 ]; then
    "$BINDIR/proxy-rotate" init || true
fi

say "Service"
if [ "$WITH_SERVICE" -eq 1 ]; then
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
        echo "  Aucun init système reconnu : service non installé."
    fi
else
    echo "  Ignoré (--no-service)."
fi

say "Skill OpenCode (optionnel)"
cat <<EOF
  Pour installer le skill :
    install -Dm644 "$HERE/skills/oplire/SKILL.md" \\
      "\$HOME/.config/opencode/skills/oplire/SKILL.md"
EOF

say "Terminé"
cat <<EOF
  proxy-hunt --limit 400      # remplit le pool
  reset-opencode              # rotation + test
  proxy-rotate auto status    # état du mode automatique
EOF
