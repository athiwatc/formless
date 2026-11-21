# Running Formless in Docker

The image `ghcr.io/athiwatc/formless` runs the **daemon** headless on a
server: Runs, scheduled jobs, MCP servers and the optimiser keep going with
nobody connected (`formless daemon serve --stay`). You talk to it through
the **relay** (the browser UI, end-to-end encrypted) or with `docker exec`.

## Start it

Choose a published `vX.Y.Z` tag from the [releases](https://github.com/athiwatc/formless/releases)
and replace the placeholder below. For a rootful Linux Docker Engine with
default user mapping, run these commands in Bash. They prompt for the key
without echoing it or adding it to your shell history. The key file lives
outside your project in a private directory.

```bash
image=ghcr.io/athiwatc/formless:vX.Y.Z
secret_dir="${XDG_CONFIG_HOME:-$HOME/.config}/formless-docker"
secret_file="$secret_dir/api_key"
(
  set -e
  set +x
  set -o noclobber
  umask 077
  install -d -m 0700 "$secret_dir"
  IFS= read -r -s -p 'Provider API key: ' provider_key
  printf '\n'
  test -n "$provider_key"
  printf '%s\n' "$provider_key" > "$secret_file"
  unset provider_key
  sudo chown 10001 "$secret_file"
  sudo chmod 0400 "$secret_file"
)
```

After the file is ready, create the volumes and start the daemon using the
same shell variables.

```sh
docker volume create formless-data
docker volume create formless-config
docker run -d --name formless --restart unless-stopped \
  -v formless-data:/data -v formless-config:/config \
  --mount "type=bind,source=$secret_file,target=/run/secrets/api_key,readonly" \
  -e FORMLESS_API_KEY_FILE=/run/secrets/api_key \
  "$image"
```

Then give it a provider. Write a `config.toml` (any setting from the
[settings table](../SPEC.md#13-configuration-complete-settings-table) can go here):

```toml
[provider]
base_url = "https://openrouter.ai/api/v1"
model = "anthropic/claude-sonnet-5.5"
```

Copy it into a new config volume, restart, and check. **The copy command
replaces all of `/config/config.toml`, including any unrelated settings.**
Follow [the existing-volume instructions](#update-an-existing-provider-configuration)
if the volume already has settings you want to keep.

```sh
docker exec -i formless sh -c 'cat > /config/config.toml' < config.toml
docker restart formless
docker exec formless formless doctor
```

The doctor command can contact the configured provider and report network
problems or failures from other checks.

### Update an existing provider configuration

To change `provider.base_url`, edit that key in the volume's existing
`config.toml`, keeping the other settings, and restart the container.
The endpoint is a file-only setting.

To change the model on a running container, use the daemon-validated
`config set` command:

```sh
docker exec formless formless config set provider.model 'anthropic/claude-sonnet-5.5'
```

Changing the endpoint and model in this order makes two separate updates.
If the model update fails, the earlier endpoint edit stays saved.
`FORMLESS_BASE_URL` and `FORMLESS_MODEL` can override the saved settings.
If an environment override applies to the model, `config set` reports the
model the daemon uses. To inspect what the CLI loads inside the container,
including its environment overrides, run:

```sh
docker exec formless formless config get provider.base_url
docker exec formless formless config get provider.model
```

Successful setting validation confirms the configuration values. Provider
connectivity and model availability still depend on the endpoint.

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

- Releases publish `:vX.Y.Z` with a leading `v`, plus the moving
  `:latest` tag. Pin a release tag or image digest on servers.
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

## Upgrading and rolling back

Keep the previous image tag or digest. Wait for active Runs to finish
before stopping the container. Runs still in flight may be interrupted.
Enabled, approved jobs keep their schedule in the data volume, and missed
firings of a due job coalesce into one execution after the daemon restarts.

Set `image` to the next pinned release, then pull it before stopping the
existing container.

```sh
image=ghcr.io/athiwatc/formless:vX.Y.Z
docker pull "$image" && docker stop formless
```

Stop any other containers using the same volumes. While those containers
are stopped, make and verify backups of both volumes. This preserves the
database, configuration, and external files together. Then remove the
stopped container.

```sh
docker rm formless
```

Run the same `docker run` command from [Start it](#start-it) with the new
`image`, the same `formless-data` and `formless-config` volumes, and the
existing secret mount. In a fresh shell, set `secret_file` to the existing
host path of that mount. After the container is healthy, verify it with
`docker exec formless formless --version` and
`docker exec formless formless doctor`. With Compose, update the image
reference in your configuration and run
`docker compose pull && docker compose stop`. Verify the same volume
backups while the containers are stopped, then run `docker compose up -d`.

Replace the image to upgrade. `formless update` reports the available
version and suggests commands for a host installation.

To roll back, recreate the container using your previous image tag or
digest. Check the release notes for data format compatibility before
using volumes touched by the newer image. If compatibility is uncertain
or a migration is incompatible, restore the pre-upgrade backups into
separate volumes and use those with the previous image.

## The API key

- `FORMLESS_API_KEY_FILE=/run/secrets/…`: the preferred way to provide the
  key. The file must be readable as container UID `10001`, including when
  mounted read-only. A host file with mode `0600` owned by another user
  will usually cause a permission error. With rootless Docker, user
  namespaces, or Docker Desktop, verify the host-to-container UID mapping.
- `FORMLESS_API_KEY=…`: the key itself, in the environment (visible to
  `docker inspect`).
- Named providers: `FORMLESS_API_KEY_<NAME>_FILE` (preferred) or
  `FORMLESS_API_KEY_<NAME>` (e.g. `FORMLESS_API_KEY_LOCAL_FILE`).

When an orchestrator manages the key, mount its secret file under
`/run/secrets` and point `FORMLESS_API_KEY_FILE` at it. Swarm services can
use Docker secrets. Compose services can use `secrets:`. A file-backed
Compose secret still needs source permissions that let container UID
`10001` read it. [Swarm secrets](https://docs.docker.com/engine/swarm/secrets/) ·
[Compose secrets](https://docs.docker.com/compose/how-tos/use-secrets/)

A file that can't be used (including a permission error) is named
in the daemon log and in `formless doctor`, which also says where the key
comes from (env, file, fallback file or keychain) without showing it. A
leading BOM and surrounding whitespace are ignored.

A key supplied through the read-only mount stays in that file. Protect
`/config` backups, which can contain the relay pairing secret and key files
created by Formless setup.

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
  "$image" --profile work daemon serve --stay
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
