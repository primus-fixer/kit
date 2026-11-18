// Настройки: вид, вход, обмен по Wi-Fi, облако, это устройство. Разделы Wi-Fi и облака — общие из kit,
// со словами приложения (WIFI_WORDS, CLOUD_WORDS): поменяйте их под свои записи.
import { useEffect, useState } from 'react';
import { CloudSection, Cut, Row, SCHEMES, Seg, WifiSection, type CloudWords, type ThemeMode, type WifiWords } from '@primus-fixer/kit/ui';
import { api, isMobileApp, LOCKED, message, scanQr, type DeviceInfo } from '../api';
import SecuritySection, { type Security } from './Security';

const WIFI_WORDS: WifiWords = { name: 'Заготовка', feminine: true, files: 'снимков', filesNote: 'Снимки едут вместе с записями.', where: 'Настройки → Обмен по Wi-Fi', port: 47520 };

const CLOUD_WORDS: CloudWords = {
  name: 'Заготовка',
  nameGen: 'Заготовки',
  feminine: true,
  what: 'Записи и снимки',
  blobs: { many: 'снимков', upload: 'Отправляю снимки', download: 'Получаю снимки', waiting: ['снимок придёт', 'снимка придут', 'снимков придут'] },
};

type Props = {
  scheme: string;
  setScheme: (s: string) => void;
  mode: ThemeMode;
  setMode: (m: ThemeMode) => void;
  security: Security;
  onSynced: () => void;
};

export default function Settings({ scheme, setScheme, mode, setMode, security, onSynced }: Props) {
  const [info, setInfo] = useState<DeviceInfo | null>(null);
  useEffect(() => {
    api.deviceInfo().then(setInfo, () => undefined);
  }, []);
  const dark = mode === 'dark' || (mode === 'system' && window.matchMedia('(prefers-color-scheme: dark)').matches);
  return (
    <div className="stack settings">
      <h1 className="screen-title">Настройки</h1>

      <Cut id="look" title="Вид" hint={`${SCHEMES[scheme]?.name ?? ''}, ${mode === 'dark' ? 'тёмная' : mode === 'light' ? 'светлая' : 'как в системе'}`}>
        <div className="schemes" role="group" aria-label="Расцветка">
          {Object.entries(SCHEMES).map(([id, s]) => {
            const t = dark ? s.dark : s.light;
            return (
              <button key={id} type="button" className={`scheme${id === scheme ? ' on' : ''}`} aria-pressed={id === scheme} onClick={() => setScheme(id)}>
                <span className="swatch" style={{ background: `linear-gradient(90deg, ${t.own} 50%, ${t.work} 50%)` }} />
                <span>{s.name}</span>
              </button>
            );
          })}
        </div>
        <Seg
          cols
          small
          label="Тема"
          value={mode}
          options={[
            ['light', 'Светлая'],
            ['dark', 'Тёмная'],
            ['system', 'Как в системе'],
          ]}
          onChange={setMode}
        />
      </Cut>

      <SecuritySection sec={security} />

      <WifiSection api={api} words={WIFI_WORDS} mobile={isMobileApp} scanQr={scanQr} message={message} locked={LOCKED} onSynced={onSynced} />

      <CloudSection api={api} words={CLOUD_WORDS} message={message} locked={LOCKED} />

      <Cut id="device" title="Это устройство" hint={info ? `версия ${info.version}` : undefined}>
        {info && (
          <>
            <Row label="Устройство">{info.name}</Row>
            <Row label="Версия">{info.version}</Row>
            <Row label="Ключ базы">{info.key_method}</Row>
            <Row label="Записей в журнале">{info.events.toLocaleString('ru-RU')}</Row>
          </>
        )}
      </Cut>
    </div>
  );
}
