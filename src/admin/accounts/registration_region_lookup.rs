use std::{
    io::{Read, Write},
    net::IpAddr,
    sync::LazyLock,
};

use flate2::read::GzDecoder;
use ip2region::{CachePolicy, Searcher};
use sha2::{Digest, Sha256};

static IPV4_DATABASE: LazyLock<Searcher> = LazyLock::new(|| {
    load_database(
        include_bytes!("../../../data/ip2region/ip2region_v4.xdb.gz"),
        11_122_642,
        "27ecba8c9a6d5e3b280aedbe7ee15c65372b179fbd0dad3845d8c608f2fcace1",
    )
});
static IPV6_DATABASE: LazyLock<Searcher> = LazyLock::new(|| {
    load_database(
        include_bytes!("../../../data/ip2region/ip2region_v6.xdb.gz"),
        37_277_813,
        "939f6b46bd2b8bec3cf7c5ceb8ba782266ae9b1f35b5ba7916700dec0b7506ed",
    )
});

/// Resolve only the registration city, without network requests or exposing the IP.
pub(super) fn city(ip: &str) -> Option<String> {
    let ip = ip.parse::<IpAddr>().ok()?.to_canonical();
    if !is_public(ip) {
        return None;
    }
    let region = match ip {
        IpAddr::V4(ip) => IPV4_DATABASE.search(ip),
        IpAddr::V6(ip) => IPV6_DATABASE.search(ip),
    }
    .expect("embedded registration-region XDB lookup failed");
    city_from_region(&region)
}

fn load_database(compressed: &[u8], size: usize, sha256: &str) -> Searcher {
    let mut data = Vec::with_capacity(size);
    GzDecoder::new(compressed)
        .read_to_end(&mut data)
        .expect("embedded registration-region gzip is corrupt");
    assert_eq!(
        data.len(),
        size,
        "embedded registration-region size mismatch"
    );
    assert_eq!(
        Sha256::digest(&data)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>(),
        sha256,
        "embedded registration-region SHA-256 mismatch"
    );
    // The official reader opens a path. Prime its full-memory cache before the
    // task-owned temporary file is removed, so subsequent queries use memory only.
    let mut file = tempfile::NamedTempFile::new()
        .expect("cannot create registration-region initialization file");
    file.write_all(&data)
        .expect("cannot write registration-region initialization file");
    let searcher = Searcher::new(
        file.path().to_string_lossy().into_owned(),
        CachePolicy::FullMemory,
    )
    .expect("embedded registration-region XDB header is invalid");
    searcher
        .vector_index()
        .expect("cannot initialize registration-region memory cache");
    searcher
}

fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !ip.is_private()
                && !ip.is_loopback()
                && !ip.is_link_local()
                && !ip.is_documentation()
                && !ip.is_multicast()
                && a != 0
                && a < 240
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && matches!(b, 18 | 19))
                && !(a == 192 && ((b == 0 && c == 0) || (b == 88 && c == 99)))
        }
        IpAddr::V6(ip) => {
            let [a, b, ..] = ip.segments();
            // Global unicast only; omit protocol assignments, documentation and 6to4.
            (a & 0xe000) == 0x2000
                && !(a == 0x2001 && (b <= 0x01ff || b == 0x0db8))
                && a != 0x2002
                && !(a == 0x3fff && b < 0x1000)
        }
    }
}

fn city_from_region(region: &str) -> Option<String> {
    if region.is_empty() {
        return None;
    }
    // Pinned ip2region v3 data: country|province|city|ISP|country_code.
    let fields: Vec<_> = region.split('|').collect();
    assert_eq!(
        fields.len(),
        5,
        "embedded registration-region format mismatch"
    );
    if fields[0] == "Reserved" {
        return None;
    }
    let city = fields[2].trim();
    let city = if city.is_empty() || city == "0" {
        match fields[1].trim().trim_end_matches('市') {
            municipality @ ("北京" | "上海" | "天津" | "重庆") => municipality,
            _ => return None,
        }
    } else {
        city.strip_suffix('市').unwrap_or(city)
    };
    (!city.is_empty()).then(|| city.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_public_ipv4_samples_to_city_only() {
        // Public DNS / website addresses; results are tied to data/ip2region/SOURCE.txt.
        for (ip, expected) in [
            ("180.76.76.76", "北京"),
            ("123.125.114.144", "北京"),
            ("180.163.0.1", "上海"),
            ("223.5.5.5", "杭州"),
            ("::ffff:223.5.5.5", "杭州"),
        ] {
            assert_eq!(city(ip).as_deref(), Some(expected), "{ip}");
        }
    }

    #[test]
    fn resolves_public_ipv6_samples() {
        for (ip, expected) in [
            ("2001:250::", "北京"),
            ("2001:250:600f::", "上海"),
            ("2001:da8:e00e::", "杭州"),
            ("2400:3200::1", "杭州"),
        ] {
            assert_eq!(city(ip).as_deref(), Some(expected), "{ip}");
        }
    }

    #[test]
    fn rejects_invalid_private_loopback_and_reserved_addresses() {
        for ip in [
            "",
            "invalid",
            "12345",
            "1.2.3.999",
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "192.0.0.1",
            "192.0.2.1",
            "192.88.99.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:192.168.1.1",
            "fc00::1",
            "fe80::1",
            "ff02::1",
            "2001:2::1",
            "2001:db8::1",
            "2002:7f00:1::",
            "3fff::1",
        ] {
            assert_eq!(city(ip), None, "{ip}");
        }
    }

    #[test]
    fn returns_none_for_public_addresses_without_a_known_city() {
        assert_eq!(city("8.8.8.8"), None);
        assert_eq!(city("3000::1"), None);
        assert_eq!(city_from_region(""), None);
        assert_eq!(city_from_region("中国|浙江省|0|电信|CN"), None);
        assert_eq!(city_from_region("中国|浙江省||电信|CN"), None);
        assert_eq!(city_from_region("中国|0|0|0|CN"), None);
    }

    #[test]
    fn only_municipalities_can_fall_back_to_province() {
        for expected in ["北京", "上海", "天津", "重庆"] {
            assert_eq!(
                city_from_region(&format!("中国|{expected}市|0|0|CN")).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(
            city_from_region("中国|浙江省|杭州市|电信|CN").as_deref(),
            Some("杭州")
        );
        assert_eq!(
            city_from_region("United States|California|0|Google|US"),
            None
        );
    }

    #[test]
    fn embedded_databases_have_the_pinned_checksums() {
        assert!(!IPV4_DATABASE.filepath.is_empty());
        assert!(!IPV6_DATABASE.filepath.is_empty());
        assert!(!std::path::Path::new(&IPV4_DATABASE.filepath).exists());
        assert!(!std::path::Path::new(&IPV6_DATABASE.filepath).exists());
    }

    #[test]
    fn database_final_reserved_vectors_use_the_eof_sentinel() {
        assert_eq!(
            IPV4_DATABASE.search("255.255.255.255").unwrap(),
            "Reserved|Reserved|Reserved|0|0"
        );
        assert_eq!(
            IPV6_DATABASE
                .search("ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff")
                .unwrap(),
            "Reserved|Reserved|Reserved|0|0"
        );
    }

    #[test]
    #[should_panic(expected = "embedded registration-region gzip is corrupt")]
    fn corrupt_embedded_data_is_an_initialization_failure() {
        load_database(b"not gzip", 0, "");
    }
}
