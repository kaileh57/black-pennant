use super::*;
use std::net::{Ipv4Addr, Ipv6Addr};

#[test]
fn double_nat_ranges() {
    for ip in ["10.1.2.3", "172.16.0.1", "172.31.255.255", "192.168.1.1", "100.64.0.1", "100.127.255.254", "169.254.3.4"] {
        assert!(is_double_nat(ip.parse().unwrap()), "{ip}");
    }
    for ip in ["203.0.113.7", "8.8.8.8", "100.63.255.255", "100.128.0.0", "172.32.0.1", "1.1.1.1"] {
        assert!(!is_double_nat(ip.parse().unwrap()), "{ip}");
    }
    assert!(is_double_nat("fd00::1".parse().unwrap()));
    assert!(is_double_nat("fe80::1".parse().unwrap()));
    assert!(!is_double_nat("2001:db8::1".parse().unwrap()));
    assert!(is_double_nat(IpAddr::V6(Ipv4Addr::new(192, 168, 0, 9).to_ipv6_mapped())));
}

#[test]
fn vpn_interfaces_are_recognised() {
    let ts = [IpAddr::V4(Ipv4Addr::new(100, 101, 102, 103))];
    let lan = [IpAddr::V4(Ipv4Addr::new(192, 168, 1, 5))];
    assert_eq!(vpn_kind("utun4", None, None, &ts), Some(VpnKind::Tailscale));
    assert_eq!(vpn_kind("tailscale0", None, None, &ts), Some(VpnKind::Tailscale));
    assert_eq!(vpn_kind("{GUID}", Some("Tailscale"), None, &ts), Some(VpnKind::Tailscale));
    // A utun without a CGNAT address is some other tunnel.
    assert_eq!(vpn_kind("utun0", None, None, &[]), None);
    assert_eq!(vpn_kind("ztabcdef12", None, None, &lan), Some(VpnKind::ZeroTier));
    assert_eq!(vpn_kind("{GUID}", None, Some("ZeroTier Virtual Port"), &lan), Some(VpnKind::ZeroTier));
    assert_eq!(vpn_kind("en0", Some("Wi-Fi"), None, &lan), None);
}

#[test]
fn local_addresses_never_lists_loopback_or_link_local() {
    let a = local_addresses();
    for ip in a.lan.iter().chain(a.vpn.iter().map(|v| &v.ip)) {
        assert!(!ip.is_loopback() && !ip.is_unspecified() && !is_link_local(*ip), "{ip}");
    }
    let _ = default_gateway();
    let _ = Ipv6Addr::LOCALHOST;
}

#[test]
fn options_carry_the_spec_numbers() {
    let o = PortMapOptions::new(47610);
    assert_eq!((o.timeout_s, o.upnp_lease_s, o.upnp_renew_s, o.natpmp_lease_s), (3, 3_600, 1_800, 7_200));
}

/// The loopback integration test (L1007).
mod mock_gateway {
    use super::super::mock::{MockBehaviour, MockIgd, MockPcp};
    use super::super::*;
    use std::time::{Duration, Instant};

    /// The PCP mock owns UDP 5351; tests that touch it take turns.
    static PCP_PORT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn first_outcome(m: &mut PortMapper, within: Duration) -> MappingOutcome {
        let start = Instant::now();
        loop {
            if let Some(o) = m.poll() {
                return o;
            }
            assert!(start.elapsed() < within, "no outcome within {within:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn options(port: u16, igd: Option<&MockIgd>, pcp: Option<&MockPcp>) -> PortMapOptions {
        let mut o = PortMapOptions::new(port);
        o.timeout_s = 1;
        o.gateway = GatewayOverride { igd_root_url: igd.map(|g| g.root_url.clone()), pcp_gateway: pcp.map(|p| p.addr) };
        o
    }

    /// The PCP mock, or `None` (the section is skipped, with a note) when something else on the machine already
    /// holds UDP 5351, so a busy CI runner cannot turn the suite red.
    fn pcp_mock(behaviour: MockBehaviour, nat_pmp_only: bool) -> Option<MockPcp> {
        match MockPcp::start_with(behaviour, nat_pmp_only) {
            Ok(m) => Some(m),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied) => {
                eprintln!("skipping PCP mock section: UDP 5351 unavailable ({e})");
                None
            }
            Err(e) => panic!("PCP mock: {e}"),
        }
    }

    #[test]
    fn a_failed_renewal_is_reported_once_and_keeps_the_mapping_current() {
        let igd = MockIgd::start(MockBehaviour::Accept).unwrap();
        let mut o = options(47621, Some(&igd), None);
        o.upnp_renew_s = 1;
        let mut m = PortMapper::start(o);
        assert!(matches!(first_outcome(&mut m, Duration::from_secs(3)), MappingOutcome::Mapped { .. }));
        // The gateway goes away; the next renewal fails.
        drop(igd);
        match first_outcome(&mut m, Duration::from_secs(4)) {
            MappingOutcome::RenewFailed { protocol: MapProtocol::UpnpIgd, .. } => {}
            o => panic!("{o:?}"),
        }
        assert_eq!(m.external(), Some("203.0.113.7:47621".parse().unwrap()));
        m.stop();
    }

    fn wait_until(mut f: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn upnp_maps_renews_nothing_early_and_deletes_on_stop() {
        let igd = MockIgd::start(MockBehaviour::Accept).unwrap();
        let mut m = PortMapper::start(options(47611, Some(&igd), None));
        let o = first_outcome(&mut m, Duration::from_secs(3));
        assert_eq!(o, MappingOutcome::Mapped { protocol: MapProtocol::UpnpIgd, external: "203.0.113.7:47611".parse().unwrap(), lease_s: 3_600, permanent: false });
        assert_eq!(m.external(), Some("203.0.113.7:47611".parse().unwrap()));
        assert_eq!(igd.mappings(), vec![(47611, 47611, 3_600)]);
        m.stop();
        assert!(wait_until(|| igd.mappings().is_empty()));
    }

    #[test]
    fn permanent_only_gateway_gets_lease_zero_and_an_explicit_delete() {
        let igd = MockIgd::start(MockBehaviour::PermanentOnly).unwrap();
        let mut m = PortMapper::start(options(47612, Some(&igd), None));
        let o = first_outcome(&mut m, Duration::from_secs(3));
        assert!(matches!(o, MappingOutcome::Mapped { protocol: MapProtocol::UpnpIgd, lease_s: 0, permanent: true, .. }), "{o:?}");
        assert_eq!(igd.mappings(), vec![(47612, 47612, 0)]);
        m.stop();
        assert!(wait_until(|| igd.mappings().is_empty()));
    }

    #[test]
    fn cgnat_external_is_double_nat() {
        let igd = MockIgd::start(MockBehaviour::CgnatExternal).unwrap();
        let mut m = PortMapper::start(options(47613, Some(&igd), None));
        let o = first_outcome(&mut m, Duration::from_secs(3));
        assert_eq!(o, MappingOutcome::DoubleNat { protocol: MapProtocol::UpnpIgd, external: "100.64.12.34:47613".parse().unwrap() });
        assert_eq!(m.external(), None);
        m.stop();
    }

    #[test]
    fn refusal_and_silence_fail_both_methods() {
        let igd = MockIgd::start(MockBehaviour::Refuse).unwrap();
        let mut m = PortMapper::start(options(47614, Some(&igd), None));
        match first_outcome(&mut m, Duration::from_secs(3)) {
            MappingOutcome::Failed { upnp: MapFailure::Refused(_), nat_pmp_pcp: MapFailure::NoGateway } => {}
            o => panic!("{o:?}"),
        }
        m.stop();

        let silent = MockIgd::start(MockBehaviour::Silent).unwrap();
        let mut m = PortMapper::start(options(47615, Some(&silent), None));
        match first_outcome(&mut m, Duration::from_secs(3)) {
            MappingOutcome::Failed { upnp: MapFailure::NoGateway | MapFailure::Timeout, .. } => {}
            o => panic!("{o:?}"),
        }
        m.stop();
    }

    #[test]
    fn pcp_and_nat_pmp_map_and_delete() {
        let _turn = PCP_PORT.lock().unwrap_or_else(|e| e.into_inner());
        {
            let Some(pcp) = pcp_mock(MockBehaviour::Accept, false) else { return };
            let mut m = PortMapper::start(options(47616, None, Some(&pcp)));
            let o = first_outcome(&mut m, Duration::from_secs(3));
            assert_eq!(o, MappingOutcome::Mapped { protocol: MapProtocol::Pcp, external: "203.0.113.7:47616".parse().unwrap(), lease_s: 7_200, permanent: false });
            assert_eq!(pcp.mappings(), vec![(47616, 47616, 7_200)]);
            m.stop();
            assert!(wait_until(|| pcp.mappings().is_empty()));
        }
        {
            let Some(pcp) = pcp_mock(MockBehaviour::Accept, true) else { return };
            let mut m = PortMapper::start(options(47617, None, Some(&pcp)));
            let o = first_outcome(&mut m, Duration::from_secs(3));
            assert_eq!(o, MappingOutcome::Mapped { protocol: MapProtocol::NatPmp, external: "203.0.113.7:47617".parse().unwrap(), lease_s: 7_200, permanent: false });
            m.stop();
            assert!(wait_until(|| pcp.mappings().is_empty()));
        }
        {
            let Some(pcp) = pcp_mock(MockBehaviour::CgnatExternal, false) else { return };
            let mut m = PortMapper::start(options(47618, None, Some(&pcp)));
            assert!(matches!(first_outcome(&mut m, Duration::from_secs(3)), MappingOutcome::DoubleNat { protocol: MapProtocol::Pcp, .. }));
            m.stop();
        }
        {
            let Some(pcp) = pcp_mock(MockBehaviour::Refuse, false) else { return };
            let mut m = PortMapper::start(options(47619, None, Some(&pcp)));
            match first_outcome(&mut m, Duration::from_secs(3)) {
                MappingOutcome::Failed { upnp: MapFailure::NoGateway, nat_pmp_pcp: MapFailure::Refused(_) } => {}
                o => panic!("{o:?}"),
            }
            m.stop();
        }
    }

    #[test]
    fn both_gateways_answer_and_both_mappings_are_deleted() {
        let _turn = PCP_PORT.lock().unwrap_or_else(|e| e.into_inner());
        let igd = MockIgd::start(MockBehaviour::Accept).unwrap();
        let Some(pcp) = pcp_mock(MockBehaviour::Accept, false) else { return };
        let mut m = PortMapper::start(options(47620, Some(&igd), Some(&pcp)));
        let o = first_outcome(&mut m, Duration::from_secs(3));
        assert!(matches!(o, MappingOutcome::Mapped { .. }), "{o:?}");
        assert!(wait_until(|| igd.mappings().len() == 1 && pcp.mappings().len() == 1));
        m.stop();
        let ok = wait_until(|| igd.mappings().is_empty() && pcp.mappings().is_empty());
        assert!(ok, "igd {:?} pcp {:?}", igd.mappings(), pcp.mappings());
    }
}
