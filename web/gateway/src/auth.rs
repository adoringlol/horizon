//! Device-code sign-in follows launcher/src/account.rs: Microsoft -> Xbox ->
//! XSTS -> Minecraft ownership/profile. All requests use the selected route.
//! Only the Minecraft token is retained, in the gateway session's memory.
use crate::route::Route;
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const MINECRAFT: &str = "https://api.minecraftservices.com";
// Public Minecraft Launcher client identifier, as requested for this fork.
// Live flow/scope: PrismarineJS/prismarine-auth src/TokenManagers/LiveTokenManager.js
// and src/MicrosoftAuthFlow.js. Verified device-code issuance 2026-10-23.
const MINECRAFT_LAUNCHER_ID: &str = "00000000402b5328";

struct OAuth {
    client_id: String,
    live: bool,
}

impl OAuth {
    fn for_client(client_id: String, flow: Option<&str>) -> Result<Self, String> {
        let live = match flow {
            Some("live") => true,
            Some("azure") => false,
            Some(_) => return Err("HORIZON_MSA_FLOW must be live or azure.".into()),
            None => client_id.eq_ignore_ascii_case(MINECRAFT_LAUNCHER_ID),
        };
        Ok(Self { client_id, live })
    }

    fn configured() -> Result<Self, String> {
        let id = std::env::var("HORIZON_MSA_CLIENT_ID")
            .ok()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| MINECRAFT_LAUNCHER_ID.into());
        Self::for_client(id, std::env::var("HORIZON_MSA_FLOW").ok().as_deref())
    }

    fn device_url(&self) -> &'static str {
        if self.live {
            "https://login.live.com/oauth20_connect.srf"
        } else {
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode"
        }
    }

    fn token_url(&self) -> &'static str {
        if self.live {
            "https://login.live.com/oauth20_token.srf"
        } else {
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/token"
        }
    }

    fn scope(&self) -> &'static str {
        if self.live {
            "service::user.auth.xboxlive.com::MBI_SSL"
        } else {
            "XboxLive.signin"
        }
    }

    fn rps_ticket(&self, token: &str) -> String {
        if self.live {
            token.to_owned()
        } else {
            format!("d={token}")
        }
    }
}

pub struct Device {
    oauth: OAuth,
    code: String,
    cookies: String,
    pub next_poll: Instant,
    pub deadline: Instant,
    pub interval: u64,
}

pub struct Account {
    pub name: String,
    pub uuid: String,
    token: String,
    deadline: Instant,
}

impl Account {
    pub fn public(&self) -> Value {
        json!({"name": self.name, "uuid": self.uuid})
    }
    pub async fn join(&self, route: &Route, hash: &str) -> Result<(), String> {
        if Instant::now() >= self.deadline {
            return Err("Your account session expired. Sign in again.".into());
        }
        let digits = hash.strip_prefix('-').unwrap_or(hash);
        if digits.len() > 40 || digits.is_empty() || !digits.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err("Invalid server login hash.".into());
        }
        let client = route.http()?;
        let r = client
            .post("https://sessionserver.mojang.com/session/minecraft/join")
            .json(
                &json!({"accessToken": self.token, "selectedProfile": self.uuid, "serverId": hash}),
            )
            .send()
            .await
            .map_err(|_| "Could not reach the Minecraft session service through this route.")?;
        match r.status().as_u16() {
            204 => Ok(()),
            401 | 403 => Err("Minecraft rejected the account session. Check multiplayer access or sign in again.".into()),
            _ => Err("The Minecraft session service did not accept this login.".into()),
        }
    }
}

async fn json_response(r: reqwest::Response, service: &str) -> Result<(u16, Value), String> {
    let status = r.status().as_u16();
    // Upstream errors may include request details. Only our fixed error strings
    // cross the browser boundary; upstream tokens/errors are never logged.
    if r.content_length().is_some_and(|n| n > 1024 * 1024) {
        return Err(format!("{service} sent an oversized response."));
    }
    let body = r
        .bytes()
        .await
        .map_err(|_| format!("Could not read {service}'s response."))?;
    if body.len() > 1024 * 1024 {
        return Err(format!("{service} sent an oversized response."));
    }
    let v = serde_json::from_slice(&body)
        .map_err(|_| format!("{service} sent an invalid response."))?;
    Ok((status, v))
}

fn field(v: &Value, name: &str) -> Result<String, String> {
    v.get(name)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| "The account service returned an incomplete response.".into())
}

pub async fn start(route: &Route) -> Result<(Device, Value), String> {
    let client = route.http()?;
    let oauth = OAuth::configured()?;
    let mut form = vec![
        ("client_id", oauth.client_id.as_str()),
        ("scope", oauth.scope()),
    ];
    if oauth.live {
        form.push(("response_type", "device_code"));
    }
    let r = client
        .post(oauth.device_url())
        .form(&form)
        .send()
        .await
        .map_err(|_| "Could not reach Microsoft through this route.")?;
    // Live Connect can bind device-code polling to a cookie. Keep only cookie
    // name/value pairs, in this pending sign-in, and send them only to Microsoft.
    let cookies = r
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");
    let (status, v) = json_response(r, "Microsoft").await?;
    if status != 200 {
        return Err("Microsoft refused device sign-in for this client ID and OAuth flow.".into());
    }
    let interval = v["interval"].as_u64().unwrap_or(5).clamp(1, 60);
    let expires = v["expires_in"].as_u64().unwrap_or(900).clamp(1, 1800);
    let public = json!({
        "user_code": field(&v, "user_code")?,
        // Fixed verification page: never turn an upstream string into an arbitrary link.
        "verification_uri": "https://www.microsoft.com/link",
        "interval": interval, "expires_in": expires,
    });
    Ok((
        Device {
            oauth,
            code: field(&v, "device_code")?,
            cookies,
            next_poll: Instant::now() + Duration::from_secs(interval),
            deadline: Instant::now() + Duration::from_secs(expires),
            interval,
        },
        public,
    ))
}

pub enum Poll {
    Pending,
    Slower,
    Done(Account),
}

pub async fn poll(route: &Route, device: &Device) -> Result<Poll, String> {
    if Instant::now() > device.deadline {
        return Err("The sign-in code expired. Start again.".into());
    }
    let client = route.http()?;
    // Pin the ID/flow and cookie to the device code that started this login.
    let oauth = &device.oauth;
    let mut request = client.post(oauth.token_url());
    if oauth.live {
        request = request.query(&[("client_id", oauth.client_id.as_str())]);
        if !device.cookies.is_empty() {
            request = request.header(reqwest::header::COOKIE, &device.cookies);
        }
    }
    let r = request
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("client_id", oauth.client_id.as_str()),
            ("device_code", device.code.as_str()),
        ])
        .send()
        .await
        .map_err(|_| "Could not reach Microsoft through this route.")?;
    let (status, v) = json_response(r, "Microsoft").await?;
    match v["error"].as_str() {
        Some("authorization_pending") => return Ok(Poll::Pending),
        Some("slow_down") => return Ok(Poll::Slower),
        Some("authorization_declined") => {
            return Err("Sign-in was declined in the Microsoft page.".into());
        }
        Some("expired_token") => return Err("The sign-in code expired. Start again.".into()),
        Some(_) => return Err("Microsoft could not complete this sign-in.".into()),
        None => {}
    }
    if status != 200 {
        return Err("Microsoft could not complete this sign-in.".into());
    }
    let ms = field(&v, "access_token")?;
    let xbl = post(&client, "https://user.auth.xboxlive.com/user/authenticate", json!({
        "Properties": { "AuthMethod": "RPS", "SiteName": "user.auth.xboxlive.com", "RpsTicket": oauth.rps_ticket(&ms) },
        "RelyingParty": "http://auth.xboxlive.com", "TokenType": "JWT"
    }), "Xbox Live").await?;
    let xsts = post(
        &client,
        "https://xsts.auth.xboxlive.com/xsts/authorize",
        json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [field(&xbl, "Token")?] },
            "RelyingParty": "rp://api.minecraftservices.com/", "TokenType": "JWT"
        }),
        "Xbox Live (check the account's Xbox profile and family settings)",
    )
    .await?;
    let uhs = xsts
        .pointer("/DisplayClaims/xui/0/uhs")
        .and_then(Value::as_str)
        .ok_or("Xbox Live returned no user identity.")?;
    let mc = post(
        &client,
        &format!("{MINECRAFT}/authentication/login_with_xbox"),
        json!({
            "identityToken": format!("XBL3.0 x={uhs};{}", field(&xsts, "Token")?)
        }),
        "Minecraft (the application must have Minecraft API approval)",
    )
    .await?;
    let token = field(&mc, "access_token")?;
    let ownership = get(
        &client,
        &format!("{MINECRAFT}/entitlements/mcstore"),
        &token,
    )
    .await?;
    if !ownership["items"].as_array().is_some_and(|a| {
        a.iter().any(|i| {
            matches!(
                i["name"].as_str(),
                Some("game_minecraft" | "product_minecraft")
            )
        })
    }) {
        return Err("This account does not own Minecraft: Java Edition.".into());
    }
    let profile = get(&client, &format!("{MINECRAFT}/minecraft/profile"), &token).await?;
    let uuid = field(&profile, "id")?;
    if uuid.len() != 32 || !uuid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Minecraft returned an invalid profile ID.".into());
    }
    Ok(Poll::Done(Account {
        name: field(&profile, "name")?,
        uuid,
        token,
        deadline: Instant::now()
            + Duration::from_secs(
                mc["expires_in"]
                    .as_u64()
                    .unwrap_or(86400)
                    .clamp(60, 86400)
                    .saturating_sub(30),
            ),
    }))
}

async fn post(
    client: &reqwest::Client,
    url: &str,
    body: Value,
    service: &str,
) -> Result<Value, String> {
    let r = client
        .post(url)
        .json(&body)
        .send()
        .await
        .map_err(|_| format!("Could not reach {service} through this route."))?;
    let (status, v) = json_response(r, service).await?;
    if status != 200 {
        return Err(format!("Sign-in was refused by {service}."));
    }
    Ok(v)
}

async fn get(client: &reqwest::Client, url: &str, token: &str) -> Result<Value, String> {
    let r = client
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| "Could not reach Minecraft through this route.")?;
    let (status, v) = json_response(r, "Minecraft").await?;
    if status != 200 {
        return Err("Minecraft could not verify ownership or load the account profile.".into());
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_id_selects_live_endpoints_scope_and_unprefixed_xbox_ticket() {
        let oauth = OAuth::for_client(MINECRAFT_LAUNCHER_ID.into(), None).unwrap();
        assert_eq!(
            oauth.device_url(),
            "https://login.live.com/oauth20_connect.srf"
        );
        assert_eq!(
            oauth.token_url(),
            "https://login.live.com/oauth20_token.srf"
        );
        assert_eq!(oauth.scope(), "service::user.auth.xboxlive.com::MBI_SSL");
        assert_eq!(oauth.rps_ticket("test-token"), "test-token");
    }

    #[test]
    fn custom_azure_id_preserves_azure_device_flow_and_d_prefix() {
        let oauth = OAuth::for_client("custom-azure-application".into(), None).unwrap();
        assert_eq!(
            oauth.device_url(),
            "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode"
        );
        assert_eq!(oauth.scope(), "XboxLive.signin");
        assert_eq!(oauth.rps_ticket("test-token"), "d=test-token");
        assert!(OAuth::for_client("id".into(), Some("unknown")).is_err());
    }
}
