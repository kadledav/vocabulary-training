# Deploying the vocabulary app

Owner steps for the TrueNAS host (design §12; R-80, R-81, R-82, R-83, R-85).
`vocab.example.home` stands for the real host name, which is kept out of this
repository. Commands run as root on the TrueNAS shell unless noted.

Files here:

- `docker-compose.example.yml` → the home repo as `docker/vocabulary/docker-compose.yml`
  (configuration is in its `environment:`; set `VOCAB_PUBLIC_ORIGIN` to
  `https://<real host name>` — no path, no trailing slash, no `:443`)
- `.env.example` → `docker/vocabulary/.env` (the two secrets)

## 1. Storage (once)

TrueNAS UI: Datasets → `tank/services` → Add dataset `vocabulary`, preset
*Generic*. The app runs as the TrueNAS user **3101:3101** (`user:` in the
compose file; change the group there if yours differs):

```sh
mkdir -p /mnt/tank/services/vocabulary/{data,cache}
chown 3101:3101 /mnt/tank/services/vocabulary/data /mnt/tank/services/vocabulary/cache
chmod 0700      /mnt/tank/services/vocabulary/data /mnt/tank/services/vocabulary/cache
```

Backup is the existing daily replication `tank/services` →
`backup-tank/services`; after its first run check that
`backup-tank/services/vocabulary` exists. The app has no backup logic.

## 2. Secrets (`.env`)

The app refuses to start without both values.

- `VOCAB_PASSWORD_HASH_B64` — in a private shell (the password is visible
  while typing; at least 8 characters):

  ```sh
  docker run --rm -it ghcr.io/kadledav/vocabulary-training:X.Y.Z@sha256:<digest> hash-password
  ```

  Paste the printed line. A new value logs out every device.
- `VOCAB_IMPORT_TOKEN` — `openssl rand -hex 32` (at least 32 characters).
  Used only by the Claude Desktop connector (M11); keep a copy for it.

Protect `.env`: never commit it (the home repo must git-ignore `.env`), and
on the host `chmod 600 .env`. Alternatively leave `.env` out and enter the
two values as environment variables of the Portainer stack.

## 3. Network and DNS

- Traefik: the stack attaches to `proxy-net` and has **only** the
  `vocabulary-home` router on the `home` entrypoint. Never add a router for
  `websecure`, and never a router without an `entrypoints` label.
- DNS: add a host override `vocab.example.home` → TrueNAS LAN IP on the
  LAN/VPN resolver (local DNS), like the other `home` services.
- No mesh VPN route (R-81).

## 4. Deploy and check

1. In `docker-compose.yml` set `image:` to the tag **and** digest from the
   GitHub release notes, e.g.
   `ghcr.io/kadledav/vocabulary-training:0.1.0@sha256:…`.
2. Deploy the stack (Portainer, or `docker compose up -d` in `docker/vocabulary/`).
3. Checks (once, R-80/R-81/R-83):
   - `docker ps` shows `vocabulary` as `healthy`.
   - `https://vocab.example.home` from the LAN or VPN shows the login
     screen; logging in and a practice block work on the phone.
   - Not public: `curl -sk -o /dev/null -w '%{http_code}\n' https://<public-ip>:PUBLIC_PORT -H 'Host: vocab.example.home'`
     prints `404`.

## 5. Updates

1. Read the release notes. **If they mention a database migration**, take a
   snapshot first: `zfs snapshot tank/services/vocabulary@pre-vX.Y.Z`.
2. Replace tag and digest in `image:` (home repo commit), redeploy.
3. Check `healthy` and log in. Migrations run at start; a failed start logs
   `database check failed` — then restore (§6) from the snapshot.

## 6. Restore (tested once before phase 1 is accepted, R-85)

1. Stop the stack (Portainer → Stop, or `docker compose down`).
2. Pick a snapshot:
   `zfs list -t snapshot -o name,creation -s creation tank/services/vocabulary`
   (local) or `backup-tank/services/vocabulary` (replica).
3. Move the current data aside and create an empty `data`:

   ```sh
   cd /mnt/tank/services/vocabulary
   mv data "data.broken-$(date +%F)"
   mkdir data && chown 3101:3101 data && chmod 0700 data
   ```

4. Copy the **whole** `data/` directory of the snapshot — the database always
   together with its `-wal`/`-shm` files, never the `.sqlite` alone:

   ```sh
   cp -a /mnt/tank/services/vocabulary/.zfs/snapshot/<snap>/data/. /mnt/tank/services/vocabulary/data/
   # from the replica: /mnt/backup-tank/services/vocabulary/.zfs/snapshot/<snap>/data/.
   ```

   Alternative: `zfs rollback -r tank/services/vocabulary@<snap>` rolls back
   the cache too and destroys newer snapshots — only if intended.
5. Start the stack. The app runs `PRAGMA quick_check` and the migrations at
   start and refuses to start (`database check failed`) on a damaged file.
6. Verify in the UI: the words are there and a word's last review date is as
   expected.
