import type { CSSProperties } from 'react';

// Контурные значки 24×24 (docs/design.md, «Значки»). Здесь — те, что нужны элементам kit; свои
// приложение добавляет через addIcons().
const ICONS: Record<string, string> = {
  plus: 'M5 12h14M12 5v14',
  minus: 'M5 12h14',
  back: 'M10 5a2 2 0 0 0-1.344.519l-6.328 5.74a1 1 0 0 0 0 1.481l6.328 5.741A2 2 0 0 0 10 19h10a2 2 0 0 0 2-2V7a2 2 0 0 0-2-2zM12 9l6 6M18 9l-6 6',
  finger: 'M8.5 4.8A8 8 0 0 1 20 12v1M4 12a8 8 0 0 1 2-5.3M7 12a5 5 0 0 1 10 0v2.5c0 2-.5 4-1.3 5.6M12 12v3c0 2.4-.7 4.6-1.8 6.4M9.5 15.5c0 1.6-.4 3.1-1.1 4.4M4.2 16.5c.5-1.3.8-2.8.8-4.5',
  lock: 'M6 11h12v10H6zM8.5 11V8a3.5 3.5 0 0 1 7 0v3',
  x: 'M18 6 6 18M6 6l12 12',
  left: 'M15 18l-6-6 6-6',
  right: 'M9 18l6-6-6-6',
  down: 'M6 9l6 6 6-6',
  up: 'M18 15l-6-6-6 6',
  check: 'M20 6 9 17l-5-5',
};

/** Добавить свои значки (или заменить значки kit): имя → путь SVG в поле 24×24. */
export function addIcons(icons: Record<string, string>) {
  Object.assign(ICONS, icons);
}

/** Значок по имени или по готовому пути `d`. Цвет — цвет текста рядом. */
export function Icon({ name, d, size = 20, stroke = 1.75, style }: { name?: string; d?: string; size?: number; stroke?: number; style?: CSSProperties }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={stroke} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" style={style}>
      <path d={d ?? (name ? ICONS[name] : undefined) ?? ''} />
    </svg>
  );
}
