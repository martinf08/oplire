# WARP sur Alpine sans client officiel

`oplire` pilote Cloudflare WARP via deux binaires : `wgcf` (création de compte
et de profil WireGuard) et `wg-quick` (tunnel). Le client officiel n'est pas
nécessaire. Cette page reproduit le montage minimal.

> Adapté d'un lab Alpine 3.24. Les chemins d'état sont ceux par défaut des
> scripts (`OPLIRE_STATE_DIR=/root`). Ailleurs, exporte la variable.

## 1. Dépendances

```sh
apk add wireguard-tools wireguard-tools-openrc wireguard-tools-wg-quick curl
```

`wgcf` n'est pas toujours empaqueté. Deux options :

```sh
# binaire statique
curl -L -o /usr/local/bin/wgcf \
  https://github.com/ViRb3/wgcf/releases/latest/download/wgcf_linux_amd64
chmod 755 /usr/local/bin/wgcf

# ou via Go
go install github.com/ViRb3/wgcf@latest
```

## 2. Compte et profil

```sh
mkdir -p /root/.warp && cd /root/.warp
wgcf register --accept-tos
wgcf generate
```

**Retire la ligne `DNS`** du profil : sur une VM derrière un routeur/NAT, le
DNS WARP peut casser la résolution.

```sh
sed -i '/^DNS/d' wgcf-profile.conf
```

Installe le profil :

```sh
install -d -m 700 /etc/wireguard
install -m 600 wgcf-profile.conf /etc/wireguard/wg0.conf
cp -f wgcf-account.toml wgcf-profile.conf /root/
```

Monte le tunnel et vérifie :

```sh
wg-quick up wg0
curl -fsS https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='
```

`warp=on` confirme que le trafic sort par Cloudflare.

## 3. Les deux wrappers attendus par `oplire`

`oplire` appelle `warp-cli` et `systemctl` comme s'il était sur une machine
avec le client officiel. Sur Alpine, deux shims suffisent.

### `/usr/local/bin/warp-cli`

```sh
cat > /usr/local/bin/warp-cli <<'EOF'
#!/bin/sh
set -e
CMD="$1"; shift || true
case "$CMD" in
    --version|-v|-V|version) echo "warp-cli 2026.10.21"; exit 0 ;;
    status)
        if ip link show wg0 2>/dev/null | grep -q "UP"; then
            echo "Status update: Connected"
        else
            echo "Status update: Disconnected"
        fi
        echo "Success"; exit 0 ;;
    disconnect)
        wg-quick down wg0 2>/dev/null || ip link set wg0 down 2>/dev/null || true
        echo "Success"; exit 0 ;;
    connect)
        if ! ip link show wg0 2>/dev/null | grep -q "UP"; then
            wg-quick up wg0 2>/dev/null || ip link set wg0 up 2>/dev/null || true
        fi
        echo "Success"; exit 0 ;;
    registration)
        if [ "$1" = "new" ]; then
            TMP=$(mktemp -d); cd "$TMP"
            if wgcf register --accept-tos >/dev/null 2>&1 && wgcf generate >/dev/null 2>&1; then
                sed -i '/^DNS/d' wgcf-profile.conf
                install -d -m 700 /etc/wireguard /var/lib/cloudflare-warp
                install -m 600 wgcf-profile.conf /etc/wireguard/wg0.conf
                cp -f wgcf-account.toml wgcf-profile.conf /root/
                cp -f wgcf-account.toml wgcf-profile.conf /var/lib/cloudflare-warp/
                cd /; rm -rf "$TMP"; echo "Success"; exit 0
            fi
            cd /; rm -rf "$TMP"; echo "Registration failed" >&2; exit 1
        fi
        echo "Success"; exit 0 ;;
    *) echo "Success"; exit 0 ;;
esac
EOF
chmod 755 /usr/local/bin/warp-cli
```

### `/usr/local/bin/systemctl` (Alpine sans systemd)

```sh
cat > /usr/local/bin/systemctl <<'EOF'
#!/bin/sh
# Shim : oplire appelle systemctl, absent sur Alpine/OpenRC.
exit 0
EOF
chmod 755 /usr/local/bin/systemctl
```

Ne l'installe **pas** sur une machine où systemd est réellement utilisé.

## 4. Installer `oplire`

```sh
# copie depuis une machine où il est déjà présent
scp root@<ancienne-machine>:/usr/bin/oplire /usr/local/bin/oplire
chmod 755 /usr/local/bin/oplire
oplire --version
```

Ou compilation depuis les sources :

```sh
apk add cargo git
git clone https://github.com/BerkeOruc/oplire.git
cd oplire && cargo build --release
install -m 755 target/release/oplire /usr/local/bin/oplire
```

## 5. Vérifier et tourner

```sh
oplire status
oplire reset
curl -fsS https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='
```

`oplire reset` recrée un compte WARP (`registration new`), remonte `wg0` et
change donc l'IP publique. C'est l'action lancée par `reset-opencode`.

## Dépannage

| Symptôme | Cause probable |
| --- | --- |
| `warp=off` malgré `wg0` UP | DNS ou route par défaut non prise ; vérifier `ip route` |
| Résolution cassée | ligne `DNS` encore présente dans `wg0.conf` |
| `Registration failed` | `wgcf` absent ou pas de réseau ; tester `wgcf register` à la main |
| `wg0` absent après `--purge` | normal : `reset-opencode` régénère le profil |
| `oplire: command not found` | binaire non installé ou hors `PATH` |
| MTU/timeouts | essayer `MTU = 1280` dans `wg0.conf` |

## Sécurité

- `wg0.conf` et `wgcf-account.toml` contiennent des clés privées : `600`,
  dossier `700`, jamais dans un dépôt ni sur un Drive en clair.
- Un `--purge` de `reset-opencode-default` sauvegarde ces fichiers dans
  `$OPLIRE_STATE_DIR/warp-backup-<date>.tar.gz` avant suppression.
