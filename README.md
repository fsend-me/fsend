# fsend

Send a file, get a link. A small self-hosted file sharing service written in Rust,
a simpler take on [transfer.sh](https://github.com/dutchcoders/transfer.sh).

- Upload from the **web** (drag & drop, paste, progress, QR code) or with plain **`curl`**
- Files **delete themselves** after a set time and/or number of downloads
- Optional **upload token**, with per-IP lockout against guessing
- **Image previews**, light and dark themes, works on phones
- Works on **any domain**: links are built from the request, nothing to configure
- One static binary, SQLite, local disk. The Docker image is about 13 MB (amd64 and arm64)

## Quick start

```bash
mkdir fsend && cd fsend
curl -fsSLO https://raw.githubusercontent.com/fsend-me/fsend/main/docker-compose.yml
curl -fsSL https://raw.githubusercontent.com/fsend-me/fsend/main/.env.example -o .env
mkdir -p data              # create it yourself, see "Data folder" below
docker compose up -d
```

Open <http://localhost:8080>. To require a token for uploads, set `FSEND_TOKEN` in `.env`
(for example `openssl rand -hex 24`) and run `docker compose up -d` again.

`.env` is optional. Without it fsend runs with the defaults: anyone can upload, files expire after
3 days, 10 GB per file.

## Configuration

Set these in `.env` (or as environment variables).

| Variable | Default | Description |
|---|---|---|
| `FSEND_TOKEN` | *(none)* | Token required to upload, from the web and the CLI. Unset: anyone can upload. |
| `FSEND_MAX_UPLOAD_SIZE` | `10G` | Max size per file (`K`, `M`, `G`, `T`). |
| `FSEND_DEFAULT_DAYS` | `3` | How long files are kept by default. Decimals allowed (`0.5` = 12 hours). |
| `FSEND_MAX_DAYS` | `30` | Longest retention a client may ask for. |
| `FSEND_ID_LENGTH` | `3` | Length of the random id in `/{id}/{filename}`. Grows automatically on collisions. |
| `FSEND_CLEANUP_INTERVAL` | `600` | Seconds between sweeps that delete expired files from disk. |
| `FSEND_AUTH_MAX_FAILS` | `5` | Wrong tokens allowed per IP before a lockout. |
| `FSEND_AUTH_LOCKOUT_MINUTES` | `15` | Lockout length, also the window failures are counted in. |
| `FSEND_TRUST_PROXY` | `false` | Take the client IP from `X-Forwarded-For`. Enable only behind a reverse proxy. |
| `FSEND_LISTEN` | `0.0.0.0:8080` | Listen address inside the container. |
| `FSEND_DATA_DIR` | `/data` in Docker | Where files and the database are stored. |
| `FSEND_PORT` | `8080` | Host port. *Compose only.* |
| `FSEND_UID` / `FSEND_GID` | `1000` | Host user the container runs as. *Compose only.* |

## Usage

### Web

Drop files anywhere on the page, pick from your computer, or paste from the clipboard. Choose how
long the file lives and how many downloads it allows, then copy the link or show its QR code.
Your recent uploads stay listed in your browser so you can copy or delete them later.

### Command line

Plain `curl`, nothing to install:

```bash
curl --upload-file ./photo.jpg https://your.host/photo.jpg
```

The response is the download link. If the server has `FSEND_TOKEN` set, add
`--oauth2-bearer <token>` (curl has no `--token` flag; this is its built-in equivalent, sending
`Authorization: Bearer <token>`). `-u :<token>` works too.

```bash
# with a token
curl --oauth2-bearer <token> --upload-file ./photo.jpg https://your.host/photo.jpg

# expire after 1 day, allow a single download
curl --oauth2-bearer <token> -H "Max-Days: 1" -H "Max-Downloads: 1" \
     --upload-file ./secret.zip https://your.host/secret.zip

# pipe from stdin
tar czf - ./project | curl --oauth2-bearer <token> --upload-file - https://your.host/project.tar.gz

# delete early (the link is in the X-Url-Delete response header)
curl -X DELETE https://your.host/abc/photo.jpg/<delete-token>
```

| Request header | Meaning |
|---|---|
| `Max-Days: N` | Expire after N days (decimals allowed). |
| `Max-Downloads: N` | Delete after N downloads. |
| `Accept: application/json` | Get a JSON body instead of the plain link. |

| Response header | Meaning |
|---|---|
| `X-Url-Delete` | Link that deletes the file. |
| `X-Expires-At` | Expiry time (unix seconds). |

Forgot the token? The server answers `401` and shows the right command:

```
Error: this server requires an upload token.

  curl --oauth2-bearer <token> --upload-file photo.jpg https://your.host/photo.jpg
```

A wrong token reports how many tries are left. After 5 wrong tokens the IP gets `429` for 15 minutes.

**Optional shell helper:**

```bash
eval "$(curl -fsSL https://your.host/install.sh)"
fsend --token <token> --days 1 --downloads 1 ./report.pdf
```

## How files expire

- **By time.** Once a file passes its expiry it returns `404` right away. A background sweep
  (every `FSEND_CLEANUP_INTERVAL` seconds, and at startup) removes it from disk.
- **By downloads.** With `Max-Downloads`, each download counts when it starts. The file is
  deleted as soon as the last allowed download begins; that download still finishes.
- **What counts as a download.** Downloading the file does (the Download button, or `curl`/`wget`
  on the link). Opening the link in a browser does not: browsers get an info page first. `HEAD`
  requests and resumed downloads don't count either. Image previews are turned off for files with
  a download limit, so the limit can't be bypassed.
- **Deleted by the sender.** The delete link removes the file immediately.

Caveat: some chat apps fetch links to build a preview. If their bot doesn't ask for HTML, that
fetch counts as a download and can use up a one-time link.

## Deployment

### Data folder

Files and the database live in `./data` next to `docker-compose.yml`. The container runs as your
host user (`FSEND_UID:FSEND_GID`, default `1000:1000`), so the folder stays owned by you.

**Create `./data` yourself before the first start.** If it doesn't exist, Docker creates it owned
by root and fsend can't write to it. fsend then stops with a message like:

```
error: cannot write to data directory /data: Permission denied (os error 13)

fsend runs as UID:GID 1000:1000, which cannot write to this folder.
```

Fix it with:

```bash
sudo chown -R "$(id -u):$(id -g)" data
docker compose up -d
```

- Your user isn't `1000:1000`? Add `FSEND_UID=$(id -u)` and `FSEND_GID=$(id -g)` to `.env`.
- SELinux (Fedora, RHEL, Rocky): use `./data:/data:Z` in `docker-compose.yml`.
- Docker Desktop on macOS and Windows usually hides permission problems. Test on Linux.

### Behind a reverse proxy (HTTPS)

fsend speaks plain HTTP. Put a reverse proxy in front for HTTPS, forward the `Host` header and
`X-Forwarded-Proto`, and set `FSEND_TRUST_PROXY=true` so the token lockout sees real client IPs.

Caddy (handles HTTPS and the headers on its own):

```
files.example.com {
    reverse_proxy localhost:8080
}
```

nginx:

```nginx
location / {
    proxy_pass http://127.0.0.1:8080;
    proxy_set_header Host $host;
    proxy_set_header X-Forwarded-Proto $scheme;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    client_max_body_size 0;          # fsend enforces FSEND_MAX_UPLOAD_SIZE itself
    proxy_request_buffering off;     # stream large uploads instead of buffering them
}
```

### Updating and versions

```bash
docker compose pull && docker compose up -d
```

Images are published to GHCR for `linux/amd64` and `linux/arm64`:

| Tag | Follows |
|---|---|
| `latest`, `main` | Every push to `main` |
| `0.1` | Latest `0.1.x` release |
| `0.1.2` | Exactly that release |
| `sha-<short>` | A single commit |

To stay on a release line, change `image:` in `docker-compose.yml` to `ghcr.io/fsend-me/fsend:0.1`.

### Without Compose

```bash
docker run -d --name fsend -p 8080:8080 \
  -e FSEND_TOKEN=change-me \
  -v fsend-data:/data \
  ghcr.io/fsend-me/fsend:latest
```

A named volume like `fsend-data` needs no permission setup. Without `--user`, the image runs as
UID:GID `65532:65532`, so a bind-mounted folder needs `sudo chown -R 65532:65532 ./data`.

## HTTP API

| Method | Path | |
|---|---|---|
| `PUT` | `/{filename}` | Upload. Headers: `Authorization`, `Max-Days`, `Max-Downloads`. |
| `GET` | `/{id}/{filename}` | Browsers get an info page with a preview; other clients get the file. |
| `GET` | `/{id}/{filename}?dl=1` | Always download the file. |
| `GET` | `/{id}/{filename}?inline=1` | Inline image preview (images without a download limit). |
| `GET` | `/{id}/{filename}/qr.svg` | QR code of the link. |
| `DELETE` | `/{id}/{filename}/{delete-token}` | Delete the file. |
| `GET` | `/install.sh` | Shell helper. |
| `GET` | `/api/config` | Limits and whether a token is required. |
| `GET` | `/healthz` | Health check. |

## Security

- Download links need both the id **and** the exact filename, so short ids can't be enumerated alone.
- Uploads stream to disk, and the size limit is enforced while streaming.
- Filenames are sanitized; files are stored on disk by id only.
- Files are always served as `attachment` with `nosniff`. Only raster images (png, jpeg, gif, webp,
  avif, bmp; detected from the content, never SVG) are previewed inline.
- Wrong upload tokens are rate limited per IP, so even short tokens resist guessing.
- Delete tokens are stored hashed. Pages use a strict Content-Security-Policy and load nothing
  from third parties (fonts are self-hosted).
- The container runs as non-root, with a read-only filesystem and no capabilities.

## Development

```bash
# run tests
docker run --rm -v "$PWD":/src -w /src rust:1-alpine sh -c 'apk add -q musl-dev && cargo test'

# build and run from source
mkdir -p data
docker compose -f docker-compose.yml -f docker-compose.dev.yml up -d --build
```

GitHub Actions runs the tests and publishes the multi-arch image on every push to `main` and on
`v*` tags (see `.github/workflows/docker.yml`).

## License

[MIT](LICENSE). The bundled Geist and Geist Mono fonts (`web/fonts/`) are under the
[SIL Open Font License 1.1](web/fonts/OFL.txt).
