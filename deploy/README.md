# Deploying the vocabulary app

Owner steps for a Docker host with ZFS storage, e.g. TrueNAS (design §12;
R-80, R-81, R-82, R-83, R-85). The names here are placeholders for your own:
`vocab.example.home` (host name), `tank` (ZFS pool, mounted at `/mnt/tank`),
`backup-tank` (replication target), `proxy-net` (the reverse proxy's Docker
network), `home` (its LAN-only entrypoint), `PUBLIC_PORT` (its public port)
and user ID `3101`. Commands run as root on the host's shell unless noted.

File here: `docker-compose.example.yml` → your deployment repo, e.g. as
`docker/vocabulary/docker-compose.yml` (configuration is in its
`environment:`; set `VOCAB_PUBLIC_ORIGIN` to `https://<real host name>` — no
path, no trailing slash, no `:443`). There is no `.env` and no secret in the
environment: the password and the connector token are set in the app.

## 0. Image access (once, after the first release)

GitHub creates the package `ghcr.io/kadledav/vocabulary-training` as
**private** on the first push. Make it public once: GitHub → profile →
Packages → `vocabulary-training` → Package settings → *Change visibility* →
Public. Then check that it can be pulled without any login (a fresh, empty
Docker config):

```sh
DOCKER_CONFIG="$(mktemp -d)" docker buildx imagetools inspect ghcr.io/kadledav/vocabulary-training:0.1.0
# or: curl -s 'https://ghcr.io/token?scope=repository:kadledav/vocabulary-training:pull' must print a token
```

It lists the `linux/amd64` and `linux/arm64` images; an `unauthorized` or
`denied` error means the package is still private.

## 1. Storage (once)

Create the dataset `tank/services/vocabulary` (TrueNAS UI: Datasets →
`tank/services` → Add dataset `vocabulary`, preset *Generic*). Then set its
permissions (Datasets → `vocabulary` → Permissions → Edit): owner user
**e.g. 3101**, owner group **e.g. 3101** (the app's `user:` in the compose
file; use your own IDs), access for the owner only (`0700`),
*Apply recursively*. No `mkdir` is needed: the dataset root is mounted at
`/data` and the app creates the database there and its cache directory
`cache/` (mode 0700) on first start. If it cannot, it stops with
`storage is not usable` in `docker logs vocabulary` — then check the owner.

Limit the dataset so a runaway import or audio cache cannot fill the pool
(`refquota` counts the data itself, `quota` also its snapshots):

```sh
zfs set refquota=5G tank/services/vocabulary
zfs set quota=20G tank/services/vocabulary
zfs get quota,refquota,used tank/services/vocabulary
```

Backup is the existing daily replication `tank/services` →
`backup-tank/services`; after its first run check that
`backup-tank/services/vocabulary` exists. The app has no backup logic.

## 2. First start: set the password

On a start without a password the app is in **setup state** and writes a
one-time setup code to its log (a new code on every start until set up):

```sh
docker logs vocabulary 2>&1 | grep 'Setup required'
# … WARN Setup required: open the app and enter setup code ABCD-EFGH-JK
```

Open `https://vocab.example.home` on the phone: it shows *Set up*. Enter the
code (case and dashes do not matter) and a password of at least 12
characters, twice. You are logged in; from then on the app shows the normal
login and refuses setup for good. Wrong codes count toward the login lockout.

**Change the password:** Settings → Account → *Change password* (current +
new twice). Every other device is logged out; this one stays logged in.

**Forgotten password or suspected compromise (reset):** stop the stack, clear
the password, every session and the connector token, start again, and set a
new password with the new code (`reset-password` refuses with "stop the
stack first" while the app runs, and with "there is no database" on a wrong
mount):

```sh
docker compose stop vocabulary            # in docker/vocabulary/
docker compose run --rm vocabulary reset-password
docker compose up -d
docker logs vocabulary 2>&1 | grep 'Setup required'
```

Afterwards create a new connector token (Settings) and enter it in the
connector: the reset revoked the old one.

**Connector token** (Claude Desktop connector, M11): Settings → Account →
enter your password, *Create token*. The token is shown once — copy it into
the connector right away. Creating a new one replaces the old one (the old one stops working at
once). Without a token, the connector routes refuse every request.

## 3. Network and DNS

- Reverse proxy (the example uses Traefik labels): the stack attaches to
  the proxy's network (`proxy-net`) and has **only** the `vocabulary-home`
  router on your reverse proxy's LAN-only entrypoint (`home`). Never add a
  router for the public entrypoint (`websecure`), and never a router
  without an `entrypoints` label (Traefik would attach it to all of them).
- DNS: on the resolver your LAN and VPN clients use, add a host override
  `vocab.example.home` → the host's LAN IP.
- No mesh VPN route (R-81).

## 4. Deploy and check

1. In `docker-compose.yml` set `image:` to the tag **and** digest from the
   GitHub release notes, e.g.
   `ghcr.io/kadledav/vocabulary-training:0.1.0@sha256:…`.
2. Deploy the stack (Portainer, or `docker compose up -d` in `docker/vocabulary/`).
3. Checks (once, R-80/R-81/R-83):
   - `docker ps` shows `vocabulary` as `healthy`.
   - `https://vocab.example.home` from the LAN or VPN shows the setup
     screen (§2); after setup a practice block works on the phone.
   - Not public: `curl -sk -o /dev/null -w '%{http_code}\n' https://<public-ip>:PUBLIC_PORT -H 'Host: vocab.example.home'`
     prints `404`.

### Neural audio: `vocabulary-tts` (M6, R-40, R-83)

The second service turns headwords and sentences into MP3 clips (Kokoro
voices, US `af_heart` and UK `bf_emma`). It needs no setup:

- `image:` from the same release notes,
  `ghcr.io/kadledav/vocabulary-training-tts:X.Y.Z@sha256:…` (≈ 0.45 GB: the
  model is inside; the image has no shell and runs read-only).
- It is only on the network `vocab-internal` (`internal: true`, created by
  the stack): no published port, no Traefik router, no route to the LAN or
  the internet; only the app can reach it, as `http://vocabulary-tts:8090`.
  The app also joins `vocab-internal` (and stays on `proxy-net` for Traefik).
- Limits: 1280 MB memory without swap, 2 CPUs, 64 PIDs (measured peak about
  0.9 GB for the longest text, 5 threads). `docker ps` shows it `healthy`
  about 10 s after the start (model load and a self-test of both voices).
- Its log has one line per clip with lengths and a hash of the text, never
  the text itself (`docker logs vocabulary-tts`).
- The app uses it from the release that brings the audio module; until a
  clip is ready the app falls back to the browser's speech (R-41), so the
  service may also be stopped at any time.
- Licences: the image is MIT/Apache-2.0 with LGPL LAME (see `/licenses` in
  the image, `docker cp vocabulary-tts:/licenses .`).

## 5. Updates

1. Read the release notes. **If they mention a database migration**, take a
   snapshot first: `zfs snapshot tank/services/vocabulary@pre-vX.Y.Z`.
2. Replace tag and digest in both `image:` lines (home repo commit),
   redeploy.
3. Check `healthy` and log in. Migrations run at start; a failed start logs
   `database check failed` — then restore (§6) from the snapshot.

If an earlier build with secrets in `.env` is deployed: remove `.env` and the two
`VOCAB_PASSWORD_HASH_B64`/`VOCAB_IMPORT_TOKEN` lines from `environment:` (the
app refuses to start with unknown `VOCAB_*` settings), replace the two
volumes with the single dataset mount of the example, move the database
files up one level (`mv data/vocabulary.sqlite* . && rmdir data cache`, with
the stack stopped), start, and set the password with the setup code (§2).

## 6. Restore (tested once before phase 1 is accepted, R-85)

1. Stop the stack (Portainer → Stop, or `docker compose down`).
2. Pick a snapshot:
   `zfs list -t snapshot -o name,creation -s creation tank/services/vocabulary`
   (local) or `backup-tank/services/vocabulary` (replica).
3. Move the current database files aside:

   ```sh
   cd /mnt/tank/services/vocabulary
   mkdir "broken-$(date +%F)" && mv vocabulary.sqlite* "broken-$(date +%F)/"
   ```

4. Copy the database files of the snapshot — the database always together
   with its `-wal`/`-shm` files, never the `.sqlite` alone (`cp -a` keeps
   owner 3101):

   ```sh
   cp -a /mnt/tank/services/vocabulary/.zfs/snapshot/<snap>/vocabulary.sqlite* /mnt/tank/services/vocabulary/
   # from the replica: /mnt/backup-tank/services/vocabulary/.zfs/snapshot/<snap>/vocabulary.sqlite*
   ```

   The audio cache (`cache/`) can stay; it is regenerated from the database.
   Alternative: `zfs rollback -r tank/services/vocabulary@<snap>` rolls back
   the cache too and destroys newer snapshots — only if intended.
5. Start the stack. The app runs `PRAGMA quick_check` and the migrations at
   start and refuses to start (`database check failed`) on a damaged file.
6. Verify in the UI: the words are there and a word's last review date is as
   expected. The restored database brings the password (and connector token)
   it had then; if that password is unknown, reset it (§2).
7. Log out every session the snapshot brought back (devices that were
   logged in back then, or have been logged out since). Like
   `reset-password` it runs with the stack stopped:

   ```sh
   docker compose stop vocabulary            # in docker/vocabulary/
   docker compose run --rm vocabulary logout-all
   docker compose up -d
   ```

   Then log in again on each device.
