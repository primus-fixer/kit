// Команды ядра. Общие — из kit: вход (lockCommands), связь по Wi-Fi и облако (их данные — типы kit).
import { invoke } from '@tauri-apps/api/core';
import { cloudCommands, lockCommands, wifiCommands } from '@primus-fixer/kit/ui';

export { LOCKED, type LockStatus } from '@primus-fixer/kit/ui';

/** Запись приложения (в заготовке — заметка): те же поля, что в `starter-core/src/model.rs`. */
export type Note = { id: string; text: string; created: number; image?: string | null };

export type DeviceInfo = { name: string; version: string; key_method: string; events: number };

export const api = {
  ...lockCommands(),
  ...wifiCommands(),
  ...cloudCommands(),
  notes: () => invoke<Note[]>('notes'),
  noteSave: (note: Note) => invoke<Note>('note_save', { note }),
  noteDelete: (id: string) => invoke<void>('note_delete', { id }),
  deviceInfo: () => invoke<DeviceInfo>('device_info'),
};

/** Приложение открыто внутри Tauri, а не в обычном браузере. */
export const inApp = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/** Телефон: QR-код связи сканируется камерой. */
export const isMobileApp = inApp && /android|iphone|ipad/i.test(navigator.userAgent);

export function message(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

/** QR-код связи с другого устройства — камерой телефона. */
export async function scanQr(): Promise<string> {
  const scanner = await import('@tauri-apps/plugin-barcode-scanner');
  let perm = await scanner.checkPermissions();
  if (perm !== 'granted') perm = await scanner.requestPermissions();
  if (perm !== 'granted') throw new Error('Нет доступа к камере. Разрешите его в настройках телефона');
  const res = await scanner.scan({ windowed: false, formats: [scanner.Format.QRCode] });
  return res.content;
}
