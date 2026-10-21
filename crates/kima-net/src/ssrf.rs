//! The private-address rule for image downloads (spec §12.6): an image URL comes from a
//! third party, so kima only fetches it from public unicast addresses.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether `ip` is a public unicast address an image download may connect to. False for
/// loopback, private, link-local, CGNAT (`100.64.0.0/10`), multicast, broadcast, unspecified,
/// IPv6 ULA (`fc00::/7`), IPv6 link-local, and the IPv4-mapped forms of those.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4_is_public(v4),
        IpAddr::V6(v6) => v6_is_public(v6),
    }
}

fn v4_is_public(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    let reserved = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        || ip.is_documentation()
        || a == 0 // "this network"
        || (a == 100 && (64..128).contains(&b)) // CGNAT 100.64.0.0/10
        || (a == 192 && b == 0 && c == 0) // IETF protocol assignments
        || (a == 198 && (b == 18 || b == 19)) // benchmarking 198.18.0.0/15
        || a >= 240; // reserved 240.0.0.0/4
    !reserved
}

fn v6_is_public(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    // IPv4-mapped (::ffff:a.b.c.d) and the deprecated IPv4-compatible (::a.b.c.d) forms.
    if let Some(v4) = ip.to_ipv4_mapped() {
        return v4_is_public(v4);
    }
    if seg[..6].iter().all(|&s| s == 0) {
        // `::`, `::1` and `::a.b.c.d`.
        return !ip.is_unspecified() && !ip.is_loopback() && v4_is_public(embedded(seg[6], seg[7]));
    }
    // NAT64 (64:ff9b::/96) carries an IPv4 address in its last 32 bits.
    if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        return v4_is_public(embedded(seg[6], seg[7]));
    }
    // 6to4 (2002::/16) carries one in bits 16-48.
    if seg[0] == 0x2002 {
        return v4_is_public(embedded(seg[1], seg[2]));
    }
    // Only global unicast (2000::/3) is public; that excludes ULA fc00::/7, link-local
    // fe80::/10, multicast ff00::/8 and the rest of the reserved space.
    let global = seg[0] & 0xe000 == 0x2000;
    let documentation = seg[0] == 0x2001 && seg[1] == 0x0db8;
    let teredo = seg[0] == 0x2001 && seg[1] == 0;
    global && !documentation && !teredo
}

fn embedded(hi: u16, lo: u16) -> Ipv4Addr {
    let [a, b] = hi.to_be_bytes();
    let [c, d] = lo.to_be_bytes();
    Ipv4Addr::new(a, b, c, d)
}
