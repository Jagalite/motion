//! DNS observations for direct delivery and diagnostics. Production queries the
//! operating system's configured resolvers through `hickory-resolver`; tests
//! and isolated qualification use an explicit in-memory fixture. Answers are
//! reduced to the distinctions delivery policy needs: records, no records,
//! nonexistent domain, Null MX (RFC 7505) and temporary failure.
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{Arc, OnceLock},
    time::Duration,
};

use hickory_resolver::{
    TokioResolver,
    proto::rr::{RData, RecordType},
};

pub const MAX_EXCHANGES: usize = 10;
pub const MAX_ADDRESSES: usize = 4;
const QUERY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mx {
    /// Exchanges sorted by preference, then name. Never includes the root.
    Exchanges(Vec<(u16, String)>),
    /// RFC 7505: the domain explicitly accepts no mail.
    Null,
    /// The domain exists without MX records: RFC 5321 §5.1 implicit MX.
    None,
    NxDomain,
    Temporary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer<T> {
    Records(T),
    NoRecords,
    NxDomain,
    Temporary,
}

/// Lower-case, without a trailing root dot; ASCII LDH labels only.
pub fn host_name(name: &str) -> Option<String> {
    let name = name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase();
    (!name.is_empty()
        && name.len() <= 253
        && name.parse::<IpAddr>().is_err()
        && name.split('.').all(|label| {
            (1..=63).contains(&label.len())
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }))
    .then_some(name)
}

/// Bound and order MX answers. A single root exchange is Null MX; a root
/// exchange mixed with others is a misconfiguration and is ignored.
pub fn exchanges(records: Vec<(u16, String)>) -> Mx {
    if records.len() == 1 && matches!(records[0].1.as_str(), "" | ".") {
        return Mx::Null;
    }
    let mut out: Vec<(u16, String)> = records
        .into_iter()
        .filter_map(|(preference, name)| host_name(&name).map(|name| (preference, name)))
        .collect();
    out.sort();
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|(_, name)| seen.insert(name.clone()));
    out.truncate(MAX_EXCHANGES);
    if out.is_empty() {
        Mx::Temporary
    } else {
        Mx::Exchanges(out)
    }
}

/// In-memory DNS for tests and isolated qualification. Absent names are
/// NXDOMAIN; listed names without a record type have no records of that type.
#[derive(Clone, Debug, Default)]
pub struct Fixture {
    pub mx: BTreeMap<String, Answer<Vec<(u16, String)>>>,
    pub addresses: BTreeMap<String, Answer<Vec<IpAddr>>>,
    pub txt: BTreeMap<String, Answer<Vec<String>>>,
    pub ptr: BTreeMap<IpAddr, Answer<Vec<String>>>,
}

#[derive(Clone)]
pub enum Resolver {
    System(Arc<OnceLock<Option<TokioResolver>>>),
    Fixture(Arc<Fixture>),
}

impl Default for Resolver {
    fn default() -> Self {
        Self::System(Arc::new(OnceLock::new()))
    }
}

fn system(cell: &OnceLock<Option<TokioResolver>>) -> Option<&TokioResolver> {
    cell.get_or_init(|| {
        TokioResolver::builder_tokio()
            .ok()
            .and_then(|builder| builder.build().ok())
    })
    .as_ref()
}

fn classify<T>(result: Result<T, hickory_resolver::net::NetError>) -> Answer<T> {
    match result {
        Ok(value) => Answer::Records(value),
        Err(error) if error.is_nx_domain() => Answer::NxDomain,
        Err(error) if error.is_no_records_found() => Answer::NoRecords,
        Err(_) => Answer::Temporary,
    }
}

fn fixture<T: Clone>(map: &BTreeMap<String, Answer<T>>, name: &str) -> Answer<T> {
    map.get(name).cloned().unwrap_or(Answer::NxDomain)
}

impl Resolver {
    pub async fn mx(&self, domain: &str) -> Mx {
        let answer = match self {
            Self::Fixture(f) => fixture(&f.mx, domain),
            Self::System(cell) => {
                let Some(resolver) = system(cell) else {
                    return Mx::Temporary;
                };
                let lookup =
                    tokio::time::timeout(QUERY_TIMEOUT, resolver.mx_lookup(format!("{domain}.")))
                        .await
                        .unwrap_or(Err(hickory_resolver::net::NetError::Timeout));
                match classify(lookup) {
                    Answer::Records(lookup) => Answer::Records(
                        lookup
                            .answers()
                            .iter()
                            .filter_map(|record| match &record.data {
                                RData::MX(mx) => Some((mx.preference, mx.exchange.to_ascii())),
                                _ => None,
                            })
                            .collect(),
                    ),
                    Answer::NoRecords => Answer::NoRecords,
                    Answer::NxDomain => Answer::NxDomain,
                    Answer::Temporary => Answer::Temporary,
                }
            }
        };
        match answer {
            Answer::Records(records) if records.is_empty() => Mx::None,
            Answer::Records(records) => exchanges(records),
            Answer::NoRecords => Mx::None,
            Answer::NxDomain => Mx::NxDomain,
            Answer::Temporary => Mx::Temporary,
        }
    }

    /// A and AAAA addresses for `host`, IPv4 first, bounded.
    pub async fn addresses(&self, host: &str) -> Answer<Vec<IpAddr>> {
        let answer = match self {
            Self::Fixture(f) => fixture(&f.addresses, host),
            Self::System(cell) => {
                let Some(resolver) = system(cell) else {
                    return Answer::Temporary;
                };
                let lookup =
                    tokio::time::timeout(QUERY_TIMEOUT, resolver.lookup_ip(format!("{host}.")))
                        .await
                        .unwrap_or(Err(hickory_resolver::net::NetError::Timeout));
                match classify(lookup) {
                    Answer::Records(lookup) => Answer::Records(lookup.iter().collect()),
                    Answer::NoRecords => Answer::NoRecords,
                    Answer::NxDomain => Answer::NxDomain,
                    Answer::Temporary => Answer::Temporary,
                }
            }
        };
        match answer {
            Answer::Records(mut addresses) => {
                addresses.sort_by_key(|address| address.is_ipv6());
                addresses.dedup();
                addresses.truncate(MAX_ADDRESSES);
                if addresses.is_empty() {
                    Answer::NoRecords
                } else {
                    Answer::Records(addresses)
                }
            }
            other => other,
        }
    }

    /// TXT strings for `name`, each record's character-strings concatenated.
    pub async fn txt(&self, name: &str) -> Answer<Vec<String>> {
        match self {
            Self::Fixture(f) => fixture(&f.txt, name),
            Self::System(cell) => {
                let Some(resolver) = system(cell) else {
                    return Answer::Temporary;
                };
                let lookup = tokio::time::timeout(
                    QUERY_TIMEOUT,
                    resolver.lookup(format!("{name}."), RecordType::TXT),
                )
                .await
                .unwrap_or(Err(hickory_resolver::net::NetError::Timeout));
                match classify(lookup) {
                    Answer::Records(lookup) => Answer::Records(
                        lookup
                            .answers()
                            .iter()
                            .filter_map(|record| match &record.data {
                                RData::TXT(txt) => Some(
                                    txt.txt_data
                                        .iter()
                                        .map(|part| String::from_utf8_lossy(part).into_owned())
                                        .collect::<String>(),
                                ),
                                _ => None,
                            })
                            .take(32)
                            .collect(),
                    ),
                    Answer::NoRecords => Answer::NoRecords,
                    Answer::NxDomain => Answer::NxDomain,
                    Answer::Temporary => Answer::Temporary,
                }
            }
        }
    }

    pub async fn ptr(&self, address: IpAddr) -> Answer<Vec<String>> {
        match self {
            Self::Fixture(f) => f.ptr.get(&address).cloned().unwrap_or(Answer::NxDomain),
            Self::System(cell) => {
                let Some(resolver) = system(cell) else {
                    return Answer::Temporary;
                };
                let lookup = tokio::time::timeout(
                    QUERY_TIMEOUT,
                    resolver.reverse_lookup(
                        hickory_resolver::proto::rr::Name::from(address).to_ascii(),
                    ),
                )
                .await
                .unwrap_or(Err(hickory_resolver::net::NetError::Timeout));
                match classify(lookup) {
                    Answer::Records(lookup) => Answer::Records(
                        lookup
                            .answers()
                            .iter()
                            .filter_map(|record| match &record.data {
                                RData::PTR(name) => host_name(&name.0.to_ascii()),
                                _ => None,
                            })
                            .take(8)
                            .collect(),
                    ),
                    Answer::NoRecords => Answer::NoRecords,
                    Answer::NxDomain => Answer::NxDomain,
                    Answer::Temporary => Answer::Temporary,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mx_answers_distinguish_null_mixed_and_bounded_exchanges() {
        assert_eq!(exchanges(vec![(0, ".".into())]), Mx::Null);
        assert_eq!(
            exchanges(vec![(0, ".".into()), (10, "MX.Example.com.".into())]),
            Mx::Exchanges(vec![(10, "mx.example.com".into())])
        );
        assert_eq!(
            exchanges(vec![
                (20, "b.example".into()),
                (10, "c.example".into()),
                (10, "a.example".into()),
                (30, "a.example".into())
            ]),
            Mx::Exchanges(vec![
                (10, "a.example".into()),
                (10, "c.example".into()),
                (20, "b.example".into())
            ])
        );
        assert_eq!(exchanges(vec![(10, "192.0.2.1".into())]), Mx::Temporary);
        let many = (0..20).map(|i| (i, format!("mx{i}.example"))).collect();
        assert!(matches!(exchanges(many), Mx::Exchanges(e) if e.len() == MAX_EXCHANGES));
        assert_eq!(host_name("bad..example"), None);
        assert_eq!(host_name("-bad.example"), None);
    }
}
