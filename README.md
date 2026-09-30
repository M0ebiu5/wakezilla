# Wakezilla 🦖
![Crates.io Version](https://img.shields.io/crates/v/wakezilla) [![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT) [![CI](https://github.com/guibeira/wakezilla/actions/workflows/ci.yml/badge.svg)](https://github.com/guibeira/wakezilla/actions/workflows/ci.yml)
<img width="200" height="159" src="https://github.com/user-attachments/assets/e88f084b-47b8-467b-a5c6-d64327805792" align="left" alt="wakezilla"/>

⚡ Wake-on-LAN made simple → power on your machines remotely whenever needed.

🌐 Reverse proxy → intercepts traffic and wakes the server automatically if it’s offline.

🔌 Automatic shutdown → saves energy by powering down idle machines after configurable thresholds.



## Web interface
<img width="531" height="727" alt="image" src="https://github.com/user-attachments/assets/e9e744c4-35ec-4ca0-8de2-696e447cce7a" />

## Features

- **Wake-on-LAN**: Send magic packets to wake sleeping machines
- **TCP Proxy**: Forward ports to remote machines with automatic WOL
- **Web Interface**: Manage machines, ports, and monitor activity through a web dashboard
- **Automatic Shutdown**: Automatically turn off machines after inactivity periods
- **Network Scanner**: Discover machines on your local network

## Installation

### Install from cargo (recommended)

```bash
cargo install wakezilla
```

### Install via Homebrew

```bash
brew tap guibeira/wakezilla https://github.com/guibeira/wakezilla
brew install wakezilla
```

### Using pre-built docker image

1. **Run the proxy server**:
```bash
docker run -d \
 --name wakezilla-proxy \
 --network host \
 -e WAKEZILLA__SERVER__PROXY_PORT=3000 \
 -v ${PWD}/wakezilla-data:/opt/wakezilla \
 guibeira/wakezilla:latest proxy-server
```
Note:
- `--network host` is required for Wake-on-LAN to work properly.
- add `-v ${PWD}/wakezilla-data:/opt/wakezilla` to save configuration data persistently.

2. **Run the client server**:
```bash
docker run -d \
 --name wakezilla-client \
 -p 3001:3001 \
 guibeira/wakezilla:latest client-server
```

### Install from source

1. **Install Rust**:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   source $HOME/.cargo/env
   ```

2. **Build and Install**:
   ```bash
   git clone git@github.com:guibeira/wakezilla.git
   cd wakezilla
   make install
   ```

3. **Verify Installation**:
   ```bash
   wakezilla --version
   ```
### Run proxy server 

1. **Run the Server**:
   ```bash
    wakezilla proxy-server
   ```
   
   By default, the web interface runs on port 3000.

### Run Client 

1. **Run the Server**:
   ```bash
    wakezilla client-server
   ```
   
   By default, the web interface runs on port 3001.
   You can check the health of the client server by visiting:
   http://<client-ip>:3001/health


## Usage

### Web Interface
Access the web interface at `http://<server-ip>:3000` to:
- Add and manage machines
- Configure port forwards
- View network scan results
- Send WOL packets manually
- Configure automatic shutdown settings

### Adding Machines
1. Navigate to the web interface
2. Click "Add Machine" or use the network scanner
3. Fill in MAC address, IP, and name
4. Configure:
   - Turn-off port (if remote shutdown is needed)
   - Inactivity Period: Time in minutes before automatic shutdown (default: 30 minutes)
   - Port forwards as needed

### Configuring Automatic Shutdown
1. When adding or editing a machine, enable "Can be turned off remotely"
2. Set the "Turn Off Port" (typically 3001 for the client server)
3. Configure the Inactivity Period:
   - Set the number of minutes of inactivity before automatic shutdown
   - The system monitors when the last request was received for each machine
   - If no requests are received within the inactivity period, the machine will be automatically shut down
4. The machine will automatically shut down after the configured inactivity period of no activity

### Port Forwarding
1. Add a machine to the system
2. Configure port forwards for that machine:
   - Local Port: Port on the server to listen on
   - Target Port: Port on the remote machine to forward to
3. When traffic hits the local port, the machine will be woken up if needed and traffic forwarded


### Machine Configuration
Each machine can be configured with:
- MAC Address
- IP Address
- Name and Description
- Turn-off Port (for remote shutdown)
- Inactivity Period: Time in minutes before automatic shutdown (default: 30 minutes)
- Port Forwards:
  - Local Port: Port on the server
  - Target Port: Port on the remote machine
  - Script on new connection / Script when idle (optional, see [Port-forward scripts](#port-forward-scripts))
  - Web link (`http` default, `https`, or off for non-web services like SSH) and optional link path:
    the dashboard's **Services** card links to each forward as "Machine · Service". Links open the
    forwarded port on the host you reached wakezilla by, so opening one wakes the machine and runs
    its connect script

## How It Works

1. **Server Mode**: Runs the web interface and proxy services
2. **Client Mode**: Runs on target machines to enable remote shutdown
3. **WOL Process**: 
   - When traffic hits a configured port, the server sends a WOL packet
   - Waits for the machine to become reachable
   - Forwards traffic once the machine is up
4. **Automatic Shutdown**: 
   - A **single global inactivity monitor** runs continuously, checking all machines every second
   - A machine's `last_request` timestamp is updated by proxied connections that actually carry traffic
   - Merely accepting a connection does **not** count: an uptime check or dashboard that opens a socket
     every minute would otherwise hold the idle timer at zero and the machine would never suspend.
     A connection refreshes the timer each time it transfers `health.activity_min_bytes`
     (default 4096) within one minute, in either direction, so a connection that is merely held
     open (e.g. an idle browser tab's websocket trickling keep-alives) does not keep the machine
     awake; set that to `0` to count every accepted connection, as older versions did
   - The monitor compares the time since `last_request` against the configured `inactivity_period` (in minutes)
   - If no requests are received within the inactivity period, a shutdown signal is sent via HTTP to the client
   - When a machine configuration is updated (e.g., inactivity period changed), the monitor is automatically stopped and restarted with the new settings
   - This ensures only one monitor instance runs at a time, preventing duplicate shutdown signals

### Port-forward scripts

Each port forward can have two shell scripts that the machine's client server runs
(`sh -c` on Linux/macOS, `cmd /C` on Windows):

- **Script on new connection** runs for each new connection, after the machine is awake
  (and woken if needed) and before the connection is forwarded. The connection waits for it,
  so it can e.g. start the service behind the port. Connections arriving while it runs share
  the next run instead of starting one each. Requests matching "Paths that don't wake the
  machine" never run it, even while the machine is up, so a browser tab left open doesn't keep
  re-running it.
- **Script when idle** runs when the machine's inactivity period ends. The machine is turned off
  once the script exits, so its run time is the grace period (e.g. `sleep 60`, or wait for a job
  to finish); activity while it runs cancels the turn-off. On machines that can't be turned off,
  the script still runs. While the machine stays idle, it runs again after every further
  inactivity period, so a script can decline to act (e.g. while a download is running) and
  try again later.

Scripts get `WAKEZILLA_EVENT` (`connect` or `idle`), `WAKEZILLA_LOCAL_PORT` and
`WAKEZILLA_TARGET_PORT` in their environment and are killed after
`server.script_timeout_secs` (default 120). Failures are logged and never block
forwarding or the turn-off.

Running scripts is **off by default**. Start the client with `wakezilla client-server --allow-scripts`
(or `WAKEZILLA__SERVER__ALLOW_SCRIPTS=true`), otherwise it answers 403.

## Security Considerations

- The server should be run on a trusted network
- Access to the web interface should be restricted if exposed to the internet
- The turn-off endpoint on clients should only be accessible from the server
- With `--allow-scripts`, anyone who can reach the client port can run shell commands as the
  client server's user; firewall that port so only the server can reach it

## Development
### Prerequisites
- Rust and Cargo installed
- Clone the repository
- Install dependencies with `make dependencies`

on frontend folder run:
```bash
trunk serve
```
this will initialize the frontend in watch mode on port 8080

on the root folder run:
```bash
cargo watch -x 'run -- proxy-server'
```
this will initialize the backend in watch mode on port 3000


## Troubleshooting

### Common Issues

1. **Machine not waking up**:
   - Verify the MAC address is correct
   - Ensure WOL is enabled in the machine's BIOS/UEFI
   - Check firewall settings on the target machine
   - Verify the target machine supports WOL

2. **Proxy not working**:
   - Check that the target port is correct
   - Verify the machine is reachable after WOL
   - Ensure no firewall is blocking the connection

3. **Automatic shutdown not working**:
   - Verify the turn-off port is configured correctly
   - Ensure the client is running on the target machine
   - Check that the client can receive HTTP requests from the server
   - Verify the inactivity period is configured correctly (in minutes)
   - Check logs to see when the last request was received for the machine
   - Ensure traffic is actually reaching the proxy (requests update the last_request timestamp)
   - If the machine never goes idle, look for a monitoring tool polling a forwarded port on a fixed
     interval. Point it at the Wakezilla server's own port instead, or raise
     `WAKEZILLA__HEALTH__ACTIVITY_MIN_BYTES` above the size of the response it fetches
   - Note that a connection also wakes a sleeping machine, so a poller can wake it right back up

### Logs
Check the terminal output for detailed logs about:
- WOL packets sent
- Connection attempts
- Proxy activity
- Shutdown requests
- Errors and warnings

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make your changes
4. Run `cargo fmt` and `cargo clippy`
5. Commit your changes
6. Push to the branch
7. Create a pull request

## License

This project is licensed under the MIT License - see the LICENSE file for details.
