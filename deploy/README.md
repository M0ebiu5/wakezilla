# Deployment

Files that install and run wakezilla on the two machines of this setup. They
are copies of what is installed; after changing one here, install it again
(commands below).

| Machine | Role | Address |
|---|---|---|
| **monschein** | wakezilla server (web UI, forwarders) and a client for its own services | 192.168.4.221 |
| **White** | GPU machine, woken and suspended by wakezilla | 192.168.4.120 |

```
deploy/
├── deploy-wakezilla.sh          build output -> /usr/local/bin/wakezilla on monschein
├── monschein/
│   ├── qbittorrent-ctl          -> /usr/local/bin/  (connect/idle scripts of the qBittorrent forward)
│   └── systemd/                 -> /etc/systemd/system/
│       ├── wakezilla.service                        proxy server
│       ├── wakezilla.service.d/script-timeout.conf  scripts may run 300s (GPU switch)
│       └── wakezilla-client.service                 client, scripts enabled (--allow-scripts)
└── white/
    ├── gpu-switch               -> /usr/local/bin/  (connect scripts of the webui and comfyui forwards)
    └── systemd/                 -> /etc/systemd/system/
        ├── wakezilla-client.service
        └── wakezilla-client.service.d/allow-scripts.conf  scripts enabled
```

## Deploying a new wakezilla build

On monschein:

```sh
cargo build --release -p wakezilla
sudo bash deploy/deploy-wakezilla.sh
```

The script backs up the old binary to `/usr/local/bin/wakezilla.bak-<timestamp>`,
restarts `wakezilla`, and restarts `wakezilla-client`, which runs the same binary.

White only needs a new binary when the client side (`src/client_server.rs`) changes:

```sh
scp target/release/wakezilla m00n@192.168.4.120:/tmp/wakezilla.new
ssh m00n@192.168.4.120 'sudo install -m 755 /tmp/wakezilla.new /usr/local/bin/wakezilla \
  && sudo systemctl restart wakezilla-client'
```

## Installing the scripts and units

```sh
# monschein
sudo install -m 755 deploy/monschein/qbittorrent-ctl /usr/local/bin/
sudo cp -r deploy/monschein/systemd/. /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl restart wakezilla wakezilla-client

# White
scp -r deploy/white m00n@192.168.4.120:/tmp/wz-deploy
ssh m00n@192.168.4.120 'sudo install -m 755 /tmp/wz-deploy/gpu-switch /usr/local/bin/ \
  && sudo cp -r /tmp/wz-deploy/systemd/. /etc/systemd/system/ \
  && sudo systemctl daemon-reload && sudo systemctl restart wakezilla-client \
  && rm -r /tmp/wz-deploy'
```

## The scripts

- **`gpu-switch comfy|webui [--dry-run]`** (White): gives the GPU to ComfyUI or to
  the vLLM server behind Open WebUI. Stops every other container that reserves the
  GPU (vLLM through its `qwen38-single` systemd unit), kills other GPU compute
  processes, then starts the requested app. Waits until ComfyUI answers; doesn't
  wait for vLLM, which loads for minutes. Returns in ~0.1s when the GPU already
  belongs to the app.
- **`qbittorrent-ctl start|idle`** (monschein): `start` starts the qBittorrent
  container if needed and waits for its WebUI. `idle` stops it unless a torrent is
  downloading, stalled, queued, checking or fetching metadata; seeding alone doesn't
  keep it running. It queries the WebUI API from inside the container, which needs
  *bypass authentication for clients on localhost* enabled in qBittorrent.

## Outside this folder

- `~/projects/qbittorrent/docker-compose.yml` on monschein publishes the qBittorrent
  WebUI on port **18080**; wakezilla's forward listens on 8080, so browsers keep using
  `http://<server>:8080`.
- Port 3001 (wakezilla client, running as root with scripts enabled) is open to the
  LAN on both machines. Anything that can reach it can run commands as root.
