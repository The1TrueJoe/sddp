//! End-to-end: an announcer and a discovery/listener in one process, talking
//! over multicast loopback on the real SDDP group and port.
//!
//! Multicast loopback is not available everywhere (containers, some CI
//! runners, machines without a network). The test first checks for it on a
//! private port and skips with a message when it is missing, rather than
//! failing on something the crate cannot control.

use std::net::{Ipv4Addr, UdpSocket};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sddp::{Announcer, Discovery, Event, Listener, MULTICAST_ADDR};

/// Can this machine send a multicast datagram to itself on `interface`?
fn multicast_loopback_works(interface: Ipv4Addr) -> bool {
    let probe = || -> std::io::Result<bool> {
        let rx = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
        let port = rx.local_addr()?.port();
        rx.join_multicast_v4(&MULTICAST_ADDR, &interface)?;
        rx.set_read_timeout(Some(Duration::from_secs(1)))?;
        let tx = socket_on(interface)?;
        tx.send_to(b"sddp-loopback-probe", (MULTICAST_ADDR, port))?;
        let mut buf = [0u8; 64];
        let (n, _) = rx.recv_from(&mut buf)?;
        Ok(&buf[..n] == b"sddp-loopback-probe")
    };
    probe().unwrap_or(false)
}

fn socket_on(interface: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind((interface, 0))?;
    s.set_multicast_loop_v4(true)?;
    Ok(s)
}

fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{}-{}", std::process::id(), nanos % 1_000_000_000)
}

#[tokio::test]
async fn announcer_is_found_and_says_goodbye() {
    let interface = match sddp::default_interface() {
        Ok(i) => i,
        Err(e) => {
            eprintln!("skipping: no usable network interface ({e})");
            return;
        }
    };
    if !multicast_loopback_works(interface) {
        eprintln!("skipping: multicast loopback is not available on {interface}");
        return;
    }

    let id = unique();
    let host = format!("loopback-test-{id}");
    // A type nothing else on the network has, so real devices can't interfere.
    let device_type = format!("example:loopback-{id}");

    let announcer = Announcer::builder(host.clone(), device_type.clone())
        .manufacturer("Example")
        .model("LB-1")
        .proxies(["one", "two"])
        .header("X-Test", "yes")
        .interface(interface)
        .start()
        .await
        .expect("announcer starts");

    // Exact-type search.
    let found = Discovery::new()
        .target(device_type.clone())
        .timeout(Duration::from_millis(1500))
        .interface(interface)
        .discover()
        .await
        .expect("discovery runs");
    assert_eq!(found.len(), 1, "expected only our device, got {found:#?}");
    let d = &found[0];
    assert_eq!(d.host, host);
    assert_eq!(d.device_type, device_type);
    assert_eq!(d.address, interface);
    assert_eq!(d.port, sddp::PORT);
    assert_eq!(d.manufacturer.as_deref(), Some("Example"));
    assert_eq!(d.model.as_deref(), Some("LB-1"));
    assert_eq!(d.proxies, ["one", "two"]);
    assert_eq!(d.max_age, Some(1800));
    assert_eq!(d.headers.get("x-test"), Some("yes"));

    // Wildcard search finds it too (alongside any real devices).
    let all = Discovery::new()
        .timeout(Duration::from_millis(1500))
        .interface(interface)
        .discover()
        .await
        .expect("discovery runs");
    assert!(
        all.iter().any(|d| d.host == host),
        "wildcard search missed our device"
    );

    // A prefix pattern is not a wildcard.
    let none = Discovery::new()
        .target("example:*")
        .timeout(Duration::from_millis(800))
        .interface(interface)
        .discover()
        .await
        .expect("discovery runs");
    assert!(none.iter().all(|d| d.host != host));

    // Shutdown is heard as OFFLINE.
    let mut listener = Listener::bind(Some(interface))
        .await
        .expect("listener binds");
    announcer.shutdown().await.expect("offline sent");
    let offline = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Event::Offline(d) = listener.next().await.expect("listener receives") {
                if d.host == host {
                    return d;
                }
            }
        }
    })
    .await
    .expect("NOTIFY OFFLINE heard within 3s");
    assert_eq!(offline.device_type, device_type);
}
