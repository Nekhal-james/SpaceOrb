'use client';

import { useEffect, useState, useCallback, useRef } from 'react';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface SystemState {
    epoch: string;
    queue_depth: number;
    vault_sealed_count: number;
    soc_percent: number;
    power_state: string;
    active_anomalies: number;
    dtn_link_active: boolean;
    chunks_pending: number;
    usb_primary_util: number;
    usb_mirror_util: number;
}

// ---------------------------------------------------------------------------
// WebSocket Hook — connects to the 500ms state stream
// ---------------------------------------------------------------------------

function useWebSocketState(url: string) {
    const [state, setState] = useState<SystemState | null>(null);
    const [connected, setConnected] = useState(false);
    const wsRef = useRef<WebSocket | null>(null);
    const reconnectTimer = useRef<NodeJS.Timeout | null>(null);

    const connect = useCallback(() => {
        try {
            const ws = new WebSocket(url);
            wsRef.current = ws;

            ws.onopen = () => {
                setConnected(true);
                console.log('[WS] Connected to mission-core state stream');
            };

            ws.onmessage = (event) => {
                try {
                    const data: SystemState = JSON.parse(event.data);
                    setState(data);
                } catch (e) {
                    console.error('[WS] Failed to parse state:', e);
                }
            };

            ws.onclose = () => {
                setConnected(false);
                console.log('[WS] Disconnected — reconnecting in 2s...');
                reconnectTimer.current = setTimeout(connect, 2000);
            };

            ws.onerror = () => {
                ws.close();
            };
        } catch (e) {
            console.error('[WS] Connection error:', e);
            reconnectTimer.current = setTimeout(connect, 2000);
        }
    }, [url]);

    useEffect(() => {
        connect();
        return () => {
            wsRef.current?.close();
            if (reconnectTimer.current) clearTimeout(reconnectTimer.current);
        };
    }, [connect]);

    return { state, connected };
}

// ---------------------------------------------------------------------------
// Demo State (for exhibition when supervisor is not running)
// ---------------------------------------------------------------------------

function useDemoState(): SystemState {
    const [state, setState] = useState<SystemState>({
        epoch: new Date().toISOString(),
        queue_depth: 42,
        vault_sealed_count: 1847,
        soc_percent: 72.5,
        power_state: 'Solar',
        active_anomalies: 0,
        dtn_link_active: true,
        chunks_pending: 7,
        usb_primary_util: 43.2,
        usb_mirror_util: 43.1,
    });

    useEffect(() => {
        const interval = setInterval(() => {
            setState(prev => {
                const newSoc = prev.soc_percent + (Math.random() - 0.48) * 2;
                const clampedSoc = Math.max(5, Math.min(100, newSoc));
                const newAnomalies = Math.random() > 0.95 ? 1 : prev.active_anomalies > 0 && Math.random() > 0.7 ? 0 : prev.active_anomalies;
                return {
                    ...prev,
                    epoch: new Date().toISOString(),
                    queue_depth: Math.max(0, prev.queue_depth + Math.floor(Math.random() * 5 - 2)),
                    vault_sealed_count: prev.vault_sealed_count + Math.floor(Math.random() * 3),
                    soc_percent: clampedSoc,
                    power_state: clampedSoc > 50 ? 'Solar' : clampedSoc < 30 ? 'Eclipse' : 'Transition',
                    active_anomalies: newAnomalies,
                    dtn_link_active: Math.random() > 0.05,
                    chunks_pending: Math.max(0, prev.chunks_pending + Math.floor(Math.random() * 4 - 2)),
                    usb_primary_util: Math.min(100, prev.usb_primary_util + (Math.random() - 0.45) * 0.5),
                    usb_mirror_util: Math.min(100, prev.usb_mirror_util + (Math.random() - 0.45) * 0.5),
                };
            });
        }, 500);

        return () => clearInterval(interval);
    }, []);

    return state;
}

// ---------------------------------------------------------------------------
// Components
// ---------------------------------------------------------------------------

function StatusIndicator({ active, label }: { active: boolean; label: string }) {
    return (
        <div className="flex items-center gap-2">
            <div className={`w-2.5 h-2.5 rounded-full ${active ? 'bg-orb-success animate-pulse' : 'bg-orb-danger'}`} />
            <span className="text-sm text-orb-muted">{label}</span>
        </div>
    );
}

function MetricCard({
    title,
    value,
    unit,
    subtitle,
    alert,
}: {
    title: string;
    value: string | number;
    unit?: string;
    subtitle?: string;
    alert?: boolean;
}) {
    return (
        <div
            className={`
        rounded-xl border p-5 transition-all duration-300
        ${alert
                    ? 'border-orb-anomaly bg-red-950/30 animate-red-alert'
                    : 'border-orb-border bg-orb-surface hover:border-orb-accent/50 hover:shadow-lg hover:shadow-orb-accent/5'
                }
      `}
        >
            <p className="text-xs font-medium uppercase tracking-wider text-orb-muted mb-2">
                {title}
            </p>
            <div className="flex items-baseline gap-1.5">
                <span
                    className={`text-3xl font-semibold font-mono ${alert ? 'text-orb-anomaly' : 'text-white'
                        }`}
                >
                    {value}
                </span>
                {unit && <span className="text-sm text-orb-muted">{unit}</span>}
            </div>
            {subtitle && (
                <p className="text-xs text-orb-muted mt-1.5">{subtitle}</p>
            )}
        </div>
    );
}

function ProgressBar({
    value,
    max = 100,
    label,
    danger = false,
}: {
    value: number;
    max?: number;
    label: string;
    danger?: boolean;
}) {
    const percent = Math.min((value / max) * 100, 100);
    const barColor = danger || percent > 85
        ? 'bg-orb-danger'
        : percent > 70
            ? 'bg-orb-warning'
            : 'bg-orb-accent';

    return (
        <div className="space-y-1.5">
            <div className="flex justify-between text-xs text-orb-muted">
                <span>{label}</span>
                <span className="font-mono">{percent.toFixed(1)}%</span>
            </div>
            <div className="h-2 rounded-full bg-orb-bg overflow-hidden">
                <div
                    className={`h-full rounded-full transition-all duration-500 ${barColor}`}
                    style={{ width: `${percent}%` }}
                />
            </div>
        </div>
    );
}

function PowerBadge({ state }: { state: string }) {
    const config: Record<string, { bg: string; text: string; icon: string }> = {
        Solar: { bg: 'bg-emerald-900/50', text: 'text-emerald-400', icon: '☀️' },
        Eclipse: { bg: 'bg-red-900/50', text: 'text-red-400', icon: '🌑' },
        Transition: { bg: 'bg-amber-900/50', text: 'text-amber-400', icon: '🌗' },
    };
    const c = config[state] || config.Solar;

    return (
        <span className={`inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-medium ${c.bg} ${c.text}`}>
            <span>{c.icon}</span>
            {state}
        </span>
    );
}

// ---------------------------------------------------------------------------
// Main Dashboard Page
// ---------------------------------------------------------------------------

export default function MissionControlDashboard() {
    // Try WebSocket first; fall back to demo state for exhibition
    const ws = useWebSocketState('ws://localhost:9090/ws');
    const demo = useDemoState();
    const state = ws.state || demo;
    const isLive = ws.connected;
    const isRedAlert = state.active_anomalies > 0;

    return (
        <div className="min-h-screen grid-bg relative">
            {/* Red Alert Overlay */}
            {isRedAlert && <div className="red-alert-overlay" />}

            {/* Header */}
            <header className="border-b border-orb-border bg-orb-surface/80 backdrop-blur-sm sticky top-0 z-40">
                <div className="max-w-7xl mx-auto px-6 py-4 flex items-center justify-between">
                    <div className="flex items-center gap-4">
                        <div className="w-10 h-10 rounded-xl bg-gradient-to-br from-orb-accent to-indigo-600 flex items-center justify-center text-white font-bold text-lg shadow-lg shadow-orb-accent/20">
                            ◉
                        </div>
                        <div>
                            <h1 className="text-lg font-semibold text-white tracking-tight">
                                SpaceOrb V7.6
                            </h1>
                            <p className="text-xs text-orb-muted">Mission Control — Ground Station</p>
                        </div>
                    </div>

                    <div className="flex items-center gap-6">
                        <PowerBadge state={state.power_state} />
                        <StatusIndicator active={isLive} label={isLive ? 'LIVE' : 'DEMO'} />
                        <StatusIndicator active={state.dtn_link_active} label="DTN Link" />
                        {isRedAlert && (
                            <span className="inline-flex items-center gap-1.5 px-3 py-1 rounded-full text-xs font-bold bg-red-600 text-white animate-pulse">
                                🚨 RED ALERT
                            </span>
                        )}
                    </div>
                </div>
            </header>

            {/* Main Grid */}
            <main className="max-w-7xl mx-auto px-6 py-8 space-y-8 animate-fade-in">
                {/* Epoch & Time */}
                <section className="rounded-xl border border-orb-border bg-orb-surface p-5">
                    <div className="flex items-center justify-between">
                        <div>
                            <p className="text-xs font-medium uppercase tracking-wider text-orb-muted mb-1">
                                Virtual Epoch
                            </p>
                            <p className="text-xl font-mono text-orb-accent">
                                {state.epoch ? new Date(state.epoch).toISOString().replace('T', '  ').replace('Z', ' UTC') : '—'}
                            </p>
                        </div>
                        <div className="text-right">
                            <p className="text-xs text-orb-muted mb-1">Battery SoC</p>
                            <p className={`text-3xl font-mono font-semibold ${state.soc_percent < 30 ? 'text-orb-danger' : state.soc_percent < 50 ? 'text-orb-warning' : 'text-orb-success'
                                }`}>
                                {state.soc_percent.toFixed(1)}%
                            </p>
                        </div>
                    </div>
                </section>

                {/* Key Metrics Grid */}
                <section className="grid grid-cols-2 md:grid-cols-4 gap-4">
                    <MetricCard
                        title="Queue Depth"
                        value={state.queue_depth}
                        subtitle="Entries in priority heap"
                    />
                    <MetricCard
                        title="Vault Sealed"
                        value={state.vault_sealed_count.toLocaleString()}
                        subtitle="Crash-consistent objects"
                    />
                    <MetricCard
                        title="Active Anomalies"
                        value={state.active_anomalies}
                        alert={state.active_anomalies > 0}
                        subtitle={state.active_anomalies > 0 ? 'C=1000 DETECTED' : 'All clear'}
                    />
                    <MetricCard
                        title="DTN Pending"
                        value={state.chunks_pending}
                        unit="chunks"
                        subtitle="1MB blocks awaiting ACK"
                    />
                </section>

                {/* Storage Section */}
                <section className="rounded-xl border border-orb-border bg-orb-surface p-6 space-y-4">
                    <h2 className="text-sm font-semibold text-white uppercase tracking-wider">
                        Dual USB Vault Storage
                    </h2>
                    <div className="grid md:grid-cols-2 gap-6">
                        <ProgressBar
                            value={state.usb_primary_util}
                            label="USB Primary"
                            danger={state.usb_primary_util > 85}
                        />
                        <ProgressBar
                            value={state.usb_mirror_util}
                            label="USB Mirror"
                            danger={state.usb_mirror_util > 85}
                        />
                    </div>
                    <div className="flex items-center gap-4 pt-2 text-xs text-orb-muted">
                        <span>Eviction trigger: &gt;85%</span>
                        <span>•</span>
                        <span>Target: &lt;70%</span>
                        <span>•</span>
                        <span>ext4 (journal disabled)</span>
                    </div>
                </section>

                {/* System Architecture Overview */}
                <section className="grid md:grid-cols-3 gap-4">
                    {/* Vault Pipeline */}
                    <div className="rounded-xl border border-orb-border bg-orb-surface p-5 space-y-3">
                        <h3 className="text-sm font-semibold text-white uppercase tracking-wider">
                            Vault Pipeline
                        </h3>
                        <div className="space-y-2 text-xs font-mono text-orb-muted">
                            {['Stage (tmpfs)', 'Process (Zstd+AES)', 'Pending (.pending)', 'fsync()', 'Commit (rename)', 'Dir sync_all()'].map((step, i) => (
                                <div key={i} className="flex items-center gap-2">
                                    <span className="w-5 h-5 rounded-full bg-orb-accent/20 text-orb-accent flex items-center justify-center text-[10px] font-bold">
                                        {i + 1}
                                    </span>
                                    <span>{step}</span>
                                </div>
                            ))}
                        </div>
                    </div>

                    {/* Priority Formula */}
                    <div className="rounded-xl border border-orb-border bg-orb-surface p-5 space-y-3">
                        <h3 className="text-sm font-semibold text-white uppercase tracking-wider">
                            P_Score Algorithm
                        </h3>
                        <div className="bg-orb-bg rounded-lg p-3 font-mono text-sm text-orb-accent">
                            P = (Wc·C) + (Wa·T) - (Ws·S)
                        </div>
                        <div className="space-y-1 text-xs text-orb-muted">
                            <p>C: 1000 (Anomaly) | 1 (Routine)</p>
                            <p>T: Seconds since ingest</p>
                            <p>S: Payload size in MB</p>
                        </div>
                    </div>

                    {/* Power Governor */}
                    <div className="rounded-xl border border-orb-border bg-orb-surface p-5 space-y-3">
                        <h3 className="text-sm font-semibold text-white uppercase tracking-wider">
                            Power Governor
                        </h3>
                        <div className="space-y-2 text-xs">
                            <div className={`flex items-center justify-between p-2 rounded-lg ${state.power_state === 'Solar' ? 'bg-emerald-900/30 text-emerald-400' : 'bg-orb-bg text-orb-muted'}`}>
                                <span>☀️ Solar (SoC &gt; 50%)</span>
                                <span className="font-mono">CPU 200%</span>
                            </div>
                            <div className={`flex items-center justify-between p-2 rounded-lg ${state.power_state === 'Eclipse' ? 'bg-red-900/30 text-red-400' : 'bg-orb-bg text-orb-muted'}`}>
                                <span>🌑 Eclipse (SoC &lt; 30%)</span>
                                <span className="font-mono">CPU 5%</span>
                            </div>
                        </div>
                    </div>
                </section>

                {/* Footer */}
                <footer className="text-center text-xs text-orb-muted py-4 border-t border-orb-border">
                    SpaceOrb V7.6 — Autonomous Orbital Edge Daemon — Code ReCET 3.0
                </footer>
            </main>
        </div>
    );
}
