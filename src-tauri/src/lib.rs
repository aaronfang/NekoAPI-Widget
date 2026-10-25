use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use keyring::Entry;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use sha2::Sha256;
use std::{
    fs,
    path::PathBuf,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(target_os = "linux")]
use std::{
    io::{BufRead, BufReader, Write},
    thread,
};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, PhysicalPosition, Rect,
};
use tauri_plugin_notification::NotificationExt;

// The bare domain's TLS endpoint became unreliable, so all requests go to www.
const API_BASE: &str = "https://www.999555999.com/api/v1";
const KEYRING_SERVICE: &str = "com.nekoapi.widget";
const KEYRING_USER: &str = "session";
const TRAY_ID: &str = "nekoapi-status";

#[cfg(target_os = "linux")]
const LINUX_PANEL_DIR: &str = ".cache/nekoapi-widget";
#[cfg(target_os = "linux")]
const LINUX_PANEL_STATUS_FILE: &str = "panel-status";
#[cfg(target_os = "linux")]
const LINUX_CONTROL_SOCKET: &str = "control.sock";

#[derive(Clone)]
struct ApiState {
    client: Client,
}

#[derive(Default)]
struct PopupState {
    last_blur: Mutex<Option<Instant>>,
}

#[derive(Default)]
struct LotteryState {
    enabled: Mutex<bool>,
    last_notified_round_id: Mutex<Option<i64>>,
}

#[derive(Default)]
struct LotteryMarker(Mutex<bool>);

#[derive(Default)]
struct TrayCache {
    title: Mutex<String>,
    tooltip: Mutex<String>,
    channel_state: Mutex<Option<ChannelLightState>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Session {
    access_token: String,
    refresh_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LotterySettings {
    enabled: bool,
    last_notified_round_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
struct LotteryRoundInfo {
    id: i64,
    number: Option<i64>,
    title: Option<String>,
    status: Option<String>,
    reward_amount: Option<f64>,
    participant_count: Option<i64>,
    target_participants: Option<i64>,
    closes_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LotteryEligibility {
    eligible: Option<bool>,
    reason_code: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LotterySnapshot {
    server_enabled: bool,
    has_open_round: bool,
    participated: bool,
    eligibility: LotteryEligibility,
    current_round: Option<LotteryRoundInfo>,
    fetched_at: u64,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum LoginOutcome {
    Authenticated,
    TwoFactor { temp_token: String },
}

#[derive(Debug, Serialize)]
struct DashboardSnapshot {
    profile: Value,
    stats: Value,
    subscriptions: Value,
    subscription_summary: SubscriptionSummary,
    subscriptions_error: Option<String>,
    current_api_key: Option<ApiKeyRoute>,
    api_keys_error: Option<String>,
    channel_monitors: Value,
    channel_monitors_error: Option<String>,
    fetched_at: u64,
}

struct DashboardData {
    profile: Value,
    stats: Value,
    subscriptions: Value,
    subscriptions_error: Option<String>,
    api_keys: Value,
    api_keys_error: Option<String>,
    groups: Value,
    channel_monitors: Value,
    channel_monitors_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum QuotaPeriod {
    Daily,
    Weekly,
    Monthly,
}

impl QuotaPeriod {
    fn label(&self) -> &'static str {
        match self {
            Self::Daily => "日额度",
            Self::Weekly => "周额度",
            Self::Monthly => "月额度",
        }
    }

    fn tie_break_priority(&self) -> u8 {
        match self {
            Self::Daily => 0,
            Self::Weekly => 1,
            Self::Monthly => 2,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct QuotaSummaryItem {
    subscription_id: String,
    subscription_name: String,
    platform: String,
    period: QuotaPeriod,
    used: f64,
    limit: f64,
    percent: f64,
}

#[derive(Debug, Clone, Default, Serialize)]
struct SubscriptionSummary {
    active_subscription_count: usize,
    quota_count: usize,
    selected: Option<QuotaSummaryItem>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
struct ApiKeyRoute {
    key_id: String,
    key_name: String,
    group_id: String,
    group_name: String,
    platform: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChannelLightState {
    Operational,
    Degraded,
    Failed,
    Unknown,
}

impl ChannelLightState {
    fn label(self) -> &'static str {
        match self {
            Self::Operational => "正常",
            Self::Degraded => "性能下降",
            Self::Failed => "故障",
            Self::Unknown => "未知",
        }
    }

    fn severity(self) -> u8 {
        match self {
            Self::Unknown => 0,
            Self::Operational => 1,
            Self::Degraded => 2,
            Self::Failed => 3,
        }
    }

    fn color(self) -> [u8; 4] {
        match self {
            // Match the muted status palette used by `.channel-dot` in the UI.
            Self::Operational => [130, 168, 154, 255],
            Self::Degraded => [180, 154, 114, 255],
            Self::Failed => [185, 120, 126, 255],
            Self::Unknown => [83, 90, 101, 255],
        }
    }
}

fn session_entry() -> Result<Entry, String> {
    Entry::new(KEYRING_SERVICE, KEYRING_USER)
        .map_err(|error| format!("无法连接系统钥匙串：{error}"))
}

fn load_session() -> Result<Option<Session>, String> {
    match session_entry()?.get_password() {
        Ok(value) => serde_json::from_str(&value)
            .map(Some)
            .map_err(|_| "钥匙串中的登录信息已损坏，请重新登录".to_string()),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(format!("无法读取系统钥匙串：{error}")),
    }
}

fn save_session(session: &Session) -> Result<(), String> {
    let value = serde_json::to_string(session).map_err(|error| error.to_string())?;
    session_entry()?
        .set_password(&value)
        .map_err(|error| format!("无法保存登录信息：{error}"))
}

fn delete_session() -> Result<(), String> {
    match session_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(format!("无法清除登录信息：{error}")),
    }
}

fn api_error(value: &Value, fallback: &str) -> String {
    value
        .get("message")
        .or_else(|| value.get("msg"))
        .or_else(|| value.get("error"))
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_string()
}

fn unwrap_payload(value: Value) -> Result<Value, String> {
    if let Some(code) = value.get("code").and_then(Value::as_i64) {
        if code != 0 && code != 200 {
            return Err(api_error(&value, "接口返回了错误"));
        }
    }

    Ok(value.get("data").cloned().unwrap_or(value))
}

async fn response_json(response: reqwest::Response) -> Result<Value, String> {
    let status = response.status();
    let value = response
        .json::<Value>()
        .await
        .map_err(|_| format!("站点返回了无法解析的数据（HTTP {status}）"))?;

    if !status.is_success() {
        return Err(api_error(&value, &format!("请求失败（HTTP {status}）")));
    }

    unwrap_payload(value)
}

fn session_from_payload(
    value: &Value,
    fallback_refresh: Option<String>,
) -> Result<Session, String> {
    let access_token = value
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "登录成功，但站点没有返回访问令牌".to_string())?
        .to_string();
    let refresh_token = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or(fallback_refresh);

    Ok(Session {
        access_token,
        refresh_token,
    })
}

async fn post_auth(client: &Client, path: &str, body: Value) -> Result<Value, String> {
    let response = client
        .post(format!("{API_BASE}{path}"))
        .json(&body)
        .send()
        .await
        .map_err(|error| format!("无法连接 NekoAPI：{error}"))?;
    response_json(response).await
}

#[derive(Debug, Serialize, Deserialize)]
struct AltchaChallenge {
    parameters: AltchaParameters,
    signature: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AltchaParameters {
    algorithm: String,
    nonce: String,
    salt: String,
    cost: u32,
    key_length: usize,
    key_prefix: String,
    data: Value,
    expires_at: Option<u64>,
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    if value.len() % 2 != 0 {
        return Err("站点验证码参数无效".to_string());
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| "站点验证码参数无效".to_string())
        })
        .collect()
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut key_block = [0u8; 64];
    if key.len() > 64 {
        key_block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Vec::with_capacity(64 + message.len());
    inner.extend(key_block.iter().map(|byte| byte ^ 0x36));
    inner.extend_from_slice(message);
    let inner_hash = Sha256::digest(&inner);
    let mut outer = Vec::with_capacity(64 + 32);
    outer.extend(key_block.iter().map(|byte| byte ^ 0x5c));
    outer.extend_from_slice(&inner_hash);
    Sha256::digest(&outer).into()
}

fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32, key_length: usize) -> Vec<u8> {
    let mut result = Vec::with_capacity(key_length);
    for block_index in 1..=((key_length + 31) / 32) {
        let mut block_input = salt.to_vec();
        block_input.extend_from_slice(&(block_index as u32).to_be_bytes());
        let mut u = hmac_sha256(password, &block_input);
        let mut block = u;
        for _ in 1..iterations {
            u = hmac_sha256(password, &u);
            for (target, byte) in block.iter_mut().zip(u) {
                *target ^= byte;
            }
        }
        result.extend_from_slice(&block);
    }
    result.truncate(key_length);
    result
}

async fn altcha_payload(client: &Client) -> Result<String, String> {
    let response = client
        .get(format!("{API_BASE}/auth/captcha/altcha/challenge"))
        .send()
        .await
        .map_err(|error| format!("无法连接 NekoAPI：{error}"))?;
    let challenge: AltchaChallenge = serde_json::from_value(response_json(response).await?)
        .map_err(|_| "站点验证码响应格式无效".to_string())?;
    if challenge.parameters.algorithm != "PBKDF2/SHA-256" {
        return Err("站点验证码算法暂不支持，请更新应用".to_string());
    }
    let nonce = decode_hex(&challenge.parameters.nonce)?;
    let salt = decode_hex(&challenge.parameters.salt)?;
    let prefix = challenge.parameters.key_prefix.to_ascii_lowercase();
    let solution_started = Instant::now();
    let mut counter = 0u32;
    let solution = loop {
        let mut password = nonce.clone();
        password.extend_from_slice(&counter.to_be_bytes());
        let derived = pbkdf2_sha256(
            &password,
            &salt,
            challenge.parameters.cost,
            challenge.parameters.key_length,
        );
        let derived_hex = derived
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if derived_hex.starts_with(&prefix) {
            break (counter, derived_hex);
        }
        counter = counter.saturating_add(1);
    };
    Ok(BASE64.encode(
        serde_json::to_vec(&json!({
            "challenge": {
                "parameters": challenge.parameters,
                "signature": challenge.signature,
            },
            "solution": {
                "counter": solution.0,
                "derivedKey": solution.1,
                "took": solution_started.elapsed().as_millis(),
            },
        }))
        .map_err(|error| error.to_string())?,
    ))
}

fn is_expired_captcha_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    error.contains("verification expired")
        || error.contains("verification is invalid")
        || (error.contains("captcha") && (error.contains("expired") || error.contains("invalid")))
        || (error.contains("altcha") && (error.contains("expired") || error.contains("invalid")))
}

#[tauri::command]
async fn login(
    state: tauri::State<'_, ApiState>,
    account: String,
    password: String,
) -> Result<LoginOutcome, String> {
    let account = account.trim();
    if account.is_empty() || password.is_empty() {
        return Err("请输入账号和密码".to_string());
    }

    // The upstream accepts an email login. Supplying username as well keeps this
    // compatible with deployments that enable username-based authentication.
    let mut login_value = None;
    for attempt in 0..=1 {
        let altcha_payload = altcha_payload(&state.client).await?;
        let result = post_auth(
            &state.client,
            "/auth/login",
            // The web client sends ALTCHA's payload through the shared challenge
            // field used by the other captcha providers.
            json!({ "email": account, "username": account, "password": password, "turnstile_token": altcha_payload }),
        )
        .await;
        match result {
            Ok(value) => {
                login_value = Some(value);
                break;
            }
            Err(error) if attempt == 0 && is_expired_captcha_error(&error) => continue,
            Err(error) => return Err(error),
        }
    }
    let value = login_value.ok_or_else(|| "验证码已过期，请重试登录".to_string())?;

    if value
        .get("requires_2fa")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let temp_token = value
            .get("temp_token")
            .and_then(Value::as_str)
            .ok_or_else(|| "站点要求二次验证，但没有返回临时令牌".to_string())?;
        return Ok(LoginOutcome::TwoFactor {
            temp_token: temp_token.to_string(),
        });
    }

    save_session(&session_from_payload(&value, None)?)?;
    Ok(LoginOutcome::Authenticated)
}

#[tauri::command]
async fn login_two_factor(
    state: tauri::State<'_, ApiState>,
    temp_token: String,
    code: String,
) -> Result<LoginOutcome, String> {
    let code = code.trim();
    if code.len() != 6 || !code.chars().all(|character| character.is_ascii_digit()) {
        return Err("请输入 6 位验证码".to_string());
    }

    let value = post_auth(
        &state.client,
        "/auth/login/2fa",
        json!({ "temp_token": temp_token, "totp_code": code }),
    )
    .await?;
    save_session(&session_from_payload(&value, None)?)?;
    Ok(LoginOutcome::Authenticated)
}

async fn refresh_session(client: &Client, session: &Session) -> Result<Session, String> {
    let refresh_token = session
        .refresh_token
        .as_ref()
        .ok_or_else(|| "登录已过期，请重新登录".to_string())?;
    let value = post_auth(
        client,
        "/auth/refresh",
        json!({ "refresh_token": refresh_token }),
    )
    .await?;
    let refreshed = session_from_payload(&value, session.refresh_token.clone())?;
    save_session(&refreshed)?;
    Ok(refreshed)
}

async fn get_authorized(
    client: &Client,
    path: &str,
    token: &str,
) -> Result<(StatusCode, Value), String> {
    let response = client
        .get(format!("{API_BASE}{path}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| format!("无法连接 NekoAPI：{error}"))?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .await
        .map_err(|_| format!("站点返回了无法解析的数据（HTTP {status}）"))?;
    Ok((status, value))
}

async fn post_authorized(
    client: &Client,
    path: &str,
    token: &str,
) -> Result<(StatusCode, Value), String> {
    let response = client
        .post(format!("{API_BASE}{path}"))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_LENGTH, "0")
        .send()
        .await
        .map_err(|error| format!("无法连接 NekoAPI：{error}"))?;
    let status = response.status();
    let value = response
        .json::<Value>()
        .await
        .map_err(|_| format!("站点返回了无法解析的数据（HTTP {status}）"))?;
    Ok((status, value))
}

async fn fetch_dashboard_data(client: &Client, token: &str) -> Result<DashboardData, String> {
    let (
        profile_result,
        stats_result,
        subscriptions_result,
        api_keys_result,
        groups_result,
        channel_monitors_result,
    ) = tokio::join!(
        get_authorized(client, "/user/profile", token),
        get_authorized(client, "/usage/dashboard/stats", token),
        get_authorized(client, "/subscriptions/active", token),
        get_authorized(client, "/keys", token),
        get_authorized(client, "/groups/available", token),
        get_authorized(client, "/channel-monitors", token)
    );
    let (profile_status, profile) = profile_result?;
    let (stats_status, stats) = stats_result?;
    let (subscriptions_status, subscriptions) = subscriptions_result?;
    let (api_keys_status, api_keys) = api_keys_result?;
    let (groups_status, groups) = groups_result?;
    let (channel_monitors_status, channel_monitors) = channel_monitors_result?;

    if profile_status == StatusCode::UNAUTHORIZED
        || stats_status == StatusCode::UNAUTHORIZED
        || subscriptions_status == StatusCode::UNAUTHORIZED
        || api_keys_status == StatusCode::UNAUTHORIZED
        || groups_status == StatusCode::UNAUTHORIZED
        || channel_monitors_status == StatusCode::UNAUTHORIZED
    {
        return Err("__SESSION_EXPIRED__".to_string());
    }
    if !profile_status.is_success() {
        return Err(api_error(
            &profile,
            &format!("用户资料请求失败（HTTP {profile_status}）"),
        ));
    }
    if !stats_status.is_success() {
        return Err(api_error(
            &stats,
            &format!("统计请求失败（HTTP {stats_status}）"),
        ));
    }

    let (subscriptions, subscriptions_error) = if subscriptions_status.is_success() {
        (unwrap_payload(subscriptions)?, None)
    } else {
        (
            Value::Array(Vec::new()),
            Some(api_error(
                &subscriptions,
                &format!("订阅请求失败（HTTP {subscriptions_status}）"),
            )),
        )
    };
    let (api_keys, api_keys_error) = if api_keys_status.is_success() {
        (unwrap_payload(api_keys)?, None)
    } else {
        (
            Value::Array(Vec::new()),
            Some(api_error(
                &api_keys,
                &format!("API 密钥请求失败（HTTP {api_keys_status}）"),
            )),
        )
    };
    let (groups, groups_error) = if groups_status.is_success() {
        (unwrap_payload(groups)?, None)
    } else {
        (
            Value::Array(Vec::new()),
            Some(api_error(
                &groups,
                &format!("分组请求失败（HTTP {groups_status}）"),
            )),
        )
    };
    let api_keys_error = api_keys_error.or(groups_error);
    let (channel_monitors, channel_monitors_error) = if channel_monitors_status.is_success() {
        (unwrap_payload(channel_monitors)?, None)
    } else {
        (
            Value::Array(Vec::new()),
            Some(api_error(
                &channel_monitors,
                &format!("渠道状态请求失败（HTTP {channel_monitors_status}）"),
            )),
        )
    };

    Ok(DashboardData {
        profile: unwrap_payload(profile)?,
        stats: unwrap_payload(stats)?,
        subscriptions,
        subscriptions_error,
        api_keys,
        api_keys_error,
        groups,
        channel_monitors,
        channel_monitors_error,
    })
}

fn json_number(value: &Value, key: &str) -> f64 {
    value
        .get(key)
        .and_then(|value| value.as_f64().or_else(|| value.as_str()?.parse().ok()))
        .unwrap_or_default()
}

fn compact_tokens(value: f64) -> String {
    if value >= 1_000_000.0 {
        let digits = if value >= 10_000_000.0 { 1 } else { 2 };
        format!("{:.*}M", digits, value / 1_000_000.0)
    } else if value >= 1_000.0 {
        format!("{:.1}K", value / 1_000.0)
    } else {
        format!("{value:.0}")
    }
}

fn subscription_items(subscriptions: &Value) -> &[Value] {
    subscriptions
        .as_array()
        .or_else(|| subscriptions.get("items").and_then(Value::as_array))
        .or_else(|| subscriptions.get("subscriptions").and_then(Value::as_array))
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn string_value(value: Option<&Value>, fallback: &str) -> String {
    value
        .and_then(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .or_else(|| value.as_i64().map(|number| number.to_string()))
                .or_else(|| value.as_u64().map(|number| number.to_string()))
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

fn subscription_summary(subscriptions: &Value) -> SubscriptionSummary {
    let mut candidates = Vec::new();
    let mut active_subscription_count = 0;

    for (index, subscription) in subscription_items(subscriptions).iter().enumerate() {
        let status = subscription
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("active");
        if status != "active" {
            continue;
        }
        active_subscription_count += 1;

        let group = subscription.get("group").unwrap_or(&Value::Null);
        let subscription_id = string_value(subscription.get("id"), &index.to_string());
        let subscription_name = string_value(
            group.get("name").or_else(|| subscription.get("name")),
            "订阅套餐",
        );
        let platform = string_value(
            group
                .get("platform")
                .or_else(|| subscription.get("platform")),
            "subscription",
        );

        for (period, limit_key, usage_key) in [
            (QuotaPeriod::Daily, "daily_limit_usd", "daily_usage_usd"),
            (QuotaPeriod::Weekly, "weekly_limit_usd", "weekly_usage_usd"),
            (
                QuotaPeriod::Monthly,
                "monthly_limit_usd",
                "monthly_usage_usd",
            ),
        ] {
            let limit = json_number(group, limit_key);
            if !limit.is_finite() || limit <= 0.0 {
                continue;
            }
            let used = json_number(subscription, usage_key).max(0.0);
            candidates.push(QuotaSummaryItem {
                subscription_id: subscription_id.clone(),
                subscription_name: subscription_name.clone(),
                platform: platform.clone(),
                period,
                used,
                limit,
                // Preserve over-quota values for truthful labels. Only visual
                // progress bars should clamp their width to 100%.
                percent: used / limit * 100.0,
            });
        }
    }

    candidates.sort_by(|left, right| {
        right
            .percent
            .total_cmp(&left.percent)
            .then_with(|| {
                left.period
                    .tie_break_priority()
                    .cmp(&right.period.tie_break_priority())
            })
            .then_with(|| left.subscription_id.cmp(&right.subscription_id))
    });

    SubscriptionSummary {
        active_subscription_count,
        quota_count: candidates.len(),
        selected: candidates.into_iter().next(),
    }
}

fn current_api_key_route(api_keys: &Value, groups: &Value) -> Option<ApiKeyRoute> {
    let key = subscription_items(api_keys)
        .iter()
        .filter(|key| {
            key.get("status")
                .and_then(Value::as_str)
                .unwrap_or("active")
                == "active"
        })
        .max_by(|left, right| {
            left.get("last_used_at")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .cmp(
                    right
                        .get("last_used_at")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
                .then_with(|| json_number(left, "id").total_cmp(&json_number(right, "id")))
        })?;
    let group_id = string_value(key.get("group_id"), "");
    let group = subscription_items(groups)
        .iter()
        .find(|group| string_value(group.get("id"), "") == group_id)?;

    Some(ApiKeyRoute {
        key_id: string_value(key.get("id"), ""),
        key_name: string_value(key.get("name"), "API Key"),
        group_id,
        group_name: string_value(group.get("name"), "未知分组"),
        platform: string_value(group.get("platform"), ""),
    })
}

fn is_discount_route(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    ["特惠", "优惠", "discount", "budget", "cheap"]
        .iter()
        .any(|marker| normalized.contains(marker))
}

fn channel_light_state(
    channel_monitors: &Value,
    platform: Option<&str>,
    group_name: Option<&str>,
) -> ChannelLightState {
    let normalized_platform = platform.map(str::to_ascii_lowercase);
    let discount_route = group_name.map(is_discount_route);
    subscription_items(channel_monitors)
        .iter()
        .filter(|monitor| {
            let Some(platform) = normalized_platform.as_ref() else {
                return true;
            };
            monitor
                .get("provider")
                .or_else(|| monitor.get("platform"))
                .and_then(Value::as_str)
                .is_some_and(|provider| provider.eq_ignore_ascii_case(platform))
        })
        .filter(|monitor| {
            let Some(discount_route) = discount_route else {
                return true;
            };
            let monitor_route = monitor
                .get("group_name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .or_else(|| monitor.get("name").and_then(Value::as_str))
                .unwrap_or_default();
            is_discount_route(monitor_route) == discount_route
        })
        .map(|monitor| {
            match monitor
                .get("primary_status")
                .or_else(|| monitor.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
            {
                "operational" => ChannelLightState::Operational,
                "degraded" => ChannelLightState::Degraded,
                "failed" | "error" => ChannelLightState::Failed,
                _ => ChannelLightState::Unknown,
            }
        })
        .max_by_key(|state| state.severity())
        .unwrap_or(ChannelLightState::Unknown)
}

fn status_light_icon(state: ChannelLightState, lottery_marker: bool) -> Image<'static> {
    const SIZE: u32 = 32;
    const CENTER: f32 = (SIZE as f32 - 1.0) / 2.0;
    let mut rgba = vec![0; (SIZE * SIZE * 4) as usize];
    let fill = state.color();

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - CENTER;
            let dy = y as f32 - CENTER;
            let distance = (dx * dx + dy * dy).sqrt();
            let pixel = ((y * SIZE + x) * 4) as usize;
            // The UI uses a small flat dot with a subtle same-color glow.
            // Render the same shape at tray resolution without the previous
            // white ring, which looked like a generic traffic light.
            let mut color = if distance <= 6.5 {
                fill
            } else if distance <= 10.5 {
                let fade = ((10.5 - distance) / 4.0).clamp(0.0, 1.0);
                [
                    fill[0],
                    fill[1],
                    fill[2],
                    (55.0 * fade * fade).round() as u8,
                ]
            } else {
                [0, 0, 0, 0]
            };
            // An in-progress lucky draw is surfaced as a small amber badge on
            // the status light, on every platform.
            if lottery_marker {
                let badge_dx = x as f32 - 25.0;
                let badge_dy = y as f32 - 7.0;
                if (badge_dx * badge_dx + badge_dy * badge_dy).sqrt() <= 4.5 {
                    color = [226, 178, 108, 255];
                }
            }
            rgba[pixel..pixel + 4].copy_from_slice(&color);
        }
    }

    Image::new_owned(rgba, SIZE, SIZE)
}

fn update_tray_summary(
    app: &tauri::AppHandle,
    stats: &Value,
    summary: &SubscriptionSummary,
    current_api_key: Option<&ApiKeyRoute>,
    channel_monitors: &Value,
) {
    let today_tokens = json_number(stats, "today_tokens");
    let token_label = compact_tokens(today_tokens);
    // The light follows the most recently used active API key. Quota text can
    // still come from a different subscription.
    let channel_state = channel_light_state(
        channel_monitors,
        current_api_key
            .map(|key| key.platform.as_str())
            .or_else(|| {
                summary
                    .selected
                    .as_ref()
                    .map(|quota| quota.platform.as_str())
            }),
        current_api_key.map(|key| key.group_name.as_str()),
    );
    let key_route_label = current_api_key
        .map(|key| format!("\n当前密钥：{} · {}", key.key_name, key.group_name))
        .unwrap_or_default();
    let (title, tooltip) = if let Some(quota) = &summary.selected {
        let extra_subscriptions = summary.active_subscription_count.saturating_sub(1);
        let extra_label = if extra_subscriptions > 0 {
            format!("\n另有 {extra_subscriptions} 个有效订阅")
        } else {
            String::new()
        };
        (
            format!("{token_label} · {:.0}%", quota.percent),
            format!(
                "渠道状态：{}\n今日 Token {token_label}\n最高占用：{} · {}\n${:.2} / ${:.2}（{:.1}%）{}{}",
                channel_state.label(),
                quota.subscription_name,
                quota.period.label(),
                quota.used,
                quota.limit,
                quota.percent,
                key_route_label,
                extra_label
            ),
        )
    } else {
        (
            format!("{token_label} · --%"),
            format!(
                "渠道状态：{}\n今日 Token {token_label}\n未检测到有效订阅额度（{} 个有效订阅）{}",
                channel_state.label(),
                summary.active_subscription_count,
                key_route_label,
            ),
        )
    };

    set_status_summary(app, &title, &tooltip, channel_state);
}

fn set_status_summary(
    app: &tauri::AppHandle,
    title: &str,
    tooltip: &str,
    channel_state: ChannelLightState,
) {
    let cache = app.state::<TrayCache>();
    if let Ok(mut current) = cache.title.lock() {
        *current = title.to_string();
    }
    if let Ok(mut current) = cache.tooltip.lock() {
        *current = tooltip.to_string();
    }
    if let Ok(mut current) = cache.channel_state.lock() {
        *current = Some(channel_state);
    }
    render_tray(app);
}

fn render_tray(app: &tauri::AppHandle) {
    let (title, tooltip, channel_state) = {
        let cache = app.state::<TrayCache>();
        let title = cache.title.lock().ok().map(|s| s.clone());
        let tooltip = cache
            .tooltip
            .lock()
            .ok()
            .map(|s| s.clone())
            .unwrap_or_default();
        let channel_state = cache
            .channel_state
            .lock()
            .ok()
            .and_then(|s| *s)
            .unwrap_or(ChannelLightState::Unknown);
        (title, tooltip, channel_state)
    };
    let marker = app
        .state::<LotteryMarker>()
        .0
        .lock()
        .map(|guard| *guard)
        .unwrap_or(false);

    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    // The icon can be updated even before the first dashboard sync, which keeps
    // the lucky-draw badge responsive when only the lottery poll has run.
    let _ = tray.set_icon_with_as_template(Some(status_light_icon(channel_state, marker)), false);

    let Some(title) = title else { return };
    if title.is_empty() {
        return;
    }
    // The amber badge on the status light already marks an in-progress lucky
    // draw, so the title stays the plain token/quota summary.
    // Windows truncates tray tooltips at 128 characters, so the lucky-draw
    // marker leads the tooltip to stay visible no matter how long the quota
    // and key details below it get.
    let marker_tooltip = if marker {
        "幸运抽奖进行中\n"
    } else {
        ""
    };
    let final_title = title;
    let final_tooltip = format!("{marker_tooltip}{tooltip}");

    #[cfg(target_os = "linux")]
    write_linux_panel_status(&final_title);

    let _ = tray.set_title(Some(final_title.as_str()));
    let _ = tray.set_tooltip(Some(final_tooltip.as_str()));
}

fn set_lottery_marker(app: &tauri::AppHandle, has_open_round: bool) {
    let marker_state = app.state::<LotteryMarker>();
    let changed = {
        let Ok(mut guard) = marker_state.0.lock() else {
            return;
        };
        if *guard == has_open_round {
            false
        } else {
            *guard = has_open_round;
            true
        }
    };
    if changed {
        render_tray(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_only_expired_or_invalid_captcha_errors() {
        assert!(is_expired_captcha_error(
            "verification expired or invalid; please try again"
        ));
        assert!(is_expired_captcha_error("ALTCHA challenge expired"));
        assert!(!is_expired_captcha_error("Invalid username or password"));
        assert!(!is_expired_captcha_error("Too many login attempts"));
    }

    #[test]
    fn selects_highest_usage_across_subscriptions_and_periods() {
        let subscriptions = json!([
            {
                "id": 1,
                "status": "active",
                "daily_usage_usd": 9,
                "weekly_usage_usd": 20,
                "group": {
                    "name": "Claude",
                    "platform": "anthropic",
                    "daily_limit_usd": 10,
                    "weekly_limit_usd": 50
                }
            },
            {
                "id": 2,
                "status": "active",
                "monthly_usage_usd": 12,
                "group": {
                    "name": "OpenAI",
                    "platform": "openai",
                    "monthly_limit_usd": 100
                }
            }
        ]);

        let summary = subscription_summary(&subscriptions);
        let selected = summary.selected.expect("a quota should be selected");

        assert_eq!(summary.active_subscription_count, 2);
        assert_eq!(summary.quota_count, 3);
        assert_eq!(selected.subscription_name, "Claude");
        assert_eq!(selected.period, QuotaPeriod::Daily);
        assert_eq!(selected.percent, 90.0);
    }

    #[test]
    fn prefers_shorter_period_when_percentages_are_equal() {
        let subscriptions = json!([{
            "id": 1,
            "status": "active",
            "daily_usage_usd": 5,
            "monthly_usage_usd": 50,
            "group": {
                "name": "Equal Quotas",
                "daily_limit_usd": 10,
                "monthly_limit_usd": 100
            }
        }]);

        let selected = subscription_summary(&subscriptions)
            .selected
            .expect("a quota should be selected");

        assert_eq!(selected.period, QuotaPeriod::Daily);
    }

    #[test]
    fn preserves_over_quota_percentage() {
        let subscriptions = json!([{
            "id": 1,
            "status": "active",
            "daily_usage_usd": 12.5,
            "group": {
                "daily_limit_usd": 10
            }
        }]);

        let selected = subscription_summary(&subscriptions)
            .selected
            .expect("a quota should be selected");

        assert_eq!(selected.percent, 125.0);
    }

    #[test]
    fn ignores_inactive_subscriptions_and_zero_limits() {
        let subscriptions = json!([
            {
                "id": 1,
                "status": "expired",
                "daily_usage_usd": 10,
                "group": { "daily_limit_usd": 10 }
            },
            {
                "id": 2,
                "status": "active",
                "daily_usage_usd": 1,
                "group": { "daily_limit_usd": 0 }
            }
        ]);

        let summary = subscription_summary(&subscriptions);

        assert_eq!(summary.active_subscription_count, 1);
        assert_eq!(summary.quota_count, 0);
        assert!(summary.selected.is_none());
    }

    #[test]
    fn accepts_wrapped_subscription_arrays() {
        let subscriptions = json!({
            "items": [{
                "id": "wrapped",
                "status": "active",
                "monthly_usage_usd": "25",
                "group": {
                    "name": "Wrapped",
                    "monthly_limit_usd": "100"
                }
            }]
        });

        let selected = subscription_summary(&subscriptions)
            .selected
            .expect("a quota should be selected");

        assert_eq!(selected.subscription_id, "wrapped");
        assert_eq!(selected.percent, 25.0);
    }

    #[test]
    fn selects_the_worst_status_for_the_active_subscription_platform() {
        let monitors = json!({
            "items": [
                { "provider": "anthropic", "primary_status": "operational" },
                { "provider": "openai", "primary_status": "operational" },
                { "provider": "anthropic", "primary_status": "degraded" },
                { "provider": "gemini", "primary_status": "failed" }
            ]
        });

        assert_eq!(
            channel_light_state(&monitors, Some("anthropic"), None),
            ChannelLightState::Degraded
        );
        assert_eq!(
            channel_light_state(&monitors, Some("openai"), None),
            ChannelLightState::Operational
        );
    }

    #[test]
    fn normal_openai_group_does_not_inherit_discount_channel_failure() {
        let monitors = json!({
            "items": [
                {
                    "name": "gpt-5.6-sol（随时拉闸特惠）",
                    "provider": "openai",
                    "primary_status": "error"
                },
                {
                    "name": "GPT-5.6 Sol",
                    "provider": "openai",
                    "primary_status": "operational"
                }
            ]
        });

        assert_eq!(
            channel_light_state(&monitors, Some("openai"), Some("pro")),
            ChannelLightState::Operational
        );
    }

    #[test]
    fn resolves_the_most_recently_used_api_key_group() {
        let keys = json!({
            "items": [
                {
                    "id": 1,
                    "name": "特惠 key",
                    "group_id": 39,
                    "status": "active",
                    "last_used_at": "2026-08-23T10:00:00+08:00"
                },
                {
                    "id": 2,
                    "name": "正常 key",
                    "group_id": 32,
                    "status": "active",
                    "last_used_at": "2026-08-24T10:00:00+08:00"
                }
            ]
        });
        let groups = json!([
            { "id": 32, "name": "pro订阅卡", "platform": "openai" },
            { "id": 39, "name": "特惠pro订阅卡", "platform": "openai" }
        ]);

        assert_eq!(
            current_api_key_route(&keys, &groups),
            Some(ApiKeyRoute {
                key_id: "2".to_string(),
                key_name: "正常 key".to_string(),
                group_id: "32".to_string(),
                group_name: "pro订阅卡".to_string(),
                platform: "openai".to_string(),
            })
        );
    }

    #[test]
    fn falls_back_to_worst_published_status_without_selected_subscription() {
        let monitors = json!([
            { "provider": "anthropic", "primary_status": "operational" },
            { "provider": "openai", "primary_status": "error" }
        ]);

        assert_eq!(
            channel_light_state(&monitors, None, None),
            ChannelLightState::Failed
        );
    }

    #[test]
    fn status_light_icon_has_expected_dimensions_and_center_color() {
        let icon = status_light_icon(ChannelLightState::Operational, false);
        let debug = format!("{icon:?}");

        assert!(debug.contains("width: 32"));
        assert!(debug.contains("height: 32"));
        assert!(debug.contains("4096"));
    }

    #[test]
    fn lottery_badge_changes_the_status_light_icon() {
        let plain = status_light_icon(ChannelLightState::Operational, false)
            .rgba()
            .to_vec();
        let badged = status_light_icon(ChannelLightState::Operational, true)
            .rgba()
            .to_vec();

        assert_eq!(plain.len(), 32 * 32 * 4);
        assert_eq!(badged.len(), plain.len());
        assert_ne!(plain, badged);
    }

    #[test]
    fn parses_lottery_snapshot_with_open_round() {
        let payload = json!({
            "enabled": true,
            "current_round": {
                "id": 42,
                "number": 7,
                "title": "测试抽奖",
                "status": "open",
                "participant_count": "10",
                "target_participants": 100,
                "reward_amount": "5.00",
                "closes_at": "2026-09-15T10:00:00Z"
            },
            "eligibility": { "eligible": true }
        });
        let snapshot = parse_lottery_snapshot(payload, 1_700_000_000);
        assert!(snapshot.server_enabled);
        assert!(snapshot.has_open_round);
        let round = snapshot.current_round.expect("round present");
        assert_eq!(round.id, 42);
        assert_eq!(round.number, Some(7));
        assert_eq!(round.title.as_deref(), Some("测试抽奖"));
        assert_eq!(round.reward_amount, Some(5.0));
        assert_eq!(round.participant_count, Some(10));
        assert_eq!(round.target_participants, Some(100));
        assert_eq!(snapshot.eligibility.eligible, Some(true));
        assert!(!snapshot.participated);
    }

    #[test]
    fn parses_lottery_participation_and_ineligibility() {
        let payload = json!({
            "enabled": true,
            "participated": true,
            "current_round": {
                "id": 8,
                "number": 3,
                "status": "open",
                "participant_count": 4,
                "target_participants": 20
            },
            "eligibility": {
                "eligible": false,
                "reason_code": "email_verification_required",
                "reason": "需要先验证邮箱"
            }
        });
        let snapshot = parse_lottery_snapshot(payload, 0);
        assert!(snapshot.participated);
        assert_eq!(snapshot.eligibility.eligible, Some(false));
        assert_eq!(
            snapshot.eligibility.reason_code.as_deref(),
            Some("email_verification_required")
        );
        assert_eq!(
            snapshot.eligibility.reason.as_deref(),
            Some("需要先验证邮箱")
        );
    }

    #[test]
    fn parses_lottery_snapshot_ignores_closed_or_full_rounds() {
        let closed = json!({
            "enabled": true,
            "current_round": {
                "id": 1,
                "status": "closed",
                "participant_count": 5,
                "target_participants": 10
            }
        });
        assert!(!parse_lottery_snapshot(closed, 0).has_open_round);

        let full = json!({
            "enabled": true,
            "current_round": {
                "id": 2,
                "status": "open",
                "participant_count": 100,
                "target_participants": 100
            }
        });
        assert!(!parse_lottery_snapshot(full, 0).has_open_round);
    }

    #[test]
    fn parses_lottery_snapshot_without_current_round() {
        let payload = json!({ "enabled": false, "current_round": null });
        let snapshot = parse_lottery_snapshot(payload, 0);
        assert!(!snapshot.server_enabled);
        assert!(!snapshot.has_open_round);
        assert!(snapshot.current_round.is_none());
    }

    #[test]
    fn lottery_notification_body_includes_reward_and_progress() {
        let round = LotteryRoundInfo {
            id: 9,
            number: Some(12),
            title: Some("嘉年华".to_string()),
            status: Some("open".to_string()),
            reward_amount: Some(3.5),
            participant_count: Some(15),
            target_participants: Some(30),
            closes_at: Some("2026-12-31T15:59:00Z".to_string()),
        };
        let body = format_lottery_notification_body(&round);
        assert!(body.contains("第12期"));
        assert!(body.contains("$3.50"));
        assert!(body.contains("15/30"));
    }
}

#[tauri::command]
async fn fetch_dashboard(
    app: tauri::AppHandle,
    state: tauri::State<'_, ApiState>,
) -> Result<DashboardSnapshot, String> {
    let mut session = load_session()?.ok_or_else(|| "请先登录".to_string())?;
    let data = match fetch_dashboard_data(&state.client, &session.access_token).await {
        Ok(data) => data,
        Err(error) if error == "__SESSION_EXPIRED__" => {
            session = refresh_session(&state.client, &session)
                .await
                .map_err(|error| {
                    let _ = delete_session();
                    error
                })?;
            fetch_dashboard_data(&state.client, &session.access_token)
                .await
                .map_err(|error| {
                    if error == "__SESSION_EXPIRED__" {
                        let _ = delete_session();
                        "登录已过期，请重新登录".to_string()
                    } else {
                        error
                    }
                })?
        }
        Err(error) => return Err(error),
    };

    let fetched_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let subscription_summary = subscription_summary(&data.subscriptions);
    let current_api_key = current_api_key_route(&data.api_keys, &data.groups);
    update_tray_summary(
        &app,
        &data.stats,
        &subscription_summary,
        current_api_key.as_ref(),
        &data.channel_monitors,
    );
    Ok(DashboardSnapshot {
        profile: data.profile,
        stats: data.stats,
        subscriptions: data.subscriptions,
        subscription_summary,
        subscriptions_error: data.subscriptions_error,
        current_api_key,
        api_keys_error: data.api_keys_error,
        channel_monitors: data.channel_monitors,
        channel_monitors_error: data.channel_monitors_error,
        fetched_at,
    })
}

async fn get_payload_with_refresh(
    client: &Client,
    path: &str,
    session: &mut Session,
) -> Result<Value, String> {
    let (mut status, mut value) = get_authorized(client, path, &session.access_token).await?;
    if status == StatusCode::UNAUTHORIZED {
        *session = refresh_session(client, session).await.map_err(|error| {
            let _ = delete_session();
            error
        })?;
        (status, value) = get_authorized(client, path, &session.access_token).await?;
    }

    if status == StatusCode::UNAUTHORIZED {
        let _ = delete_session();
        return Err("登录已过期，请重新登录".to_string());
    }
    if !status.is_success() {
        return Err(api_error(&value, &format!("请求失败（HTTP {status}）")));
    }
    unwrap_payload(value)
}

async fn post_payload_with_refresh(
    client: &Client,
    path: &str,
    session: &mut Session,
) -> Result<Value, String> {
    let (mut status, mut value) = post_authorized(client, path, &session.access_token).await?;
    if status == StatusCode::UNAUTHORIZED {
        *session = refresh_session(client, session).await.map_err(|error| {
            let _ = delete_session();
            error
        })?;
        (status, value) = post_authorized(client, path, &session.access_token).await?;
    }

    if status == StatusCode::UNAUTHORIZED {
        let _ = delete_session();
        return Err("登录已过期，请重新登录".to_string());
    }
    if !status.is_success() {
        return Err(api_error(&value, &format!("请求失败（HTTP {status}）")));
    }
    unwrap_payload(value)
}

fn valid_date(value: &str) -> bool {
    value.len() == 10
        && value
            .chars()
            .enumerate()
            .all(|(index, character)| match index {
                4 | 7 => character == '-',
                _ => character.is_ascii_digit(),
            })
}

fn urlencode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

#[tauri::command]
async fn fetch_usage_trend(
    state: tauri::State<'_, ApiState>,
    start_date: String,
    end_date: String,
) -> Result<Value, String> {
    if !valid_date(&start_date) || !valid_date(&end_date) {
        return Err("趋势日期格式无效".to_string());
    }
    let mut session = load_session()?.ok_or_else(|| "请先登录".to_string())?;
    let path = format!(
        "/usage/dashboard/trend?start_date={start_date}&end_date={end_date}&granularity=day"
    );
    get_payload_with_refresh(&state.client, &path, &mut session).await
}

#[tauri::command]
async fn fetch_usage_records(state: tauri::State<'_, ApiState>) -> Result<Value, String> {
    let mut session = load_session()?.ok_or_else(|| "请先登录".to_string())?;
    get_payload_with_refresh(
        &state.client,
        "/usage?page=1&page_size=18&sort_by=created_at&sort_order=desc",
        &mut session,
    )
    .await
}

const LOTTERY_STATE_FILE: &str = "lottery-state.json";

fn lottery_state_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .app_config_dir()
        .ok()
        .map(|dir| dir.join(LOTTERY_STATE_FILE))
}

fn load_lottery_settings(app: &tauri::AppHandle) -> LotterySettings {
    lottery_state_path(app)
        .and_then(|path| fs::read_to_string(&path).ok())
        .and_then(|contents| serde_json::from_str(&contents).ok())
        .unwrap_or(LotterySettings {
            enabled: true,
            last_notified_round_id: None,
        })
}

fn save_lottery_settings(app: &tauri::AppHandle, settings: &LotterySettings) -> Result<(), String> {
    let path = lottery_state_path(app).ok_or_else(|| "无法定位配置目录".to_string())?;
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let payload = serde_json::to_string(settings).map_err(|error| error.to_string())?;
    fs::write(&path, payload).map_err(|error| format!("保存抽奖状态失败：{error}"))
}

fn lottery_value_to_i64(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    if let Some(text) = value.as_str() {
        return text.parse::<i64>().ok();
    }
    if let Some(number) = value.as_f64() {
        return Some(number as i64);
    }
    None
}

fn lottery_value_to_f64(value: &Value) -> Option<f64> {
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    if let Some(text) = value.as_str() {
        return text.parse::<f64>().ok();
    }
    None
}

fn parse_lottery_snapshot(payload: Value, fetched_at: u64) -> LotterySnapshot {
    let server_enabled = payload
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let round = payload.get("current_round").cloned().unwrap_or(Value::Null);
    let has_round_object = round
        .as_object()
        .map(|object| !object.is_empty())
        .unwrap_or(false);
    let status = round.get("status").and_then(Value::as_str).unwrap_or("");
    let participant_count =
        lottery_value_to_i64(round.get("participant_count").unwrap_or(&Value::Null)).unwrap_or(0);
    let target_participants =
        lottery_value_to_i64(round.get("target_participants").unwrap_or(&Value::Null)).unwrap_or(0);
    let is_open = has_round_object && status == "open" && participant_count < target_participants;

    let current_round = if has_round_object {
        lottery_value_to_i64(round.get("id").unwrap_or(&Value::Null)).map(|id| LotteryRoundInfo {
            id,
            number: lottery_value_to_i64(round.get("number").unwrap_or(&Value::Null)),
            title: round
                .get("title")
                .and_then(Value::as_str)
                .map(String::from)
                .or_else(|| {
                    payload
                        .get("activity_title")
                        .and_then(Value::as_str)
                        .map(String::from)
                }),
            status: round
                .get("status")
                .and_then(Value::as_str)
                .map(String::from),
            reward_amount: lottery_value_to_f64(round.get("reward_amount").unwrap_or(&Value::Null)),
            participant_count: Some(participant_count),
            target_participants: Some(target_participants),
            closes_at: round
                .get("closes_at")
                .and_then(Value::as_str)
                .map(String::from),
        })
    } else {
        None
    };

    LotterySnapshot {
        server_enabled,
        has_open_round: is_open && current_round.is_some(),
        participated: payload
            .get("participated")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        eligibility: LotteryEligibility {
            eligible: payload
                .get("eligibility")
                .and_then(|value| value.get("eligible"))
                .and_then(Value::as_bool),
            reason_code: payload
                .get("eligibility")
                .and_then(|value| value.get("reason_code"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(String::from),
            reason: payload
                .get("eligibility")
                .and_then(|value| value.get("reason"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(String::from),
        },
        current_round,
        fetched_at,
    }
}

fn lottery_round_label(round: &LotteryRoundInfo) -> String {
    match round.number {
        Some(number) => format!("第{number}期"),
        None => "新一期".to_string(),
    }
}

fn format_lottery_notification_body(round: &LotteryRoundInfo) -> String {
    let mut body = lottery_round_label(round);
    if let Some(amount) = round.reward_amount {
        body.push_str(&format!(" · 单人中奖 ${amount:.2}"));
    }
    if let (Some(current), Some(target)) = (round.participant_count, round.target_participants) {
        if target > 0 {
            body.push_str(&format!(" · 已报名 {current}/{target}"));
        }
    }
    body
}

async fn fetch_lottery_payload(client: &Client, timezone: &str) -> Result<LotterySnapshot, String> {
    let trimmed = timezone.trim();
    let tz = if trimmed.is_empty() { "UTC" } else { trimmed };
    let mut session = load_session()?.ok_or_else(|| "请先登录".to_string())?;
    let path = format!("/lottery?timezone={}", urlencode(tz));
    let value = get_payload_with_refresh(client, &path, &mut session).await?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(parse_lottery_snapshot(value, now))
}

#[tauri::command]
async fn fetch_lottery(
    state: tauri::State<'_, ApiState>,
    timezone: String,
) -> Result<LotterySnapshot, String> {
    fetch_lottery_payload(&state.client, &timezone).await
}

#[tauri::command]
async fn poll_lottery(
    app: tauri::AppHandle,
    state: tauri::State<'_, ApiState>,
    lottery_state: tauri::State<'_, LotteryState>,
    timezone: String,
) -> Result<LotterySnapshot, String> {
    let snapshot = fetch_lottery_payload(&state.client, &timezone).await?;
    let enabled = lottery_state
        .enabled
        .lock()
        .map(|value| *value)
        .unwrap_or(true);
    let mut persisted = load_lottery_settings(&app);
    persisted.enabled = enabled;
    let last_notified = lottery_state
        .last_notified_round_id
        .lock()
        .ok()
        .and_then(|guard| *guard)
        .or(persisted.last_notified_round_id);

    let mut state_changed = false;
    if let Some(round) = &snapshot.current_round {
        if snapshot.has_open_round {
            let new_last = Some(round.id);
            if last_notified != new_last {
                if let Ok(mut guard) = lottery_state.last_notified_round_id.lock() {
                    *guard = new_last;
                }
                persisted.last_notified_round_id = new_last;
                state_changed = true;
                let first_poll = last_notified.is_none();
                if !first_poll && enabled {
                    let body = format_lottery_notification_body(round);
                    let title_text = round
                        .title
                        .clone()
                        .unwrap_or_else(|| "幸运抽奖".to_string());
                    if let Err(error) = app
                        .notification()
                        .builder()
                        .title(&title_text)
                        .body(&body)
                        .show()
                    {
                        eprintln!("[lottery] failed to show notification: {error}");
                    }
                }
            }
        }
    }
    if state_changed || persisted.enabled != enabled {
        let _ = save_lottery_settings(&app, &persisted);
    }
    set_lottery_marker(&app, snapshot.has_open_round);
    Ok(snapshot)
}

#[tauri::command]
async fn join_lottery(state: tauri::State<'_, ApiState>, round_id: i64) -> Result<Value, String> {
    if round_id <= 0 {
        return Err("抽奖期号无效".to_string());
    }
    let mut session = load_session()?.ok_or_else(|| "请先登录".to_string())?;
    let path = format!("/lottery/rounds/{round_id}/join");
    post_payload_with_refresh(&state.client, &path, &mut session).await
}

#[tauri::command]
fn get_lottery_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, LotteryState>,
) -> Result<LotterySettings, String> {
    let mut settings = load_lottery_settings(&app);
    if let Ok(guard) = state.enabled.lock() {
        settings.enabled = *guard;
    }
    Ok(settings)
}

#[tauri::command]
fn set_lottery_notification_enabled(
    app: tauri::AppHandle,
    state: tauri::State<'_, LotteryState>,
    enabled: bool,
) -> Result<(), String> {
    if let Ok(mut guard) = state.enabled.lock() {
        *guard = enabled;
    }
    let mut settings = load_lottery_settings(&app);
    settings.enabled = enabled;
    save_lottery_settings(&app, &settings)
}

#[tauri::command]
fn has_session(app: tauri::AppHandle) -> Result<bool, String> {
    let has_session = load_session()?.is_some();
    if !has_session {
        set_status_summary(
            &app,
            "未登录",
            "NekoAPI Widget · 未登录",
            ChannelLightState::Unknown,
        );
    }
    Ok(has_session)
}

#[tauri::command]
fn logout(app: tauri::AppHandle) -> Result<(), String> {
    delete_session()?;
    set_status_summary(
        &app,
        "未登录",
        "NekoAPI Widget · 未登录",
        ChannelLightState::Unknown,
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_panel_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(LINUX_PANEL_DIR))
}

#[cfg(target_os = "linux")]
fn linux_control_socket_path() -> Option<PathBuf> {
    linux_panel_dir().map(|path| path.join(LINUX_CONTROL_SOCKET))
}

#[cfg(target_os = "linux")]
fn linux_panel_bridge_enabled() -> bool {
    std::env::var("NEKOAPI_PANEL_BRIDGE").is_ok_and(|value| value == "1")
}

#[cfg(target_os = "linux")]
fn write_linux_panel_status(status: &str) {
    let Some(directory) = linux_panel_dir() else {
        return;
    };
    if fs::create_dir_all(&directory).is_ok() {
        let _ = fs::write(directory.join(LINUX_PANEL_STATUS_FILE), status);
    }
}

#[cfg(not(target_os = "windows"))]
fn position_bubble(window: &tauri::WebviewWindow, rect: Rect) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let tray_position = rect.position.to_physical::<f64>(scale);
    let tray_size = rect.size.to_physical::<f64>(scale);
    let window_size = match window.outer_size() {
        Ok(size) => size,
        Err(_) => return,
    };
    let anchor_x = tray_position.x + tray_size.width / 2.0;
    let anchor_y = tray_position.y + tray_size.height;
    let mut x = anchor_x - window_size.width as f64 / 2.0;
    let mut y = anchor_y + 4.0;

    if let Ok(Some(monitor)) = window.monitor_from_point(anchor_x, anchor_y) {
        let work = monitor.work_area();
        let min_x = work.position.x as f64 + 6.0;
        let max_x =
            (work.position.x as f64 + work.size.width as f64 - window_size.width as f64 - 6.0)
                .max(min_x);
        x = x.clamp(min_x, max_x);
        y = y.max(work.position.y as f64 + 2.0);
    }

    let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

#[cfg(target_os = "linux")]
fn position_bubble_at(window: &tauri::WebviewWindow, anchor_x: i32, anchor_y: i32) {
    // The GTK backend reports a stale physical size while this window is
    // hidden. These are the fixed logical dimensions from tauri.conf.json.
    let width = 392;
    let height = 720;
    let mut x = anchor_x - width / 2;
    let mut y = anchor_y + 4;

    if let Ok(Some(monitor)) = window.monitor_from_point(anchor_x as f64, anchor_y as f64) {
        let work = monitor.work_area();
        let min_x = work.position.x + 6;
        let max_x = (work.position.x + work.size.width as i32 - width - 6).max(min_x);
        let min_y = work.position.y + 2;
        let max_y = (work.position.y + work.size.height as i32 - height - 6).max(min_y);
        x = x.clamp(min_x, max_x);
        if y > max_y {
            y = anchor_y - height - 4;
        }
        y = y.clamp(min_y, max_y);
    }

    let _ = window.set_position(PhysicalPosition::new(x, y));
}

#[cfg(target_os = "windows")]
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct PopoverPlacement {
    placement: &'static str,
    anchor_offset: f64,
}

#[cfg(target_os = "windows")]
fn position_bubble(window: &tauri::WebviewWindow, rect: Rect) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let tray_position = rect.position.to_physical::<f64>(scale);
    let tray_size = rect.size.to_physical::<f64>(scale);
    let window_size = match window.outer_size() {
        Ok(size) => size,
        Err(_) => return,
    };
    let width = window_size.width as f64;
    let height = window_size.height as f64;
    let anchor_x = tray_position.x + tray_size.width / 2.0;
    let anchor_y = tray_position.y + tray_size.height / 2.0;

    let (x, y, placement, anchor_offset) =
        if let Ok(Some(monitor)) = window.monitor_from_point(anchor_x, anchor_y) {
            let full_left = monitor.position().x as f64;
            let full_top = monitor.position().y as f64;
            let full_right = full_left + monitor.size().width as f64;
            let full_bottom = full_top + monitor.size().height as f64;
            let work = monitor.work_area();
            let work_left = work.position.x as f64;
            let work_top = work.position.y as f64;
            let work_right = work_left + work.size.width as f64;
            let work_bottom = work_top + work.size.height as f64;
            let gap = 6.0;
            let min_x = work_left + gap;
            let max_x = (work_right - width - gap).max(min_x);
            let min_y = work_top + gap;
            let max_y = (work_bottom - height - gap).max(min_y);
            let centered_x = (anchor_x - width / 2.0).clamp(min_x, max_x);
            let centered_y = (anchor_y - height / 2.0).clamp(min_y, max_y);
            let distances = [
                (anchor_y - full_top).abs(),
                (full_bottom - anchor_y).abs(),
                (anchor_x - full_left).abs(),
                (full_right - anchor_x).abs(),
            ];
            let nearest_edge = distances
                .iter()
                .enumerate()
                .min_by(|left, right| left.1.total_cmp(right.1))
                .map(|(index, _)| index)
                .unwrap_or(1);

            match nearest_edge {
                // Taskbar at the top: open below it.
                0 => (
                    centered_x,
                    work_top + gap,
                    "below",
                    (anchor_x - centered_x).clamp(24.0, width - 24.0),
                ),
                // Taskbar at the bottom: open above it.
                1 => (
                    centered_x,
                    work_bottom - height - gap,
                    "above",
                    (anchor_x - centered_x).clamp(24.0, width - 24.0),
                ),
                // Taskbar at the left: open to its right.
                2 => (
                    work_left + gap,
                    centered_y,
                    "right",
                    (anchor_y - centered_y).clamp(24.0, height - 24.0),
                ),
                // Taskbar at the right: open to its left.
                _ => (
                    work_right - width - gap,
                    centered_y,
                    "left",
                    (anchor_y - centered_y).clamp(24.0, height - 24.0),
                ),
            }
        } else {
            let x = anchor_x - width / 2.0;
            let y = (tray_position.y - height - 6.0).max(0.0);
            (x, y, "above", width / 2.0)
        };

    let _ = window.emit(
        "popover-placement",
        PopoverPlacement {
            placement,
            anchor_offset,
        },
    );
    let _ = window.set_position(PhysicalPosition::new(x.round() as i32, y.round() as i32));
}

fn toggle_main_window(app: &tauri::AppHandle, tray_rect: Option<Rect>) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        } else {
            let blurred_while_clicking_tray = app
                .state::<PopupState>()
                .last_blur
                .lock()
                .ok()
                .and_then(|last_blur| *last_blur)
                .is_some_and(|instant| instant.elapsed() < Duration::from_millis(250));
            if tray_rect.is_some() && blurred_while_clicking_tray {
                return;
            }
            if let Some(rect) = tray_rect {
                position_bubble(&window, rect);
            }
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

#[cfg(target_os = "linux")]
fn toggle_main_window_at(app: &tauri::AppHandle, anchor_x: i32, anchor_y: i32) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        } else {
            let blurred_while_clicking_panel = app
                .state::<PopupState>()
                .last_blur
                .lock()
                .ok()
                .and_then(|last_blur| *last_blur)
                .is_some_and(|instant| instant.elapsed() < Duration::from_millis(250));
            if blurred_while_clicking_panel {
                return;
            }
            position_bubble_at(&window, anchor_x, anchor_y);
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

#[cfg(target_os = "linux")]
fn start_linux_control_socket(app: tauri::AppHandle) {
    use std::os::unix::net::UnixListener;

    let Some(path) = linux_control_socket_path() else {
        return;
    };
    let Some(directory) = path.parent() else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    let _ = fs::remove_file(&path);
    let Ok(listener) = UnixListener::bind(path) else {
        return;
    };

    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut request = String::new();
            if BufReader::new(stream).read_line(&mut request).is_err() {
                continue;
            }
            let mut fields = request.split_whitespace();
            if fields.next() != Some("toggle") {
                continue;
            }
            let (Ok(x), Ok(y)) = (
                fields.next().unwrap_or_default().parse::<i32>(),
                fields.next().unwrap_or_default().parse::<i32>(),
            ) else {
                continue;
            };
            let app = app.clone();
            let callback_app = app.clone();
            let _ = app.run_on_main_thread(move || toggle_main_window_at(&callback_app, x, y));
        }
    });
}

#[cfg(target_os = "linux")]
pub fn handle_linux_toggle_request() -> bool {
    use std::os::unix::net::UnixStream;

    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--toggle") {
        return false;
    }
    let (Some(x), Some(y)) = (args.next(), args.next()) else {
        return true;
    };
    if let Some(path) = linux_control_socket_path() {
        if let Ok(mut stream) = UnixStream::connect(path) {
            let _ = writeln!(stream, "toggle {x} {y}");
        }
    }
    true
}

#[cfg(not(target_os = "linux"))]
pub fn handle_linux_toggle_request() -> bool {
    false
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏", true, None::<&str>)?;
    let refresh = MenuItem::with_id(app, "refresh", "刷新数据", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&toggle, &refresh, &quit])?;
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(status_light_icon(ChannelLightState::Unknown, false))
        .icon_as_template(false)
        .title("-- · --%")
        .tooltip("NekoAPI Widget")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "toggle" => toggle_main_window(app, None),
            "refresh" => {
                let _ = app.emit("refresh-requested", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                rect,
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                toggle_main_window(tray.app_handle(), Some(rect));
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("NekoAPI-Widget/0.5.2 (+low-frequency account monitor)")
        .build()
        .expect("failed to build HTTP client");

    tauri::Builder::default()
        .manage(ApiState { client })
        .manage(PopupState::default())
        .manage(LotteryState::default())
        .manage(LotteryMarker::default())
        .manage(TrayCache::default())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            login,
            login_two_factor,
            fetch_dashboard,
            fetch_usage_trend,
            fetch_usage_records,
            fetch_lottery,
            poll_lottery,
            join_lottery,
            get_lottery_settings,
            set_lottery_notification_enabled,
            has_session,
            logout
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Restore persisted lottery settings (enabled flag) into the
            // managed state so the frontend reads the same value the user
            // previously chose.
            let handle = app.handle().clone();
            let persisted = load_lottery_settings(&handle);
            if let Some(state) = app.try_state::<LotteryState>() {
                if let Ok(mut guard) = state.enabled.lock() {
                    *guard = persisted.enabled;
                }
                if let Ok(mut guard) = state.last_notified_round_id.lock() {
                    *guard = persisted.last_notified_round_id;
                }
            }

            #[cfg(target_os = "linux")]
            {
                if linux_panel_bridge_enabled() {
                    write_linux_panel_status("-- · --%");
                    start_linux_control_socket(app.handle().clone());
                } else {
                    build_tray(app)?;
                }
            }

            #[cfg(not(target_os = "linux"))]
            {
                build_tray(app)?;
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            tauri::WindowEvent::Focused(false) => {
                if let Ok(mut last_blur) =
                    window.app_handle().state::<PopupState>().last_blur.lock()
                {
                    *last_blur = Some(Instant::now());
                }
                let _ = window.hide();
            }
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running NekoAPI Widget");
}
