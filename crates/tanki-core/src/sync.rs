use crate::{CoreError, Result};
use reqwest::Url;

/// An explicit self-hosted sync URL. There is deliberately no default endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncEndpoint(Url);

fn forbidden_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "ankiweb.net" || host.ends_with(".ankiweb.net")
}

impl SyncEndpoint {
    pub fn parse(value: &str) -> Result<Self> {
        let url = Url::parse(value)
            .map_err(|_| CoreError::Invalid("Configure an explicit sync server URL.".into()))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            return Err(CoreError::Invalid(
                "Sync requires an HTTP(S) server URL.".into(),
            ));
        }
        if forbidden_host(url.host_str().unwrap()) {
            return Err(CoreError::Invalid(
                "AnkiWeb's terms exclude third-party clients; configure a self-hosted server."
                    .into(),
            ));
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CoreError::Invalid(
                "Sync URL cannot contain credentials, query or fragment.".into(),
            ));
        }
        Ok(Self(
            url.join("./")
                .map_err(|e| CoreError::Invalid(e.to_string()))?,
        ))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    #[cfg(test)]
    pub(super) fn auth(&self, hkey: String) -> anki::sync::login::SyncAuth {
        anki::sync::login::SyncAuth {
            hkey,
            endpoint: Some(self.0.clone()),
            io_timeout_secs: Some(5),
        }
    }
}

// Anki follows protocol-level 308 redirects itself, using the Location header.
// Blocking automatic redirects alone cannot prevent a redirected AnkiWeb request.
// Deny its DNS resolution too, and disable proxies so they cannot resolve it instead.
#[cfg(test)]
pub(super) fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .http1_only()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(std::sync::Arc::new(SelfHostedResolver))
        .build()
        .map_err(|e| CoreError::Other(e.to_string()))
}

#[cfg(test)]
struct SelfHostedResolver;

#[cfg(test)]
pub(super) static BLOCKED_RESOLUTIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
impl reqwest::dns::Resolve for SelfHostedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        Box::pin(async move {
            if forbidden_host(name.as_str()) {
                BLOCKED_RESOLUTIONS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "AnkiWeb sync endpoints are forbidden",
                )
                .into());
            }
            let addresses = tokio::net::lookup_host((name.as_str(), 0)).await?;
            Ok(Box::new(addresses.collect::<Vec<_>>().into_iter()) as reqwest::dns::Addrs)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_self_hosted_endpoint_required() {
        for bad in [
            "",
            "sync.example.com",
            "file:///tmp/x",
            "https://ankiweb.net/",
            "https://sync.ankiweb.net/",
            "https://a.b.ANKIWEB.NET./",
            "http://u:p@example.com/",
        ] {
            assert!(SyncEndpoint::parse(bad).is_err(), "accepted {bad}");
        }
        assert_eq!(
            SyncEndpoint::parse("http://127.0.0.1:1234/")
                .unwrap()
                .as_str(),
            "http://127.0.0.1:1234/"
        );
        assert!(SyncEndpoint::parse("https://ankiweb.net.example.com/").is_ok());
    }
}
