import { request } from './api';
export type EmailFilter = { success: boolean; failure: boolean };
export type EmailSettings = {
  language?: 'fr' | 'en';
  enabled: boolean; installation: string; host: string; port: number;
  tls: 'starttls' | 'implicit'; username: string; sender: string; sender_name: string;
  recipients: string[]; events: Record<string, EmailFilter>;
};
export type EmailConfiguration = { revision: number; settings: EmailSettings; has_password: boolean };
export type EmailPreview = { subject: string; html: string; text: string };
export type EmailDelivery = {
  id: string; recipient: string; status: string; attempts: number; category: string | null;
  created_at: number; event: { kind: string; outcome: string; subject: string; vtl_name?: string };
};
export type EmailHistory = { items: EmailDelivery[]; has_more: boolean; offset: number };
export const emailConfiguration = () => request<EmailConfiguration>('/v1/notifications/settings');
export const emailPreview = () => request<EmailPreview>('/v1/notifications/preview');
export const emailHistory = (offset: number) => request<EmailHistory>(`/v1/notifications/deliveries?offset=${offset}`);
export const saveEmailConfiguration = (revision: number, settings: EmailSettings, password: string | null, clear_password: boolean) => request<EmailConfiguration>('/v1/notifications/settings', { method: 'PUT', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ revision, settings, password, clear_password }) });
export const sendTestEmail = () => request<{ status: string }>('/v1/notifications/test', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: '{}' });
