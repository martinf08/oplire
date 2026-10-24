# oplire — rotation WARP + pool de proxies pour OpenCode

> Débloquer les erreurs **HTTP 429**, **« Free usage limit reached »** et
> **« Quota Exceeded »** d'OpenCode en changeant d'identité réseau :
> rotation Cloudflare WARP et/ou pool de proxies avec bascule **à chaud**.

Ce dépôt contient les scripts d'exploitation autour du binaire
[`oplire`](https://github.com/BerkeOruc/oplire) : il **n'est pas** le projet
upstream et n'embarque ni binaire ni secret.

---

## Sommaire

- [Le problème](#le-problème)
- [Comment ça marche](#comment-ça-marche)
- [Contenu du dépôt](#contenu-du-dépôt)
- [Prérequis](#prérequis)
- [Installation](#installation)
- [Utilisation](#utilisation)
- [Configuration](#configuration)
- [Fonctionnement détaillé](#fonctionnement-détaillé)
- [Sécurité](#sécurité)
- [Dépannage](#dépannage)
- [Désinstallation](#désinstallation)
- [Licence](#licence)

---

## Le problème

OpenCode (et d'autres clients) limite l'usage gratuit par IP. Quand la limite
est atteinte, deux leviers existent :

1. **Changer d'IP** : WARP fournit une nouvelle identité réseau à chaque
   `oplire reset`.
2. **Passer par un proxy** : un pool de proxies permet de changer d'IP sans
   toucher au tunnel, et même **sans redémarrer OpenCode**.

Les deux sont complémentaires : WARP est fiable mais lent à tourner ; les
proxies sont instantanés mais éphémères (les listes publiques meurent vite).

## Comment ça marche

```text
                 ┌─────────────────────────────────────────────┐
                 │              OpenCode                       │
                 └───────────────┬─────────────────────────────┘
                                 │ HTTPS_PROXY
                                 ▼
                 ┌─────────────────────────────────────────────┐
                 │   proxy-rotate daemon  127.0.0.1:8899       │
                 │   (relaie vers le proxy courant du pool)    │
                 └───────────────┬─────────────────────────────┘
                                 │
                                 ▼
                 ┌─────────────────────────────────────────────┐
                 │   proxy #N  (pool SQLite, fraîcheur 30 min) │
                 └───────────────┬─────────────────────────────┘
                                 │
                                 ▼
                          Internet  ──►  API OpenCode

  proxy-rotate watch ── lit les logs OpenCode
                     ── détecte un vrai 429
                     ── met le proxy en cooldown, passe au suivant
                     ── si le pool est épuisé : oplire reset (WARP)
```

Deux modes d'application :

| Mode | `HTTPS_PROXY` | Rotation |
| --- | --- | --- |
| `direct` | l'URL du proxy courant | nécessite un redémarrage d'OpenCode |
| `local` | `http://127.0.0.1:8899` | **à chaud**, sans redémarrage |

Le mode `local` est celui utilisé par `auto on` : le daemon écoute sur
`127.0.0.1:8899` et relaie vers le proxy sélectionné. Changer de proxy ne
modifie que l'état interne du daemon.

## Contenu du dépôt

| Fichier | Rôle |
| --- | --- |
| `bin/reset-opencode` | point d'entrée : hunt → rotation → vérification → test |
| `bin/reset-opencode-default` | retour à la connexion par défaut (coupe WARP) |
| `bin/proxy-rotate` | pool SQLite, daemon local, veilleur 429, rotation |
| `bin/proxy-hunt` | récupère et teste des proxies depuis des listes publiques |
| `init.d/proxy-rotate` | service OpenRC (Alpine) |
| `systemd/proxy-rotate.service` | unité systemd équivalente |
| `skills/oplire/SKILL.md` | skill OpenCode : diagnostic et commandes |
| `install.sh` | installation automatique |

## Prérequis

- Linux avec `iproute2`, `curl`, `python3` (3.9+), `sqlite3` côté Python.
- **WARP** : `wireguard-tools`, `wgcf`, le binaire `oplire`
  (voir [docs/WARP.md](docs/WARP.md)).
- **Proxies** : rien de plus ; `proxy-hunt` utilise `curl`.
- Alpine 3.20+ / OpenRC testé ; systemd supporté via l'unité fournie.

## Installation

```sh
git clone https://github.com/martinf08/oplire.git
cd oplire
sudo ./install.sh
```

`install.sh` copie les scripts dans `/usr/local/bin`, installe le service,
initialise la base et affiche les prochaines étapes. Options :

```sh
./install.sh --prefix /usr/local   # préfixe
./install.sh --no-service          # ne pas activer le service
./install.sh --dry-run             # montre les actions
```

Installation manuelle (root) :

```sh
install -Dm755 bin/reset-opencode          /usr/local/bin/reset-opencode
install -Dm755 bin/reset-opencode-default  /usr/local/bin/reset-opencode-default
install -Dm755 bin/proxy-rotate            /usr/local/bin/proxy-rotate
install -Dm755 bin/proxy-hunt              /usr/local/bin/proxy-hunt
install -Dm755 init.d/proxy-rotate         /etc/init.d/proxy-rotate
rc-update add proxy-rotate default
proxy-rotate init
```

Sous systemd :

```sh
install -Dm644 systemd/proxy-rotate.service /etc/systemd/system/proxy-rotate.service
systemctl daemon-reload
systemctl enable --now proxy-rotate
```

## Utilisation

### Débloquer OpenCode

```sh
reset-opencode            # hunt + rotation (proxy ou WARP) + test
reset-opencode --fast     # sans requête de test
reset-opencode --no-hunt  # sans rafraîchir le pool avant rotation
reset-opencode --proxy    # force le proxy suivant du pool
reset-opencode --default  # revient à la connexion par défaut (coupe WARP)
```

Déroulé de `reset-opencode` :

1. `proxy-hunt` (sauf `--no-hunt`) pour ajouter des proxies frais ;
2. activation du mode `local` si le pool contient un proxy actif ;
3. fermeture des processus OpenCode résiduels ;
4. rotation vers un proxy frais, sinon `oplire reset` (WARP) ;
5. vérification de l'IP de sortie et test d'une requête OpenCode
   (3 tentatives en cas de 429).

### Gérer le pool

```sh
proxy-hunt --limit 400              # remplit le pool (listes publiques)
proxy-rotate init                   # crée la base
proxy-rotate add 'http://user:pass@host:8080' --label ovh-fr --country FR
proxy-rotate import proxies.txt     # une URL par ligne
proxy-rotate list                   # état, fraîcheur, compteurs
proxy-rotate test --all             # test réel via curl
proxy-rotate refresh --jobs 40      # re-teste tout le pool
proxy-rotate rotate                 # proxy suivant + applique
proxy-rotate use 12                 # applique le proxy #12
proxy-rotate 429                    # cooldown du courant + rotation
proxy-rotate ok | fail              # marque le résultat du proxy courant
proxy-rotate status                 # résumé lisible
proxy-rotate reset | clear | prune  # remise à zéro / purge
proxy-rotate export                 # exporte le pool (JSON)
```

### Mode automatique

```sh
proxy-rotate auto on        # daemon local + veilleur
proxy-rotate auto status
proxy-rotate auto off
```

En mode `auto`, le service `proxy-rotate` :
- démarre le daemon `127.0.0.1:8899` ;
- surveille les logs OpenCode ;
- change de proxy dès qu'un vrai 429 apparaît ;
- fait un `oplire reset` si le pool est épuisé ;
- vérifie la santé du proxy courant toutes les 90 s.

### Après un reboot

```sh
reset-opencode --fast
```

Le service remonte tout seul ; `--fast` force une rotation sans consommer de
requête de test.

## Configuration

Toutes les valeurs sont surchargeables par variable d'environnement.

| Variable | Défaut | Rôle |
| --- | --- | --- |
| `OPLIRE_STATE_DIR` | `/root` (root) ou `~/.local/share/oplire` | fichiers d'état (DB, sauvegardes WARP) |
| `OPLIRE_DB` | `$OPLIRE_STATE_DIR/proxies.db` | base SQLite du pool |
| `OPLIRE_ENV_FILE` | `/etc/opencode-proxy.env` | env appliqué à OpenCode |
| `OPLIRE_PROFILE_FILE` | `/etc/profile.d/99-opencode-proxy.sh` | env des shells de connexion |
| `OPLIRE_RUN_DIR` | `/run/proxy-rotate` | pidfiles et verrous |
| `OPENCODE_LOG` | `~/.local/share/opencode/log/opencode.log` | log surveillé |
| `OPLIRE_NO_PROXY` | `localhost` + réseaux locaux détectés | hôtes non proxifiés |

Exemple pour un utilisateur non-root :

```sh
export OPLIRE_STATE_DIR="$HOME/.local/state/oplire"
export OPLIRE_ENV_FILE="$HOME/.config/opencode/proxy.env"
export OPLIRE_RUN_DIR="$HOME/.run/oplire"
```

## Fonctionnement détaillé

### Fraîcheur des proxies

Chaque test réussi met à jour `last_seen`. La rotation privilégie les proxies
vus depuis moins de **30 minutes** ; les autres passent en cooldown. Les
listes publiques ayant ~70 % de mortalité en 10 minutes, `proxy-hunt` et
`refresh` doivent être relancés régulièrement.

### Cooldown et mort

- Un 429 met le proxy courant en cooldown (`DEFAULT_COOLDOWN`, 600 s).
- Trois échecs consécutifs le marquent mort (`DEFAULT_DEAD_AFTER`).
- `prune` supprime les entrées mortes et anciennes.

### Anti-boucle

- Rotation limitée à une toutes les 20 s (`ROTATE_DEBOUNCE`).
- Reset WARP limité à un toutes les 120 s (`WARP_DEBOUNCE`).
- Le veilleur utilise `flock` : une seule instance.

### Détection des 429

Le veilleur ne réagit qu'aux vraies erreurs de rate limit dans le log OpenCode
et ignore les lignes de bruit (`spawning process`, `args=`). Un faux positif
provoquerait une rotation inutile, donc une perte de proxies sains.

## Sécurité

- Aucun secret dans le dépôt : les clés WARP restent dans
  `$OPLIRE_STATE_DIR/wgcf-account.toml`, la configuration WireGuard dans
  `/etc/wireguard/wg0.conf`.
- La base du pool est créée en `600`.
- Les proxies publics sont **non fiables** : ne fais pas transiter de
  credentials sensibles par le pool. Ce projet sert à contourner une limite
  de débit, pas à garantir l'anonymat.
- Changer d'IP peut contrevenir aux conditions d'utilisation de certains
  fournisseurs. À utiliser de façon responsable.

## Dépannage

| Symptôme | Piste |
| --- | --- |
| `warp=off` | `oplire status`, `wg show`, relancer `reset-opencode` |
| `wg0` absent | `--purge` a supprimé la config : `reset-opencode` en régénère une |
| `proxy-rotate: database is locked` | un autre processus écrit ; attendre ou `proxy-rotate status` |
| `429` en boucle | pool vide ou tokens invalides : `proxy-hunt`, `proxy-rotate refresh` |
| OpenCode ignore le proxy | vérifier `OPLIRE_ENV_FILE` et redémarrer OpenCode |
| Service ne démarre pas | `rc-service proxy-rotate status`, `/var/log/proxy-rotate.log` |
| Aucune IP ne change | le proxy est transparent ou mort : `proxy-rotate test --all` |

Diagnostic manuel :

```sh
proxy-rotate status
proxy-rotate list --all
curl -x http://127.0.0.1:8899 https://www.cloudflare.com/cdn-cgi/trace
oplire status
tail -f /var/log/proxy-rotate.log
```

## Désinstallation

```sh
rc-service proxy-rotate stop
rc-update del proxy-rotate default
rm -f /usr/local/bin/{reset-opencode,reset-opencode-default,proxy-rotate,proxy-hunt}
rm -f /etc/init.d/proxy-rotate
rm -rf /run/proxy-rotate
# facultatif : la base et les sauvegardes
rm -rf /root/proxies.db /root/warp-backup-*.tar.gz
```

## Licence

MIT — voir [LICENSE](LICENSE).
