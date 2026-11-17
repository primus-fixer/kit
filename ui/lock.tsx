// Вход в приложение: PIN-код из четырёх цифр, отпечаток на телефоне или Windows Hello на компьютере,
// «Забыли PIN?» — подтверждение блокировкой устройства. Плюс экраны «задать, сменить, выключить PIN».
// Команды входа — из kit-tauri (guard); приложение передаёт их в `api` (в демо — свои, без Tauri).
import { useCallback, useEffect, useRef, useState } from 'react';
import { Icon } from './icons';

/** Состояние входа — ответ команд входа kit-tauri. */
export type LockStatus = {
  pin: boolean;
  locked: boolean;
  /** Вход по отпечатку или Windows Hello включён. */
  bio: boolean;
  /** …и доступен на устройстве сейчас. */
  bio_ready: boolean;
  bio_kind: '' | 'fingerprint' | 'face' | 'hello';
  /** Чем подтвердить себя, если PIN забыт: device (экран блокировки телефона), hello или пусто. */
  recover: '' | 'device' | 'hello';
  /** Секунд до следующей попытки. */
  wait: number;
  /** Сколько ещё можно ошибиться до паузы. */
  left: number;
  /** Устройство подтвердило владельца — можно задать новый PIN без старого. */
  recovery: boolean;
  /** Чем защищён ключ базы. */
  key: string;
  /** Сообщение при запуске, например если прежнюю базу не удалось открыть. */
  notice?: string | null;
  error: string | null;
};

/** Команды входа, которые нужны экранам входа. */
export type LockApi = {
  pinUnlock: (code: string) => Promise<LockStatus>;
  /** Новый PIN; `current` — нынешний (если PIN уже есть и это не сброс). */
  pinSet: (code: string, current: string | null) => Promise<LockStatus>;
  pinOff: (current: string | null) => Promise<LockStatus>;
  bioUnlock: () => Promise<LockStatus>;
  /** «Забыли PIN?»: подтвердить себя блокировкой устройства. */
  pinRecover: () => Promise<LockStatus>;
};

/** Название приложения на экране входа: именительный и винительный падеж («вход в „…“»). */
export type LockNames = { name: string; nameAcc: string };

/** Ответ команды вида «locked» — приложение заблокировано, нужен экран входа. */
export const LOCKED = 'locked';

/** Команды входа kit-tauri через Tauri (в демо приложение подставляет свои). */
export function lockCommands() {
  const call = async <T,>(cmd: string, args?: Record<string, unknown>) => (await import('@tauri-apps/api/core')).invoke<T>(cmd, args);
  return {
    lockStatus: () => call<LockStatus>('lock_status'),
    pinUnlock: (code: string) => call<LockStatus>('pin_unlock', { code }),
    pinSet: (code: string, current: string | null) => call<LockStatus>('pin_set', { code, current }),
    pinOff: (current: string | null) => call<LockStatus>('pin_off', { current }),
    bioUnlock: () => call<LockStatus>('bio_unlock'),
    bioSet: (on: boolean) => call<LockStatus>('bio_set', { on }),
    pinRecover: () => call<LockStatus>('pin_recover'),
    lockNow: () => call<LockStatus>('lock_now'),
    /** Экран свернули или снова открыли. */
    appVisible: (visible: boolean) => call<LockStatus>('app_visible', { visible }),
    /** Приложение заблокировалось само (5 минут в фоне). */
    onLocked: async (listener: () => void): Promise<() => void> => (await import('@tauri-apps/api/event')).listen('locked', listener),
  };
}

function message(e: unknown): string {
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  return String(e);
}

const PIN_LEN = 4;

/** Компьютер под Windows (для подписей «Windows Hello»). */
export const isWindows = typeof navigator !== 'undefined' && /windows/i.test(navigator.userAgent);

export function bioLabel(kind: LockStatus['bio_kind']): { verb: string; name: string; title: string } {
  if (kind === 'hello' || (kind === '' && isWindows)) return { verb: 'Войти через Windows Hello', name: 'Windows Hello', title: 'Вход через Windows Hello' };
  if (kind === 'face') return { verb: 'Войти по лицу', name: 'лицо', title: 'Вход по лицу' };
  return { verb: 'Войти по отпечатку', name: 'отпечаток пальца', title: 'Вход по отпечатку' };
}

function mmss(sec: number): string {
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${m}:${String(s).padStart(2, '0')}`;
}

/** Секунды до конца паузы, тикают раз в секунду. */
function useCountdown(wait: number, stamp: unknown): number {
  const [left, setLeft] = useState(wait);
  useEffect(() => {
    setLeft(wait);
    if (wait <= 0) return;
    const end = Date.now() + wait * 1000;
    const id = window.setInterval(() => {
      const s = Math.max(0, Math.ceil((end - Date.now()) / 1000));
      setLeft(s);
      if (s === 0) window.clearInterval(id);
    }, 250);
    return () => window.clearInterval(id);
  }, [wait, stamp]);
  return left;
}

/** Четыре точки и цифровая клавиатура; на компьютере можно набирать на клавиатуре. */
function PinPad({
  onComplete,
  disabled,
  reset,
  shake,
  extra,
}: {
  onComplete: (code: string) => void;
  disabled: boolean;
  /** Смена значения стирает набранное. */
  reset: number;
  /** Смена значения стирает набранное и встряхивает точки (ошибка). */
  shake: number;
  extra?: { icon: string; label: string; onPress: () => void } | null;
}) {
  const [code, setCode] = useState('');
  const codeRef = useRef('');
  const set = (c: string) => {
    codeRef.current = c;
    setCode(c);
  };
  useEffect(() => set(''), [reset, shake]);
  const press = useCallback(
    (k: string) => {
      if (disabled) return;
      const c = codeRef.current;
      if (k === 'back') return set(c.slice(0, -1));
      if (c.length >= PIN_LEN) return;
      const next = c + k;
      set(next);
      if (next.length === PIN_LEN) window.setTimeout(() => onComplete(next), 80);
    },
    [disabled, onComplete],
  );
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (/^[0-9]$/.test(e.key)) press(e.key);
      else if (e.key === 'Backspace') press('back');
      else return;
      e.preventDefault();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [press]);
  return (
    <>
      <div className={'pindots' + (shake ? ' shake' : '')} key={shake} role="status" aria-label={`Введено цифр: ${code.length} из ${PIN_LEN}`}>
        {Array.from({ length: PIN_LEN }, (_, i) => (
          <i key={i} className={i < code.length ? 'on' : ''} />
        ))}
      </div>
      <div className="pinpad">
        {['1', '2', '3', '4', '5', '6', '7', '8', '9'].map((k) => (
          <button key={k} type="button" disabled={disabled} onClick={() => press(k)}>
            {k}
          </button>
        ))}
        {extra ? (
          <button type="button" className="padextra" aria-label={extra.label} title={extra.label} onClick={extra.onPress}>
            <Icon name={extra.icon} size={26} />
          </button>
        ) : (
          <span />
        )}
        <button type="button" disabled={disabled} onClick={() => press('0')}>
          0
        </button>
        <button type="button" aria-label="Стереть" disabled={disabled} onClick={() => press('back')}>
          <Icon name="back" size={24} />
        </button>
      </div>
    </>
  );
}

function hint(st: LockStatus, wait: number): { text: string; warn: boolean } {
  if (wait > 0) return { text: `Слишком много попыток. Повторите через ${mmss(wait)}`, warn: true };
  if (st.error === 'Неверный PIN-код') {
    return { text: st.left > 0 && st.left < 5 ? `Неверный PIN-код. Осталось попыток: ${st.left}` : 'Неверный PIN-код. Следующая ошибка — пауза', warn: true };
  }
  if (st.error) return { text: st.error, warn: true };
  return { text: 'Введите PIN-код', warn: false };
}

/** Экран входа: PIN-код, отпечаток или Windows Hello, «Забыли PIN?». */
export function LockScreen({ status, api, names, onUnlocked }: { status: LockStatus; api: LockApi; names: LockNames; onUnlocked: (st: LockStatus) => void }) {
  const [st, setSt] = useState(status);
  const [busy, setBusy] = useState(false);
  const [shake, setShake] = useState(0);
  const [stamp, setStamp] = useState(0);
  const wait = useCountdown(st.wait, stamp);
  const asked = useRef(false);
  const rootRef = useRef<HTMLDivElement>(null);

  const apply = useCallback(
    (next: LockStatus) => {
      setSt(next);
      setStamp((x) => x + 1);
      if (!next.locked) onUnlocked(next);
      else if (next.error) setShake((x) => x + 1);
    },
    [onUnlocked],
  );
  const call = useCallback(
    async (fn: () => Promise<LockStatus>) => {
      setBusy(true);
      try {
        apply(await fn());
      } catch (e) {
        setSt((s) => ({ ...s, error: message(e) }));
      } finally {
        setBusy(false);
      }
    },
    [apply],
  );

  const canBio = st.bio && st.bio_ready;
  const bio = bioLabel(st.bio_kind);
  useEffect(() => {
    // Фокус — с полей под экраном входа, чтобы цифры не попали в них.
    (document.activeElement as HTMLElement | null)?.blur?.();
    rootRef.current?.focus();
    if (canBio && !asked.current) {
      asked.current = true;
      call(api.bioUnlock);
    }
  }, [canBio, call, api]);

  const h = hint(st, wait);
  return (
    <div className="lock" role="dialog" aria-modal="true" aria-label={`Вход в «${names.nameAcc}»`} tabIndex={-1} ref={rootRef}>
      <p className="lockname">{names.name}</p>
      <p className={'lock-hint' + (h.warn ? ' lock-warn' : '')} aria-live="polite">
        {h.text}
      </p>
      <PinPad
        onComplete={(code) => call(() => api.pinUnlock(code))}
        disabled={busy || wait > 0}
        reset={0}
        shake={shake}
        extra={canBio ? { icon: 'finger', label: bio.verb, onPress: () => call(api.bioUnlock) } : null}
      />
      <button type="button" className="lock-forgot" disabled={busy} onClick={() => call(api.pinRecover)}>
        Забыли PIN?
      </button>
      {st.recover && (
        <p className="lock-note">{st.recover === 'hello' ? 'Подтвердите себя через Windows Hello и задайте новый PIN.' : 'Подтвердите себя блокировкой экрана телефона и задайте новый PIN.'}</p>
      )}
    </div>
  );
}

export type PinMode = 'create' | 'change' | 'off' | 'reset';

/** Задать, сменить или выключить PIN-код. */
export function PinFlow({ mode, api, onDone, onClose }: { mode: PinMode; api: LockApi; onDone: (st: LockStatus, text: string) => void; onClose: () => void }) {
  type Step = 'current' | 'new' | 'confirm';
  const first: Step = mode === 'change' || mode === 'off' ? 'current' : 'new';
  const [step, setStep] = useState<Step>(first);
  const [current, setCurrent] = useState('');
  const [fresh, setFresh] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [shake, setShake] = useState(0);
  const [reset, setReset] = useState(0);

  const fail = (text: string, back: Step) => {
    setError(text);
    setStep(back);
    setShake((x) => x + 1);
  };
  const submit = async (fn: () => Promise<LockStatus>, ok: string) => {
    setBusy(true);
    try {
      const st = await fn();
      if (st.error) fail(st.error, first);
      else onDone(st, ok);
    } catch (e) {
      fail(message(e), first);
    } finally {
      setBusy(false);
    }
  };
  const onCode = (code: string) => {
    setError(null);
    if (step === 'current') {
      setCurrent(code);
      if (mode === 'off') return submit(() => api.pinOff(code), 'Вход по PIN-коду выключен');
      setStep('new');
      setReset((x) => x + 1);
      return;
    }
    if (step === 'new') {
      setFresh(code);
      setStep('confirm');
      setReset((x) => x + 1);
      return;
    }
    if (code !== fresh) return fail('PIN-коды не совпали, наберите заново', 'new');
    submit(() => api.pinSet(code, mode === 'change' ? current : null), mode === 'change' ? 'PIN-код изменён' : 'PIN-код задан');
  };

  const title = step === 'current' ? 'Текущий PIN-код' : step === 'new' ? (mode === 'reset' ? 'Новый PIN-код' : 'Придумайте PIN-код') : 'Повторите PIN-код';
  return (
    <div className="lock" role="dialog" aria-modal="true" aria-label={title}>
      <button type="button" className="lockclose" aria-label="Закрыть" onClick={onClose}>
        <Icon name="x" size={22} stroke={2} />
      </button>
      <p className="lockname small-name">{title}</p>
      <p className={'lock-hint' + (error ? ' lock-warn' : '')} aria-live="polite">
        {error || (step === 'new' ? '4 цифры. Его будет спрашивать только это устройство' : step === 'confirm' ? 'Ещё раз, чтобы не ошибиться' : '')}
      </p>
      <PinPad onComplete={onCode} disabled={busy} reset={reset} shake={shake} />
    </div>
  );
}
