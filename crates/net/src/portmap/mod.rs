//! Unit `net-portmap`. Port mapping, double-NAT detection, local and VPN addresses. DESIGN.md L994-1007.
//!
//! 1. UPnP IGD through igd-next 0.18 (tokio backend) and NAT-PMP or PCP through crab_nat 0.8 start in
//!    parallel, each with a 3 s timeout; the first success wins. Both run on a private current-thread tokio
//!    runtime on one background thread, so neither gdclient's main thread nor the sim pool ever blocks.
//! 2. UPnP leases 3,600 s, renewed every 1,800 s; a gateway that only accepts permanent leases gets lease 0
//!    and an explicit delete. NAT-PMP and PCP leases 7,200 s, renewed at half. Mappings are deleted when
//!    hosting ends ([`PortMapper::stop`]); after a crash they expire with the lease.
//! 3. A mapped external address that is private (RFC 1918) or CGNAT (100.64.0.0/10) means double NAT.
//! 4. On failure or double NAT the host panel shows manual forwarding and the mesh VPN option; with Tailscale
//!    or ZeroTier running the panel offers a join code for the VPN address ([`local_addresses`]).
//!
//! No relay server exists. Co-op members map their own port in the background after joining (L1005).
//! crab_nat needs the gateway address: [`default_gateway`] reads it through netdev (MIT).
//! The result goes to the connection log (`quality::LogKind::PortMapping`). CI: `mock.rs` (feature
//! `mock-gateway`, and always in this crate's own tests so `cargo test --workspace` runs the integration test on
//! every CI platform) runs a mock IGD (SSDP plus the SOAP control URL) and a mock PCP server on loopback, and
//! [`GatewayOverride`] points the mapper at them.
#[cfg(any(test, feature = "mock-gateway"))]
pub mod mock;
mod worker;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MapProtocol { UpnpIgd, NatPmp, Pcp }

/// Why one method failed (developer-facing; goes to the connection log).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MapFailure { Timeout, NoGateway, Refused(String), Io(String) }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MappingOutcome {
    Mapped { protocol: MapProtocol, external: SocketAddr, lease_s: u32, permanent: bool },
    /// Mapped, but the external address is private or CGNAT: outside players cannot reach the host.
    DoubleNat { protocol: MapProtocol, external: SocketAddr },
    Failed { upnp: MapFailure, nat_pmp_pcp: MapFailure },
    /// A renewal failed after an earlier success; the old mapping may lapse.
    RenewFailed { protocol: MapProtocol, failure: MapFailure },
}

/// Test hook: talk to these gateways instead of discovering real ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GatewayOverride { pub igd_root_url: Option<String>, pub pcp_gateway: Option<SocketAddr> }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortMapOptions {
    pub internal_port: u16,
    pub timeout_s: u32,
    pub upnp_lease_s: u32,
    pub upnp_renew_s: u32,
    pub natpmp_lease_s: u32,
    /// Mapping description shown in router admin pages.
    pub description: String,
    pub gateway: GatewayOverride,
}
impl PortMapOptions {
    pub fn new(internal_port: u16) -> Self { Self { internal_port, timeout_s: 3, upnp_lease_s: 3_600, upnp_renew_s: 1_800, natpmp_lease_s: 7_200, description: "Black Pennant".into(), gateway: GatewayOverride::default() } }
}

/// Handle to the background mapping thread.
pub struct PortMapper {
    pub(crate) rx: std::sync::mpsc::Receiver<MappingOutcome>,
    pub(crate) stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub(crate) thread: Option<std::thread::JoinHandle<()>>,
    pub(crate) current: Option<MappingOutcome>,
}

/// How long [`PortMapper::stop`] waits for the deletes (L998: mappings are deleted when hosting ends).
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

impl Drop for PortMapper {
    /// Dropping the handle without `stop` still ends renewals and deletes the mappings in the background.
    fn drop(&mut self) { self.stop.store(true, std::sync::atomic::Ordering::SeqCst); }
}

impl PortMapper {
    /// Start mapping in the background; renewals continue until [`PortMapper::stop`]. Unit: `net-portmap`.
    pub fn start(options: PortMapOptions) -> PortMapper {
        let (tx, rx) = std::sync::mpsc::channel();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let fallback = tx.clone();
        let thread = std::thread::Builder::new().name("bp-portmap".into()).spawn(move || worker::run(options, tx, flag));
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                let f = MapFailure::Io(format!("thread: {e}"));
                let _ = fallback.send(MappingOutcome::Failed { upnp: f.clone(), nat_pmp_pcp: f });
                None
            }
        };
        PortMapper { rx, stop, thread, current: None }
    }
    /// New outcomes since the last poll (first result, renewals that fail). Unit: `net-portmap`.
    pub fn poll(&mut self) -> Option<MappingOutcome> {
        let outcome = self.rx.try_recv().ok()?;
        // A failed renewal leaves the last good mapping current: it may still work until its lease ends.
        if !matches!(outcome, MappingOutcome::RenewFailed { .. }) {
            self.current = Some(outcome.clone());
        }
        Some(outcome)
    }
    pub fn current(&self) -> Option<&MappingOutcome> { self.current.as_ref() }
    /// External address to put in the join code, if mapped without double NAT.
    pub fn external(&self) -> Option<SocketAddr> { match &self.current { Some(MappingOutcome::Mapped { external, .. }) => Some(*external), _ => None } }
    /// Delete mappings and end the thread, waiting at most 2 s. Unit: `net-portmap`.
    pub fn stop(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        let Some(thread) = self.thread.take() else { return };
        let start = std::time::Instant::now();
        while !thread.is_finished() {
            if start.elapsed() >= STOP_WAIT {
                // Leave it to finish its deletes on its own; it exits as soon as they return.
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = thread.join();
    }
}

/// RFC 1918 or 100.64.0.0/10 (IPv4); unique-local or link-local (IPv6). Unit: `net-portmap`.
pub fn is_double_nat(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            // RFC 1918, shared CGNAT space, and link-local (a WAN side with no address yet is just as unreachable).
            v4.is_private() || (o[0] == 100 && (o[1] & 0xC0) == 64) || v4.is_link_local()
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_double_nat(IpAddr::V4(v4)),
            None => (v6.segments()[0] & 0xfe00) == 0xfc00 || (v6.segments()[0] & 0xffc0) == 0xfe80,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VpnKind { Tailscale, ZeroTier }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VpnAddr { pub kind: VpnKind, pub ip: IpAddr }

/// Addresses the host panel shows: LAN addresses of up interfaces (for LAN joins) and mesh VPN addresses
/// (Tailscale: interface names `tailscale*`/`utun*` with an address in 100.64.0.0/10; ZeroTier: names
/// `zt*` or descriptions containing "ZeroTier").
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalAddrs { pub lan: Vec<IpAddr>, pub vpn: Vec<VpnAddr> }

/// Enumerate interfaces through netdev. Unit: `net-portmap`.
pub fn local_addresses() -> LocalAddrs {
    let mut lan: Vec<(bool, IpAddr)> = Vec::new();
    let mut vpn = Vec::new();
    for iface in netdev::get_interfaces() {
        if !iface.is_up() || iface.is_loopback() {
            continue;
        }
        let v4: Vec<IpAddr> = iface.ipv4.iter().map(|n| IpAddr::V4(n.addr())).filter(|ip| !ip.is_unspecified()).collect();
        let v6: Vec<IpAddr> = iface.ipv6.iter().map(|n| IpAddr::V6(n.addr())).filter(|ip| !is_link_local(*ip) && !ip.is_loopback()).collect();
        match vpn_kind(&iface.name, iface.friendly_name.as_deref(), iface.description.as_deref(), &v4) {
            Some(kind) => {
                for ip in v4.iter().chain(v6.iter()) {
                    if !is_link_local(*ip) {
                        vpn.push(VpnAddr { kind, ip: *ip });
                    }
                }
            }
            None => {
                for ip in v4.iter().chain(v6.iter()) {
                    if !is_link_local(*ip) {
                        lan.push((iface.default, *ip));
                    }
                }
            }
        }
    }
    // The default route's interface first, IPv4 before IPv6.
    lan.sort_by_key(|(default, ip)| (!*default, ip.is_ipv6(), *ip));
    let mut out: Vec<IpAddr> = Vec::with_capacity(lan.len());
    for (_, ip) in lan {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    vpn.dedup();
    LocalAddrs { lan: out, vpn }
}

/// Mesh VPN interface detection (see [`LocalAddrs`]).
fn vpn_kind(name: &str, friendly: Option<&str>, description: Option<&str>, v4: &[IpAddr]) -> Option<VpnKind> {
    let name_l = name.to_ascii_lowercase();
    let text = |needle: &str| {
        name_l.contains(needle)
            || friendly.is_some_and(|f| f.to_ascii_lowercase().contains(needle))
            || description.is_some_and(|d| d.to_ascii_lowercase().contains(needle))
    };
    let cgnat = v4.iter().any(|ip| matches!(ip, IpAddr::V4(a) if a.octets()[0] == 100 && (a.octets()[1] & 0xC0) == 64));
    if (name_l.starts_with("tailscale") || name_l.starts_with("utun") || text("tailscale")) && cgnat {
        return Some(VpnKind::Tailscale);
    }
    if name_l.starts_with("zt") || text("zerotier") {
        return Some(VpnKind::ZeroTier);
    }
    None
}

fn is_link_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_link_local(),
        IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
    }
}
/// The default gateway through netdev. Unit: `net-portmap`.
pub fn default_gateway() -> Option<IpAddr> {
    let gw = netdev::get_default_gateway().ok()?;
    gw.ipv4
        .iter()
        .find(|ip| !ip.is_unspecified())
        .map(|ip| IpAddr::V4(*ip))
        .or_else(|| gw.ipv6.iter().find(|ip| !ip.is_unspecified()).map(|ip| IpAddr::V6(*ip)))
}
