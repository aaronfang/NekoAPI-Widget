<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { api, type DashboardSnapshot, type LoginOutcome } from './api'

const CACHE_KEY = 'nekoapi-widget:dashboard-cache:v2'
const VIEW_CACHE_KEY = 'nekoapi-widget:view-cache:v1'
const INTERVAL_KEY = 'nekoapi-widget:refresh-minutes'
const MANUAL_COOLDOWN_MS = 30_000
const DETAIL_STALE_MS = 15 * 60_000
const DAY_MS = 86_400_000

type Tab = 'overview' | 'records'
type JsonRecord = Record<string, unknown>

interface DetailCache {
  trend: unknown
  records: unknown
  trendFetchedAt: number
  recordsFetchedAt: number
}

interface QuotaRow {
  label: string
  used: number
  limit: number
  remaining: number
  percent: number
  resetAt?: string
}

interface SubscriptionCard {
  id: string
  name: string
  platform: string
  status: string
  expiresAt?: string
  quotas: QuotaRow[]
}

interface UsageRecord {
  id: string
  model: string
  tokens: number
  cost: number
  time?: string
  source: string
  success: boolean
}

const previewMode = import.meta.env.DEV && new URLSearchParams(location.search).has('preview')
const previewTab = previewMode && new URLSearchParams(location.search).get('tab') === 'records' ? 'records' : 'overview'
const previewSnapshot: DashboardSnapshot = {
  profile: { display_name: '瑞雪大侠', balance: 38.62 },
  stats: {
    today_actual_cost: 3.284,
    total_actual_cost: 126.48,
    today_tokens: 428_600,
    total_tokens: 18_730_000,
    today_input_tokens: 361_200,
    today_output_tokens: 67_400,
  },
  subscriptions: [
    { id: 1, status: 'active', expires_at: '2026-08-26T00:00:00Z', daily_usage_usd: 6.72, weekly_usage_usd: 24.8, daily_window_start: '2026-07-18T00:00:00Z', weekly_window_start: '2026-07-14T00:00:00Z', group: { name: 'Claude Pro 开发者套餐', platform: 'anthropic', daily_limit_usd: 10, weekly_limit_usd: 50 } },
    { id: 2, status: 'active', expires_at: '2026-10-01T00:00:00Z', monthly_usage_usd: 18.4, monthly_window_start: '2026-07-01T00:00:00Z', group: { name: 'OpenAI Codex 月度包', platform: 'openai', monthly_limit_usd: 80 } },
  ],
  fetched_at: Math.floor(Date.now() / 1000),
}
const previewTrend = { trend: [
  { date: '2026-07-12', actual_cost: 1.2 }, { date: '2026-07-13', actual_cost: 2.9 },
  { date: '2026-07-14', actual_cost: .8 }, { date: '2026-07-15', actual_cost: 5.7 },
  { date: '2026-07-16', actual_cost: 3.4 }, { date: '2026-07-17', actual_cost: 4.9 },
  { date: '2026-07-18', actual_cost: 3.284 },
] }
const previewRecords = { items: Array.from({ length: 11 }, (_, index) => ({
  id: index + 1,
  model: index % 3 === 0 ? 'claude-sonnet-4-5' : index % 3 === 1 ? 'gpt-5.3-codex' : 'claude-opus-4',
  input_tokens: 12_400 + index * 3_240,
  output_tokens: 2_180 + index * 310,
  actual_cost: .08 + index * .027,
  created_at: new Date(Date.now() - index * 430_000).toISOString(),
  group: { name: index % 2 ? 'OpenAI Codex 月度包' : 'Claude Pro 开发者套餐' },
  status_code: index === 7 ? 429 : 200,
})) }

const detailCache = readDetailCache()
const authenticated = ref(previewMode)
const booting = ref(!previewMode)
const loading = ref(false)
const detailLoading = ref(false)
const settingsOpen = ref(false)
const activeTab = ref<Tab>(previewTab)
const authStep = ref<'login' | 'totp'>('login')
const account = ref('')
const password = ref('')
const totpCode = ref('')
const tempToken = ref('')
const error = ref('')
const notice = ref('')
const snapshot = ref<DashboardSnapshot | null>(previewMode ? previewSnapshot : readCache())
const trendPayload = ref<unknown>(previewMode ? previewTrend : detailCache.trend)
const recordsPayload = ref<unknown>(previewMode ? previewRecords : detailCache.records)
const trendFetchedAt = ref(previewMode ? Date.now() : detailCache.trendFetchedAt)
const recordsFetchedAt = ref(previewMode ? Date.now() : detailCache.recordsFetchedAt)
const refreshMinutes = ref(readInterval())
const failureCount = ref(0)
let refreshTimer: number | undefined
let noticeTimer: number | undefined
let lastManualRefresh = 0
let unlistenRefresh: UnlistenFn | undefined

function readCache(): DashboardSnapshot | null {
  try {
    const cached = localStorage.getItem(CACHE_KEY)
    return cached ? (JSON.parse(cached) as DashboardSnapshot) : null
  } catch {
    return null
  }
}

function readDetailCache(): DetailCache {
  try {
    const cached = localStorage.getItem(VIEW_CACHE_KEY)
    if (cached) return JSON.parse(cached) as DetailCache
  } catch { /* ignore an invalid cache */ }
  return { trend: null, records: null, trendFetchedAt: 0, recordsFetchedAt: 0 }
}

function persistDetails() {
  localStorage.setItem(VIEW_CACHE_KEY, JSON.stringify({
    trend: trendPayload.value,
    records: recordsPayload.value,
    trendFetchedAt: trendFetchedAt.value,
    recordsFetchedAt: recordsFetchedAt.value,
  }))
}

function readInterval(): number {
  const value = Number(localStorage.getItem(INTERVAL_KEY))
  return [15, 30, 60].includes(value) ? value : 15
}

function asRecord(value: unknown): JsonRecord {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonRecord : {}
}

function valueFrom(source: unknown, paths: string[][]): unknown {
  for (const path of paths) {
    let value: unknown = source
    let valid = true
    for (const key of path) {
      if (!value || typeof value !== 'object' || !(key in value)) {
        valid = false
        break
      }
      value = (value as JsonRecord)[key]
    }
    if (valid) return value
  }
  return undefined
}

function numberFrom(source: unknown, paths: string[][]): number {
  const value = valueFrom(source, paths)
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value === 'string' && value.trim() && Number.isFinite(Number(value))) return Number(value)
  return 0
}

function stringFrom(source: unknown, paths: string[][]): string | undefined {
  const value = valueFrom(source, paths)
  return typeof value === 'string' && value.trim() ? value : undefined
}

function arrayFrom(source: unknown, paths: string[][]): unknown[] {
  if (Array.isArray(source)) return source
  const value = valueFrom(source, paths)
  return Array.isArray(value) ? value : []
}

const profile = computed(() => snapshot.value?.profile ?? {})
const stats = computed(() => snapshot.value?.stats ?? {})
const balance = computed(() => numberFrom(profile.value, [['balance'], ['user', 'balance']]))
const displayName = computed(() => stringFrom(profile.value, [['display_name'], ['username'], ['email'], ['user', 'display_name'], ['user', 'email']]) ?? 'NekoAPI 用户')
const todayCost = computed(() => numberFrom(stats.value, [['today_actual_cost'], ['today_cost']]))
const totalCost = computed(() => numberFrom(stats.value, [['total_actual_cost'], ['total_cost']]))
const todayTokens = computed(() => numberFrom(stats.value, [['today_tokens']]))
const totalTokens = computed(() => numberFrom(stats.value, [['total_tokens']]))
const todayInput = computed(() => numberFrom(stats.value, [['today_input_tokens']]))
const todayOutput = computed(() => numberFrom(stats.value, [['today_output_tokens']]))

const subscriptions = computed<SubscriptionCard[]>(() => {
  return arrayFrom(snapshot.value?.subscriptions, [['items'], ['subscriptions']]).map((item, index) => {
    const subscription = asRecord(item)
    const group = asRecord(subscription.group)
    const quotas: QuotaRow[] = []
    const quotaDefinitions = [
      ['每日额度', 'daily_usage_usd', 'daily_limit_usd', 'daily_window_start', 24],
      ['每周额度', 'weekly_usage_usd', 'weekly_limit_usd', 'weekly_window_start', 168],
      ['每月额度', 'monthly_usage_usd', 'monthly_limit_usd', 'monthly_window_start', 720],
    ] as const

    for (const [label, usageKey, limitKey, windowKey, hours] of quotaDefinitions) {
      const limit = Number(group[limitKey] ?? 0)
      if (!Number.isFinite(limit) || limit <= 0) continue
      const used = Math.max(0, Number(subscription[usageKey] ?? 0))
      const windowStart = typeof subscription[windowKey] === 'string' ? subscription[windowKey] as string : undefined
      quotas.push({
        label,
        used,
        limit,
        remaining: Math.max(0, limit - used),
        percent: Math.min(100, limit ? used / limit * 100 : 0),
        resetAt: windowStart ? new Date(new Date(windowStart).getTime() + hours * 3_600_000).toISOString() : undefined,
      })
    }

    return {
      id: String(subscription.id ?? index),
      name: String(group.name ?? subscription.name ?? '订阅套餐'),
      platform: platformName(String(group.platform ?? subscription.platform ?? 'subscription')),
      status: String(subscription.status ?? 'active'),
      expiresAt: typeof subscription.expires_at === 'string' ? subscription.expires_at : undefined,
      quotas,
    }
  })
})

const primarySubscription = computed(() => subscriptions.value[0])

const trendPoints = computed(() => {
  const raw = arrayFrom(trendPayload.value, [['trend'], ['items'], ['data']])
  const points = raw.map((item, index) => {
    const row = asRecord(item)
    return {
      label: shortDate(String(row.date ?? row.day ?? row.period ?? row.time ?? '')),
      value: Number(row.actual_cost ?? row.cost ?? row.total_actual_cost ?? 0),
      tokens: Number(row.total_tokens ?? row.tokens ?? 0),
      index,
    }
  })
  const max = Math.max(...points.map(point => point.value), 0)
  return points.map(point => ({ ...point, height: max > 0 ? Math.max(5, point.value / max * 100) : 4 }))
})

const sevenDayCost = computed(() => trendPoints.value.reduce((sum, point) => sum + point.value, 0))

const usageRecords = computed<UsageRecord[]>(() => {
  return arrayFrom(recordsPayload.value, [['items'], ['records']]).map((item, index) => {
    const row = asRecord(item)
    const group = asRecord(row.group)
    const apiKey = asRecord(row.api_key)
    const tokens = Number(row.total_tokens ?? 0) || Number(row.input_tokens ?? 0) + Number(row.output_tokens ?? 0)
    const status = Number(row.status_code ?? row.status ?? 200)
    return {
      id: String(row.id ?? row.request_id ?? index),
      model: String(row.model ?? row.upstream_model ?? '未知模型'),
      tokens,
      cost: Number(row.actual_cost ?? row.total_cost ?? 0),
      time: typeof row.created_at === 'string' ? row.created_at : undefined,
      source: String(group.name ?? apiKey.name ?? row.request_type ?? 'API 请求'),
      success: !Number.isFinite(status) || status < 400,
    }
  })
})

function platformName(value: string): string {
  return ({ anthropic: 'Claude', openai: 'OpenAI', gemini: 'Gemini', antigravity: 'Antigravity', grok: 'Grok' } as Record<string, string>)[value] ?? value
}

function formatMoney(value: number, digits = 2): string {
  return new Intl.NumberFormat('zh-CN', { minimumFractionDigits: digits, maximumFractionDigits: 4 }).format(value)
}

function formatCompact(value: number): string {
  return new Intl.NumberFormat('zh-CN', { notation: value >= 10_000 ? 'compact' : 'standard', maximumFractionDigits: 1 }).format(value)
}

function formatTime(timestamp?: number): string {
  if (!timestamp) return '尚未同步'
  return new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit' }).format(timestamp * 1000)
}

function formatDateTime(value?: string): string {
  if (!value) return '时间未知'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return value
  return new Intl.DateTimeFormat('zh-CN', { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }).format(date)
}

function relativeDate(value?: string, prefix = '到期') {
  if (!value) return '长期有效'
  const remaining = new Date(value).getTime() - Date.now()
  if (!Number.isFinite(remaining)) return formatDateTime(value)
  if (remaining <= 0) return '已到期'
  const days = Math.ceil(remaining / DAY_MS)
  if (days <= 1) return `${prefix}于今天`
  return `${days} 天后${prefix}`
}

function shortDate(value: string): string {
  const date = new Date(value)
  return Number.isNaN(date.getTime()) ? value.slice(5, 10) : `${date.getMonth() + 1}/${date.getDate()}`
}

function friendlyError(reason: unknown): string {
  if (typeof reason === 'string') return reason
  if (reason instanceof Error) return reason.message
  return '发生未知错误，请稍后重试'
}

function showNotice(message: string) {
  notice.value = message
  if (noticeTimer) window.clearTimeout(noticeTimer)
  noticeTimer = window.setTimeout(() => (notice.value = ''), 2800)
}

function scheduleRefresh(delay?: number) {
  if (refreshTimer) window.clearTimeout(refreshTimer)
  if (!authenticated.value) return
  const base = delay ?? refreshMinutes.value * 60_000
  refreshTimer = window.setTimeout(() => refreshData(false), base * (0.9 + Math.random() * 0.2))
}

function dateRange(days: number) {
  const end = new Date()
  const start = new Date(end.getTime() - (days - 1) * DAY_MS)
  const format = (date: Date) => `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, '0')}-${String(date.getDate()).padStart(2, '0')}`
  return { start: format(start), end: format(end) }
}

async function loadTrend(force = false) {
  if (detailLoading.value || (!force && trendFetchedAt.value && Date.now() - trendFetchedAt.value < DETAIL_STALE_MS)) return
  detailLoading.value = true
  try {
    const range = dateRange(7)
    trendPayload.value = await api.fetchUsageTrend(range.start, range.end)
    trendFetchedAt.value = Date.now()
    persistDetails()
  } catch (reason) {
    showNotice(`趋势暂不可用：${friendlyError(reason)}`)
  } finally {
    detailLoading.value = false
  }
}

async function loadRecords(force = false) {
  if (detailLoading.value || (!force && recordsFetchedAt.value && Date.now() - recordsFetchedAt.value < DETAIL_STALE_MS)) return
  detailLoading.value = true
  try {
    recordsPayload.value = await api.fetchUsageRecords()
    recordsFetchedAt.value = Date.now()
    persistDetails()
  } catch (reason) {
    showNotice(`记录暂不可用：${friendlyError(reason)}`)
  } finally {
    detailLoading.value = false
  }
}

async function selectTab(tab: Tab) {
  activeTab.value = tab
  settingsOpen.value = false
  if (tab === 'overview') await loadTrend(false)
  else await loadRecords(false)
}

async function refreshData(manual = false) {
  if (loading.value || !authenticated.value) return
  if (manual && Date.now() - lastManualRefresh < MANUAL_COOLDOWN_MS) {
    showNotice('刷新过于频繁，请稍后再试')
    return
  }
  if (manual) lastManualRefresh = Date.now()

  loading.value = true
  error.value = ''
  try {
    const data = await api.fetchDashboard()
    snapshot.value = data
    localStorage.setItem(CACHE_KEY, JSON.stringify(data))
    failureCount.value = 0
    scheduleRefresh()
    if (activeTab.value === 'overview') await loadTrend(manual)
    else if (manual) await loadRecords(true)
    if (manual) showNotice('数据已更新')
  } catch (reason) {
    const message = friendlyError(reason)
    error.value = message
    if (message.includes('请先登录') || message.includes('登录已过期')) {
      authenticated.value = false
      authStep.value = 'login'
      return
    }
    failureCount.value += 1
    scheduleRefresh(Math.min(refreshMinutes.value * 60_000 * 2 ** failureCount.value, 2 * 60 * 60_000))
  } finally {
    loading.value = false
  }
}

async function handleLogin() {
  if (loading.value) return
  loading.value = true
  error.value = ''
  try {
    const result = await api.login(account.value, password.value)
    password.value = ''
    await finishLogin(result)
  } catch (reason) {
    error.value = friendlyError(reason)
  } finally {
    loading.value = false
  }
}

async function handleTwoFactor() {
  if (loading.value) return
  loading.value = true
  error.value = ''
  try {
    const result = await api.loginTwoFactor(tempToken.value, totpCode.value)
    totpCode.value = ''
    await finishLogin(result)
  } catch (reason) {
    error.value = friendlyError(reason)
  } finally {
    loading.value = false
  }
}

async function finishLogin(result: LoginOutcome) {
  if (result.kind === 'two_factor') {
    tempToken.value = result.temp_token
    authStep.value = 'totp'
    return
  }
  authenticated.value = true
  loading.value = false
  await refreshData(false)
}

async function handleLogout() {
  await api.logout()
  authenticated.value = false
  settingsOpen.value = false
  snapshot.value = null
  trendPayload.value = null
  recordsPayload.value = null
  localStorage.removeItem(CACHE_KEY)
  localStorage.removeItem(VIEW_CACHE_KEY)
  if (refreshTimer) window.clearTimeout(refreshTimer)
}

function updateInterval() {
  localStorage.setItem(INTERVAL_KEY, String(refreshMinutes.value))
  scheduleRefresh()
  showNotice(`自动刷新已调整为 ${refreshMinutes.value} 分钟`)
}

function cancelTwoFactor() {
  authStep.value = 'login'
  tempToken.value = ''
  totpCode.value = ''
  error.value = ''
}

function onVisibilityChange() {
  if (document.hidden) scheduleRefresh(Math.max(refreshMinutes.value, 30) * 60_000)
}

onMounted(async () => {
  if (previewMode) return
  try {
    authenticated.value = await api.hasSession()
    if (authenticated.value) await refreshData(false)
  } catch (reason) {
    error.value = friendlyError(reason)
  } finally {
    booting.value = false
  }
  unlistenRefresh = await listen('refresh-requested', () => refreshData(true))
  document.addEventListener('visibilitychange', onVisibilityChange)
})

onBeforeUnmount(() => {
  if (refreshTimer) window.clearTimeout(refreshTimer)
  if (noticeTimer) window.clearTimeout(noticeTimer)
  document.removeEventListener('visibilitychange', onVisibilityChange)
  unlistenRefresh?.()
})
</script>

<template>
  <main class="shell">
    <div v-if="booting" class="center-state">
      <div class="brand-mark">N</div><span class="spinner"></span><p>正在读取安全登录状态…</p>
    </div>

    <section v-else-if="!authenticated" class="auth-view">
      <div class="auth-brand"><div class="brand-mark">N</div><div><h1>NekoAPI</h1><p>低负荷用量监视器</p></div></div>
      <div class="auth-card">
        <template v-if="authStep === 'login'">
          <div class="section-heading"><h2>连接你的账号</h2><p>凭据仅发送至 NekoAPI，令牌保存在系统钥匙串。</p></div>
          <form @submit.prevent="handleLogin">
            <label><span>邮箱或用户名</span><input v-model.trim="account" autocomplete="username" placeholder="name@example.com" required></label>
            <label><span>密码</span><input v-model="password" type="password" autocomplete="current-password" placeholder="输入登录密码" required></label>
            <p v-if="error" class="form-error">{{ error }}</p>
            <button class="primary-button" :disabled="loading" type="submit"><span v-if="loading" class="button-spinner"></span>{{ loading ? '正在连接…' : '安全登录' }}</button>
          </form>
        </template>
        <template v-else>
          <button class="back-button" @click="cancelTwoFactor">← 返回</button>
          <div class="section-heading"><h2>二次验证</h2><p>输入验证器应用中显示的 6 位动态验证码。</p></div>
          <form @submit.prevent="handleTwoFactor">
            <input v-model="totpCode" class="totp-input" inputmode="numeric" maxlength="6" autocomplete="one-time-code" placeholder="000000" autofocus required>
            <p v-if="error" class="form-error">{{ error }}</p>
            <button class="primary-button" :disabled="loading" type="submit">{{ loading ? '正在验证…' : '完成验证' }}</button>
          </form>
        </template>
      </div>
      <div class="auth-footnote"><span></span>默认每 15 分钟刷新，不抓取网页</div>
    </section>

    <section v-else class="widget-view">
      <header class="utility-bar">
        <div class="utility-brand"><div class="brand-mark compact">N</div><div><strong>NekoAPI</strong><span>{{ displayName }}</span></div></div>
        <div class="balance-pill"><span>余额</span><strong>${{ formatMoney(balance) }}</strong></div>
        <div class="utility-actions">
          <button class="round-button" :class="{ rotating: loading }" title="刷新" @click="refreshData(true)"><svg viewBox="0 0 24 24"><path d="M20 6v5h-5M4 18v-5h5M6.1 9a7 7 0 0 1 11.5-2.6L20 8.5M4 15.5l2.4 2.1A7 7 0 0 0 17.9 15"/></svg></button>
          <button class="round-button" title="设置" @click="settingsOpen = !settingsOpen"><svg viewBox="0 0 24 24"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6v.2h-4V21a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9A1.7 1.7 0 0 0 3 14H2.8v-4H3a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9L4.2 7 7 4.2l.1.1a1.7 1.7 0 0 0 1.9.3A1.7 1.7 0 0 0 10 3V2.8h4V3a1.7 1.7 0 0 0 1 1.6 1.7 1.7 0 0 0 1.9-.3l.1-.1L19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1h.2v4H21a1.7 1.7 0 0 0-1.6 1Z"/></svg></button>
        </div>
      </header>

      <div class="panel-shell">
        <div class="panel-pointer"></div>
        <nav class="segmented"><button :class="{ active: activeTab === 'overview' }" @click="selectTab('overview')">概览</button><button :class="{ active: activeTab === 'records' }" @click="selectTab('records')">记录</button></nav>

        <div v-if="settingsOpen" class="settings-popover">
          <div><strong>自动刷新</strong><span>常规同步仅 3 个只读请求</span></div>
          <select v-model="refreshMinutes" @change="updateInterval"><option :value="15">15 分钟</option><option :value="30">30 分钟</option><option :value="60">60 分钟</option></select>
          <button @click="handleLogout">退出登录</button>
        </div>

        <div v-if="error || notice" class="toast" :class="{ danger: error }">{{ error || notice }}</div>

        <div class="profile-row"><div><strong>{{ displayName }}</strong><span>{{ primarySubscription?.platform ?? '按量计费账户' }}</span></div><small>更新于 {{ formatTime(snapshot?.fetched_at) }}</small></div>

        <div v-if="activeTab === 'overview'" class="tab-content overview-content">
          <section class="summary-strip">
            <div><span>今日消费</span><strong>${{ formatMoney(todayCost) }}</strong></div>
            <div><span>7 日成本</span><strong>${{ formatMoney(sevenDayCost) }}</strong></div>
            <div><span>今日 Token</span><strong>{{ formatCompact(todayTokens) }}</strong></div>
          </section>

          <section class="subscription-section">
            <div class="section-title"><div><strong>订阅额度</strong><span>{{ subscriptions.length ? `${subscriptions.length} 个有效订阅` : '未检测到有效订阅' }}</span></div><small v-if="snapshot?.subscriptions_error">读取失败</small></div>
            <div v-if="subscriptions.length" class="subscription-list">
              <article v-for="subscription in subscriptions" :key="subscription.id" class="subscription-card">
                <div class="subscription-head"><div><strong>{{ subscription.name }}</strong><span>{{ subscription.platform }} · {{ relativeDate(subscription.expiresAt) }}</span></div><i>{{ subscription.status === 'active' ? '有效' : subscription.status }}</i></div>
                <div v-if="subscription.quotas.length" class="quota-list">
                  <div v-for="quota in subscription.quotas" :key="quota.label" class="quota-row">
                    <div><span>{{ quota.label }}</span><strong>剩余 ${{ formatMoney(quota.remaining) }}</strong></div>
                    <div class="progress-track"><i :class="{ warning: quota.percent >= 80 }" :style="{ width: `${quota.percent}%` }"></i></div>
                    <small>已用 ${{ formatMoney(quota.used) }} / ${{ formatMoney(quota.limit) }} · {{ quota.percent.toFixed(1) }}%<em v-if="quota.resetAt">{{ relativeDate(quota.resetAt, '重置') }}</em></small>
                  </div>
                </div>
                <div v-else class="unlimited-row"><span>∞</span><div><strong>不限额度</strong><small>此订阅未设置周期消费上限</small></div></div>
              </article>
            </div>
            <div v-else class="empty-subscription"><div>◇</div><strong>当前没有有效订阅</strong><span>余额与按量消费仍会正常统计</span></div>
          </section>

          <section class="usage-section">
            <div class="section-title"><div><strong>每日用量</strong><span>最近 7 天实际消费</span></div><small>${{ formatMoney(sevenDayCost) }}</small></div>
            <div class="bar-chart" :class="{ loading: detailLoading }">
              <div v-for="point in trendPoints" :key="point.index" class="bar-column" :title="`${point.label} · $${formatMoney(point.value)}`"><i :style="{ height: `${point.height}%` }"></i><span>{{ point.label }}</span></div>
              <div v-if="!trendPoints.length" class="chart-empty">{{ detailLoading ? '正在读取趋势…' : '暂无趋势数据' }}</div>
            </div>
          </section>

          <section class="token-foot"><span>累计 Token <strong>{{ formatCompact(totalTokens) }}</strong></span><span>输入 {{ formatCompact(todayInput) }} · 输出 {{ formatCompact(todayOutput) }}</span><span>累计消费 ${{ formatMoney(totalCost) }}</span></section>
        </div>

        <div v-else class="tab-content records-content">
          <div class="records-title"><div><strong>最近请求</strong><span>仅在打开本页时按需加载</span></div><small>{{ usageRecords.length }} 条</small></div>
          <div v-if="usageRecords.length" class="record-list">
            <article v-for="record in usageRecords" :key="record.id" class="record-item">
              <span class="record-status" :class="{ failed: !record.success }"></span>
              <div class="record-main"><strong>{{ record.model }}</strong><span>{{ formatCompact(record.tokens) }} tokens · ${{ formatMoney(record.cost, 4) }}</span><small>{{ formatDateTime(record.time) }} · {{ record.source }}</small></div>
            </article>
          </div>
          <div v-else class="records-empty"><span class="spinner" v-if="detailLoading"></span><strong>{{ detailLoading ? '正在读取记录…' : '暂无用量记录' }}</strong><small>应用不会持续轮询明细</small></div>
        </div>

        <footer class="panel-footer"><span class="online-dot" :class="{ stale: error }"></span><span>{{ error ? '使用缓存数据' : '连接正常' }}</span><span class="spacer"></span><span>{{ refreshMinutes }} 分钟刷新</span></footer>
      </div>
    </section>
  </main>
</template>
