// Настройки → «Вход»: PIN-код, отпечаток или Windows Hello — у каждого устройства свой.
import { Cut, bioLabel, isWindows, type PinMode } from '@primus-fixer/kit/ui';
import type { LockStatus } from '../api';

export type Security = {
  lock: LockStatus | null;
  onPin: (mode: PinMode) => void;
  onBio: (on: boolean) => void;
  onLockNow: () => void;
};

const isAndroid = typeof navigator !== 'undefined' && /android/i.test(navigator.userAgent);

export default function SecuritySection({ sec }: { sec: Security }) {
  const lock = sec.lock;
  if (!lock) return null;
  const bio = bioLabel(lock.bio_kind);
  const bioNote = !lock.pin ? 'Сначала задайте PIN-код — он останется запасным' : lock.bio_ready ? 'PIN-код остаётся запасным' : 'На этом устройстве недоступно';
  const recover = isWindows ? 'через Windows Hello' : isAndroid ? 'блокировкой экрана телефона' : 'средствами системы';
  return (
    <Cut id="security" title="Вход" hint={!lock.pin ? 'без кода' : lock.bio ? `PIN-код и ${bio.name}` : 'по PIN-коду'}>
      <p className="small muted">{lock.pin ? 'PIN-код спрашивается при запуске и после 5 минут в фоне.' : 'Сейчас приложение открывается без кода.'} PIN-код у каждого устройства свой.</p>
      <div className="sec-actions">
        {!lock.pin ? (
          <button type="button" className="btn primary small" onClick={() => sec.onPin('create')}>
            Задать PIN-код
          </button>
        ) : (
          <>
            <button type="button" className="btn line small" onClick={() => sec.onPin('change')}>
              Сменить PIN
            </button>
            <button type="button" className="btn line small" onClick={() => sec.onPin('off')}>
              Выключить
            </button>
            <button type="button" className="btn line small" onClick={sec.onLockNow}>
              Заблокировать сейчас
            </button>
          </>
        )}
      </div>
      <label className="cloud-check" style={{ opacity: lock.pin && lock.bio_ready ? 1 : 0.65 }}>
        <input type="checkbox" role="switch" aria-label={bio.title} checked={lock.bio} disabled={!lock.pin || !lock.bio_ready} onChange={(e) => sec.onBio(e.target.checked)} />
        <span className="row-label">
          <span>{bio.title}</span>
          <span className="small muted">{bioNote}</span>
        </span>
      </label>
      {lock.pin && <p className="small muted">Забыли PIN-код — на экране входа нажмите «Забыли PIN?» и подтвердите себя {recover}. Данные останутся.</p>}
    </Cut>
  );
}
