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

GitHub creates the packages `ghcr.io/kadledav/vocabulary-training` and
(from v0.2.0) `ghcr.io/kadledav/vocabulary-training-tts` as **private** on
the first push. Make each public once: GitHub → profile → Packages →
`vocabulary-training` (and `vocabulary-training-tts`) → Package settings →
*Change visibility* → Public. Then check that it can be pulled without any login (a fresh, empty
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
  `ghcr.io/kadledav/vocabulary-training-tts:X.Y.Z@sha256:…` (≈ 0.25 GB: the
  int8 model is inside; the image has no shell and runs read-only).
- It is only on the network `vocab-internal` (`internal: true`, created by
  the stack): no published port, no Traefik router, no route to the LAN or
  the internet; only the app can reach it, as `http://vocabulary-tts:8090`
  (`VOCAB_TTS_URL` in the app's `environment:`). The app also joins
  `vocab-internal` (and stays on `proxy-net` for Traefik). The service has
  no password or token: the internal network is its protection, so the app
  accepts only an internal `VOCAB_TTS_URL` — a host name without dots (the
  compose service name), `localhost`, or a loopback, private or link-local
  IP address — and refuses to start with anything else.
- Memory: the model is loaded only while clips are being made, in an
  engine process that ends `VOCAB_TTS_IDLE_SECONDS` (default 180) after the
  last clip; then the container is back at a few MiB. While it works, two
  kinds of memory count against its limit: its own (anonymous) memory,
  about 350 MiB for words and sentences and at most about 435 MiB for the
  longest and worst texts the service accepts, at any speed; and about
  180 MiB of file cache — the model, which is read again for every chunk of
  text. The file cache is not RAM the service holds: the kernel drops it
  when the host needs the memory (the next chunk then reads the model from
  disk). Measured with the container's cgroup numbers,
  docs/design/tts-spike.md, "Small memory". `GET /healthz` says
  `"engine": "running"` or `"stopped"`.
- Limits: 640 MB memory without swap (a cap that fits both, not a
  reservation), 1 CPU, 64 PIDs. Texts that would need more audio (more
  than 50 s, or much slower speech than the app asks for) are refused
  before the engine starts. If a text ever needs more memory anyway, the
  kernel stops only the engine process: that clip fails (after three tries,
  "crashed the audio service"), the service stays up. If the engine cannot
  start at all (for example a limit set too low), the service answers
  "unavailable"; the app then waits and retries every minute without
  counting it against any clip, and logs it once.
  `docker ps` shows it `healthy` a few seconds after the start (the model is
  loaded once for a self-test of both voices, then released).
- Stopping or redeploying it is safe at any time, also in the middle of a
  clip: it stops its engine at once, tells the app "shutting down" (the
  clip is retried later, nothing is counted against it) and exits within
  seconds (`stop_grace_period: 30s` is only a backstop).
- Its log has one line per clip with lengths and a hash of the text, never
  the text itself (`docker logs vocabulary-tts`).
- The app queues a clip for every headword (the pack's `speak` text if
  set) and every visible English sentence, in the voice chosen in Settings,
  and fetches them one at a time (words first). Until a clip is ready, ▶
  uses the browser's speech (R-41); the service may be stopped at any time.
  Settings → Audio shows clips ready / being made / failed, how many have an
  approximate pronunciation (a word in no dictionary), and *Retry failed
  clips*. A text that makes the service crash three times is marked failed
  ("crashed the audio service") and never blocks the other clips.
- Clips live in `/data/cache/audio` (regenerable, R-85: a restore without
  the cache only means the clips are made again). Disk: about 35 MB per
  1 000 sentences of 15–20 words per voice (≈ 7.5 KB a second of speech;
  a headword ≈ 5 KB); a voice switch keeps the other voice's clips while
  their text exists. The cache is capped at 2 GiB, and the app stops making
  clips while less than about 257 MiB is free on the dataset (the database
  needs room too); in both cases its log says so once and Settings → Audio
  shows why no new clips are made.
- Time: the small (int8) model on one thread is slow but fine for clips
  made once in the background: on an arm64 laptop core about 2.2–2.7 s per
  sentence and 1 s per word (300 clips: ~10 minutes), about twice that at
  half a CPU; a NAS core may be several times slower (300 clips: perhaps
  30–60 minutes). The first clip after an idle time also loads the
  lexicons (about 0.5 s on the laptop). A request may take up to 10 minutes
  before it counts as failed.
- Licences: the image is MIT/Apache-2.0/BSD with LGPL LAME (see
  `/licenses` in the image, `docker cp vocabulary-tts:/licenses .`).

## 5. Updates

1. Read the release notes. **If they mention a database migration**, take a
   snapshot first: `zfs snapshot tank/services/vocabulary@pre-vX.Y.Z`.
2. Replace tag and digest in both `image:` lines (home repo commit),
   redeploy.
3. Check `healthy` and log in. Migrations run at start; a failed start logs
   `database check failed` — then restore (§6) from the snapshot.

### v0.2.0 → v0.2.1 (smaller audio service)

1. Take the new `vocabulary-tts` settings from `docker-compose.example.yml`:
   `VOCAB_TTS_THREADS=1`, `VOCAB_TTS_IDLE_SECONDS=180`, `mem_limit` and
   `memswap_limit: 640m`, `cpus: 1.0`, `stop_grace_period: 30s`; set both
   `image:` lines from the release notes; redeploy. No migration.
2. The service now uses the small (int8) model, so the app makes every clip
   again, in the background (the old clips play until their replacement is
   ready, then they are deleted). Expect the audio container at a few
   hundred MiB while that runs, then a few MiB (`docker stats`).

### v0.1.1 → v0.2.0 (neural audio)

1. Snapshot first (migration `0006` adds the audio tables; nothing else
   changes): `zfs snapshot tank/services/vocabulary@pre-v0.2.0`.
2. Make the package `vocabulary-training-tts` public (§0).
3. Take the new `docker-compose.example.yml` parts: the `vocabulary-tts`
   service, the `vocab-internal` network (also in the app's `networks:`)
   and `VOCAB_TTS_URL=http://vocabulary-tts:8090` in the app's
   `environment:`; set both `image:` lines from the release notes; redeploy.
4. Check: both containers `healthy`; `docker logs vocabulary` shows
   `neural audio on` and soon `audio clips synced queued=…`; Settings →
   Audio counts clips. Audio appears gradually as the queue drains (the
   words first); meanwhile ▶ speaks with the browser as before.
5. Auto-play is off by default now; turn it on in Settings if wanted.

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
