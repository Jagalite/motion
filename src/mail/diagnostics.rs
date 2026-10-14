//! Setup diagnostics for native direct delivery. Each finding separates what
//! Motion observed from what it cannot prove (inbox placement, reputation, or
//! whether a published SPF policy authorizes this server's public address).
use serde::Serialize;

use super::{Routing, dkim, dns, smtp};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Warning,
    Error,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub check: &'static str,
    pub status: Status,
    pub detail: String,
}

fn finding(check: &'static str, status: Status, detail: impl Into<String>) -> Finding {
    Finding {
        check,
        status,
        detail: detail.into(),
    }
}

pub struct Setup<'a> {
    pub from_domain: &'a str,
    pub envelope_domain: &'a str,
    pub hostname: &'a str,
    pub selector: &'a str,
    pub public_key: Option<&'a str>,
}

fn bounded(text: &str) -> String {
    let clean: String = text.chars().filter(|c| !c.is_control()).take(300).collect();
    clean
}

fn tag<'a>(record: &'a str, name: &str) -> Option<&'a str> {
    record.split(';').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        (key.trim().eq_ignore_ascii_case(name)).then(|| value.trim())
    })
}

/// DNS and alignment observations for the configured sending identity.
pub async fn records(routing: &Routing, setup: &Setup<'_>) -> Vec<Finding> {
    let resolver = &routing.resolver;
    let mut findings = Vec::new();
    let dkim_name = format!("{}._domainkey.{}", setup.selector, setup.from_domain);
    findings.push(match (setup.public_key, resolver.txt(&dkim_name).await) {
        (None, _) => finding("dkim", Status::Error, "No DKIM signing key is configured"),
        (Some(_), dns::Answer::Temporary) => finding(
            "dkim",
            Status::Unknown,
            format!("The DKIM record at {dkim_name} could not be looked up; try again"),
        ),
        (Some(expected), dns::Answer::Records(records)) => {
            let published: Vec<String> = records
                .iter()
                .filter_map(|r| tag(r, "p"))
                .map(|p| p.chars().filter(|c| !c.is_whitespace()).collect())
                .collect();
            if published.iter().any(|p| p == expected) {
                finding(
                    "dkim",
                    Status::Ok,
                    format!("{dkim_name} publishes the configured public key"),
                )
            } else if published.is_empty() {
                finding(
                    "dkim",
                    Status::Error,
                    format!("{dkim_name} has no DKIM key record; publish the record shown"),
                )
            } else {
                finding(
                    "dkim",
                    Status::Error,
                    format!("{dkim_name} publishes a different key; update it to the record shown"),
                )
            }
        }
        (Some(_), _) => finding(
            "dkim",
            Status::Error,
            format!("{dkim_name} does not exist; publish the record shown"),
        ),
    });
    let spf = match resolver.txt(setup.envelope_domain).await {
        dns::Answer::Records(records) => records
            .into_iter()
            .filter(|r| r.to_ascii_lowercase().starts_with("v=spf1"))
            .collect::<Vec<_>>(),
        dns::Answer::Temporary => {
            findings.push(finding(
                "spf",
                Status::Unknown,
                "The SPF record could not be looked up; try again",
            ));
            vec!["?".into()]
        }
        _ => Vec::new(),
    };
    match spf.as_slice() {
        [only] if only == "?" => {}
        [] => findings.push(finding(
            "spf",
            Status::Warning,
            format!(
                "{} publishes no SPF record. Publish one that authorizes this server's public sending address",
                setup.envelope_domain
            ),
        )),
        [record] => findings.push(finding(
            "spf",
            Status::Ok,
            format!(
                "Published: {}. Motion cannot verify that it authorizes this server's public address",
                bounded(record)
            ),
        )),
        _ => findings.push(finding(
            "spf",
            Status::Error,
            format!(
                "{} publishes more than one SPF record, which is a permanent SPF error",
                setup.envelope_domain
            ),
        )),
    }
    let dmarc_name = format!("_dmarc.{}", setup.from_domain);
    findings.push(match resolver.txt(&dmarc_name).await {
        dns::Answer::Records(records) => {
            match records
                .iter()
                .find(|r| r.to_ascii_lowercase().starts_with("v=dmarc1"))
            {
                Some(record) => finding(
                    "dmarc",
                    Status::Ok,
                    format!(
                        "Published policy p={}",
                        bounded(tag(record, "p").unwrap_or("missing"))
                    ),
                ),
                None => finding(
                    "dmarc",
                    Status::Warning,
                    format!("{dmarc_name} has no DMARC record"),
                ),
            }
        }
        dns::Answer::Temporary => finding(
            "dmarc",
            Status::Unknown,
            "The DMARC record could not be looked up; try again",
        ),
        _ => finding(
            "dmarc",
            Status::Warning,
            format!("{dmarc_name} has no DMARC record"),
        ),
    });
    let aligned = setup.envelope_domain == setup.from_domain
        || setup
            .envelope_domain
            .ends_with(&format!(".{}", setup.from_domain));
    findings.push(if aligned {
        finding(
            "alignment",
            Status::Ok,
            "DKIM signs as the From domain; the envelope sender is aligned with it",
        )
    } else {
        finding(
            "alignment",
            Status::Warning,
            "The envelope sender domain is not aligned with the From domain; DMARC relies on DKIM alone",
        )
    });
    findings.push(match resolver.addresses(setup.hostname).await {
        dns::Answer::Records(_) => finding(
            "hostname",
            Status::Ok,
            format!("{} resolves", setup.hostname),
        ),
        dns::Answer::Temporary => finding(
            "hostname",
            Status::Unknown,
            "The sending hostname could not be looked up; try again",
        ),
        _ => finding(
            "hostname",
            Status::Warning,
            format!(
                "{} does not resolve; recipient servers may distrust the EHLO name",
                setup.hostname
            ),
        ),
    });
    findings
}

/// Probe outbound SMTP to `domain`'s first permitted exchanger, then examine the
/// observed local address. A private local address means NAT: the public
/// address and its PTR record cannot be observed from here.
pub async fn connectivity(routing: &Routing, domain: &str, hostname: &str) -> Vec<Finding> {
    let routes = match super::routes(routing, domain).await {
        Ok(routes) => routes,
        Err((_, detail)) => {
            return vec![finding(
                "connectivity",
                Status::Error,
                format!("No delivery route for {domain}: {detail}"),
            )];
        }
    };
    let route = &routes[0];
    let probe = smtp::probe(&route.host, route.address, hostname, &routing.tls).await;
    let mut findings = vec![finding(
        "connectivity",
        if probe.ok { Status::Ok } else { Status::Error },
        format!(
            "{} (port {}): {}",
            route.host,
            route.address.port(),
            probe.detail
        ),
    )];
    findings.push(match probe.local.map(|l| l.ip()) {
        None => finding(
            "reverse_dns",
            Status::Unknown,
            "No outbound connection was established, so the sending address is unknown",
        ),
        Some(ip) if !crate::signin_protocol::is_public_address(ip) => finding(
            "reverse_dns",
            Status::Unknown,
            "This server connects from a private address (NAT). Motion cannot observe the public address or its PTR record; ask the network provider",
        ),
        Some(ip) => match routing.resolver.ptr(ip).await {
            dns::Answer::Records(names) if names.iter().any(|n| n == hostname) => {
                match routing.resolver.addresses(hostname).await {
                    dns::Answer::Records(addresses) if addresses.contains(&ip) => finding(
                        "reverse_dns",
                        Status::Ok,
                        format!("{ip} has PTR {hostname}, which resolves back to it"),
                    ),
                    _ => finding(
                        "reverse_dns",
                        Status::Warning,
                        format!("{ip} has PTR {hostname}, but {hostname} does not resolve back to it"),
                    ),
                }
            }
            dns::Answer::Records(names) if !names.is_empty() => finding(
                "reverse_dns",
                Status::Warning,
                format!(
                    "{ip} has PTR {}, not the sending hostname {hostname}",
                    bounded(&names.join(", "))
                ),
            ),
            dns::Answer::Temporary => finding(
                "reverse_dns",
                Status::Unknown,
                "The PTR record could not be looked up; try again",
            ),
            _ => finding(
                "reverse_dns",
                Status::Warning,
                format!("{ip} has no PTR record; the IP owner or hosting provider must set one"),
            ),
        },
    });
    findings
}

/// Facts outside Motion's observation, always reported with diagnostics.
pub const UNPROVABLE: &[&str] = &[
    "Inbox placement: acceptance by a mail server does not prove delivery to the inbox.",
    "Reputation: recipient providers assess domain and IP reputation independently.",
    "Later bounces: a refusal after acceptance arrives only at the envelope sender mailbox.",
];

/// The records an administrator publishes for the current configuration.
pub fn publish(setup: &Setup<'_>) -> Vec<(String, String, String)> {
    let mut records = Vec::new();
    if let Some(public) = setup.public_key {
        records.push((
            "TXT".into(),
            format!("{}._domainkey.{}", setup.selector, setup.from_domain),
            dkim::dns_record(public),
        ));
    }
    records.push((
        "TXT".into(),
        setup.envelope_domain.to_owned(),
        "v=spf1 ip4:<public sending IPv4> ip6:<public sending IPv6> -all".into(),
    ));
    records.push((
        "TXT".into(),
        format!("_dmarc.{}", setup.from_domain),
        format!(
            "v=DMARC1; p=none; rua=mailto:postmaster@{}",
            setup.from_domain
        ),
    ));
    records.push((
        "PTR".into(),
        "<public sending address>".into(),
        setup.hostname.to_owned(),
    ));
    records
}
