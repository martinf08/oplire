---
name: oplire
description: Reset Cloudflare WARP IP or rotate a proxy pool to bypass OpenCode 429 Rate Limit (Free usage limit reached) or Quota Exceeded errors.
---

# OpenCode 429 — rotation WARP et proxies

## Quand utiliser ce skill

- OpenCode renvoie **HTTP 429**, **« Free usage limit reached »** ou
  **« Rate limit exceeded / Quota Exceeded »** ;
- les requêtes échouent après un changement d'IP ou un redémarrage ;
- `oplire status` indique `Disconnected`, ou `warp=off` dans le trace
  Cloudflare.

## Commande recommandée

```sh
reset-opencode
```

Déroulé :

1. `proxy-hunt` pour ajouter des proxies frais ;
2. activation du mode proxy local si le pool est utilisable ;
3. fermeture des processus OpenCode résiduels ;
4. rotation vers un proxy frais, sinon `oplire reset` (WARP) ;
5. vérification de l'IP et test d'une requête (3 essais en cas de 429).

Variantes :

| Commande | Effet |
| --- | --- |
| `reset-opencode --fast` | sans requête de test (après reboot) |
| `reset-opencode --no-hunt` | ne pas rafraîchir le pool avant rotation |
| `reset-opencode --proxy` | force le proxy suivant du pool |
| `reset-opencode --default` | coupe WARP, connexion par défaut |

## Pool de proxies

Base SQLite : `$OPLIRE_STATE_DIR/proxies.db` (défaut `/root/proxies.db`,
mode 600). Les proxies publics sont éphémères : rafraîchir souvent.

```sh
proxy-hunt --limit 400              # listes publiques → tests → import
proxy-rotate list                   # état, fraîcheur, compteurs
proxy-rotate test --all             # test réel via curl
proxy-rotate refresh --jobs 40      # re-teste tout le pool
proxy-rotate rotate                 # proxy suivant + applique
proxy-rotate use 12                 # applique le proxy #12
proxy-rotate 429                    # cooldown + rotation
proxy-rotate 429 --warp             # fallback WARP si pool vide
proxy-rotate ok | fail              # marque le résultat courant
proxy-rotate status                 # résumé
proxy-rotate prune                  # supprime les entrées mortes
```

## Mode automatique

```sh
proxy-rotate auto on       # daemon 127.0.0.1:8899 + veilleur
proxy-rotate auto status
proxy-rotate auto off
```

Le veilleur lit le log OpenCode, détecte les vrais 429, met le proxy en
cooldown et bascule sur le suivant. Pool épuisé → `oplire reset`.

Service :

```sh
rc-service proxy-rotate status      # OpenRC
systemctl status proxy-rotate       # systemd
tail -f /var/log/proxy-rotate.log
```

## Revenir à la connexion par défaut

```sh
reset-opencode --default
# ou
reset-opencode-default [--dry-run] [--purge]
```

`--purge` sauvegarde puis supprime `wg0.conf` et les fichiers `wgcf`
(`$OPLIRE_STATE_DIR/warp-backup-<date>.tar.gz`).

## Diagnostic

```sh
oplire status
curl -fsS https://www.cloudflare.com/cdn-cgi/trace | grep -E '^(ip|warp)='
proxy-rotate status
proxy-rotate list --all
curl -x http://127.0.0.1:8899 https://www.cloudflare.com/cdn-cgi/trace
ip rule show
wg show
```

| Symptôme | Piste |
| --- | --- |
| `warp=off` | tunnel down ou DNS : `reset-opencode` |
| `429` en boucle | pool vide/mort : `proxy-hunt`, `refresh` |
| IP inchangée | proxy transparent ou mort : `test --all` |
| OpenCode ignore le proxy | vérifier `OPLIRE_ENV_FILE`, redémarrer OpenCode |
| service KO | `rc-service proxy-rotate status`, log |

## Installation du skill

```sh
install -Dm644 skills/oplire/SKILL.md \
  "$HOME/.config/opencode/skills/oplire/SKILL.md"
```

Dépôt : <https://github.com/martinf08/oplire>
