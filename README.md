# fsend

A small, self-hosted file sharing service written in Rust — a simpler take on
[transfer.sh](https://github.com/dutchcoders/transfer.sh).

- Upload from the **web** (drag & drop, paste, progress, QR code) or the **CLI** (`curl`)
- Files **expire automatically** (by time and/or number of downloads)
- **Image previews** on the download page
- One static binary, SQLite metadata, local disk storage
- Works on **any domain** — links are built from the request's `Host` / `X-Forwarded-*` headers

## Run with Docker Compose

```bash
curl -fsSLO https://raw.githubusercontent.com/fsend-me/fsend/main/docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/fsend-me/fsend/main/.env.example -o .env
# optional: set FSEND_TOKEN in .env (e.g. `openssl rand -hex 24`)
docker compose up -d
open http://localhost:8080
```

This pulls the prebuilt image `ghcr.io/fsend-me/fsend:latest`. The `.env` file is optional;
without it the defaults apply (no token, 3-day expiry, 10 GB limit). Update with
`docker compose pull && docker compose up -d`.

Data lives in `./data` next to the compose file (`/data` in the container).
The container runs as non-root with a read-only filesystem.

### Data folder permissions

fsend runs as your host user (`FSEND_UID` / `FSEND_GID`, default `1000:1000`) so files in `./data`
stay owned by you. Docker creates a missing `./data` owned by root, so the compose file starts a
tiny one-shot `fsend-init` container first that `chown`s `./data` to that user and exits; fsend
starts only after it succeeds. Nothing to do by hand, unless your IDs are not 1000:

```bash
echo "FSEND_UID=$(id -u)" >> .env
echo "FSEND_GID=$(id -g)" >> .env
docker compose up -d
```

Running the image without compose and without `--user`, it runs as UID:GID `65532:65532`; give a
bind-mounted folder to it with `sudo chown -R 65532:65532 ./data`. Named volumes
(`-v fsend-data:/data`) need nothing: Docker copies the image's ownership into a new volume.
On SELinux hosts (Fedora/RHEL) add `:Z` to both bind mounts (`./data:/data:Z`).

If the permissions are wrong, fsend exits at startup with a message like:

```
error: cannot write to data directory /data: Permission denied (os error 13)

fsend runs as UID:GID 1000:1000, which cannot write to this folder.
...
  sudo chown -R 1000:1000 ./data
```

For HTTPS, put any reverse proxy in front and forward `Host` and `X-Forwarded-Proto`.

### Prebuilt image

Multi-arch images (`linux/amd64`, `linux/arm64`) are published to GHCR on every push to `main`
and for every `v*` tag:

```bash
docker run -d --name fsend -p 8080:8080 \
  -e FSEND_TOKEN=change-me \
  -v fsend-data:/data \
  ghcr.io/fsend-me/fsend:latest
```

Tags: `latest` (main), `main`, `sha-<short>`, and `X.Y.Z` / `X.Y` for releases.
Pin a release instead of `latest` by changing `image:` in `docker-compose.yml` (e.g. `ghcr.io/fsend-me/fsend:0.1`).

## Configuration

| Variable | Default | Description |
|---|---|---|
| `FSEND_TOKEN` | *(none)* | Token required to upload (web + CLI). If unset, anyone can upload. |
| `FSEND_ID_LENGTH` | `3` | Length of the random id in `/{id}/{filename}`. Grows automatically on collisions. |
| `FSEND_MAX_UPLOAD_SIZE` | `10G` | Max size per file (`K`/`M`/`G`/`T`). |
| `FSEND_DEFAULT_DAYS` | `3` | Default retention. Decimals allowed. |
| `FSEND_MAX_DAYS` | `30` | Max retention a client can request. |
| `FSEND_CLEANUP_INTERVAL` | `600` | Seconds between expiry sweeps. |
| `FSEND_AUTH_MAX_FAILS` | `5` | Wrong tokens allowed per IP before a lockout. |
| `FSEND_AUTH_LOCKOUT_MINUTES` | `15` | Lockout length (also the window failures are counted in). |
| `FSEND_TRUST_PROXY` | `false` | Use the client IP from `X-Forwarded-For` (last hop). Enable only behind a reverse proxy. |
| `FSEND_LISTEN` | `0.0.0.0:8080` | Listen address. |
| `FSEND_DATA_DIR` | `./data` (`/data` in Docker) | Storage directory. |
| `FSEND_PORT` | `8080` | Host port (compose only). |
| `FSEND_UID` / `FSEND_GID` | `1000` | Host user the container runs as (compose only). |

## CLI

Plain `curl`, nothing to install. If the server has `FSEND_TOKEN` set, pass it with
`--oauth2-bearer <token>` (curl has no `--token` flag; this one sends `Authorization: Bearer <token>`).
`-u :<token>` also works.

```bash
# upload
curl --oauth2-bearer <token> --upload-file ./photo.jpg https://your.host/photo.jpg

# expire after 1 day, allow a single download
curl --oauth2-bearer <token> -H "Max-Days: 1" -H "Max-Downloads: 1" \
     --upload-file ./secret.zip https://your.host/secret.zip

# pipe
tar czf - dir | curl --oauth2-bearer <token> --upload-file - https://your.host/dir.tar.gz

# delete (URL comes from the X-Url-Delete response header)
curl -X DELETE https://your.host/abc/photo.jpg/<delete-token>
```

Without a token (when one is required) the server answers `401` with a hint:

```
Error: this server requires an upload token.

  curl --oauth2-bearer <token> --upload-file photo.jpg https://your.host/photo.jpg
```

Optional shell helper: `fsend [--token T] [--days D] [--downloads N] <file>`

```bash
eval "$(curl -fsSL https://your.host/install.sh)"
fsend --token <token> --days 1 ./report.pdf
```

The upload response body is the download URL. Response headers:
`X-Url-Delete` (delete link) and `X-Expires-At` (unix time).
Send `Accept: application/json` to get a JSON body instead.

## HTTP API

| Method | Path | |
|---|---|---|
| `PUT` | `/{filename}` | Upload (headers: `Authorization`, `Max-Days`, `Max-Downloads`) |
| `GET` | `/{id}/{filename}` | Browser: info page with preview. CLI: raw file. |
| `GET` | `/{id}/{filename}?dl=1` | Force download |
| `GET` | `/{id}/{filename}?inline=1` | Inline image preview |
| `GET` | `/{id}/{filename}/qr.svg` | QR code of the link |
| `DELETE` | `/{id}/{filename}/{token}` | Delete |
| `GET` | `/install.sh`, `/api/config`, `/healthz` | Helpers |

## Security notes

- Download URLs need both the id **and** the exact filename, so short ids are not enumerable on their own.
- Uploads stream to disk; size limits are enforced while streaming.
- Filenames are sanitized; files are stored on disk by id only.
- Files are always served as `attachment` with `nosniff`. Only raster images
  (png/jpeg/gif/webp/avif/bmp, detected by content, never SVG) are previewed inline.
- Previews are disabled for files with a download limit, so the limit can't be bypassed.
- Wrong upload tokens are rate limited per IP (5 tries, then a 15-minute lockout, HTTP 429), so even short tokens resist brute force.
- Delete tokens are stored hashed. Strict CSP and security headers on all pages.

## Development

```bash
docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build   # build from source
docker run --rm -v "$PWD":/src -w /src rust:1-alpine sh -c 'apk add musl-dev && cargo test'
```

## License

[MIT](LICENSE). The bundled Geist and Geist Mono fonts (`web/fonts/`) are licensed under the
[SIL Open Font License 1.1](web/fonts/OFL.txt).
