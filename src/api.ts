import { invoke } from '@tauri-apps/api/core'

export interface DashboardSnapshot {
  profile: Record<string, unknown>
  stats: Record<string, unknown>
  subscriptions: unknown
  subscription_summary?: {
    active_subscription_count: number
    quota_count: number
    selected?: {
      subscription_id: string
      subscription_name: string
      platform: string
      period: 'daily' | 'weekly' | 'monthly'
      used: number
      limit: number
      percent: number
    } | null
  }
  subscriptions_error?: string | null
  current_api_key?: {
    key_id: string
    key_name: string
    group_id: string
    group_name: string
    platform: string
  } | null
  api_keys_error?: string | null
  channel_monitors?: unknown
  channel_monitors_error?: string | null
  fetched_at: number
}

export type LoginOutcome =
    | { kind: 'authenticated' }
    | { kind: 'two_factor'; temp_token: string }

export interface LotteryRoundInfo {
  id: number
  number?: number
  title?: string
  status?: string
  reward_amount?: number
  participant_count?: number
  target_participants?: number
  closes_at?: string
}

export interface LotteryEligibility {
  eligible?: boolean
  reason_code?: string
  reason?: string
}

export interface LotterySnapshot {
  server_enabled: boolean
  has_open_round: boolean
  participated: boolean
  eligibility: LotteryEligibility
  current_round: LotteryRoundInfo | null
  fetched_at: number
}

export interface LotterySettings {
  enabled: boolean
  last_notified_round_id: number | null
}

export const api = {
  hasSession: () => invoke<boolean>('has_session'),
  login: (account: string, password: string) =>
    invoke<LoginOutcome>('login', { account, password }),
  loginTwoFactor: (tempToken: string, code: string) =>
    invoke<LoginOutcome>('login_two_factor', { tempToken, code }),
  fetchDashboard: () => invoke<DashboardSnapshot>('fetch_dashboard'),
  fetchUsageTrend: (startDate: string, endDate: string) =>
    invoke<unknown>('fetch_usage_trend', { startDate, endDate }),
  fetchUsageRecords: () => invoke<unknown>('fetch_usage_records'),
  fetchLottery: (timezone: string) =>
    invoke<LotterySnapshot>('fetch_lottery', { timezone }),
  pollLottery: (timezone: string) =>
    invoke<LotterySnapshot>('poll_lottery', { timezone }),
  joinLottery: (roundId: number) =>
    invoke<unknown>('join_lottery', { roundId }),
  getLotterySettings: () => invoke<LotterySettings>('get_lottery_settings'),
  setLotteryNotificationEnabled: (enabled: boolean) =>
    invoke<void>('set_lottery_notification_enabled', { enabled }),
  logout: () => invoke<void>('logout'),
}
