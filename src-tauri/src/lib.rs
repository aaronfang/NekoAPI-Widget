use keyring::Entry;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
#[cfg(target_os = "linux")]
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    thread,
};
use std::{
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, PhysicalPosition, Rect,
};

const API_BASE: &str = "https://999555999.com/api/v1";
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Session {
    access_token: String,
    refresh_token: Option<String>,
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
    channel_monitors: Value,
    channel_monitors_error: Option<String>,
    fetched_at: u64,
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
    let value = post_auth(
        &state.client,
        "/auth/login",
        json!({ "email": account, "username": account, "password": password }),
    )
    .await?;

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

async fn fetch_dashboard_data(
    client: &Client,
    token: &str,
) -> Result<(Value, Value, Value, Option<String>, Value, Option<String>), String> {
    let (profile_result, stats_result, subscriptions_result, channel_monitors_result) = tokio::join!(
        get_authorized(client, "/user/profile", token),
        get_authorized(client, "/usage/dashboard/stats", token),
        get_authorized(client, "/subscriptions/active", token),
        get_authorized(client, "/channel-monitors", token)
    );
    let (profile_status, profile) = profile_result?;
    let (stats_status, stats) = stats_result?;
    let (subscriptions_status, subscriptions) = subscriptions_result?;
    let (channel_monitors_status, channel_monitors) = channel_monitors_result?;

    if profile_status == StatusCode::UNAUTHORIZED
        || stats_status == StatusCode::UNAUTHORIZED
        || subscriptions_status == StatusCode::UNAUTHORIZED
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

    Ok((
        unwrap_payload(profile)?,
        unwrap_payload(stats)?,
        subscriptions,
        subscriptions_error,
        channel_monitors,
        channel_monitors_error,
    ))
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

fn channel_light_state(channel_monitors: &Value, platform: Option<&str>) -> ChannelLightState {
    let normalized_platform = platform.map(str::to_ascii_lowercase);
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

fn status_light_icon(state: ChannelLightState) -> Image<'static> {
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
            let color = if distance <= 6.5 {
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
            rgba[pixel..pixel + 4].copy_from_slice(&color);
        }
    }

    Image::new_owned(rgba, SIZE, SIZE)
}

fn update_tray_summary(
    app: &tauri::AppHandle,
    stats: &Value,
    summary: &SubscriptionSummary,
    channel_monitors: &Value,
) {
    let today_tokens = json_number(stats, "today_tokens");
    let token_label = compact_tokens(today_tokens);
    // Keep the light and percentage about the same subscription. If there is
    // no quota-selected subscription, fall back to the worst published status.
    let channel_state = channel_light_state(
        channel_monitors,
        summary
            .selected
            .as_ref()
            .map(|quota| quota.platform.as_str()),
    );
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
                "渠道状态：{}\n今日 Token {token_label}\n最高占用：{} · {}\n${:.2} / ${:.2}（{:.1}%）{}",
                channel_state.label(),
                quota.subscription_name,
                quota.period.label(),
                quota.used,
                quota.limit,
                quota.percent,
                extra_label
            ),
        )
    } else {
        (
            format!("{token_label} · --%"),
            format!(
                "渠道状态：{}\n今日 Token {token_label}\n未检测到有效订阅额度（{} 个有效订阅）",
                channel_state.label(),
                summary.active_subscription_count,
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
    #[cfg(target_os = "linux")]
    write_linux_panel_status(title);

    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_icon_with_as_template(Some(status_light_icon(channel_state)), false);
        let _ = tray.set_title(Some(title));
        let _ = tray.set_tooltip(Some(tooltip));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            channel_light_state(&monitors, Some("anthropic")),
            ChannelLightState::Degraded
        );
        assert_eq!(
            channel_light_state(&monitors, Some("openai")),
            ChannelLightState::Operational
        );
    }

    #[test]
    fn falls_back_to_worst_published_status_without_selected_subscription() {
        let monitors = json!([
            { "provider": "anthropic", "primary_status": "operational" },
            { "provider": "openai", "primary_status": "error" }
        ]);

        assert_eq!(
            channel_light_state(&monitors, None),
            ChannelLightState::Failed
        );
    }

    #[test]
    fn status_light_icon_has_expected_dimensions_and_center_color() {
        let icon = status_light_icon(ChannelLightState::Operational);
        let debug = format!("{icon:?}");

        assert!(debug.contains("width: 32"));
        assert!(debug.contains("height: 32"));
        assert!(debug.contains("4096"));
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
    let subscription_summary = subscription_summary(&data.2);
    update_tray_summary(&app, &data.1, &subscription_summary, &data.4);
    Ok(DashboardSnapshot {
        profile: data.0,
        stats: data.1,
        subscriptions: data.2,
        subscription_summary,
        subscriptions_error: data.3,
        channel_monitors: data.4,
        channel_monitors_error: data.5,
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
        .icon(status_light_icon(ChannelLightState::Unknown))
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
        .user_agent("NekoAPI-Widget/0.5.0 (+low-frequency account monitor)")
        .build()
        .expect("failed to build HTTP client");

    tauri::Builder::default()
        .manage(ApiState { client })
        .manage(PopupState::default())
        .invoke_handler(tauri::generate_handler![
            login,
            login_two_factor,
            fetch_dashboard,
            fetch_usage_trend,
            fetch_usage_records,
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
