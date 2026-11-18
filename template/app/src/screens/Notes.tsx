// Записи приложения. В заготовке — заметки: список, новая заметка, удаление. Замените своим экраном.
import { useState } from 'react';
import { api, message, type Note } from '../api';

export default function Notes({ notes, onChange, toast }: { notes: Note[]; onChange: (notes: Note[]) => void; toast: (text: string) => void }) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);

  const add = async () => {
    setBusy(true);
    try {
      const n = await api.noteSave({ id: '', text, created: 0 });
      onChange([n, ...notes]);
      setText('');
    } catch (e) {
      toast(message(e));
    } finally {
      setBusy(false);
    }
  };

  const remove = async (id: string) => {
    try {
      await api.noteDelete(id);
      onChange(notes.filter((n) => n.id !== id));
      toast('Заметка удалена');
    } catch (e) {
      toast(message(e));
    }
  };

  return (
    <div className="stack">
      <h1 className="screen-title">Записи</h1>
      <div className="stack-s">
        <label className="field">
          <span>Новая заметка</span>
          <textarea className="input" rows={3} value={text} onChange={(e) => setText(e.target.value)} />
        </label>
        <div className="sec-actions">
          <button type="button" className="btn primary" disabled={busy || !text.trim()} onClick={add}>
            Записать
          </button>
        </div>
      </div>
      <section>
        <h2 className="sec">Все заметки</h2>
        {notes.length === 0 && <p className="row muted">Заметок пока нет.</p>}
        {notes.map((n) => (
          <div key={n.id} className="row note-row">
            <span className="note-text">{n.text}</span>
            <button type="button" className="link-btn small warn-text" onClick={() => remove(n.id)}>
              Удалить
            </button>
          </div>
        ))}
      </section>
    </div>
  );
}
