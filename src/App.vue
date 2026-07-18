<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { api, type DashboardSnapshot, type LoginOutcome } from './api'

const CACHE_KEY = 'nekoapi-widget:dashboard-cache:v1'
const INTERVAL_KEY = 'nekoapi-widget:refresh-minutes'
const MANUAL_COOLDOWN_MS = 30_000

const authenticated = ref(false)
const booting = ref(true)
const loading = ref(false)
const settingsOpen = ref(false)
const authStep = ref<'login' | 'totp'>('login')
const account = ref('')
const password = ref('')
const totpCode = ref('')
const tempToken = ref('')
const error = ref('')
const notice = ref('')
const snapshot = ref<DashboardSnapshot | null>(readCache())
const refreshMinutes = ref(readInterval())
const nextRefreshAt = ref<number | null>(null)
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

function readInterval(): number {
  const value = Number(localStorage.getItem(INTERVAL_KEY))
  return [15, 30, 60].includes(value) ? value : 15
}

function numberFrom(source: unknown, paths: string[][]): number {
  for (const path of paths) {
    let value: unknown = source
    let valid = true
    for (const key of path) {
      if (!value || typeof value !== 'object' || !(key in value)) {
        valid = false
        break
      }
      value = (value as Record<string, unknown>)[key]
    }
    if (!valid) continue
    if (typeof value === 'number' && Number.isFinite(value)) return value
    if (typeof value === 'string' && value.trim() !== '' && Number.isFinite(Number(value))) {
      return Number(value)
    }
  }
  return 0
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
      value = (value as Record<string, unknown>)[key]
    }
    if (valid) return value
  }
  return undefined
}

const profile = computed(() => snapshot.value?.profile ?? {})
const stats = computed(() => snapshot.value?.stats ?? {})
const balance = computed(() => numberFrom(profile.value, [['balance'], ['user', 'balance']]))
const todayCost = computed(() => numberFrom(stats.value, [['today_actual_cost'], ['today_cost']]))
const totalCost = computed(() => numberFrom(stats.value, [['total_actual_cost'], ['total_cost']]))
const todayTokens = computed(() => numberFrom(stats.value, [['today_tokens']]))
const totalTokens = computed(() => numberFrom(stats.value, [['total_tokens']]))
const todayInput = computed(() => numberFrom(stats.value, [['today_input_tokens']]))
const todayOutput = computed(() => numberFrom(stats.value, [['today_output_tokens']]))

interface PlatformRow {
  name: string
  cost: number
  tokens: number
  width: number
}

const platforms = computed<PlatformRow[]>(() => {
  const raw = valueFrom(stats.value, [['by_platform']])
  if (!Array.isArray(raw)) return []
  const rows = raw
    .map((item) => {
      const record = item && typeof item === 'object' ? (item as Record<string, unknown>) : {}
      return {
        name: platformName(String(record.platform ?? 'other')),
        cost: Number(record.today_actual_cost ?? record.total_actual_cost ?? 0),
        tokens: Number(record.today_tokens ?? record.total_tokens ?? 0),
        width: 0,
      }
    })
    .sort((a, b) => b.cost - a.cost)
    .slice(0, 4)
  const max = Math.max(...rows.map((row) => row.cost), 0)
  return rows.map((row) => ({ ...row, width: max > 0 ? Math.max(6, (row.cost / max) * 100) : 0 }))
})

function platformName(value: string): string {
  return ({ anthropic: 'Claude', openai: 'OpenAI', gemini: 'Gemini', grok: 'Grok' } as Record<string, string>)[value] ?? value
}

function formatMoney(value: number): string {
  return new Intl.NumberFormat('zh-CN', { minimumFractionDigits: 2, maximumFractionDigits: 4 }).format(value)
}

function formatCompact(value: number): string {
  return new Intl.NumberFormat('zh-CN', { notation: value >= 10_000 ? 'compact' : 'standard', maximumFractionDigits: 1 }).format(value)
}

function formatTime(timestamp?: number): string {
  if (!timestamp) return '尚未同步'
  return new Intl.DateTimeFormat('zh-CN', { hour: '2-digit', minute: '2-digit', second: '2-digit' }).format(timestamp * 1000)
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
  const jitter = base * (0.9 + Math.random() * 0.2)
  nextRefreshAt.value = Date.now() + jitter
  refreshTimer = window.setTimeout(() => refreshData(false), jitter)
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
    const backoff = Math.min(refreshMinutes.value * 60_000 * 2 ** failureCount.value, 2 * 60 * 60_000)
    scheduleRefresh(backoff)
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
  // The submit handler owns the loading flag. Release it before entering the
  // shared refresh path so the duplicate-request guard does not skip the first sync.
  loading.value = false
  await refreshData(false)
}

async function handleLogout() {
  await api.logout()
  authenticated.value = false
  settingsOpen.value = false
  snapshot.value = null
  localStorage.removeItem(CACHE_KEY)
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

onMounted(async () => {
  try {
    authenticated.value = await api.hasSession()
    if (authenticated.value) await refreshData(false)
  } catch (reason) {
    error.value = friendlyError(reason)
  } finally {
    booting.value = false
  }

  unlistenRefresh = await listen('refresh-requested', () => refreshData(true))
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) scheduleRefresh(Math.max(refreshMinutes.value, 30) * 60_000)
  })
})

onBeforeUnmount(() => {
  if (refreshTimer) window.clearTimeout(refreshTimer)
  if (noticeTimer) window.clearTimeout(noticeTimer)
  unlistenRefresh?.()
})
</script>

<template>
  <main class="shell">
    <div v-if="booting" class="center-state">
      <div class="brand-mark">N</div>
      <span class="spinner"></span>
      <p>正在读取安全登录状态…</p>
    </div>

    <section v-else-if="!authenticated" class="auth-view">
      <div class="auth-brand">
        <div class="brand-mark">N</div>
        <div>
          <h1>NekoAPI</h1>
          <p>低负荷用量监视器</p>
        </div>
      </div>

      <div class="auth-card">
        <template v-if="authStep === 'login'">
          <div class="section-heading">
            <h2>连接你的账号</h2>
            <p>凭据仅发送至 NekoAPI，令牌保存在系统钥匙串。</p>
          </div>
          <form @submit.prevent="handleLogin">
            <label>
              <span>邮箱或用户名</span>
              <input v-model.trim="account" autocomplete="username" placeholder="name@example.com" required />
            </label>
            <label>
              <span>密码</span>
              <input v-model="password" type="password" autocomplete="current-password" placeholder="输入登录密码" required />
            </label>
            <p v-if="error" class="error-message">{{ error }}</p>
            <button class="primary-button" :disabled="loading" type="submit">
              <span v-if="loading" class="button-spinner"></span>
              {{ loading ? '正在连接…' : '安全登录' }}
            </button>
          </form>
        </template>

        <template v-else>
          <button class="back-button" @click="cancelTwoFactor">← 返回</button>
          <div class="section-heading">
            <h2>二次验证</h2>
            <p>输入验证器应用中显示的 6 位动态验证码。</p>
          </div>
          <form @submit.prevent="handleTwoFactor">
            <input v-model="totpCode" class="totp-input" inputmode="numeric" maxlength="6" autocomplete="one-time-code" placeholder="000000" autofocus required />
            <p v-if="error" class="error-message">{{ error }}</p>
            <button class="primary-button" :disabled="loading" type="submit">{{ loading ? '正在验证…' : '完成验证' }}</button>
          </form>
        </template>
      </div>

      <div class="auth-footnote"><span></span>默认每 15 分钟刷新，不抓取网页</div>
    </section>

    <section v-else class="dashboard-view">
      <header class="topbar">
        <div class="mini-brand"><div class="brand-mark small">N</div><div><strong>NekoAPI</strong><span>Usage monitor</span></div></div>
        <div class="top-actions">
          <button class="icon-button" title="刷新数据" :class="{ spinning: loading }" @click="refreshData(true)">
            <svg viewBox="0 0 24 24"><path d="M20 6v5h-5M4 18v-5h5M6.1 9a7 7 0 0 1 11.5-2.6L20 8.5M4 15.5l2.4 2.1A7 7 0 0 0 17.9 15"/></svg>
          </button>
          <button class="icon-button" title="设置" @click="settingsOpen = !settingsOpen">
            <svg viewBox="0 0 24 24"><path d="M12 15.5a3.5 3.5 0 1 0 0-7 3.5 3.5 0 0 0 0 7Z"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1a1.7 1.7 0 0 0-1.9-.3 1.7 1.7 0 0 0-1 1.6v.2h-4V21a1.7 1.7 0 0 0-1-1.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4.2 17l.1-.1a1.7 1.7 0 0 0 .3-1.9A1.7 1.7 0 0 0 3 14H2.8v-4H3a1.7 1.7 0 0 0 1.6-1 1.7 1.7 0 0 0-.3-1.9L4.2 7 7 4.2l.1.1a1.7 1.7 0 0 0 1.9.3A1.7 1.7 0 0 0 10 3V2.8h4V3a1.7 1.7 0 0 0 1 1.6 1.7 1.7 0 0 0 1.9-.3l.1-.1L19.8 7l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 1.6 1h.2v4H21a1.7 1.7 0 0 0-1.6 1Z"/></svg>
          </button>
        </div>
      </header>

      <div v-if="settingsOpen" class="settings-panel">
        <div><strong>自动刷新</strong><span>最低 15 分钟，带随机抖动</span></div>
        <select v-model="refreshMinutes" @change="updateInterval"><option :value="15">15 分钟</option><option :value="30">30 分钟</option><option :value="60">60 分钟</option></select>
        <button class="logout-button" @click="handleLogout">退出登录</button>
      </div>

      <p v-if="error" class="banner error-banner">{{ error }} · 已保留上次数据</p>
      <p v-if="notice" class="banner notice-banner">{{ notice }}</p>

      <div class="content">
        <article class="hero-card">
          <div class="card-label"><span>今日实际消费</span><span class="live-dot">LIVE</span></div>
          <div class="money"><small>$</small>{{ formatMoney(todayCost) }}</div>
          <div class="hero-meta"><span>累计 ${{ formatMoney(totalCost) }}</span><span>余额 ${{ formatMoney(balance) }}</span></div>
        </article>

        <div class="metric-grid">
          <article class="metric-card"><div class="metric-icon violet">T</div><p>今日 Token</p><strong>{{ formatCompact(todayTokens) }}</strong><span>输入 {{ formatCompact(todayInput) }} · 输出 {{ formatCompact(todayOutput) }}</span></article>
          <article class="metric-card"><div class="metric-icon cyan">Σ</div><p>累计 Token</p><strong>{{ formatCompact(totalTokens) }}</strong><span>全部模型总计</span></article>
        </div>

        <article class="platform-card">
          <div class="card-title"><strong>平台分布</strong><span>按今日消费</span></div>
          <div v-if="platforms.length" class="platform-list">
            <div v-for="row in platforms" :key="row.name" class="platform-row">
              <div class="platform-info"><span>{{ row.name }}</span><strong>${{ formatMoney(row.cost) }}</strong></div>
              <div class="bar"><i :style="{ width: `${row.width}%` }"></i></div>
            </div>
          </div>
          <div v-else class="empty-platform">暂时没有平台用量数据</div>
        </article>
      </div>

      <footer>
        <span class="status-dot" :class="{ stale: error }"></span>
        <span>{{ error ? '等待下次重试' : `更新于 ${formatTime(snapshot?.fetched_at)}` }}</span>
        <span class="footer-spacer"></span>
        <span>{{ refreshMinutes }} 分钟自动刷新</span>
      </footer>
    </section>
  </main>
</template>
