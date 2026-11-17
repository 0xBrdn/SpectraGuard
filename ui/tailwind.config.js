/** @type {import('tailwindcss').Config} */
export default {
  content: ['./index.html', './src/**/*.{ts,tsx}'],
  theme: {
    extend: {
      colors: {
        obsidian: '#090b10',
        panel: '#0d1118',
        panelAlt: '#111621',
        slateBorder: '#1e2533',
        slateMuted: '#64748b',
        cyanTrace: '#22d3ee',
        amberHold: '#f59e0b',
        crimsonAlert: '#ef4444',
        emeraldOk: '#10b981',
      },
      fontFamily: {
        mono: [
          '"JetBrains Mono"',
          'ui-monospace',
          'SFMono-Regular',
          'Menlo',
          'Monaco',
          'Consolas',
          'monospace',
        ],
        sans: [
          'Inter',
          '-apple-system',
          'BlinkMacSystemFont',
          '"Segoe UI"',
          'Roboto',
          'sans-serif',
        ],
      },
    },
  },
  plugins: [],
};