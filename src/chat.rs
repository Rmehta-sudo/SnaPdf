use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose};
use crossbeam_channel::{Receiver, Sender, unbounded};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;
use uuid::Uuid;

const OPENAI_AUTHORIZE: &str = "https://auth.openai.com/api/accounts/authorize";
const OPENAI_TOKEN: &str = "https://auth.openai.com/api/accounts/oauth/token";
const OPENAI_DISCOVERY: &str = "https://auth.openai.com/.well-known/openid-configuration";
const OPENAI_API: &str = "https://api.openai.com/v1";
const GEMINI_API: &str = "https://generativelanguage.googleapis.com/v1beta";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatProvider {
    ChatGpt,
    Gemini,
}

impl ChatProvider {
    pub fn label(self) -> &'static str {
        match self {
            Self::ChatGpt => "ChatGPT",
            Self::Gemini => "Gemini",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
}

#[derive(Debug, Clone)]
pub struct ChatAttachment {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl ChatAttachment {
    pub fn text(name: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            mime: "text/plain".to_owned(),
            bytes: text.into().into_bytes(),
        }
    }

    pub fn kind_label(&self) -> &'static str {
        if self.mime == "application/pdf" {
            "PDF"
        } else if self.mime.starts_with("image/") {
            "Image"
        } else {
            "Text"
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub text: String,
    /// Binary attachments are deliberately session-only. Persisting them would make the
    /// workspace state unexpectedly huge; the surrounding conversation text is retained.
    #[serde(skip, default)]
    pub attachments: Vec<ChatAttachment>,
}

#[derive(Debug)]
pub enum ChatCommand {
    SignInChatGpt,
    ConnectGemini {
        api_key: String,
    },
    SignOut(ChatProvider),
    RefreshModels(ChatProvider),
    Send {
        request_id: u64,
        provider: ChatProvider,
        model: String,
        messages: Vec<ChatMessage>,
    },
    Shutdown,
}

#[derive(Debug)]
pub enum ChatEvent {
    AccountState {
        provider: ChatProvider,
        connected: bool,
        account: Option<String>,
    },
    AuthProgress {
        provider: ChatProvider,
        message: String,
    },
    Models {
        provider: ChatProvider,
        models: Vec<String>,
    },
    ResponseDelta {
        request_id: u64,
        text: String,
    },
    ResponseComplete {
        request_id: u64,
    },
    Error {
        provider: Option<ChatProvider>,
        request_id: Option<u64>,
        message: String,
    },
}

pub struct ChatWorker {
    pub commands: Sender<ChatCommand>,
    pub events: Receiver<ChatEvent>,
}

impl ChatWorker {
    pub fn spawn(repaint: eframe::egui::Context) -> Self {
        let (command_tx, command_rx) = unbounded();
        let (event_tx, event_rx) = unbounded();
        std::thread::Builder::new()
            .name("snappdf-study-chat".to_owned())
            .spawn(move || worker_loop(command_rx, event_tx, repaint))
            .expect("could not start study chat worker");
        Self {
            commands: command_tx,
            events: event_rx,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OpenAiCredential {
    client_id: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at: u64,
    account: Option<String>,
    subject: String,
    id_token: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Credentials {
    host_id: Option<String>,
    openai: Option<OpenAiCredential>,
    gemini_api_key: Option<String>,
}

fn worker_loop(
    commands: Receiver<ChatCommand>,
    events: Sender<ChatEvent>,
    repaint: eframe::egui::Context,
) {
    let client = Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent(concat!("SnapPDF/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("could not create HTTP client");
    let mut credentials = load_credentials();
    emit_account_state(&events, &credentials);
    repaint.request_repaint();

    while let Ok(command) = commands.recv() {
        let error_target = match &command {
            ChatCommand::SignInChatGpt => (Some(ChatProvider::ChatGpt), None),
            ChatCommand::ConnectGemini { .. } => (Some(ChatProvider::Gemini), None),
            ChatCommand::SignOut(provider) | ChatCommand::RefreshModels(provider) => {
                (Some(*provider), None)
            }
            ChatCommand::Send {
                provider,
                request_id,
                ..
            } => (Some(*provider), Some(*request_id)),
            ChatCommand::Shutdown => (None, None),
        };
        let outcome = match command {
            ChatCommand::SignInChatGpt => sign_in_chatgpt(&client, &events, &mut credentials)
                .and_then(|_| {
                    refresh_models(&client, &events, &mut credentials, ChatProvider::ChatGpt)
                }),
            ChatCommand::ConnectGemini { api_key } => {
                connect_gemini(&client, &events, &mut credentials, api_key)
            }
            ChatCommand::SignOut(provider) => {
                match provider {
                    ChatProvider::ChatGpt => credentials.openai = None,
                    ChatProvider::Gemini => credentials.gemini_api_key = None,
                }
                save_credentials(&credentials).map(|_| {
                    let _ = events.send(ChatEvent::AccountState {
                        provider,
                        connected: false,
                        account: None,
                    });
                })
            }
            ChatCommand::RefreshModels(provider) => {
                refresh_models(&client, &events, &mut credentials, provider)
            }
            ChatCommand::Send {
                request_id,
                provider,
                model,
                messages,
            } => match provider {
                ChatProvider::ChatGpt => send_openai(
                    &client,
                    &events,
                    &mut credentials,
                    request_id,
                    &model,
                    &messages,
                ),
                ChatProvider::Gemini => send_gemini(
                    &client,
                    &events,
                    &credentials,
                    request_id,
                    &model,
                    &messages,
                ),
            },
            ChatCommand::Shutdown => break,
        };
        if let Err(error) = outcome {
            let (provider, request_id) = error_target;
            let _ = events.send(ChatEvent::Error {
                provider,
                request_id,
                message: error.to_string(),
            });
        }
        repaint.request_repaint();
    }
}

fn emit_account_state(events: &Sender<ChatEvent>, credentials: &Credentials) {
    let _ = events.send(ChatEvent::AccountState {
        provider: ChatProvider::ChatGpt,
        connected: credentials.openai.is_some(),
        account: credentials
            .openai
            .as_ref()
            .and_then(|value| value.account.clone()),
    });
    let _ = events.send(ChatEvent::AccountState {
        provider: ChatProvider::Gemini,
        connected: credentials.gemini_api_key.is_some(),
        account: credentials
            .gemini_api_key
            .as_ref()
            .map(|_| "Google AI Studio key".to_owned()),
    });
}

fn credentials_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("snappdf")
        .join("chat-credentials.json")
}

fn load_credentials() -> Credentials {
    let Ok(text) = fs::read_to_string(credentials_path()) else {
        return Credentials::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn save_credentials(credentials: &Credentials) -> Result<()> {
    let path = credentials_path();
    fs::create_dir_all(path.parent().context("credential path has no parent")?)?;
    let temporary = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    serde_json::to_writer_pretty(&mut file, credentials)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sign_in_chatgpt(
    client: &Client,
    events: &Sender<ChatEvent>,
    credentials: &mut Credentials,
) -> Result<()> {
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).context("could not start local sign-in callback")?;
    listener.set_nonblocking(false)?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/auth/callback");
    let state = Uuid::new_v4().to_string();
    let nonce = Uuid::new_v4().to_string();
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge = general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let host_id = credentials
        .host_id
        .get_or_insert_with(|| format!("urn:uuid:{}", Uuid::new_v4()))
        .clone();
    save_credentials(credentials)?;

    let mut authorize = Url::parse(OPENAI_AUTHORIZE)?;
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", "dynamic_agent_client")
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair(
            "scope",
            "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct",
        )
        .append_pair("resource", "https://api.openai.com/v1")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("nonce", &nonce)
        .append_pair("ext_agent_host_id", &host_id)
        .append_pair("agent_name_hint", "SnapPDF");

    let _ = events.send(ChatEvent::AuthProgress {
        provider: ChatProvider::ChatGpt,
        message: "Finish signing in in your browser…".to_owned(),
    });
    webbrowser::open(authorize.as_str())
        .context("could not open the browser for ChatGPT sign-in")?;

    listener.set_nonblocking(true)?;
    let deadline = Instant::now() + Duration::from_secs(300);
    let (mut stream, _) = loop {
        match listener.accept() {
            Ok(connection) => break connection,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("ChatGPT sign-in timed out; try again when you are ready");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error).context("ChatGPT sign-in callback failed"),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    let mut request = String::new();
    BufReader::new(stream.try_clone()?)
        .read_line(&mut request)
        .context("could not read sign-in callback")?;
    let target = request
        .split_whitespace()
        .nth(1)
        .context("invalid sign-in callback")?;
    let callback = Url::parse(&format!("http://127.0.0.1{target}"))?;
    let params = callback
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    if let Some(error) = params.get("error") {
        reply_to_browser(&mut stream, false)?;
        bail!("ChatGPT sign-in was cancelled or rejected: {error}");
    }
    if params.get("state").map(|value| value.as_ref()) != Some(state.as_str()) {
        reply_to_browser(&mut stream, false)?;
        bail!("ChatGPT sign-in state did not match");
    }
    let code = params
        .get("code")
        .context("sign-in callback did not include a code")?;
    let issued_client_id = params
        .get("client_id")
        .map(|value| value.to_string())
        .context("ChatGPT registration did not return its issued client ID")?;

    let response = client
        .post(OPENAI_TOKEN)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_ref()),
            ("redirect_uri", redirect_uri.as_str()),
            ("client_id", issued_client_id.as_str()),
            ("code_verifier", verifier.as_str()),
            ("resource", "https://api.openai.com/v1"),
        ])
        .send()?;
    let token = checked_json(response, "ChatGPT token exchange")?;
    let access_token = token["access_token"]
        .as_str()
        .context("ChatGPT did not return an access token")?
        .to_owned();
    let expires_in = token["expires_in"].as_u64().unwrap_or(3600);
    let granted_scope = token["scope"]
        .as_str()
        .context("ChatGPT token response did not include granted scopes")?;
    if !granted_scope
        .split_whitespace()
        .any(|scope| scope == "chatgpt.tokens.use.direct")
    {
        bail!("ChatGPT connected, but permission to use the ChatGPT plan was not granted");
    }
    let id_token = token["id_token"]
        .as_str()
        .context("ChatGPT did not return an ID token")?
        .to_owned();
    let identity = validate_openai_id_token(client, &id_token, &issued_client_id, &nonce)?;
    let account = identity
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| identity.get("preferred_username").and_then(Value::as_str))
        .unwrap_or("ChatGPT account")
        .to_owned();
    let subject = identity["sub"]
        .as_str()
        .context("ChatGPT ID token did not include an account identity")?
        .to_owned();
    credentials.openai = Some(OpenAiCredential {
        client_id: issued_client_id,
        access_token,
        refresh_token: token["refresh_token"].as_str().map(str::to_owned),
        expires_at: now_seconds() + expires_in.saturating_sub(30),
        account: Some(account),
        subject,
        id_token,
    });
    save_credentials(credentials)?;
    reply_to_browser(&mut stream, true)?;
    let account = credentials
        .openai
        .as_ref()
        .and_then(|value| value.account.clone());
    let _ = events.send(ChatEvent::AccountState {
        provider: ChatProvider::ChatGpt,
        connected: true,
        account,
    });
    Ok(())
}

fn reply_to_browser(stream: &mut std::net::TcpStream, success: bool) -> Result<()> {
    let (title, detail) = if success {
        (
            "Connected to SnapPDF",
            "You can close this tab and return to your document.",
        )
    } else {
        (
            "SnapPDF could not connect",
            "Return to SnapPDF for details.",
        )
    };
    let body = format!(
        "<!doctype html><meta charset=utf-8><title>{title}</title><style>body{{background:#181818;color:#eee;font:16px system-ui;display:grid;place-items:center;height:100vh;margin:0}}main{{text-align:center}}h1{{font-size:24px}}</style><main><h1>{title}</h1><p>{detail}</p></main>"
    );
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    Ok(())
}

fn validate_openai_id_token(
    client: &Client,
    token: &str,
    audience: &str,
    nonce: &str,
) -> Result<Value> {
    let header = decode_header(token).context("ChatGPT returned an invalid ID token header")?;
    let key_id = header
        .kid
        .context("ChatGPT ID token did not identify its signing key")?;
    let discovery = checked_json(
        client.get(OPENAI_DISCOVERY).send()?,
        "OpenAI identity configuration",
    )?;
    let issuer = discovery["issuer"]
        .as_str()
        .context("OpenAI identity configuration did not include an issuer")?;
    if issuer != "https://auth.openai.com" {
        bail!("OpenAI identity configuration returned an unexpected issuer");
    }
    let jwks_uri = discovery["jwks_uri"]
        .as_str()
        .context("OpenAI identity configuration did not include signing keys")?;
    let jwks: JwkSet =
        checked_json(client.get(jwks_uri).send()?, "OpenAI signing keys").and_then(|value| {
            serde_json::from_value(value).context("OpenAI signing keys were invalid")
        })?;
    let jwk = jwks
        .find(&key_id)
        .context("ChatGPT ID token used an unknown signing key")?;
    let decoding_key = DecodingKey::from_jwk(jwk)?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[issuer]);
    validation.set_audience(&[audience]);
    let claims = decode::<Value>(token, &decoding_key, &validation)
        .context("ChatGPT ID token validation failed")?
        .claims;
    if claims["nonce"].as_str() != Some(nonce) {
        bail!("ChatGPT ID token nonce did not match this sign-in attempt");
    }
    Ok(claims)
}

fn ensure_openai_token(client: &Client, credentials: &mut Credentials) -> Result<String> {
    let credential = credentials
        .openai
        .as_mut()
        .context("Connect ChatGPT first")?;
    if credential.expires_at > now_seconds() + 30 {
        return Ok(credential.access_token.clone());
    }
    let refresh_token = credential
        .refresh_token
        .clone()
        .context("ChatGPT session expired; sign in again")?;
    let response = client
        .post(OPENAI_TOKEN)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token.as_str()),
            ("client_id", credential.client_id.as_str()),
            ("resource", "https://api.openai.com/v1"),
        ])
        .send()?;
    let token = checked_json(response, "ChatGPT token refresh")?;
    credential.access_token = token["access_token"]
        .as_str()
        .context("ChatGPT refresh did not return an access token")?
        .to_owned();
    credential.refresh_token = token["refresh_token"]
        .as_str()
        .map(str::to_owned)
        .or(Some(refresh_token));
    credential.expires_at = now_seconds()
        + token["expires_in"]
            .as_u64()
            .unwrap_or(3600)
            .saturating_sub(30);
    let access = credential.access_token.clone();
    save_credentials(credentials)?;
    Ok(access)
}

fn connect_gemini(
    client: &Client,
    events: &Sender<ChatEvent>,
    credentials: &mut Credentials,
    api_key: String,
) -> Result<()> {
    if api_key.trim().is_empty() {
        bail!("Enter a Gemini API key from Google AI Studio");
    }
    let models = list_gemini_models(client, api_key.trim())?;
    credentials.gemini_api_key = Some(api_key.trim().to_owned());
    save_credentials(credentials)?;
    let _ = events.send(ChatEvent::AccountState {
        provider: ChatProvider::Gemini,
        connected: true,
        account: Some("Google AI Studio key".to_owned()),
    });
    let _ = events.send(ChatEvent::Models {
        provider: ChatProvider::Gemini,
        models,
    });
    Ok(())
}

fn refresh_models(
    client: &Client,
    events: &Sender<ChatEvent>,
    credentials: &mut Credentials,
    provider: ChatProvider,
) -> Result<()> {
    let models = match provider {
        ChatProvider::ChatGpt => {
            let token = ensure_openai_token(client, credentials)?;
            let response = client
                .get(format!("{OPENAI_API}/models"))
                .bearer_auth(token)
                .send()?;
            let value = checked_json(response, "ChatGPT model list")?;
            let mut models = value["models"]
                .as_array()
                .or_else(|| value["data"].as_array())
                .into_iter()
                .flatten()
                .filter(|item| {
                    item["visibility"]
                        .as_str()
                        .is_none_or(|value| value == "list")
                })
                .filter_map(|item| item["slug"].as_str().or_else(|| item["id"].as_str()))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let mut seen = std::collections::HashSet::new();
            models.retain(|model| seen.insert(model.clone()));
            models
        }
        ChatProvider::Gemini => {
            let key = credentials
                .gemini_api_key
                .as_deref()
                .context("Connect Gemini first")?;
            list_gemini_models(client, key)?
        }
    };
    let _ = events.send(ChatEvent::Models { provider, models });
    Ok(())
}

fn list_gemini_models(client: &Client, key: &str) -> Result<Vec<String>> {
    let response = client
        .get(format!("{GEMINI_API}/models"))
        .header("x-goog-api-key", key)
        .send()?;
    let value = checked_json(response, "Gemini model list")?;
    let mut models = value["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|model| {
            model["supportedGenerationMethods"]
                .as_array()
                .is_some_and(|methods| methods.iter().any(|method| method == "generateContent"))
        })
        .filter_map(|model| model["name"].as_str())
        .map(|name| name.trim_start_matches("models/").to_owned())
        .collect::<Vec<_>>();
    models.sort();
    models.dedup();
    Ok(models)
}

fn send_openai(
    client: &Client,
    events: &Sender<ChatEvent>,
    credentials: &mut Credentials,
    request_id: u64,
    model: &str,
    messages: &[ChatMessage],
) -> Result<()> {
    let token = ensure_openai_token(client, credentials)?;
    let input = messages
        .iter()
        .map(openai_message)
        .collect::<Result<Vec<_>>>()?;
    let response = client
        .post(format!("{OPENAI_API}/responses"))
        .bearer_auth(token)
        .json(&json!({
            "model": model,
            "store": false,
            "stream": true,
            "input": input,
        }))
        .send()?;
    stream_sse(
        response,
        |value| {
            if value["type"] == "response.output_text.delta"
                && let Some(delta) = value["delta"].as_str()
            {
                let _ = events.send(ChatEvent::ResponseDelta {
                    request_id,
                    text: delta.to_owned(),
                });
            }
        },
        "ChatGPT response",
        Some("response.completed"),
    )?;
    let _ = events.send(ChatEvent::ResponseComplete { request_id });
    Ok(())
}

fn openai_message(message: &ChatMessage) -> Result<Value> {
    let role = match message.role {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    };
    if message.role == ChatRole::Assistant {
        return Ok(json!({"role": role, "content": message.text}));
    }
    let mut content = Vec::new();
    if !message.text.trim().is_empty() {
        content.push(json!({"type": "input_text", "text": message.text}));
    }
    for attachment in &message.attachments {
        let encoded = general_purpose::STANDARD.encode(&attachment.bytes);
        if attachment.mime == "application/pdf" {
            content.push(json!({
                "type": "input_file",
                "filename": attachment.name,
                "file_data": format!("data:{};base64,{}", attachment.mime, encoded),
            }));
        } else if attachment.mime.starts_with("image/") {
            content.push(json!({
                "type": "input_image",
                "image_url": format!("data:{};base64,{}", attachment.mime, encoded),
            }));
        } else {
            content.push(json!({
                "type": "input_text",
                "text": format!("Attached text — {}:\n{}", attachment.name, String::from_utf8_lossy(&attachment.bytes)),
            }));
        }
    }
    Ok(json!({"role": role, "content": content}))
}

fn send_gemini(
    client: &Client,
    events: &Sender<ChatEvent>,
    credentials: &Credentials,
    request_id: u64,
    model: &str,
    messages: &[ChatMessage],
) -> Result<()> {
    let key = credentials
        .gemini_api_key
        .as_deref()
        .context("Connect Gemini first")?;
    let contents = messages.iter().map(gemini_message).collect::<Vec<_>>();
    let response = client
        .post(format!(
            "{GEMINI_API}/models/{model}:streamGenerateContent?alt=sse"
        ))
        .header("x-goog-api-key", key)
        .json(&json!({"contents": contents}))
        .send()?;
    stream_sse(
        response,
        |value| {
            if let Some(parts) = value["candidates"][0]["content"]["parts"].as_array() {
                for part in parts {
                    if let Some(text) = part["text"].as_str() {
                        let _ = events.send(ChatEvent::ResponseDelta {
                            request_id,
                            text: text.to_owned(),
                        });
                    }
                }
            }
        },
        "Gemini response",
        None,
    )?;
    let _ = events.send(ChatEvent::ResponseComplete { request_id });
    Ok(())
}

fn gemini_message(message: &ChatMessage) -> Value {
    let role = match message.role {
        ChatRole::User => "user",
        ChatRole::Assistant => "model",
    };
    let mut parts = Vec::new();
    if !message.text.trim().is_empty() {
        parts.push(json!({"text": message.text}));
    }
    for attachment in &message.attachments {
        if attachment.mime == "text/plain" {
            parts.push(json!({
                "text": format!("Attached text — {}:\n{}", attachment.name, String::from_utf8_lossy(&attachment.bytes)),
            }));
        } else {
            parts.push(json!({
                "inline_data": {
                    "mime_type": attachment.mime,
                    "data": general_purpose::STANDARD.encode(&attachment.bytes),
                }
            }));
        }
    }
    json!({"role": role, "parts": parts})
}

fn stream_sse(
    response: Response,
    mut on_value: impl FnMut(&Value),
    context: &str,
    terminal_event: Option<&str>,
) -> Result<()> {
    if !response.status().is_success() {
        return Err(http_error(response, context));
    }
    let mut completed = terminal_event.is_none();
    for line in BufReader::new(response).lines() {
        let line = line?;
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() || data == "[DONE]" {
            continue;
        }
        let value: Value = serde_json::from_str(data)
            .with_context(|| format!("{context} returned an invalid stream event"))?;
        let event_type = value["type"].as_str().unwrap_or_default();
        if event_type == "error"
            || event_type == "response.failed"
            || event_type == "response.incomplete"
            || value.get("error").is_some()
        {
            bail!("{context}: {}", api_error_message(&value));
        }
        if terminal_event == Some(event_type) {
            completed = true;
        }
        on_value(&value);
    }
    if !completed {
        bail!("{context} ended before the provider confirmed completion");
    }
    Ok(())
}

fn checked_json(response: Response, context: &str) -> Result<Value> {
    if !response.status().is_success() {
        return Err(http_error(response, context));
    }
    response
        .json()
        .with_context(|| format!("{context} returned invalid JSON"))
}

fn http_error(mut response: Response, context: &str) -> anyhow::Error {
    let status = response.status();
    let mut body = String::new();
    let _ = response.read_to_string(&mut body);
    let message = serde_json::from_str::<Value>(&body)
        .ok()
        .map(|value| api_error_message(&value))
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| body.chars().take(300).collect());
    anyhow!("{context} failed ({status}): {message}")
}

fn api_error_message(value: &Value) -> String {
    value["error"]["message"]
        .as_str()
        .or_else(|| value["response"]["error"]["message"].as_str())
        .or_else(|| value["response"]["error"]["code"].as_str())
        .or_else(|| value["detail"].as_str())
        .or_else(|| value["message"].as_str())
        .unwrap_or("Unknown service error")
        .to_owned()
}

pub fn mime_for_path(path: &std::path::Path) -> Option<&'static str> {
    match path
        .extension()?
        .to_string_lossy()
        .to_ascii_lowercase()
        .as_str()
    {
        "pdf" => Some("application/pdf"),
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "webp" => Some("image/webp"),
        "gif" => Some("image/gif"),
        "txt" | "md" | "tex" | "csv" => Some("text/plain"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_payload_keeps_image_as_image() {
        let value = openai_message(&ChatMessage {
            role: ChatRole::User,
            text: "Explain this".to_owned(),
            attachments: vec![ChatAttachment {
                name: "figure.png".to_owned(),
                mime: "image/png".to_owned(),
                bytes: vec![1, 2, 3],
            }],
        })
        .unwrap();
        assert_eq!(value["content"][1]["type"], "input_image");
    }

    #[test]
    fn gemini_payload_keeps_pdf_inline() {
        let value = gemini_message(&ChatMessage {
            role: ChatRole::User,
            text: String::new(),
            attachments: vec![ChatAttachment {
                name: "paper.pdf".to_owned(),
                mime: "application/pdf".to_owned(),
                bytes: vec![1, 2, 3],
            }],
        });
        assert_eq!(
            value["parts"][0]["inline_data"]["mime_type"],
            "application/pdf"
        );
    }

    #[test]
    fn persisted_chat_keeps_text_without_binary_attachments() {
        let message = ChatMessage {
            role: ChatRole::User,
            text: "Explain the figure".to_owned(),
            attachments: vec![ChatAttachment {
                name: "large.png".to_owned(),
                mime: "image/png".to_owned(),
                bytes: vec![7; 1024],
            }],
        };
        let encoded = serde_json::to_string(&message).unwrap();
        assert!(!encoded.contains("large.png"));
        let restored: ChatMessage = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored.text, "Explain the figure");
        assert!(restored.attachments.is_empty());
    }
}
