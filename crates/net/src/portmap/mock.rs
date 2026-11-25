//! Unit `net-portmap` (test support, feature `mock-gateway`). A mock UPnP IGD (SSDP responder plus a minimal
//! SOAP control endpoint for AddPortMapping, DeletePortMapping and GetExternalIPAddress) and a mock PCP / NAT-PMP
//! server, both on loopback, for the CI integration test (L1007). Each mock can be told to refuse, to accept
//! only permanent leases, or to report a CGNAT external address, so every branch of the mapper is covered.
//!
//! The IGD answers SSDP over UDP and HTTP over TCP on the same loopback port, so `root_url` alone tells the
//! mapper where to search (`GatewayOverride::igd_root_url`). crab_nat always talks to UDP port 5351, so the PCP
//! mock binds `127.0.0.1:5351` and only one can run per machine at a time; tests that use it serialise.
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MockBehaviour { #[default] Accept, Refuse, PermanentOnly, CgnatExternal, Silent }

pub struct MockIgd { pub(crate) stop: std::sync::Arc<std::sync::atomic::AtomicBool>, pub root_url: String, pub(crate) state: Mappings, pub(crate) threads: Vec<std::thread::JoinHandle<()>> }
pub struct MockPcp { pub(crate) stop: std::sync::Arc<std::sync::atomic::AtomicBool>, pub addr: SocketAddr, pub(crate) state: Mappings, pub(crate) threads: Vec<std::thread::JoinHandle<()>> }

/// (external port, internal port, lease seconds).
pub(crate) type Mappings = Arc<Mutex<Vec<(u16, u16, u32)>>>;

/// External address a mock reports: TEST-NET-3 (public-looking) or shared CGNAT space.
fn external_ip(b: MockBehaviour) -> std::net::Ipv4Addr {
    if b == MockBehaviour::CgnatExternal { std::net::Ipv4Addr::new(100, 64, 12, 34) } else { std::net::Ipv4Addr::new(203, 0, 113, 7) }
}

fn lock(m: &Mappings) -> std::sync::MutexGuard<'_, Vec<(u16, u16, u32)>> { m.lock().unwrap_or_else(|e| e.into_inner()) }

fn upsert(m: &Mappings, ext: u16, int: u16, lease: u32) {
    let mut v = lock(m);
    v.retain(|e| e.0 != ext);
    v.push((ext, int, lease));
}

/// Dropping a mock stops it and waits until its sockets are closed, so the next mock can bind the same port.
fn shut(stop: &AtomicBool, threads: &mut Vec<std::thread::JoinHandle<()>>) {
    stop.store(true, Ordering::SeqCst);
    for t in threads.drain(..) {
        let _ = t.join();
    }
}
impl Drop for MockIgd {
    fn drop(&mut self) { shut(&self.stop, &mut self.threads); }
}
impl Drop for MockPcp {
    fn drop(&mut self) { shut(&self.stop, &mut self.threads); }
}

impl MockIgd {
    /// Unit: `net-portmap`.
    pub fn start(behaviour: MockBehaviour) -> std::io::Result<MockIgd> {
        // TCP and UDP on one port number: bind TCP first, then UDP on the same port (retry if taken).
        let mut bound = None;
        for _ in 0..32 {
            let tcp = TcpListener::bind("127.0.0.1:0")?;
            let port = tcp.local_addr()?.port();
            if let Ok(udp) = UdpSocket::bind(("127.0.0.1", port)) {
                bound = Some((tcp, udp, port));
                break;
            }
        }
        let (tcp, udp, port) = bound.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrInUse, "no shared tcp/udp port"))?;
        let root_url = format!("http://127.0.0.1:{port}/rootDesc.xml");
        let stop = Arc::new(AtomicBool::new(false));
        let state: Mappings = Arc::default();

        udp.set_read_timeout(Some(Duration::from_millis(20)))?;
        tcp.set_nonblocking(true)?;
        let mut mock = MockIgd { stop: stop.clone(), root_url: root_url.clone(), state: state.clone(), threads: Vec::new() };
        let s = stop.clone();
        mock.threads.push(std::thread::Builder::new().name("mock-igd-ssdp".into()).spawn(move || ssdp_loop(udp, behaviour, &root_url, &s))?);
        mock.threads.push(std::thread::Builder::new().name("mock-igd-http".into()).spawn(move || http_loop(tcp, behaviour, &state, &stop))?);
        Ok(mock)
    }
    /// Mappings currently held: (external port, internal port, lease seconds).
    pub fn mappings(&self) -> Vec<(u16, u16, u32)> { lock(&self.state).clone() }
}

fn ssdp_loop(udp: UdpSocket, behaviour: MockBehaviour, root_url: &str, stop: &AtomicBool) {
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::SeqCst) {
        let Ok((n, from)) = udp.recv_from(&mut buf) else { continue };
        let text = String::from_utf8_lossy(&buf[..n]);
        if behaviour == MockBehaviour::Silent || !text.starts_with("M-SEARCH") {
            continue;
        }
        let st = header(&text, "st").unwrap_or_else(|| "urn:schemas-upnp-org:device:InternetGatewayDevice:1".into());
        let reply = format!(
            "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=120\r\nST: {st}\r\nUSN: uuid:00000000-0000-0000-0000-000000000001::{st}\r\nEXT:\r\nSERVER: mock/1.0 UPnP/1.1 mock/1.0\r\nLOCATION: {root_url}\r\n\r\n"
        );
        let _ = udp.send_to(reply.as_bytes(), from);
    }
}

fn header(text: &str, name: &str) -> Option<String> {
    text.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.trim().to_string()))
}

fn http_loop(tcp: TcpListener, behaviour: MockBehaviour, state: &Mappings, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match tcp.accept() {
            Ok((stream, _)) => {
                let _ = serve(stream, behaviour, state);
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// One HTTP/1.1 request, then close.
fn serve(mut stream: TcpStream, behaviour: MockBehaviour, state: &Mappings) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            return Ok(());
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(i) = find(&data, b"\r\n\r\n") {
            break i + 4;
        }
        if data.len() > 64 * 1024 {
            return Ok(());
        }
    };
    let head = String::from_utf8_lossy(&data[..header_end]).to_string();
    let length = header(&head, "content-length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0).min(1 << 20);
    while data.len() < header_end + length {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&buf[..n]);
    }
    let body = String::from_utf8_lossy(&data[header_end..]).to_string();
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    let (method, path) = (first.next().unwrap_or(""), first.next().unwrap_or(""));
    let (status, xml) = match (method, path) {
        ("GET", "/rootDesc.xml") => (200, ROOT_DESC.to_string()),
        ("GET", "/WANIPCn.xml") => (200, SCPD.to_string()),
        ("POST", "/ctl/IPConn") => {
            let action = header(&head, "soapaction").unwrap_or_default();
            let action = action.trim_matches('"').rsplit('#').next().unwrap_or("").to_string();
            soap(&action, &body, behaviour, state)
        }
        _ => (404, String::new()),
    };
    let reason = match status {
        200 => "OK",
        500 => "Internal Server Error",
        _ => "Not Found",
    };
    let reply = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: text/xml; charset=\"utf-8\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}", xml.len());
    stream.write_all(reply.as_bytes())?;
    stream.flush()
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> { hay.windows(needle.len()).position(|w| w == needle) }

fn tag<'a>(body: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = body.find(&open)? + open.len();
    let end = start + body[start..].find(&close)?;
    Some(body[start..end].trim())
}

fn soap(action: &str, body: &str, behaviour: MockBehaviour, state: &Mappings) -> (u16, String) {
    const ST: &str = "urn:schemas-upnp-org:service:WANIPConnection:1";
    let ok = |inner: String| (200, format!("{ENVELOPE_HEAD}<u:{action}Response xmlns:u=\"{ST}\">{inner}</u:{action}Response>{ENVELOPE_TAIL}"));
    let fault = |code: u16, text: &str| {
        (
            500,
            format!(
                "{ENVELOPE_HEAD}<s:Fault><faultcode>s:Client</faultcode><faultstring>UPnPError</faultstring><detail><UPnPError xmlns=\"urn:schemas-upnp-org:control-1-0\"><errorCode>{code}</errorCode><errorDescription>{text}</errorDescription></UPnPError></detail></s:Fault>{ENVELOPE_TAIL}"
            ),
        )
    };
    match action {
        "GetExternalIPAddress" => ok(format!("<NewExternalIPAddress>{}</NewExternalIPAddress>", external_ip(behaviour))),
        "AddPortMapping" => {
            let ext = tag(body, "NewExternalPort").and_then(|v| v.parse::<u16>().ok());
            let int = tag(body, "NewInternalPort").and_then(|v| v.parse::<u16>().ok());
            let lease = tag(body, "NewLeaseDuration").and_then(|v| v.parse::<u32>().ok()).unwrap_or(0);
            let (Some(ext), Some(int)) = (ext, int) else { return fault(402, "Invalid Args") };
            match behaviour {
                MockBehaviour::Refuse => fault(606, "Action not authorized"),
                MockBehaviour::PermanentOnly if lease != 0 => fault(725, "OnlyPermanentLeasesSupported"),
                _ => {
                    upsert(state, ext, int, lease);
                    ok(String::new())
                }
            }
        }
        "DeletePortMapping" => {
            let Some(ext) = tag(body, "NewExternalPort").and_then(|v| v.parse::<u16>().ok()) else { return fault(402, "Invalid Args") };
            let mut v = lock(state);
            let before = v.len();
            v.retain(|e| e.0 != ext);
            if v.len() == before {
                fault(714, "NoSuchEntryInArray")
            } else {
                ok(String::new())
            }
        }
        _ => fault(401, "Invalid Action"),
    }
}

const ENVELOPE_HEAD: &str = "<?xml version=\"1.0\"?>\n<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body>";
const ENVELOPE_TAIL: &str = "</s:Body></s:Envelope>";

const ROOT_DESC: &str = r#"<?xml version="1.0"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
<specVersion><major>1</major><minor>0</minor></specVersion>
<device>
<deviceType>urn:schemas-upnp-org:device:InternetGatewayDevice:1</deviceType>
<friendlyName>Mock IGD</friendlyName>
<UDN>uuid:00000000-0000-0000-0000-000000000001</UDN>
<deviceList><device>
<deviceType>urn:schemas-upnp-org:device:WANDevice:1</deviceType>
<deviceList><device>
<deviceType>urn:schemas-upnp-org:device:WANConnectionDevice:1</deviceType>
<serviceList><service>
<serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
<serviceId>urn:upnp-org:serviceId:WANIPConn1</serviceId>
<controlURL>/ctl/IPConn</controlURL>
<eventSubURL>/evt/IPConn</eventSubURL>
<SCPDURL>/WANIPCn.xml</SCPDURL>
</service></serviceList>
</device></deviceList>
</device></deviceList>
</device>
</root>"#;

const SCPD: &str = r#"<?xml version="1.0"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
<specVersion><major>1</major><minor>0</minor></specVersion>
<actionList>
<action><name>AddPortMapping</name><argumentList>
<argument><name>NewRemoteHost</name><direction>in</direction></argument>
<argument><name>NewExternalPort</name><direction>in</direction></argument>
<argument><name>NewProtocol</name><direction>in</direction></argument>
<argument><name>NewInternalPort</name><direction>in</direction></argument>
<argument><name>NewInternalClient</name><direction>in</direction></argument>
<argument><name>NewEnabled</name><direction>in</direction></argument>
<argument><name>NewPortMappingDescription</name><direction>in</direction></argument>
<argument><name>NewLeaseDuration</name><direction>in</direction></argument>
</argumentList></action>
<action><name>DeletePortMapping</name><argumentList>
<argument><name>NewRemoteHost</name><direction>in</direction></argument>
<argument><name>NewExternalPort</name><direction>in</direction></argument>
<argument><name>NewProtocol</name><direction>in</direction></argument>
</argumentList></action>
<action><name>GetExternalIPAddress</name><argumentList>
<argument><name>NewExternalIPAddress</name><direction>out</direction></argument>
</argumentList></action>
</actionList>
</scpd>"#;

impl MockPcp {
    /// Unit: `net-portmap`.
    pub fn start(behaviour: MockBehaviour) -> std::io::Result<MockPcp> { MockPcp::start_with(behaviour, false) }

    /// Like [`MockPcp::start`]; with `nat_pmp_only` the mock answers PCP requests with NAT-PMP's
    /// "unsupported version", so the client falls back to NAT-PMP. `PermanentOnly` has no PCP or NAT-PMP
    /// meaning (the server picks lifetimes) and behaves like `Accept`.
    pub fn start_with(behaviour: MockBehaviour, nat_pmp_only: bool) -> std::io::Result<MockPcp> {
        let udp = UdpSocket::bind(("127.0.0.1", crab_nat::GATEWAY_PORT))?;
        let addr = udp.local_addr()?;
        udp.set_read_timeout(Some(Duration::from_millis(20)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let state: Mappings = Arc::default();
        let mut mock = MockPcp { stop: stop.clone(), addr, state: state.clone(), threads: Vec::new() };
        mock.threads.push(std::thread::Builder::new().name("mock-pcp".into()).spawn(move || pcp_loop(udp, behaviour, nat_pmp_only, &state, &stop))?);
        Ok(mock)
    }
    pub fn mappings(&self) -> Vec<(u16, u16, u32)> { lock(&self.state).clone() }
}

/// Gateway epoch: seconds since the mock "booted".
const EPOCH_S: u32 = 1_000;

fn pcp_loop(udp: UdpSocket, behaviour: MockBehaviour, nat_pmp_only: bool, state: &Mappings, stop: &AtomicBool) {
    let mut buf = [0u8; 1100];
    while !stop.load(Ordering::SeqCst) {
        let Ok((n, from)) = udp.recv_from(&mut buf) else { continue };
        if behaviour == MockBehaviour::Silent || n < 2 {
            continue;
        }
        let reply = match buf[0] {
            0 => natpmp_reply(&buf[..n], behaviour, state),
            2 if nat_pmp_only => {
                let mut r = vec![0, 0x80 | buf[1], 0, 1];
                r.extend_from_slice(&EPOCH_S.to_be_bytes());
                Some(r)
            }
            2 => pcp_reply(&buf[..n], behaviour, state),
            _ => None,
        };
        if let Some(r) = reply {
            let _ = udp.send_to(&r, from);
        }
    }
}

fn natpmp_reply(req: &[u8], behaviour: MockBehaviour, state: &Mappings) -> Option<Vec<u8>> {
    let op = req[1];
    let result: u16 = if behaviour == MockBehaviour::Refuse { 2 } else { 0 };
    let mut r = vec![0, 0x80 | op];
    r.extend_from_slice(&result.to_be_bytes());
    r.extend_from_slice(&EPOCH_S.to_be_bytes());
    match op {
        0 => {
            r.extend_from_slice(&external_ip(behaviour).octets());
            Some(r)
        }
        1 | 2 if req.len() >= 12 => {
            let internal = u16::from_be_bytes([req[4], req[5]]);
            let suggested = u16::from_be_bytes([req[6], req[7]]);
            let lifetime = u32::from_be_bytes([req[8], req[9], req[10], req[11]]);
            let (external, lifetime) = if result != 0 {
                (0, 0)
            } else if lifetime == 0 {
                lock(state).retain(|e| e.1 != internal);
                (0, 0)
            } else {
                let external = if suggested == 0 { internal } else { suggested };
                upsert(state, external, internal, lifetime);
                (external, lifetime)
            };
            r.extend_from_slice(&internal.to_be_bytes());
            r.extend_from_slice(&external.to_be_bytes());
            r.extend_from_slice(&lifetime.to_be_bytes());
            Some(r)
        }
        _ => None,
    }
}

fn pcp_reply(req: &[u8], behaviour: MockBehaviour, state: &Mappings) -> Option<Vec<u8>> {
    // Only MAP (opcode 1) with its 36-byte body.
    if req.len() < 60 || req[1] != 1 {
        return None;
    }
    let lifetime = u32::from_be_bytes([req[4], req[5], req[6], req[7]]);
    let nonce = &req[24..36];
    let protocol = req[36];
    let internal = u16::from_be_bytes([req[40], req[41]]);
    let suggested = u16::from_be_bytes([req[42], req[43]]);
    let (result, lifetime, external) = if behaviour == MockBehaviour::Refuse {
        (2u8, 30u32, 0u16) // NOT_AUTHORIZED, retry hint 30 s
    } else if lifetime == 0 {
        lock(state).retain(|e| e.1 != internal);
        (0, 0, 0)
    } else {
        let external = if suggested == 0 { internal } else { suggested };
        upsert(state, external, internal, lifetime);
        (0, lifetime, external)
    };
    let mut r = Vec::with_capacity(60);
    r.extend_from_slice(&[2, 0x81, 0, result]);
    r.extend_from_slice(&lifetime.to_be_bytes());
    r.extend_from_slice(&EPOCH_S.to_be_bytes());
    r.extend_from_slice(&[0; 12]);
    r.extend_from_slice(nonce);
    r.push(protocol);
    r.extend_from_slice(&[0; 3]);
    r.extend_from_slice(&internal.to_be_bytes());
    r.extend_from_slice(&external.to_be_bytes());
    r.extend_from_slice(&external_ip(behaviour).to_ipv6_mapped().octets());
    Some(r)
}
