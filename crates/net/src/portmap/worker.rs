//! Unit `net-portmap`. The background mapping thread: a private current-thread tokio runtime that runs UPnP IGD
//! and PCP / NAT-PMP side by side, reports the first success, keeps every mapping it made alive on its own
//! lease schedule, and deletes them all when the handle stops (DESIGN.md L996-998).
//!
//! Both methods are attempted in parallel; the first success is reported. If both succeed (a router that speaks
//! both protocols shares one rule table, so deleting the loser could delete the winner's rule), both are
//! renewed and both are deleted at the end. A double-NAT winner is replaced by a later clean mapping.
use super::{is_double_nat, GatewayOverride, MapFailure, MapProtocol, MappingOutcome, PortMapOptions};
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU16;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;
use tokio::time::Instant;

/// Retry a failed renewal after this long (well inside the 3,600 s and 7,200 s leases).
const RENEW_RETRY: Duration = Duration::from_secs(60);
/// How often the worker looks at the stop flag.
const STOP_POLL: Duration = Duration::from_millis(25);
/// Time budget for deleting every mapping once stopped (the handle waits 2 s in all).
const DELETE_BUDGET: Duration = Duration::from_millis(1_500);
/// Part of that budget an attempt still running at the stop may use to finish, so its mapping can be deleted.
const DRAIN_BUDGET: Duration = Duration::from_millis(750);

pub(super) fn run(options: PortMapOptions, tx: Sender<MappingOutcome>, stop: Arc<AtomicBool>) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            let f = MapFailure::Io(format!("runtime: {e}"));
            let _ = tx.send(MappingOutcome::Failed { upnp: f.clone(), nat_pmp_pcp: f });
            return;
        }
    };
    rt.block_on(mapper(options, tx, stop));
}

/// One live mapping and its renewal state.
struct Held {
    kind: HeldKind,
    external: SocketAddr,
    next_renew: Option<Instant>,
    failing: bool,
}

enum HeldKind {
    Upnp { gateway: igd_next::aio::Gateway<igd_next::aio::tokio::Tokio>, local: SocketAddr, external_port: u16, lease_s: u32 },
    Pcp(crab_nat::PortMapping),
}

impl Held {
    fn protocol(&self) -> MapProtocol {
        match &self.kind {
            HeldKind::Upnp { .. } => MapProtocol::UpnpIgd,
            HeldKind::Pcp(m) => match m.mapping_type() {
                crab_nat::PortMappingType::NatPmp => MapProtocol::NatPmp,
                crab_nat::PortMappingType::Pcp { .. } => MapProtocol::Pcp,
            },
        }
    }
    fn lease_s(&self) -> u32 {
        match &self.kind {
            HeldKind::Upnp { lease_s, .. } => *lease_s,
            HeldKind::Pcp(m) => m.lifetime(),
        }
    }
    fn outcome(&self) -> MappingOutcome {
        let protocol = self.protocol();
        if is_double_nat(self.external.ip()) {
            MappingOutcome::DoubleNat { protocol, external: self.external }
        } else {
            let lease_s = self.lease_s();
            MappingOutcome::Mapped { protocol, external: self.external, lease_s, permanent: lease_s == 0 }
        }
    }
    fn clean(&self) -> bool { !is_double_nat(self.external.ip()) }
    fn schedule(&mut self, options: &PortMapOptions) {
        let now = Instant::now();
        self.next_renew = match &self.kind {
            // Permanent leases need no renewal; they are deleted explicitly at the end.
            HeldKind::Upnp { lease_s: 0, .. } => None,
            HeldKind::Upnp { .. } => Some(now + Duration::from_secs(u64::from(options.upnp_renew_s.max(1)))),
            HeldKind::Pcp(m) => Some(now + Duration::from_secs(u64::from((m.lifetime() / 2).max(1)))),
        };
    }
}

type Attempt<'a> = Pin<Box<dyn Future<Output = Result<Held, MapFailure>> + 'a>>;

/// Two attempts polled together, watching the stop flag.
struct Race<'a> {
    upnp: Option<Attempt<'a>>,
    pcp: Option<Attempt<'a>>,
    upnp_res: Option<Result<Held, MapFailure>>,
    pcp_res: Option<Result<Held, MapFailure>>,
    /// Which finished first with a success: true for UPnP.
    first_ok_upnp: Option<bool>,
}

impl<'a> Race<'a> {
    /// Poll until `done(self)` holds or stop is requested. Returns false if stopped.
    async fn until(&mut self, stop: &AtomicBool, done: impl Fn(&Race<'a>) -> bool) -> bool {
        let mut tick = Box::pin(tokio::time::sleep(STOP_POLL));
        std::future::poll_fn(|cx| {
            if let Some(f) = self.upnp.as_mut() {
                if let Poll::Ready(r) = f.as_mut().poll(cx) {
                    if r.is_ok() && self.first_ok_upnp.is_none() {
                        self.first_ok_upnp = Some(true);
                    }
                    self.upnp_res = Some(r);
                    self.upnp = None;
                }
            }
            if let Some(f) = self.pcp.as_mut() {
                if let Poll::Ready(r) = f.as_mut().poll(cx) {
                    if r.is_ok() && self.first_ok_upnp.is_none() {
                        self.first_ok_upnp = Some(false);
                    }
                    self.pcp_res = Some(r);
                    self.pcp = None;
                }
            }
            if done(self) {
                return Poll::Ready(true);
            }
            if stop.load(Ordering::SeqCst) {
                return Poll::Ready(false);
            }
            while tick.as_mut().poll(cx).is_ready() {
                tick.as_mut().reset(Instant::now() + STOP_POLL);
            }
            Poll::Pending
        })
        .await
    }
    fn finished(&self) -> bool { self.upnp.is_none() && self.pcp.is_none() }
}

async fn mapper(options: PortMapOptions, tx: Sender<MappingOutcome>, stop: Arc<AtomicBool>) {
    let timeout = Duration::from_secs(u64::from(options.timeout_s.max(1)));
    let mut held: Vec<Held> = Vec::new();
    let mut winner: Option<usize> = None;
    let mut stopped_at: Option<Instant> = None;
    {
        let mut race = Race {
            upnp: Some(Box::pin(upnp_attempt(&options, timeout))),
            pcp: Some(Box::pin(pcp_attempt(&options, timeout))),
            upnp_res: None,
            pcp_res: None,
            first_ok_upnp: None,
        };
        let running = race.until(&stop, |r| r.first_ok_upnp.is_some() || r.finished()).await;
        if running {
            // Report the first success now; the slower method may still finish.
            let first_upnp = race.first_ok_upnp;
            let first = take_ok(&mut race, first_upnp);
            if let Some(mut h) = first {
                h.schedule(&options);
                let _ = tx.send(h.outcome());
                held.push(h);
                winner = Some(0);
                if race.until(&stop, |r| r.finished()).await {
                    let other_upnp = first_upnp.map(|u| !u);
                    let other = take_ok(&mut race, other_upnp);
                    if let Some(mut h) = other {
                        h.schedule(&options);
                        // A clean mapping beats a double-NAT one.
                        if !held[0].clean() && h.clean() {
                            let _ = tx.send(h.outcome());
                            winner = Some(held.len());
                        }
                        held.push(h);
                    }
                }
            } else {
                let upnp = failure_of(race.upnp_res.take());
                let nat_pmp_pcp = failure_of(race.pcp_res.take());
                let _ = tx.send(MappingOutcome::Failed { upnp, nat_pmp_pcp });
                return;
            }
        }
        if stop.load(Ordering::SeqCst) {
            // Hosting ended while an attempt was still talking to its gateway. Give it a moment to finish, so a
            // mapping it has already made is deleted with the rest instead of lingering until its lease ends.
            stopped_at = Some(Instant::now());
            let never = AtomicBool::new(false);
            let _ = tokio::time::timeout(DRAIN_BUDGET, race.until(&never, |r| r.finished())).await;
            for result in [race.upnp_res.take(), race.pcp_res.take()] {
                if let Some(Ok(h)) = result {
                    held.push(h);
                }
            }
        }
        // Dropping the race abandons any attempt still running; a mapping it made expires with its lease.
    }

    // Renewals until stopped.
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(STOP_POLL).await;
        let now = Instant::now();
        for (i, h) in held.iter_mut().enumerate() {
            let Some(due) = h.next_renew else { continue };
            if now < due || stop.load(Ordering::SeqCst) {
                continue;
            }
            let result = tokio::time::timeout(timeout, renew(h, &options)).await.unwrap_or(Err(MapFailure::Timeout));
            let is_winner = winner == Some(i);
            match result {
                Ok(external) => {
                    let changed = external != h.external;
                    h.external = external;
                    let recovered = h.failing;
                    h.failing = false;
                    h.schedule(&options);
                    if is_winner && (changed || recovered) {
                        let _ = tx.send(h.outcome());
                    }
                }
                Err(failure) => {
                    if is_winner && !h.failing {
                        let _ = tx.send(MappingOutcome::RenewFailed { protocol: h.protocol(), failure });
                    }
                    h.failing = true;
                    h.next_renew = Some(Instant::now() + RENEW_RETRY);
                }
            }
        }
    }

    // Delete everything we hold, inside the budget counted from the stop.
    let deadline = stopped_at.unwrap_or_else(Instant::now) + DELETE_BUDGET;
    for h in held {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let _ = tokio::time::timeout(left, delete(h)).await;
    }
}

fn take_ok(race: &mut Race<'_>, upnp: Option<bool>) -> Option<Held> {
    match upnp {
        Some(true) => race.upnp_res.take().and_then(Result::ok),
        Some(false) => race.pcp_res.take().and_then(Result::ok),
        None => None,
    }
}

fn failure_of(r: Option<Result<Held, MapFailure>>) -> MapFailure {
    match r {
        Some(Err(f)) => f,
        Some(Ok(_)) | None => MapFailure::Timeout,
    }
}

fn overridden(g: &GatewayOverride) -> bool { g.igd_root_url.is_some() || g.pcp_gateway.is_some() }

/// The local address the OS would use to reach `target` (no packet is sent).
fn local_ip_toward(target: SocketAddr) -> Result<IpAddr, MapFailure> {
    let any: SocketAddr = if target.is_ipv4() { (std::net::Ipv4Addr::UNSPECIFIED, 0).into() } else { (std::net::Ipv6Addr::UNSPECIFIED, 0).into() };
    let s = std::net::UdpSocket::bind(any).map_err(|e| MapFailure::Io(e.to_string()))?;
    s.connect(target).map_err(|e| MapFailure::Io(e.to_string()))?;
    Ok(s.local_addr().map_err(|e| MapFailure::Io(e.to_string()))?.ip())
}

/// `http://host:port/...` to the host and port; SSDP for an overridden gateway goes to that UDP port.
fn url_authority(url: &str) -> Option<SocketAddr> {
    let rest = url.strip_prefix("http://")?;
    let authority = rest.split('/').next()?;
    authority.parse::<SocketAddr>().ok().or_else(|| authority.parse::<IpAddr>().ok().map(|ip| SocketAddr::new(ip, 80)))
}

fn request_failure(e: &igd_next::RequestError) -> MapFailure {
    match e {
        igd_next::RequestError::IoError(_) | igd_next::RequestError::HyperError(_) | igd_next::RequestError::HyperClientError(_) => MapFailure::Io(e.to_string()),
        _ => MapFailure::Refused(e.to_string()),
    }
}

fn add_failure(e: igd_next::AddPortError) -> MapFailure {
    match e {
        igd_next::AddPortError::RequestError(r) => request_failure(&r),
        other => MapFailure::Refused(other.to_string()),
    }
}

fn add_any_failure(e: igd_next::AddAnyPortError) -> MapFailure {
    match e {
        igd_next::AddAnyPortError::RequestError(r) => request_failure(&r),
        other => MapFailure::Refused(other.to_string()),
    }
}

fn external_ip_failure(e: igd_next::GetExternalIpError) -> MapFailure {
    match e {
        igd_next::GetExternalIpError::RequestError(r) => request_failure(&r),
        other => MapFailure::Refused(other.to_string()),
    }
}

fn search_failure(e: igd_next::SearchError) -> MapFailure {
    match e {
        igd_next::SearchError::NoResponseWithinTimeout => MapFailure::NoGateway,
        igd_next::SearchError::IoError(e) => MapFailure::Io(e.to_string()),
        other => MapFailure::Refused(other.to_string()),
    }
}

async fn upnp_attempt(options: &PortMapOptions, timeout: Duration) -> Result<Held, MapFailure> {
    let mut search = igd_next::SearchOptions::default();
    // Leave the last half second of the budget for the control requests, so silence reads as no gateway.
    let search_time = timeout.saturating_sub(Duration::from_millis(500)).max(Duration::from_millis(500));
    search.timeout = Some(search_time);
    search.single_search_timeout = Some(search_time);
    if overridden(&options.gateway) {
        let Some(target) = options.gateway.igd_root_url.as_deref().and_then(url_authority) else { return Err(MapFailure::NoGateway) };
        search.broadcast_address = target;
        search.bind_addr = match (target.is_ipv4(), target.ip().is_loopback()) {
            (true, true) => (std::net::Ipv4Addr::LOCALHOST, 0).into(),
            (true, false) => (std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
            (false, true) => (std::net::Ipv6Addr::LOCALHOST, 0).into(),
            (false, false) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        search.allowed_gateway_ips = vec![target.ip()];
    }
    let work = async {
        let gateway = igd_next::aio::tokio::search_gateway(search).await.map_err(search_failure)?;
        let local = SocketAddr::new(local_ip_toward(gateway.addr)?, options.internal_port);
        let (external_port, lease_s) = upnp_add(&gateway, local, options).await?;
        let ip = match gateway.get_external_ip().await {
            Ok(ip) if !ip.is_unspecified() => ip,
            other => {
                let _ = gateway.remove_port(igd_next::PortMappingProtocol::UDP, external_port).await;
                return Err(match other {
                    Err(e) => external_ip_failure(e),
                    Ok(_) => MapFailure::Refused("gateway has no external address".into()),
                });
            }
        };
        Ok(Held { kind: HeldKind::Upnp { gateway, local, external_port, lease_s }, external: SocketAddr::new(ip, external_port), next_renew: None, failing: false })
    };
    tokio::time::timeout(timeout, work).await.unwrap_or(Err(MapFailure::Timeout))
}

/// Map the same external port as the internal one; fall back to any port if it is taken, and to a permanent
/// lease (0) when the gateway only accepts those. Returns the external port and the lease used.
async fn upnp_add(gateway: &igd_next::aio::Gateway<igd_next::aio::tokio::Tokio>, local: SocketAddr, o: &PortMapOptions) -> Result<(u16, u32), MapFailure> {
    use igd_next::{AddAnyPortError, AddPortError, PortMappingProtocol::UDP};
    if o.internal_port == 0 {
        return Err(MapFailure::Refused("internal port 0".into()));
    }
    let mut lease = o.upnp_lease_s;
    for _ in 0..2 {
        match gateway.add_port(UDP, o.internal_port, local, lease, &o.description).await {
            Ok(()) => return Ok((o.internal_port, lease)),
            Err(AddPortError::OnlyPermanentLeasesSupported) if lease != 0 => lease = 0,
            Err(AddPortError::PortInUse) | Err(AddPortError::SamePortValuesRequired) => {
                for _ in 0..2 {
                    match gateway.add_any_port(UDP, local, lease, &o.description).await {
                        Ok(port) => return Ok((port, lease)),
                        Err(AddAnyPortError::OnlyPermanentLeasesSupported) if lease != 0 => lease = 0,
                        Err(e) => return Err(add_any_failure(e)),
                    }
                }
                return Err(MapFailure::Refused("no lease accepted".into()));
            }
            Err(e) => return Err(add_failure(e)),
        }
    }
    Err(MapFailure::Refused("no lease accepted".into()))
}

fn crab_failure(e: crab_nat::MappingFailure) -> MapFailure {
    match e {
        crab_nat::MappingFailure::Pcp(crab_nat::pcp::Failure::Timeout) | crab_nat::MappingFailure::NatPmp(crab_nat::natpmp::Failure::Timeout) => MapFailure::Timeout,
        crab_nat::MappingFailure::Pcp(crab_nat::pcp::Failure::Socket(e)) | crab_nat::MappingFailure::NatPmp(crab_nat::natpmp::Failure::Socket(e)) => MapFailure::Io(e.to_string()),
        other => MapFailure::Refused(other.to_string()),
    }
}

fn natpmp_failure(e: crab_nat::natpmp::Failure) -> MapFailure { crab_failure(crab_nat::MappingFailure::NatPmp(e)) }

/// Retries inside the 3 s budget: 250 ms, then 500 ms, then 1 s twice.
fn crab_timeouts() -> crab_nat::TimeoutConfig {
    crab_nat::TimeoutConfig { initial_timeout: Duration::from_millis(250), max_retries: 3, max_retry_timeout: Some(Duration::from_secs(1)) }
}

async fn pcp_attempt(options: &PortMapOptions, timeout: Duration) -> Result<Held, MapFailure> {
    let gateway_ip = if overridden(&options.gateway) {
        options.gateway.pcp_gateway.map(|a| a.ip()).ok_or(MapFailure::NoGateway)?
    } else {
        super::default_gateway().ok_or(MapFailure::NoGateway)?
    };
    let port = NonZeroU16::new(options.internal_port).ok_or_else(|| MapFailure::Refused("internal port 0".into()))?;
    let client = local_ip_toward(SocketAddr::new(gateway_ip, crab_nat::GATEWAY_PORT))?;
    let mapping_options = crab_nat::PortMappingOptions {
        external_port: Some(port),
        lifetime_seconds: Some(options.natpmp_lease_s.max(1)),
        timeout_config: Some(crab_timeouts()),
    };
    let work = async {
        let mapping = crab_nat::PortMapping::new(gateway_ip.into(), client, crab_nat::InternetProtocol::Udp, port, mapping_options).await.map_err(crab_failure)?;
        let ip = external_of(&mapping).await;
        match ip {
            Ok(ip) if !ip.is_unspecified() => {
                let external = SocketAddr::new(ip, mapping.external_port().get());
                Ok(Held { kind: HeldKind::Pcp(mapping), external, next_renew: None, failing: false })
            }
            other => {
                let _ = mapping.try_drop().await;
                Err(match other {
                    Err(f) => f,
                    Ok(_) => MapFailure::Refused("gateway has no external address".into()),
                })
            }
        }
    };
    tokio::time::timeout(timeout, work).await.unwrap_or(Err(MapFailure::Timeout))
}

/// PCP answers carry the external address; NAT-PMP needs a separate request.
async fn external_of(mapping: &crab_nat::PortMapping) -> Result<IpAddr, MapFailure> {
    match mapping.mapping_type() {
        crab_nat::PortMappingType::Pcp { external_ip, .. } => Ok(external_ip),
        crab_nat::PortMappingType::NatPmp => {
            crab_nat::natpmp::external_address(mapping.gateway(), Some(crab_timeouts())).await.map(IpAddr::V4).map_err(natpmp_failure)
        }
    }
}

/// Renew one mapping; returns its (possibly new) external address.
async fn renew(h: &mut Held, o: &PortMapOptions) -> Result<SocketAddr, MapFailure> {
    match &mut h.kind {
        HeldKind::Upnp { gateway, local, external_port, lease_s } => {
            gateway.add_port(igd_next::PortMappingProtocol::UDP, *external_port, *local, *lease_s, &o.description).await.map_err(add_failure)?;
            let ip = gateway.get_external_ip().await.map_err(external_ip_failure)?;
            Ok(SocketAddr::new(ip, *external_port))
        }
        HeldKind::Pcp(m) => {
            m.renew().await.map_err(crab_failure)?;
            let ip = external_of(m).await?;
            Ok(SocketAddr::new(ip, m.external_port().get()))
        }
    }
}

async fn delete(h: Held) {
    match h.kind {
        HeldKind::Upnp { gateway, external_port, .. } => {
            let _ = gateway.remove_port(igd_next::PortMappingProtocol::UDP, external_port).await;
        }
        HeldKind::Pcp(m) => {
            let _ = m.try_drop().await;
        }
    }
}
