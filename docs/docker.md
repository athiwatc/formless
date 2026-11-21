# Running Formless in Docker

The image `ghcr.io/athiwatc/formless` runs the **daemon** headless on a
server: Runs, scheduled jobs, MCP servers and the optimiser keep going with
nobody connected (`formless daemon serve --stay`). You talk to it through
the **relay** (the browser UI, end-to-end encrypted) or with `docker exec`.

## Start it

```sh
docker volume create formless-data
docker volume create formless-config
echo "sk-…" > api_key.txt   # your provider key
docker run -d --name formless --restart unless-stopped \
  -v formless-data:/data -v formless-config:/config \
  -v "$PWD/api_key.txt:/run/secrets/api_key:ro" \
  -e FORMLESS_API_KEY_FILE=/run/secrets/api_key \
  ghcr.io/athiwatc/formless:latest
```

Then give it a provider. Write a `config.toml` (any setting from the
[settings table](../SPEC.md#13-configuration-complete-settings-table) can go here):

```toml
[provider]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-sonnet-5.5"
```

copy it into the config volume, restart, and check:

```sh
docker exec -i formless sh -c 'cat > /config/config.toml' < config.toml
docker restart formless
docker exec formless formless doctor
```

Later changes: `docker exec formless formless config set provider.model …`
(the daemon validates and applies them), or edit the file the same way.

## Volumes

| Path | What | Keep it? |
|---|---|---|
| `/data` | Sessions, Tools, metrics, artifacts, backups (`formless.db`) | Yes: this is everything Formless learned |
| `/config` | `config.toml`, the relay pairing secret | Yes |

Daily database backups land in `/data/backups` (`daemon.backups_keep`).

Named volumes (above) just work. A **bind mount** of a host folder must be
writable by the container's user, uid `10001`: `sudo chown -R 10001:10001
./formless-data ./formless-config` first, or the daemon can't start (it
can't write its database), and `docker logs formless` says so.

## Image details

- `linux/amd64` only for now; on Apple silicon or other arm64 servers it
  runs under emulation (slow) or build it yourself with
  `docker build -t formless .`.
- Bases are pinned by digest in the `Dockerfile`; a release updates them
  together.
- `HEALTHCHECK` runs `formless daemon status --check` and succeeds when
  the daemon returns its PID and version. `docker ps` shows the result.
  Release smoke tests require a successful `daemon status --json` reply
  with a positive PID and version before publishing the image.
- `docker stop` sends SIGTERM through tini. Formless uses the same shutdown
  path as `formless daemon stop` to stop its MCP servers and remove the
  daemon socket and PID marker.

## Upgrade or roll back

Release images publish a version tag with a leading `v` (for example,
`v0.1.0`) and a floating `latest` tag. Omitting the tag means `latest`.
Pin a published version tag or an image digest on servers so a recreate
does not silently switch versions.

Before upgrading, record the current image tag or digest and the original run
options. Choose a maintenance window, let active Runs finish, and stop all
containers and other processes that write to either volume before copying
both volumes to a separate backup location. This includes any other Formless
profiles that share the volumes. Keep the pairing secret and backup private. The daemon cannot run
jobs while it is stopped; check Runs and the job schedule after restart rather
than assuming interrupted work resumed.

Using the settings from [Start it](#start-it), pull the new tag before stopping the
container. Replace `vX.Y.Z` with the published target tag:

```sh
image=ghcr.io/athiwatc/formless:vX.Y.Z
docker pull "$image"
docker stop formless
```

Back up both stopped volumes before continuing. Once the backup is verified,
recreate the container:

```sh
image=ghcr.io/athiwatc/formless:vX.Y.Z
docker rm formless
docker run -d --name formless --restart unless-stopped \
  -v formless-data:/data -v formless-config:/config \
  -v "$PWD/api_key.txt:/run/secrets/api_key:ro" \
  -e FORMLESS_API_KEY_FILE=/run/secrets/api_key \
  "$image"
docker exec formless formless --version
docker exec formless formless doctor
```

Retain your existing mounts, environment, network and profile flags if they
differ. Remove only the container, not the volumes. With Compose, set the
service's `image:` to the new tag, run `docker compose pull formless`, then run
`docker compose stop formless`. Back up both volumes before running `docker compose up -d formless`.
Replace `formless` with your service name when it differs. Do not use
`docker compose down -v` for an upgrade. Check `docker ps` for a healthy
container and `docker logs formless` if startup or `doctor` fails.

Do not use `formless update` inside the container. The binary belongs to the
image; replace the image and preserve the volumes instead.

For rollback, stop the new container and any other writers. Before starting
the previous image, check whether that version can open the post-upgrade
state. If compatibility is not documented, restore the pre-upgrade data and
config backup into separate volumes first. Then recreate the container with
the previous tag or digest. If you restored the backup, mount the restored
volumes instead of the migrated originals. Keep the post-upgrade volumes
intact; the restored copy does not contain changes made since the backup.

## The API key

- `FORMLESS_API_KEY_FILE=/run/secrets/…`: a file holding the key (Docker
  secrets, Kubernetes secrets). Preferred.
- `FORMLESS_API_KEY=…`: the key itself, in the environment (visible to
  `docker inspect`).
- Named providers: `FORMLESS_API_KEY_<NAME>_FILE` (preferred) or
  `FORMLESS_API_KEY_<NAME>` (e.g. `FORMLESS_API_KEY_LOCAL_FILE`).

A file that is set but can't be used (missing, empty, not text) is named
in the daemon log and in `formless doctor`, which also says where the key
comes from (env, file, fallback file or keychain) without showing it. A
leading BOM and surrounding whitespace are ignored.

There is no keychain in the container; the key is never written to the
volumes.

## Using it from a browser

```sh
docker exec formless formless relay enable
docker exec -it formless formless relay status --qr
```

Open the printed link (or scan the QR code). Traffic goes through the relay
encrypted with the pairing secret; the relay can't read it. The local web UI
(`formless web`) listens on 127.0.0.1 inside the container only, by design;
to use it, run the container with `--network host` and open an SSH tunnel to
that port.

## Profiles

Each profile is a subfolder: `/data/<profile>` and `/config/<profile>`. Run a
second daemon for another profile with the same volumes:

```sh
docker run -d --name formless-work -v formless-data:/data -v formless-config:/config \
  ghcr.io/athiwatc/formless --profile work daemon serve --stay
```

## Approvals

Nobody answers prompts on a server: keep approvals off (the default; every
call is audit-logged), or turn them on and approve from the browser through
the relay. Headless `formless run` inside the container fails closed on
approvals unless you pass `--yes`.

## Building it yourself

```sh
docker build -t formless .
```
