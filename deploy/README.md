# Rpg — Deployment

Three supported deployment methods: Docker, systemd (Linux), launchd (macOS).

## Docker

Build and run the container image:

```bash
# Build
docker build -t rpg:latest -f deploy/Dockerfile .

# Run (connect to host Postgres)
docker run --rm \
  -e PGHOST=host.docker.internal \
  -e PGPORT=5432 \
  -e PGUSER=rpg \
  -e PGDATABASE=postgres \
  rpg:latest --daemon
```

The runtime image is Alpine-based (~15 MiB). The binary runs as a
non-root user (`rpg`, uid 1000). Port 8080 is exposed for the health
check endpoint.

Add a `.dockerignore` at the repo root with at minimum:

```
target/
.git/
```

## systemd (Linux)

```bash
# 1. Install binary
sudo install -m 0755 target/release/rpg /usr/local/bin/rpg

# 2. Create system user
sudo useradd --system --no-create-home rpg

# 3. Create config directory and file
sudo mkdir -p /etc/rpg
sudo cp config.toml /etc/rpg/config.toml

# 4. Optionally set environment variables
sudo tee /etc/rpg/env <<'EOF'
PGHOST=localhost
PGPORT=5432
PGUSER=rpg
PGDATABASE=postgres
EOF

# 5. Install and enable the unit
sudo cp deploy/rpg-daemon.service /etc/systemd/system/rpg-daemon.service
sudo systemctl daemon-reload
sudo systemctl enable --now rpg-daemon

# Status / logs
systemctl status rpg-daemon
journalctl -u rpg-daemon -f
```

## launchd (macOS)

```bash
# 1. Install binary
sudo install -m 0755 target/release/rpg /usr/local/bin/rpg

# 2. Create config directory and file
mkdir -p ~/.config/rpg
cp config.toml ~/.config/rpg/config.toml

# 3. Create log directory
mkdir -p ~/Library/Logs/rpg

# 4. Install and load the agent
cp deploy/com.rpg.daemon.plist ~/Library/LaunchAgents/com.rpg.daemon.plist
launchctl load ~/Library/LaunchAgents/com.rpg.daemon.plist

# Status
launchctl list | grep com.rpg.daemon

# Unload
launchctl unload ~/Library/LaunchAgents/com.rpg.daemon.plist
```

Logs are written to `~/Library/Logs/rpg/stdout.log` and
`~/Library/Logs/rpg/stderr.log`.
