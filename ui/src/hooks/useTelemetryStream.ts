import { useCallback, useEffect, useRef, useState } from 'react';
import {
  ConnectionState,
  DetectedBurst,
  DeviceBaselineProfile,
  InjectThreatPayload,
  SpectrumFrame,
  SystemStatus,
  TelemetryEvent,
  WidsAlert,
} from '../types/telemetry';

export interface LiveRfMetrics {
  noiseFloorDbfs: number;
  channelOccupancyPct: number;
  centerFreqHz: number;
  sampleRateHz: number;
}

export interface TelemetryHookResult {
  connectionState: ConnectionState;
  status: SystemStatus | null;
  liveMetrics: LiveRfMetrics;
  alerts: WidsAlert[];
  recentBursts: DetectedBurst[];
  latestSpectrumRef: React.MutableRefObject<SpectrumFrame | null>;
  waterfallQueueRef: React.MutableRefObject<SpectrumFrame[]>;
  resetSignalRef: React.MutableRefObject<number>;
  switchBand: (centerFreqHz: number) => Promise<void>;
  resetMaxHold: () => Promise<void>;
  injectThreat: (payload: InjectThreatPayload) => Promise<string>;
  clearAlerts: () => Promise<void>;
  addBaseline: (profile: DeviceBaselineProfile) => Promise<void>;
}

const MAX_ALERTS = 200;
const MAX_BURSTS = 100;
const MAX_WATERFALL_QUEUE = 64;

export function useTelemetryStream(): TelemetryHookResult {
  const [connectionState, setConnectionState] = useState<ConnectionState>('disconnected');
  const [status, setStatus] = useState<SystemStatus | null>(null);
  const [alerts, setAlerts] = useState<WidsAlert[]>([]);
  const [recentBursts, setRecentBursts] = useState<DetectedBurst[]>([]);
  const [liveMetrics, setLiveMetrics] = useState<LiveRfMetrics>({
    noiseFloorDbfs: -92.0,
    channelOccupancyPct: 0.0,
    centerFreqHz: 868_300_000,
    sampleRateHz: 2_048_000,
  });

  const latestSpectrumRef = useRef<SpectrumFrame | null>(null);
  const waterfallQueueRef = useRef<SpectrumFrame[]>([]);
  const resetSignalRef = useRef<number>(0);
  const lastUiMetricSyncRef = useRef<number>(0);

  const fetchInitialSnapshot = useCallback(async () => {
    try {
      const [statusRes, alertsRes] = await Promise.all([
        fetch('/api/v1/status'),
        fetch('/api/v1/alerts'),
      ]);
      if (statusRes.ok) {
        const nextStatus: SystemStatus = await statusRes.json();
        setStatus(nextStatus);
        setLiveMetrics((prev) => ({
          ...prev,
          centerFreqHz: nextStatus.center_freq_hz,
          sampleRateHz: nextStatus.sample_rate_hz,
          noiseFloorDbfs: nextStatus.baseline_noise_floor_dbfs,
        }));
      }
      if (alertsRes.ok) {
        const nextAlerts: WidsAlert[] = await alertsRes.json();
        setAlerts(nextAlerts);
      }
    } catch {
      // Handled by WebSocket reconnect cycle
    }
  }, []);

  useEffect(() => {
    let active = true;
    let socket: WebSocket | null = null;
    let reconnectTimer: number | null = null;
    let reconnectAttempt = 0;

    void fetchInitialSnapshot();

    const connect = () => {
      if (!active) return;
      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const wsUrl = `${protocol}//${window.location.host}/ws/telemetry`;
      setConnectionState(reconnectAttempt === 0 ? 'disconnected' : 'reconnecting');

      socket = new WebSocket(wsUrl);

      socket.onopen = () => {
        if (!active) return;
        reconnectAttempt = 0;
        setConnectionState('connected');
        void fetchInitialSnapshot();
      };

      socket.onmessage = (event: MessageEvent<string>) => {
        if (!active) return;
        let parsed: TelemetryEvent;
        try {
          parsed = JSON.parse(event.data) as TelemetryEvent;
        } catch {
          return;
        }

        switch (parsed.type) {
          case 'spectrum': {
            const frame = parsed.payload;
            latestSpectrumRef.current = frame;
            if (waterfallQueueRef.current.length >= MAX_WATERFALL_QUEUE) {
              waterfallQueueRef.current.shift();
            }
            waterfallQueueRef.current.push(frame);

            const now = performance.now();
            if (now - lastUiMetricSyncRef.current >= 120) {
              lastUiMetricSyncRef.current = now;
              setLiveMetrics({
                noiseFloorDbfs: frame.noise_floor_dbfs,
                channelOccupancyPct: frame.channel_occupancy_pct,
                centerFreqHz: frame.center_freq_hz,
                sampleRateHz: frame.sample_rate_hz,
              });
            }
            break;
          }
          case 'burst': {
            const burst = parsed.payload;
            setRecentBursts((prev) => {
              const next = [burst, ...prev];
              return next.length > MAX_BURSTS ? next.slice(0, MAX_BURSTS) : next;
            });
            setStatus((prev) =>
              prev
                ? {
                    ...prev,
                    total_bursts_detected: prev.total_bursts_detected + 1,
                  }
                : prev
            );
            break;
          }
          case 'alert': {
            const incoming = parsed.payload;
            setAlerts((prev) => {
              const idx = prev.findIndex((a) => a.id === incoming.id);
              if (idx >= 0) {
                const updated = [...prev];
                updated[idx] = incoming;
                return updated;
              }
              const next = [incoming, ...prev];
              return next.length > MAX_ALERTS ? next.slice(0, MAX_ALERTS) : next;
            });
            break;
          }
          case 'status': {
            const nextStatus = parsed.payload;
            setStatus(nextStatus);
            setLiveMetrics((prev) => ({
              ...prev,
              centerFreqHz: nextStatus.center_freq_hz,
              sampleRateHz: nextStatus.sample_rate_hz,
            }));
            break;
          }
        }
      };

      socket.onclose = () => {
        if (!active) return;
        setConnectionState('reconnecting');
        const delayMs = Math.min(3000, 350 * Math.pow(1.5, reconnectAttempt));
        reconnectAttempt += 1;
        reconnectTimer = window.setTimeout(connect, delayMs);
      };

      socket.onerror = () => {
        socket?.close();
      };
    };

    connect();

    const statusPoll = window.setInterval(() => {
      if (active) {
        void fetch('/api/v1/status')
          .then((res) => (res.ok ? res.json() : null))
          .then((data: SystemStatus | null) => {
            if (data && active) setStatus(data);
          })
          .catch(() => {});
      }
    }, 5000);

    return () => {
      active = false;
      if (reconnectTimer !== null) window.clearTimeout(reconnectTimer);
      window.clearInterval(statusPoll);
      socket?.close();
    };
  }, [fetchInitialSnapshot]);

  const switchBand = useCallback(async (centerFreqHz: number) => {
    const res = await fetch('/api/v1/config/band', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ center_freq_hz: centerFreqHz }),
    });
    if (!res.ok) {
      throw new Error(`Band switch failed (${res.status})`);
    }
    const updatedStatus: SystemStatus = await res.json();
    setStatus(updatedStatus);
    setLiveMetrics((prev) => ({
      ...prev,
      centerFreqHz: updatedStatus.center_freq_hz,
      sampleRateHz: updatedStatus.sample_rate_hz,
    }));
    resetSignalRef.current += 1;
  }, []);

  const resetMaxHold = useCallback(async () => {
    resetSignalRef.current += 1;
    await fetch('/api/v1/spectrum/reset-hold', { method: 'POST' });
  }, []);

  const injectThreat = useCallback(async (payload: InjectThreatPayload): Promise<string> => {
    const res = await fetch('/api/v1/sim/inject', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    });
    const body = (await res.json()) as { status?: string; threat?: string; error?: string };
    if (!res.ok) {
      throw new Error(body.error ?? `Injection failed (${res.status})`);
    }
    return body.threat ?? payload.threat;
  }, []);

  const clearAlerts = useCallback(async () => {
    const res = await fetch('/api/v1/alerts/clear', { method: 'POST' });
    if (res.ok) {
      setAlerts([]);
    }
  }, []);

  const addBaseline = useCallback(async (profile: DeviceBaselineProfile) => {
    const res = await fetch('/api/v1/baselines', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(profile),
    });
    if (!res.ok) {
      throw new Error(`Failed to add baseline (${res.status})`);
    }
    const created: DeviceBaselineProfile = await res.json();
    setStatus((prev) =>
      prev
        ? {
            ...prev,
            baselines: [...prev.baselines, created],
          }
        : prev
    );
  }, []);

  return {
    connectionState,
    status,
    liveMetrics,
    alerts,
    recentBursts,
    latestSpectrumRef,
    waterfallQueueRef,
    resetSignalRef,
    switchBand,
    resetMaxHold,
    injectThreat,
    clearAlerts,
    addBaseline,
  };
}