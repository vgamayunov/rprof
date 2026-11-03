export type Theme = 'dark' | 'light';
const STORAGE_KEY = 'rprof.theme';

export function initialTheme(): Theme {
  try {
    const saved = localStorage.getItem(STORAGE_KEY);
    if (saved === 'dark' || saved === 'light') return saved;
  } catch { /* Storage can be unavailable for offline files or private browsing. */ }
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

export function rememberTheme(theme: Theme): void {
  try { localStorage.setItem(STORAGE_KEY, theme); } catch { /* The toggle still works. */ }
}

export const canvasThemes = {
  dark: {
    background: '#111d2a', shard: '#1e3344', rank: '#1b2e3e', odd: '#162432', even: '#192838',
    border: '#2c3c4b', heading: '#e5f2f5', label: '#b4c6d2', muted: '#84a7b5',
    labelBackground: '#142330', grid: '#253847', cpu: '#63d2b2', memory: '#7cb3ff',
    partial: '#f2a895', ruler: '#1c3040', tick: '#426072',
    incomplete: '#ffe3a2', selected: '#fff4bd', link: '#ffe5a7',
    counters: ['#6bcdb0', '#65a8ec', '#e5b46d', '#b29be6', '#e17d91', '#86c6d9', '#cbc988'],
  },
  light: {
    background: '#f8fafc', shard: '#dde8f0', rank: '#e7eef4', odd: '#ffffff', even: '#f1f5f9',
    border: '#cedae3', heading: '#203b4b', label: '#314f60', muted: '#526b79',
    labelBackground: '#f0f5f8', grid: '#e0e9ef', cpu: '#167b63', memory: '#286bc1',
    partial: '#9b382f', ruler: '#e8eff5', tick: '#8fa6b6',
    incomplete: '#915a00', selected: '#784300', link: '#895208',
    counters: ['#167b63', '#2168ad', '#966511', '#7152ae', '#ae3f58', '#207b8c', '#687319'],
  },
};
