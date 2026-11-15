//! Serve the API until SIGINT/SIGTERM, then close through with_collection.
use clap::Args;
use std::sync::{atomic::AtomicBool, Arc};
use tanki_core::{config::ConnectSettings, CollectionActor};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Args, Debug)]
pub struct ServeArgs {
    #[arg(long)]
    pub bind: Option<std::net::SocketAddr>,
    #[arg(long)]
    pub api_key: Option<String>,
    #[arg(long = "allow-origin")]
    pub allow_origins: Vec<String>,
}
pub fn run(
    actor: &CollectionActor,
    args: &ServeArgs,
    json: bool,
    config: &ConnectSettings,
) -> Result<()> {
    let mut options = tanki_connect::ServeOptions::from_settings(config)?;
    if let Some(bind) = args.bind {
        options.bind = bind;
    }
    if let Some(key) = &args.api_key {
        options.api_key = Some(key.clone());
    }
    options.cors_origins.extend(args.allow_origins.clone());
    let listener = std::net::TcpListener::bind(options.bind)?;
    let address = listener.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let interrupt = signal_hook::flag::register(signal_hook::consts::SIGINT, stop.clone())?;
    let terminate = match signal_hook::flag::register(signal_hook::consts::SIGTERM, stop.clone()) {
        Ok(id) => id,
        Err(error) => {
            signal_hook::low_level::unregister(interrupt);
            return Err(error.into());
        }
    };
    if json {
        println!("{}", serde_json::json!({"address":address.to_string()}));
    } else {
        println!("AnkiConnect listening on http://{address}");
    }
    let result = tanki_connect::serve_listener(actor, &options, listener, &stop);
    signal_hook::low_level::unregister(interrupt);
    signal_hook::low_level::unregister(terminate);
    result
}
