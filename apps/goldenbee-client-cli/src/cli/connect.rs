use crate::cli::args::ConnectionOptions;
use crate::cli::profiles::Store;
use sdk::Client;

pub(crate) struct Connection {
    pub(crate) url: String,
    pub(crate) token: String,
    pub(crate) profile: Option<String>,
}

pub(crate) async fn resolve(
    options: &ConnectionOptions,
    store: &Store,
) -> Result<Connection, String> {
    let env_url = std::env::var("GOLDENBEE_GATEWAY_URL").ok();
    let env_token = std::env::var("GOLDENBEE_TOKEN").ok();
    let active = store
        .active()
        .await
        .map_err(|error| format!("gb-client: {}: {}", error.code, error.message))?;

    if let Some(url) = &options.gateway {
        let token = options
            .token
            .clone()
            .or(env_token)
            .or_else(|| {
                active
                    .as_ref()
                    .filter(|profile| &profile.url == url)
                    .map(|profile| profile.token.clone())
            })
            .ok_or_else(|| {
                "gb-client: a bearer token is required via --token or GOLDENBEE_TOKEN".to_string()
            })?;
        return Ok(Connection {
            url: url.clone(),
            token,
            profile: active
                .filter(|profile| &profile.url == url)
                .map(|profile| profile.name),
        });
    }
    if let Some(profile) = active {
        return Ok(Connection {
            url: profile.url,
            token: options.token.clone().or(env_token).unwrap_or(profile.token),
            profile: Some(profile.name),
        });
    }
    let url = env_url.ok_or_else(|| {
        "gb-client: no active Gateway. Add one with `gb-client gw add <name> <url> <token>`."
            .to_string()
    })?;
    let token = options.token.clone().or(env_token).ok_or_else(|| {
        "gb-client: a bearer token is required via --token or GOLDENBEE_TOKEN".to_string()
    })?;
    Ok(Connection {
        url,
        token,
        profile: None,
    })
}

/// Resolve a stored or explicit connection and build the SDK client.
pub(crate) async fn client(options: &ConnectionOptions) -> Result<(Client, Connection), String> {
    let store = Store::open()
        .await
        .map_err(|error| format!("gb-client: {}: {}", error.code, error.message))?;
    let connection = resolve(options, &store).await?;
    let client = Client::new(connection.url.clone(), connection.token.clone());
    Ok((client, connection))
}
