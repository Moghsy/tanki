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

    /// The auth Anki's sync calls take. `sync_login` returns one without an
    /// endpoint, which Anki treats as AnkiWeb, so every auth is built here.
    pub(crate) fn auth_with_timeout(
        &self,
        hkey: String,
        timeout_secs: u32,
    ) -> anki::sync::login::SyncAuth {
        anki::sync::login::SyncAuth {
            hkey,
            endpoint: Some(self.0.clone()),
            io_timeout_secs: (timeout_secs > 0).then_some(timeout_secs),
        }
    }

    /// Phase 0 spike form, with a short timeout.
    #[cfg(test)]
    pub(super) fn auth(&self, hkey: String) -> anki::sync::login::SyncAuth {
        self.auth_with_timeout(hkey, 5)
    }
}

/// True for `ankiweb.net` and its subdomains (any case, trailing dot).
pub(crate) fn is_ankiweb_host(host: &str) -> bool {
    forbidden_host(host)
}

/// A sync HTTP client plus a record of whether its resolver refused an
/// AnkiWeb name, so a refused redirect can be reported clearly.
pub(crate) struct SafeClient {
    pub client: reqwest::Client,
    refused: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SafeClient {
    /// Whether any request made with this client was stopped because it
    /// named an AnkiWeb host (for example after a protocol redirect).
    pub fn refused_ankiweb(&self) -> bool {
        self.refused.load(std::sync::atomic::Ordering::SeqCst)
    }
}

// Anki follows protocol-level 308 redirects itself, using the Location header,
// so reqwest's redirect policy and the check in `SyncEndpoint::parse` do not
// cover a server that redirects elsewhere. Refusing to resolve AnkiWeb's names
// does, and proxies are disabled so that none can resolve them instead.
pub(crate) fn safe_client() -> Result<SafeClient> {
    let refused = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let client = reqwest::Client::builder()
        .http1_only()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(std::sync::Arc::new(SelfHostedResolver {
            refused: refused.clone(),
        }))
        .build()
        .map_err(|e| CoreError::Other(e.to_string()))?;
    Ok(SafeClient { client, refused })
}

/// Phase 0 spike form.
#[cfg(test)]
pub(super) fn client() -> Result<reqwest::Client> {
    Ok(safe_client()?.client)
}

struct SelfHostedResolver {
    refused: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// Decides before any lookup happens; kept separate so it can be tested
/// without touching the network.
fn refuse_forbidden(name: &str) -> std::io::Result<()> {
    if forbidden_host(name) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "AnkiWeb sync endpoints are forbidden",
        ));
    }
    Ok(())
}

impl reqwest::dns::Resolve for SelfHostedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let refused = self.refused.clone();
        Box::pin(async move {
            if let Err(error) = refuse_forbidden(name.as_str()) {
                refused.store(true, std::sync::atomic::Ordering::SeqCst);
                return Err(error.into());
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

    #[test]
    fn resolver_refuses_ankiweb_names_before_lookup() {
        for name in ["ankiweb.net", "sync.ankiweb.net", "sync12.AnkiWeb.net."] {
            let error = refuse_forbidden(name).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        }
        assert!(refuse_forbidden("localhost").is_ok());
        assert!(refuse_forbidden("ankiweb.net.example.com").is_ok());
    }
}
