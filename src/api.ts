import { invoke } from '@tauri-apps/api/core'

export interface DashboardSnapshot {
  profile: Record<string, unknown>
  stats: Record<string, unknown>
  subscriptions: unknown
  subscriptions_error?: string | null
  fetched_at: number
}

export type LoginOutcome =
  | { kind: 'authenticated' }
  | { kind: 'two_factor'; temp_token: string }

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
  logout: () => invoke<void>('logout'),
}
