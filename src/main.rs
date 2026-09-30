use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::net::{IpAddr, Ipv4Addr};
use tracing::{error, info, instrument, warn};

mod api_models;
mod client_server;
mod config;
mod forward;
mod proxy_server;
mod scanner;
mod system;
mod web;
mod wol;

pub use api_models::*;

/// Simple Wake-on-LAN sender + post-WOL reachability check.
#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Send WOL packet via CLI
    Send(SendArgs),
    /// Start proxy server
    ProxyServer(ServeArgs),
    /// Start a client server
    ClientServer(ClientServerArgs),
}

#[derive(Parser, Debug)]
#[command()]
pub struct ServeArgs {
    /// Port to listen on for the web server
    #[arg(
        short,
        long,
        default_value_t = 3000,
        help_heading = "Proxy Server Options"
    )]
    port: u16,
}

#[derive(Parser, Debug)]
#[command()]
pub struct ClientServerArgs {
    /// Port to listen on for the client server
    #[arg(
        short,
        long,
        default_value_t = 3001,
        help_heading = "Client Server Options"
    )]
    port: u16,

    /// Run port-forward scripts sent by the proxy server. Anyone who can
    /// reach the client port can then run shell commands on this machine.
    #[arg(long, help_heading = "Client Server Options")]
    allow_scripts: bool,
}

#[derive(Parser, Debug)]
#[command()]
pub struct SendArgs {
    /// Target MAC address (formats: 00:11:22:33:44:55 or 001122334455, etc.)
    mac: String,

    /// Broadcast IP to use (default 255.255.255.255)
    #[arg(short, long)]
    broadcast: Option<Ipv4Addr>,

    /// UDP port (common: 9 or 7). Default: 9
    #[arg(short, long, default_value_t = 9)]
    port: u16,

    /// Number of times to send the packet (helps with flaky networks)
    #[arg(short = 'n', long, default_value_t = 3)]
    count: u32,

    /// Optional: IP/host to check after WOL (e.g., 192.168.0.200)
    #[arg(long, value_name = "IP")]
    check_ip: Option<IpAddr>,

    /// Optional: TCP port to check on the target host (default 22)
    #[arg(long, default_value_t = 22)]
    check_tcp_port: u16,

    /// Max time to wait (seconds) for the host to come up
    #[arg(long, default_value_t = 90)]
    wait_secs: u64,

    /// Poll interval (milliseconds) between checks
    #[arg(long, default_value_t = 1000)]
    interval_ms: u64,

    /// Per-attempt TCP connect timeout (milliseconds)
    #[arg(long, default_value_t = 700)]
    connect_timeout_ms: u64,
}

#[tokio::main]
#[instrument(name = "wakezilla_main", skip_all)]
async fn main() -> Result<()> {
    let env_filter =
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());

    tracing_subscriber::fmt()
        .with_writer(std::io::stdout)
        .with_env_filter(env_filter)
        .init();

    // Load configuration from environment variables
    let config = config::Config::from_env().unwrap_or_else(|e| {
        warn!(
            "Failed to load configuration from environment: {} - using defaults",
            e
        );
        Default::default()
    });

    info!(
        "Using configuration: server_proxy_port={}, server_client_port={}, wol_default_port={}, machines_db_path={}",
        config.server.proxy_port, config.server.client_port, config.wol.default_port, config.storage.machines_db_path
    );

    let cli = Cli::parse();

    match cli.command {
        Commands::Send(args) => {
            handle_send_command(args, &config).await?;
        }
        Commands::ProxyServer(_args) => {
            if let Err(e) = proxy_server::start(config.clone()).await {
                error!("Proxy server error: {}", e);
                std::process::exit(1);
            }
        }
        Commands::ClientServer(args) => {
            let allow_scripts = args.allow_scripts || config.server.allow_scripts;
            if let Err(e) = client_server::start(config.server.client_port, allow_scripts).await {
                error!("Client server error: {}", e);
                std::process::exit(1);
            }
        }
    }

    Ok(())
}

#[instrument(name = "handle_send_command", skip(args, config))]
async fn handle_send_command(args: SendArgs, config: &config::Config) -> Result<()> {
    info!("Processing WOL send command");

    let mac = wol::parse_mac(&args.mac).context("Failed to parse MAC address")?;
    let bcast = config.get_default_broadcast_addr();

    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let result = handle.block_on(async {
                wol::send_packets(&mac, args.port, args.count, config)
                    .await
                    .context("Failed to send WOL packets")?;

                info!(
                    "Sent WOL magic packet to {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} via {}:{}",
                    mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], bcast, args.port
                );

                // ---- Optional post-WOL reachability check ----
                if let Some(ip) = args.check_ip {
                    info!("Performing post-WOL reachability check for {}", ip);
                    if !wol::check_host(
                        ip,
                        args.check_tcp_port,
                        args.wait_secs,
                        args.interval_ms,
                        args.connect_timeout_ms,
                        config,
                    )
                    .await
                    {
                        anyhow::bail!(
                            "Host {}:{} did not become reachable within {} seconds",
                            ip,
                            args.check_tcp_port,
                            args.wait_secs
                        );
                    }
                    info!("Host {}:{} is now reachable", ip, args.check_tcp_port);
                }

                Ok(())
            });
            match result {
                Ok(_) => Ok(()),
                Err(e) => Err(e),
            }
        }
        Err(_) => Err(anyhow::anyhow!("No runtime context available")),
    }
}
