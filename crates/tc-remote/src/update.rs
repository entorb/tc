//! Latest release tag of tc on GitHub, for the daily update check.

use std::time::Duration;

use tc_core::{Error, Result};
use ureq::Proxy;
use ureq::tls::{RootCerts, TlsConfig};

const LATEST: &str = "https://api.github.com/repos/entorb/tc/releases/latest";

/// Tag of the latest non-prerelease release (`v1.2.3`); blocks up to 10 s.
pub fn latest_tag() -> Result<String> {
    let err = |e: ureq::Error| Error::Remote(format!("update check: {e}"));
    let body = (agent().get(LATEST).call().map_err(err)?)
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()
        .map_err(err)?;
    tag_from_json(&body).ok_or_else(|| Error::Remote("update check: no tag_name".into()))
}

/// HTTPS client through the corporate proxy, 10 s timeout; also carries crash reports.
pub fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .https_only(true)
        .user_agent("tc")
        // OS cert store: corporate TLS-inspecting proxies sign with a CA installed there
        .tls_config(
            TlsConfig::builder()
                .root_certs(RootCerts::PlatformVerifier)
                .build(),
        )
        .proxy(Proxy::try_from_env().or_else(system_proxy))
        .build()
        .new_agent()
}

/// Windows "Internet Options" proxy (`ProxyEnable` + `ProxyServer`); env vars win.
/// shortcut: no PAC/WPAD script and no NTLM auth, set `HTTPS_PROXY` for those.
#[cfg(windows)]
fn system_proxy() -> Option<Proxy> {
    let mut c = std::process::Command::new("reg");
    c.args([
        "query",
        r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
    ]);
    crate::container::no_window(&mut c);
    let out = c.output().ok()?;
    Proxy::new(&proxy_from_reg(&String::from_utf8_lossy(&out.stdout))?).ok()
}

#[cfg(not(windows))]
fn system_proxy() -> Option<Proxy> {
    None
}

/// Proxy URI from `reg query` output: `host:port` or `http=h:p;https=h:p`.
#[cfg_attr(not(windows), allow(dead_code))]
fn proxy_from_reg(out: &str) -> Option<String> {
    let value = |name: &str| {
        out.lines().find_map(|l| {
            let mut it = l.split_whitespace();
            (it.next() == Some(name)).then(|| it.nth(1).unwrap_or_default().to_owned())
        })
    };
    if value("ProxyEnable")? != "0x1" {
        return None;
    }
    let server = value("ProxyServer")?;
    let pick = |scheme: &str| {
        server
            .split(';')
            .find_map(|p| p.strip_prefix(scheme)?.strip_prefix('='))
            .map(str::to_owned)
    };
    let hp = if server.contains('=') {
        pick("https").or_else(|| pick("http"))?
    } else {
        server
    };
    (!hp.is_empty()).then(|| {
        if hp.contains("://") {
            hp
        } else {
            format!("http://{hp}")
        }
    })
}

/// Value of the first `"tag_name"` string; no JSON parser for one field.
fn tag_from_json(json: &str) -> Option<String> {
    let rest = &json[json.find("\"tag_name\"")? + "\"tag_name\"".len()..];
    let rest = rest.trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_owned())
}

#[cfg(test)]
mod tests {
    use super::{proxy_from_reg, tag_from_json};

    #[test]
    fn proxy_from_reg_query() {
        let reg = |enable: &str, server: &str| {
            format!(
                "\r\nHKEY_CURRENT_USER\\Software\r\n    ProxyEnable    REG_DWORD    {enable}\r\n    ProxyServer    REG_SZ    {server}\r\n"
            )
        };
        assert_eq!(
            proxy_from_reg(&reg("0x1", "proxy.corp:8080")).as_deref(),
            Some("http://proxy.corp:8080")
        );
        assert_eq!(
            proxy_from_reg(&reg("0x1", "http=h:80;https=s:443;ftp=f:21")).as_deref(),
            Some("http://s:443")
        );
        assert_eq!(
            proxy_from_reg(&reg("0x1", "ftp=f:21;http=h:80")).as_deref(),
            Some("http://h:80")
        );
        assert_eq!(proxy_from_reg(&reg("0x0", "proxy.corp:8080")), None);
        assert_eq!(proxy_from_reg(&reg("0x1", "ftp=f:21")), None);
        assert_eq!(proxy_from_reg("ProxyEnable    REG_DWORD    0x1"), None);
    }

    #[test]
    fn tag_compact_pretty_and_missing() {
        assert_eq!(
            tag_from_json(r#"{"id":1,"tag_name":"v1.2.3","name":"x"}"#).as_deref(),
            Some("v1.2.3")
        );
        assert_eq!(
            tag_from_json("{\n  \"tag_name\" :  \"v0.1.0\",\n}").as_deref(),
            Some("v0.1.0")
        );
        assert_eq!(tag_from_json(r#"{"name":"v1"}"#), None);
        assert_eq!(tag_from_json(r#"{"tag_name":5}"#), None);
    }

    #[test]
    #[ignore = "network: asks GitHub"]
    fn live_latest_tag() {
        let tag = match super::latest_tag() {
            Ok(tag) => tag,
            // Unauthenticated GitHub quota (60/h per IP) answers 403: not a code bug
            Err(e) if e.to_string().contains("http status: 403") => {
                eprintln!("skipped: {e}");
                return;
            }
            Err(e) => panic!("{e}"),
        };
        assert!(tag.starts_with('v'), "{tag}");
    }
}
