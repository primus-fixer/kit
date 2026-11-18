import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';

// На телефоне при разработке Tauri подставляет адрес компьютера в TAURI_DEV_HOST.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [react()],
  // Внутри репозитория kit заготовка берёт kit ссылкой на папку (file:../..) — общие библиотеки
  // берутся из папки приложения, а не из kit. В приложении, сделанном из заготовки, это ничего не меняет.
  resolve: { preserveSymlinks: true, dedupe: ['react', 'react-dom', '@tauri-apps/api'] },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: 'ws', host, port: 1421 } : undefined,
    watch: { ignored: ['**/src-tauri/**'] },
  },
  build: {
    target: 'es2022',
  },
});
