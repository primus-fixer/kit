// Элементы экрана приложений на kit: переключатель, полоса, раздел под катом, строка, верхняя строка.
// Как их применять — docs/design.md.
import { useState, type ReactNode } from 'react';
import { Icon } from './icons';
import { store, stored } from './storage';

/** Переключатель. У варианта может быть цвет выбранного: 'own' — личное, 'work' — рабочее. `cols` — колонки поровну. */
export function Seg<T extends string>({ value, options, onChange, label, cols, small }: { value: T; options: [T, string, ('own' | 'work')?][]; onChange: (v: T) => void; label: string; cols?: boolean; small?: boolean }) {
  return (
    <div
      className={`seg${small ? ' small' : ''}`}
      role="group"
      aria-label={label}
      style={cols ? { gridTemplateColumns: `repeat(${options.length}, minmax(max-content, 1fr))` } : { gridAutoFlow: 'column', gridAutoColumns: 'auto' }}
    >
      {options.map(([v, l, tone]) => (
        <button key={v} type="button" className={v === value ? (tone ? `on ${tone}` : 'on') : ''} aria-pressed={v === value} onClick={() => onChange(v)}>
          {l}
        </button>
      ))}
    </div>
  );
}

/** Полоска доли. `label` — что она показывает, для чтения с экрана; без подписи полоска только для глаз. */
export function Bar({ pct, color = 'var(--own)', thick, label }: { pct: number; color?: string; thick?: boolean; label?: string }) {
  return (
    <span className={`bar${thick ? ' thick' : ''}`} role={label ? 'img' : undefined} aria-label={label} aria-hidden={label ? undefined : true}>
      <span style={{ width: `${Math.max(0, Math.min(100, pct))}%`, background: color }} />
    </span>
  );
}

/**
 * Раздел под катом: заголовок — кнопка, содержимое — по нажатию; открытые разделы запоминаются на устройстве.
 * Если приложение само открывает разделы (переход к разделу из другого экрана), оно передаёт `open` и `onToggle`.
 */
export function Cut({ id, title, hint, className, open: shown, onToggle, children }: { id: string; title: string; hint?: string; className?: string; open?: boolean; onToggle?: (open: boolean) => void; children: ReactNode }) {
  const [own, setOwn] = useState(() => stored(`cut.${id}`, '0') === '1');
  const open = shown ?? own;
  const toggle = () => {
    if (onToggle) return onToggle(!open);
    store(`cut.${id}`, open ? '0' : '1');
    setOwn(!open);
  };
  return (
    <section className={`cut${open ? ' open' : ''}${className ? ` ${className}` : ''}`} id={id}>
      <button type="button" className="cut-head" aria-expanded={open} aria-controls={`${id}-body`} onClick={toggle}>
        <span className="cut-title">{title}</span>
        {hint && <span className="cut-hint">{hint}</span>}
        <Icon name="down" size={18} stroke={2} />
      </button>
      {open && (
        <div className="stack-s cut-body" id={`${id}-body`}>
          {children}
        </div>
      )}
    </section>
  );
}

export function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="row">
      <span className="muted">{label}</span>
      <span className="row-val">{children}</span>
    </div>
  );
}

export function Header({ title, onBack, backLabel = 'Назад', right }: { title?: string; onBack: () => void; backLabel?: string; right?: ReactNode }) {
  return (
    <div className={'topbar' + (right ? ' with-right' : '')}>
      <button type="button" className="icon-btn" aria-label={backLabel} onClick={onBack}>
        <Icon name="left" size={24} stroke={2} />
      </button>
      {title ? <h1 className="topbar-title">{title}</h1> : <span />}
      {right ?? <span />}
    </div>
  );
}
