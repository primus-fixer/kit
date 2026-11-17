// Облако — общий раздел экрана настроек приложений на kit: вход (Яндекс Диск по паролю приложения, свой
// сервер WebDAV и, если приложение умеет, вход по коду), пароль копий, обмен. Команды — `kit_tauri::cloud`.
import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { Cut, Seg } from './elements';
import { agoText } from './wifi';

export type CloudFailKind = 'auth' | 'space' | 'offline' | 'server' | 'gone' | 'replaced' | 'conflict' | 'newer' | 'format' | 'cancelled';
export type CloudStep = { step: 'events' } | { step: 'upload' | 'download' | 'files'; done: number; total: number };
export type CloudLast = {
  at: number;
  received: number;
  sent: number;
  files_received: number;
  files_sent: number;
  files_waiting: number;
  whole_received: number;
  whole_sent: number;
  whole_waiting: number;
  problems: string[];
};
export type CloudProviderView = { code: string; title: string; kind: 'password' | 'code' };
export type CloudLogin = { provider: string; title: string; user_code: string; url: string; seconds_left: number };
export type CloudStatus = {
  desktop: boolean;
  providers: CloudProviderView[];
  state: 'off' | 'signin' | 'password' | 'relogin' | 'ready';
  provider: string | null;
  title: string | null;
  login: CloudLogin | null;
  dav: { url: string; host: string; user: string } | null;
  cloud_exists: boolean;
  backup_password: boolean;
  every: 'auto' | 'manual';
  running: boolean;
  progress: CloudStep | null;
  last: CloudLast | null;
  error: { kind: CloudFailKind; text: string; at: number } | null;
  key_pending: boolean;
  notice: string | null;
};
export type CloudEvent = { running: boolean; received: number; step: CloudStep | null };
type DavKind = 'yadisk' | 'webdav';

/** Команды облака: настоящие — {@link cloudCommands}, в демо приложения — свои. Вход по коду — если умеет приложение. */
export type CloudApi = {
  cloudStatus: () => Promise<CloudStatus>;
  cloudDavLogin: (kind: string, url: string | null, user: string, password: string) => Promise<CloudStatus>;
  cloudAppPasswords: () => Promise<void>;
  cloudPassword: (password: string) => Promise<CloudStatus>;
  cloudSyncNow: () => Promise<CloudStatus>;
  cloudEvery: (every: CloudStatus['every']) => Promise<CloudStatus>;
  cloudDisconnect: (wipe: boolean) => Promise<CloudStatus>;
  onCloud: (listener: (e: CloudEvent) => void) => Promise<() => void>;
  cloudLogin?: (provider: string) => Promise<CloudStatus>;
  cloudLoginCheck?: () => Promise<CloudStatus>;
  cloudLoginCancel?: () => Promise<CloudStatus>;
  cloudOpenPage?: () => Promise<void>;
};

/** Команды `kit_tauri::cloud` приложения; `code` — ещё и вход по коду (команды приложения: cloud_login, …). */
export function cloudCommands(code = false): CloudApi {
  const call = async <T,>(cmd: string, args?: Record<string, unknown>) => (await import('@tauri-apps/api/core')).invoke<T>(cmd, args);
  const base: CloudApi = {
    cloudStatus: () => call<CloudStatus>('cloud_status'),
    cloudDavLogin: (kind, url, user, password) => call<CloudStatus>('cloud_dav_login', { kind, url, user, password }),
    cloudAppPasswords: () => call<void>('cloud_app_passwords'),
    cloudPassword: (password) => call<CloudStatus>('cloud_password', { password }),
    cloudSyncNow: () => call<CloudStatus>('cloud_sync_now'),
    cloudEvery: (every) => call<CloudStatus>('cloud_every', { every }),
    cloudDisconnect: (wipe) => call<CloudStatus>('cloud_disconnect', { wipe }),
    onCloud: async (listener) => (await import('@tauri-apps/api/event')).listen<CloudEvent>('cloud', (ev) => listener(ev.payload)),
  };
  if (!code) return base;
  return {
    ...base,
    cloudLogin: (provider) => call<CloudStatus>('cloud_login', { provider }),
    cloudLoginCheck: () => call<CloudStatus>('cloud_login_check'),
    cloudLoginCancel: () => call<CloudStatus>('cloud_login_cancel'),
    cloudOpenPage: () => call<void>('cloud_open_page'),
  };
}

/** Чем приложения отличаются на этом экране. Формы слов — для 1, 2 и 5 («обложка», «обложки», «обложек»). */
export type CloudWords = {
  /** «Полка», «Полки» и род. */
  name: string;
  nameGen: string;
  feminine: boolean;
  /** Что ходит через облако, кроме записей: «Записи и обложки», «Записи и снимки чеков». */
  what: string;
  /** Добавка к первому абзацу: «— в командировке общий Wi-Fi не нужен». */
  whyNote?: string;
  /** Вложения: «обложек» и что делается с ними во время обмена. */
  blobs: { many: string; upload: string; download: string; waiting: [string, string, string] };
  /** Большие файлы (у «Полки» — книги); нет — не показываются. */
  whole?: { label: string; many: string; waiting: [string, string, string] };
};

const NB = '\u00a0';

function plural(n: number, [one, few, many]: [string, string, string]): string {
  const m10 = n % 10;
  const m100 = n % 100;
  const w = m10 === 1 && m100 !== 11 ? one : m10 >= 2 && m10 <= 4 && (m100 < 12 || m100 > 14) ? few : many;
  return `${n} ${w}`;
}
const EVERY: [CloudStatus['every'], string][] = [
  ['auto', 'Сам'],
  ['manual', 'Только по кнопке'],
];

export function stepText(s: CloudStep | null, w: CloudWords): string {
  if (!s || s.step === 'events') return 'Обмениваюсь записями…';
  const n = `${Math.min(s.done + 1, s.total)} из ${s.total}`;
  if (s.step === 'files') return `${w.whole?.label ?? 'Файлы'}: ${n}`;
  return s.step === 'upload' ? `${w.blobs.upload}: ${n}` : `${w.blobs.download}: ${n}`;
}

export function lastText(l: CloudLast, w: CloudWords): string {
  const parts = [`получено ${l.received}, отправлено ${l.sent}`];
  if (l.files_received + l.files_sent > 0) parts.push(`${w.blobs.many}: получено ${l.files_received}, отправлено ${l.files_sent}`);
  const br = l.whole_received ?? 0;
  const bs = l.whole_sent ?? 0;
  if (br + bs > 0) parts.push(`${w.whole?.many ?? 'файлов'}: получено ${br}, отправлено ${bs}`);
  return parts.join('; ');
}

/** Пароль копий для облака: ввести (облако уже есть или пароль копий задан) или придумать. */
function CloudPassword({ api, message, words, st, onDone, onCancel }: { api: CloudApi; message: (e: unknown) => string; words: CloudWords; st: CloudStatus; onDone: (s: CloudStatus) => void; onCancel: () => void }) {
  const invent = !st.cloud_exists && !(st.desktop && st.backup_password);
  const [a, setA] = useState('');
  const [b, setB] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    if ([...a].length < 8) return setError('Пароль копий — не короче 8 символов');
    if (invent && a !== b) return setError('Пароли не совпадают');
    setBusy(true);
    setError(null);
    try {
      const s = await api.cloudPassword(a);
      setA('');
      setB('');
      onDone(s);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  const text = st.cloud_exists
    ? `В облаке уже есть данные «${words.nameGen}». Введите пароль копий, которым они зашифрованы.`
    : st.desktop && st.backup_password
      ? 'Введите пароль копий этого компьютера — им будут зашифрованы данные в облаке.'
      : `Придумайте пароль копий: им шифруются данные в облаке${st.desktop ? ' и резервные копии' : ''}. Не короче 8 символов, лучше — фраза из нескольких слов. Запишите его: без пароля новое устройство не получит данные из облака.`;
  return (
    <form
      className="key-form"
      aria-label="Пароль копий для облака"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <p className="small muted">{text}</p>
      <label className="field">
        <span>Пароль копий</span>
        <input type="password" autoComplete={invent ? 'new-password' : 'current-password'} value={a} onChange={(e) => setA(e.target.value)} disabled={busy} />
      </label>
      {invent && (
        <label className="field">
          <span>Ещё раз</span>
          <input type="password" autoComplete="new-password" value={b} onChange={(e) => setB(e.target.value)} disabled={busy} />
        </label>
      )}
      {error && <p className="small warn-text">{error}</p>}
      <div className="key-actions">
        <button type="submit" className="btn primary small" disabled={busy || !a || (invent && !b)}>
          {busy ? 'Проверяю пароль…' : 'Подключить'}
        </button>
        <button type="button" className="link-btn" disabled={busy} onClick={onCancel}>
          Отмена
        </button>
      </div>
    </form>
  );
}

/** Вход по логину и паролю: Яндекс Диск (пароль приложения) или другой сервер WebDAV. */
function DavForm({
  api,
  message,
  words,
  kind,
  st,
  again,
  onDone,
  onCancel,
}: {
  api: CloudApi;
  message: (e: unknown) => string;
  words: CloudWords;
  kind: DavKind;
  st: CloudStatus;
  again: boolean;
  onDone: (s: CloudStatus) => void;
  onCancel: () => void;
}) {
  const ya = kind === 'yadisk';
  const [url, setUrl] = useState(st.dav && st.provider === kind ? st.dav.url : '');
  const [user, setUser] = useState(st.dav && st.provider === kind ? st.dav.user : '');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      const s = await api.cloudDavLogin(kind, ya ? null : url, user, password);
      setPassword('');
      onDone(s);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className="key-form"
      aria-label={ya ? 'Вход в Яндекс Диск' : 'Вход на сервер WebDAV'}
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <p className="cloud-form-title">{ya ? 'Яндекс Диск — вход по паролю приложения' : 'Свой сервер WebDAV'}</p>
      {ya ? (
        <>
          <p className="small muted">
            Нужен <b>пароль приложения</b>, а не пароль от аккаунта Яндекса: Яндекс ID → Безопасность → Пароли приложений → «Файлы WebDAV». Яндекс показывает его один раз — сразу скопируйте. Новый пароль
            начинает действовать через 2–3{NB}часа.
          </p>
          <div className="key-actions">
            <button type="button" className="btn line small" onClick={() => api.cloudAppPasswords().catch((e) => setError(message(e)))}>
              Открыть Яндекс ID
            </button>
          </div>
        </>
      ) : (
        <>
          <p className="small muted">Подойдёт сервер WebDAV с https: Nextcloud, Synology или другой NAS. Адрес — папка WebDAV пользователя, например https://cloud.example.ru/remote.php/dav/files/имя.</p>
          <label className="field">
            <span>Адрес сервера</span>
            <input type="url" inputMode="url" autoComplete="url" spellCheck={false} placeholder="https://…" value={url} onChange={(e) => setUrl(e.target.value)} disabled={busy} />
          </label>
        </>
      )}
      <label className="field">
        <span>{ya ? 'Логин Яндекса' : 'Логин'}</span>
        <input autoComplete="username" autoCapitalize="none" spellCheck={false} value={user} onChange={(e) => setUser(e.target.value)} disabled={busy} />
      </label>
      <label className="field">
        <span>{ya ? 'Пароль приложения' : 'Пароль'}</span>
        <input type="password" autoComplete="current-password" value={password} onChange={(e) => setPassword(e.target.value)} disabled={busy} />
      </label>
      <p className="small muted3">
        Пароль хранится только на этом устройстве, зашифрованным, и не попадает в копии. «{words.name}» работает только в своей папке «{words.name}».
      </p>
      {error && <p className="small warn-text">{error}</p>}
      <div className="key-actions">
        <button type="submit" className="btn primary small" disabled={busy || !user.trim() || !password || (!ya && !url.trim())}>
          {busy ? 'Проверяю вход…' : again ? 'Войти снова' : 'Войти'}
        </button>
        {!again && (
          <button type="button" className="link-btn" disabled={busy} onClick={onCancel}>
            Отмена
          </button>
        )}
      </div>
    </form>
  );
}

/** Раздел «Облако»: Яндекс Диск по паролю приложения, свой сервер WebDAV, вход по коду (если умеет приложение).
 * `wrap` — свой заголовок раздела (по умолчанию — раздел под катом), `demo` — облако понарошку. */
export function CloudSection({
  api,
  words,
  message,
  locked,
  demo,
  wrap,
}: {
  api: CloudApi;
  words: CloudWords;
  message: (e: unknown) => string;
  locked?: string;
  demo?: boolean;
  wrap?: (hint: string | undefined, children: ReactNode) => ReactNode;
}) {
  const [st, setSt] = useState<CloudStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [asking, setAsking] = useState(false);
  const [wipe, setWipe] = useState(false);
  const [form, setForm] = useState<DavKind | null>(null);
  const [copied, setCopied] = useState(false);
  const fail = (e: unknown) => {
    if (message(e) !== locked) setError(message(e));
  };

  // eslint-disable-next-line react-hooks/exhaustive-deps
  const load = useCallback(() => api.cloudStatus().then(setSt, fail), []);
  useEffect(() => {
    load();
    const un = api.onCloud(() => load()).catch(() => () => undefined);
    return () => {
      un.then((f) => f());
    };
  }, [load]);

  // Пока виден код входа, каждые 2 секунды спрашиваем, ввели ли его.
  const signin = st?.state === 'signin';
  useEffect(() => {
    if (!signin || !api.cloudLoginCheck) return;
    let alive = true;
    const id = window.setInterval(async () => {
      try {
        const s = await api.cloudLoginCheck!();
        if (alive) setSt(s);
      } catch (e) {
        if (!alive) return;
        fail(e);
        load();
      }
    }, 2000);
    return () => {
      alive = false;
      window.clearInterval(id);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [signin, load]);
  const copy = (text: string) => {
    navigator.clipboard?.writeText(text).then(
      () => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 2000);
      },
      () => undefined,
    );
  };

  const act = async (p: () => Promise<CloudStatus>): Promise<CloudStatus | null> => {
    setBusy(true);
    setError(null);
    try {
      const s = await p();
      setSt(s);
      setNotice(s.notice);
      return s;
    } catch (e) {
      fail(e);
      return null;
    } finally {
      setBusy(false);
    }
  };
  const disconnect = async () => {
    const s = await act(() => api.cloudDisconnect(wipe));
    if (s) {
      setAsking(false);
      setWipe(false);
    }
  };
  const signedIn = (s: CloudStatus) => {
    setSt(s);
    setNotice(s.notice);
    setForm(null);
  };

  const who = st?.dav ? (st.provider === 'webdav' ? st.dav.host : st.dav.user) : null;
  const davLink: DavKind | null = st?.provider === 'yadisk' || st?.provider === 'webdav' ? st.provider : null;
  const props = { api, message, words };
  const hint = !st ? undefined : st.state === 'off' ? 'не подключено' : st.state === 'ready' ? (st.title ?? 'подключено') : 'нужно войти';

  const content = (
    <>
      {st?.state === 'off' && !form && (
        <>
          <p className="small muted">
            {words.what} ходят через ваш Яндекс Диск или свой сервер{words.whyNote ? ` ${words.whyNote}` : ' — устройствам не нужно быть в одной сети'}. В облаке лежат только зашифрованные файлы в папке «
            {words.name}»: открыть их можно только паролем копий.
          </p>
          <div className="key-actions">
            {st.providers.map((p, i) => (
              <button
                key={p.code}
                type="button"
                className={'btn small ' + (i === 0 ? 'primary' : 'line')}
                disabled={busy}
                onClick={() => {
                  setError(null);
                  setNotice(null);
                  if (p.kind === 'password') setForm(p.code as DavKind);
                  else if (api.cloudLogin) act(() => api.cloudLogin!(p.code));
                }}
              >
                {p.title}
              </button>
            ))}
          </div>
        </>
      )}

      {st?.state === 'off' && form && <DavForm key={form} {...props} kind={form} st={st} again={false} onDone={signedIn} onCancel={() => setForm(null)} />}

      {st?.state === 'signin' && st.login && (
        <div className="paircard stack-s">
          <p className="small muted">Войдите в аккаунт {st.login.provider === 'yandex' ? 'Яндекса' : 'Google'} в браузере и введите код:</p>
          <p className="paircode" aria-label="Код для входа">
            {st.login.user_code}
          </p>
          <div className="key-actions">
            <button type="button" className="btn primary small" onClick={() => copy(st.login!.user_code)}>
              {copied ? 'Скопировано' : 'Скопировать код'}
            </button>
            <button type="button" className="btn line small" onClick={() => api.cloudOpenPage?.().catch(fail)}>
              Открыть страницу
            </button>
            <button type="button" className="link-btn" disabled={busy} onClick={() => api.cloudLoginCancel && act(api.cloudLoginCancel)}>
              Отмена
            </button>
          </div>
          <p className="small muted">
            Страница входа: <span style={{ overflowWrap: 'anywhere' }}>{st.login.url}</span>. «{words.name}» получит доступ только к своей папке
            {st.login.provider === 'yandex' ? ` «Приложения/${words.name}»` : ` «${words.name}»`}. Код действует {Math.max(1, Math.round(st.login.seconds_left / 60))}
            {NB}мин.
          </p>
          <p className="small muted3">Жду, пока вы введёте код…</p>
        </div>
      )}

      {st?.state === 'password' && (
        <>
          {st.error && <p className="small warn-text">{st.error.text}</p>}
          <p className="small muted">
            {st.title}: вход выполнен{who ? ` (${who})` : ''}.
          </p>
          <CloudPassword
            {...props}
            st={st}
            onDone={(s) => {
              setSt(s);
              setNotice(s.notice);
            }}
            onCancel={() => act(() => api.cloudDisconnect(false)).then(() => setNotice(null))}
          />
        </>
      )}

      {st?.state === 'relogin' && davLink && (
        <>
          <p className="small warn-text">{st.error?.text ?? `${st.title} не принимает пароль — войдите снова.`}</p>
          <p className="small muted">Записи и пароль копий остаются на устройстве: после входа обмен продолжится сам.</p>
          <DavForm key={davLink} {...props} kind={davLink} st={st} again onDone={signedIn} onCancel={() => undefined} />
          {!asking && (
            <button type="button" className="link-btn" disabled={busy} onClick={() => setAsking(true)}>
              Отключить облако
            </button>
          )}
        </>
      )}

      {st?.state === 'relogin' && !davLink && (
        <>
          <p className="small warn-text">{st.error?.text ?? 'Вход в облако устарел — войдите снова.'}</p>
          <p className="small muted">Записи и пароль копий остаются на устройстве: после входа обмен продолжится сам.</p>
          <div className="key-actions">
            <button type="button" className="btn primary small" disabled={busy || !api.cloudLogin} onClick={() => act(() => api.cloudLogin!(st.provider!))}>
              Войти снова
            </button>
            <button type="button" className="link-btn" disabled={busy} onClick={() => setAsking(true)}>
              Отключить
            </button>
          </div>
        </>
      )}

      {st?.state === 'ready' && (
        <>
          <div className="row">
            <span className="row-label">
              <span>{st.title}</span>
              {who && <span className="small muted">{who}</span>}
            </span>
            <span className="row-val ok-text">подключён</span>
          </div>
          <div className="row">
            <span>Последний обмен</span>
            <span className="row-val cloud-last" aria-live="polite">
              {st.running ? (
                stepText(st.progress, words)
              ) : st.last ? (
                <>
                  {agoText(st.last.at)}
                  <span className="small muted">{lastText(st.last, words)}</span>
                </>
              ) : (
                <span className="small muted">ещё не было</span>
              )}
            </span>
          </div>
          {!st.running && st.last && words.whole && (st.last.whole_waiting ?? 0) > 0 && (
            <p className="small muted">Ещё {plural(st.last.whole_waiting ?? 0, words.whole.waiting)}, когда другое устройство отправит их в облако целиком.</p>
          )}
          {!st.running && st.last && st.last.files_waiting > 0 && <p className="small muted">Ещё {plural(st.last.files_waiting, words.blobs.waiting)}, когда их отправит другое устройство.</p>}
          {st.error && <p className="small warn-text">{st.error.text}</p>}
          {st.last?.problems.map((p) => (
            <p key={p} className="small muted3">
              {p}
            </p>
          ))}
          {st.key_pending && <p className="small muted">Новый пароль копий уйдёт в облако при ближайшем обмене.</p>}
          <div className="stack-s">
            <span className="small muted">Обмен через облако</span>
            <Seg label="Когда обмениваться через облако" value={st.every} options={EVERY} onChange={(v) => act(() => api.cloudEvery(v))} small />
            <p className="small muted">
              {st.every === 'auto'
                ? `При открытии «${words.nameGen}», через минуту после изменений и каждые 15 минут, пока ${words.feminine ? 'она открыта' : 'он открыт'}.`
                : 'Только по кнопке «Обменяться сейчас».'}
            </p>
          </div>
          {!asking && (
            <div className="key-actions">
              <button type="button" className="btn primary small" disabled={busy || st.running} onClick={() => act(api.cloudSyncNow)}>
                {st.running ? 'Обмениваюсь…' : 'Обменяться сейчас'}
              </button>
              <button type="button" className="link-btn" disabled={busy} onClick={() => setAsking(true)}>
                Отключить
              </button>
            </div>
          )}
        </>
      )}

      {asking && st && (st.state === 'ready' || st.state === 'relogin') && (
        <div className="paircard stack-s" role="group" aria-label="Отключить облако">
          <p>Отключить облако на этом устройстве?</p>
          {st.state === 'ready' && (
            <label className="cloud-check">
              <input type="checkbox" checked={wipe} onChange={(e) => setWipe(e.target.checked)} />
              <span className="row-label">
                <span>Удалить данные «{words.nameGen}» из облака</span>
                <span className="small muted">Другие устройства тоже перестанут обмениваться через облако. На самих устройствах всё останется.</span>
              </span>
            </label>
          )}
          <div className="key-actions">
            <button type="button" className={'btn small ' + (wipe ? 'warn' : 'line')} disabled={busy} onClick={disconnect}>
              {busy ? 'Отключаю…' : wipe ? 'Отключить и удалить' : 'Отключить'}
            </button>
            <button
              type="button"
              className="link-btn"
              disabled={busy}
              onClick={() => {
                setAsking(false);
                setWipe(false);
              }}
            >
              Отмена
            </button>
          </div>
        </div>
      )}

      {notice && <p className="small muted">{notice}</p>}
      {error && <p className="small warn-text">{error}</p>}
      {demo && <p className="small muted">В демо облако понарошку: вход и обмен только изображаются, файлы никуда не уходят.</p>}
    </>
  );
  return wrap ? (
    <>{wrap(hint, content)}</>
  ) : (
    <Cut id="cloud" title="Облако" className="cloud" hint={hint}>
      {content}
    </Cut>
  );
}
