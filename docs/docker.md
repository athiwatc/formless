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
  ghcr.io/athiwatc/formless
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
