use keyring::Entry;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager,
};

const API_BASE: &str = "https://999555999.com/api/v1";
const KEYRING_SERVICE: &str = "com.nekoapi.widget";
const KEYRING_USER: &str = "session";

#[derive(Clone)]
struct ApiState {
    client: Client,
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
    subscriptions_error: Option<String>,
    fetched_at: u64,
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
) -> Result<(Value, Value, Value, Option<String>), String> {
    let (profile_result, stats_result, subscriptions_result) = tokio::join!(
        get_authorized(client, "/user/profile", token),
        get_authorized(client, "/usage/dashboard/stats", token),
        get_authorized(client, "/subscriptions/active", token)
    );
    let (profile_status, profile) = profile_result?;
    let (stats_status, stats) = stats_result?;
    let (subscriptions_status, subscriptions) = subscriptions_result?;

    if profile_status == StatusCode::UNAUTHORIZED
        || stats_status == StatusCode::UNAUTHORIZED
        || subscriptions_status == StatusCode::UNAUTHORIZED
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

    Ok((
        unwrap_payload(profile)?,
        unwrap_payload(stats)?,
        subscriptions,
        subscriptions_error,
    ))
}

#[tauri::command]
async fn fetch_dashboard(state: tauri::State<'_, ApiState>) -> Result<DashboardSnapshot, String> {
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
    Ok(DashboardSnapshot {
        profile: data.0,
        stats: data.1,
        subscriptions: data.2,
        subscriptions_error: data.3,
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
fn has_session() -> Result<bool, String> {
    Ok(load_session()?.is_some())
}

#[tauri::command]
fn logout() -> Result<(), String> {
    delete_session()
}

fn toggle_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            let _ = window.hide();
        } else {
            let _ = window.show();
            let _ = window.set_focus();
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .user_agent("NekoAPI-Widget/0.2 (+low-frequency account monitor)")
        .build()
        .expect("failed to build HTTP client");

    tauri::Builder::default()
        .manage(ApiState { client })
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
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            let toggle = MenuItem::with_id(app, "toggle", "显示 / 隐藏", true, None::<&str>)?;
            let refresh = MenuItem::with_id(app, "refresh", "刷新数据", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&toggle, &refresh, &quit])?;
            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or("missing application icon")?;

            TrayIconBuilder::new()
                .icon(icon)
                .tooltip("NekoAPI Widget")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "toggle" => toggle_main_window(app),
                    "refresh" => {
                        let _ = app.emit("refresh-requested", ());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_main_window(tray.app_handle());
                    }
                })
                .build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running NekoAPI Widget");
}
