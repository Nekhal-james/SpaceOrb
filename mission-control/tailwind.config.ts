import type { Config } from 'tailwindcss';

const config: Config = {
    content: [
        './app/**/*.{js,ts,jsx,tsx,mdx}',
        './components/**/*.{js,ts,jsx,tsx,mdx}',
    ],
    theme: {
        extend: {
            colors: {
                // SpaceOrb color palette
                'orb-bg': '#0a0e1a',
                'orb-surface': '#111827',
                'orb-border': '#1f2937',
                'orb-accent': '#3b82f6',
                'orb-success': '#10b981',
                'orb-warning': '#f59e0b',
                'orb-danger': '#ef4444',
                'orb-anomaly': '#dc2626',
                'orb-text': '#e5e7eb',
                'orb-muted': '#6b7280',
            },
            animation: {
                'red-alert': 'red-alert-pulse 1s ease-in-out infinite',
                'orb-glow': 'orb-glow 2s ease-in-out infinite alternate',
                'data-flow': 'data-flow 1.5s ease-in-out infinite',
                'fade-in': 'fade-in 0.5s ease-out',
            },
            keyframes: {
                'red-alert-pulse': {
                    '0%, 100%': {
                        boxShadow: '0 0 20px rgba(220, 38, 38, 0.3), inset 0 0 20px rgba(220, 38, 38, 0.1)',
                        borderColor: 'rgba(220, 38, 38, 0.8)',
                    },
                    '50%': {
                        boxShadow: '0 0 40px rgba(220, 38, 38, 0.6), inset 0 0 40px rgba(220, 38, 38, 0.2)',
                        borderColor: 'rgba(220, 38, 38, 1)',
                    },
                },
                'orb-glow': {
                    '0%': {
                        boxShadow: '0 0 10px rgba(59, 130, 246, 0.3)',
                    },
                    '100%': {
                        boxShadow: '0 0 25px rgba(59, 130, 246, 0.6)',
                    },
                },
                'data-flow': {
                    '0%': { opacity: '0.3' },
                    '50%': { opacity: '1' },
                    '100%': { opacity: '0.3' },
                },
                'fade-in': {
                    '0%': { opacity: '0', transform: 'translateY(10px)' },
                    '100%': { opacity: '1', transform: 'translateY(0)' },
                },
            },
            fontFamily: {
                mono: ['JetBrains Mono', 'Fira Code', 'monospace'],
                display: ['Inter', 'system-ui', 'sans-serif'],
            },
        },
    },
    plugins: [],
};

export default config;
