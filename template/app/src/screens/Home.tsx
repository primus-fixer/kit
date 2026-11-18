import { Icon, NAME } from '../ui';

/** Главный экран. Пока пустой: записи приложения появятся здесь. `synced` меняется, когда пришло новое
 * с другого устройства, — тогда экран перечитывает данные. */
export default function Home({ synced }: { synced: number }) {
  return (
    <div className="stack" data-synced={synced}>
      <h1 className="screen-title">{NAME}</h1>
      <div className="empty">
        <div className="empty-icon">
          <Icon name="home" size={32} />
        </div>
        <p className="muted">Здесь пока пусто.</p>
      </div>
    </div>
  );
}
