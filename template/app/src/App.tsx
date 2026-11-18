// Оболочка приложения: вход по PIN-коду, тема, разделы (на телефоне — снизу, на компьютере — слева), сообщения.
// Своё приложение меняет разделы (TABS) и экраны; вход, тема и обмен — общие из kit.
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Icon, LockScreen, PinFlow, addIcons, applyTheme, store, stored, type PinMode, type ThemeMode } from '@primus-fixer/kit/ui';
import { api, LOCKED, message, type LockStatus, type Note } from './api';
import { ICONS } from './icons';
import Notes from './screens/Notes';
import Settings from './screens/Settings';

addIcons(ICONS);

const LOCK_NAMES = { name: 'Заготовка', nameAcc: 'Заготовку' };

type Tab = 'notes' | 'settings';
const TABS: { id: Tab; label: string; icon: string }[] = [
  { id: 'notes', label: 'Записи', icon: 'notes' },
  { id: 'settings', label: 'Настройки', icon: 'settings' },
];

/** Широкое окно — меню слева; узкое (телефон) — разделы снизу. */
function useWide(): boolean {
  const query = '(min-width: 720px)';
  const [wide, setWide] = useState(() => window.matchMedia(query).matches);
  useEffect(() => {
    const m = window.matchMedia(query);
    const on = () => setWide(m.matches);
    m.addEventListener('change', on);
    return () => m.removeEventListener('change', on);
  }, []);
  return wide;
}

export default function App() {
  const wide = useWide();
  const [tab, setTab] = useState<Tab>('notes');
  const [notes, setNotes] = useState<Note[] | null>(null);
  const [toast, showToast] = useState<string | null>(null);

  // Тема: схема и светлая/тёмная — у каждого устройства своя.
  const [scheme, setScheme] = useState(() => stored('starter.scheme', 'shield'));
  const [mode, setMode] = useState<ThemeMode>(() => stored('starter.mode', 'system') as ThemeMode);
  useEffect(() => {
    applyTheme(scheme, mode);
    store('starter.scheme', scheme);
    store('starter.mode', mode);
  }, [scheme, mode]);

  // Вход: с PIN-кодом данные приходят только после него. Ответ «locked» от любой команды — экран входа.
  const [lock, setLock] = useState<LockStatus | null>(null);
  const [locked, setLocked] = useState(false);
  const [lockKey, setLockKey] = useState(0);
  const [pinFlow, setPinFlow] = useState<PinMode | null>(null);
  const lockedRef = useRef(false);
  const showLock = useCallback(() => {
    if (lockedRef.current) return;
    lockedRef.current = true;
    setLockKey((k) => k + 1);
    setLocked(true);
  }, []);
  const goLocked = useCallback(() => {
    api.lockStatus().then(
      (st) => {
        setLock(st);
        if (st.locked) showLock();
      },
      () => showLock(),
    );
  }, [showLock]);
  const say = useCallback((text: string) => (text === LOCKED ? goLocked() : showToast(text)), [goLocked]);

  const load = useCallback(() => {
    api.notes().then(setNotes, (e) => say(message(e)));
  }, [say]);

  useEffect(() => {
    api.lockStatus().then(
      (st) => {
        setLock(st);
        if (st.locked) showLock();
        else load();
      },
      () => load(),
    );
    // Свернули — приложение считает время в фоне; вернулись через 5 минут — экран входа.
    const onVisible = () => {
      const visible = document.visibilityState === 'visible';
      api.appVisible(visible).then(
        (st) => {
          setLock(st);
          if (st.locked && visible) goLocked();
        },
        () => undefined,
      );
    };
    document.addEventListener('visibilitychange', onVisible);
    const unlisten = api.onLocked(goLocked).catch(() => () => undefined);
    // Обмен по Wi-Fi или через облако принёс записи — показать свежие.
    const unLan = api.onLan((e) => e.received > 0 && load()).catch(() => () => undefined);
    const unCloud = api.onCloud((e) => !e.running && e.received > 0 && load()).catch(() => () => undefined);
    return () => {
      document.removeEventListener('visibilitychange', onVisible);
      for (const u of [unlisten, unLan, unCloud]) u.then((f) => f());
    };
  }, [showLock, goLocked, load]);

  useEffect(() => {
    if (!toast) return;
    const t = window.setTimeout(() => showToast(null), 3500);
    return () => window.clearTimeout(t);
  }, [toast]);

  const onUnlocked = useCallback(
    (st: LockStatus) => {
      setLock(st);
      lockedRef.current = false;
      setLocked(false);
      load();
      if (st.recovery) setPinFlow('reset');
    },
    [load],
  );

  const security = useMemo(
    () => ({
      lock,
      onPin: (m: PinMode) => setPinFlow(m),
      onBio: (on: boolean) => {
        api.bioSet(on).then(
          (st) => {
            setLock(st);
            say(st.error ?? (on ? 'Вход по отпечатку включён' : 'Вход по отпечатку выключен'));
          },
          (e) => say(message(e)),
        );
      },
      onLockNow: () => {
        api.lockNow().then(
          (st) => {
            setLock(st);
            if (st.locked) showLock();
          },
          (e) => say(message(e)),
        );
      },
    }),
    [lock, say, showLock],
  );

  let body;
  if (tab === 'settings') body = <Settings scheme={scheme} setScheme={setScheme} mode={mode} setMode={setMode} security={security} onSynced={load} />;
  else if (!notes) body = <p className="muted">Открываю…</p>;
  else body = <Notes notes={notes} onChange={setNotes} toast={say} />;

  return (
    <div className={wide ? 'app wide' : 'app phone'}>
      {wide && (
        <aside className="side" aria-label="Разделы">
          <p className="wordmark num">Заготовка</p>
          {TABS.map((t) => (
            <button key={t.id} type="button" className={`navitem${tab === t.id ? ' on' : ''}`} aria-current={tab === t.id ? 'page' : undefined} onClick={() => setTab(t.id)}>
              <Icon name={t.icon} />
              <span>{t.label}</span>
            </button>
          ))}
        </aside>
      )}
      <main className="main">
        <div className="inner">{body}</div>
      </main>
      {!wide && (
        <nav className="bottomnav" aria-label="Разделы">
          {TABS.map((t) => (
            <button key={t.id} type="button" className={tab === t.id ? 'on' : ''} aria-current={tab === t.id ? 'page' : undefined} onClick={() => setTab(t.id)}>
              <Icon name={t.icon} size={22} stroke={tab === t.id ? 2 : 1.75} />
              <span>{t.label}</span>
            </button>
          ))}
        </nav>
      )}
      {toast && (
        <div className="toast" role="status" onClick={() => showToast(null)}>
          {toast}
        </div>
      )}
      {locked && lock && <LockScreen key={lockKey} status={lock} api={api} names={LOCK_NAMES} onUnlocked={onUnlocked} />}
      {pinFlow && !locked && (
        <PinFlow
          api={api}
          mode={pinFlow}
          onClose={() => setPinFlow(null)}
          onDone={(st, text) => {
            setLock(st);
            setPinFlow(null);
            showToast(text);
          }}
        />
      )}
    </div>
  );
}
