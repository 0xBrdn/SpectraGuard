import React, { useState } from 'react';
import { LiveRfMetrics } from '../hooks/useTelemetryStream';
import { ConnectionState, SystemStatus } from '../types/telemetry';

interface TopHeaderProps {
  connectionState: ConnectionState;
  status: SystemStatus | null;
  liveMetrics: LiveRfMetrics;
  activeAlertCount: number;
  onSwitchBand: (centerFreqHz: number) => Promise<void>;
}

function formatUptime(seconds: number): string {
  const hrs = Math.floor(seconds / 3600);
  const mins = Math.floor((seconds % 3600) / 60);
  const secs = seconds % 60;
  return `${String(hrs).padStart(2, '0')}:${String(mins).padStart(2, '0')}:${String(secs).padStart(2, '0')}`;
}

export const TopHeader: React.FC<TopHeaderProps> = ({
  connectionState,
  status,
  liveMetrics,
  activeAlertCount,
  onSwitchBand,
}) => {
  const [customMhz, setCustomMhz] = useState<string>('');
  const [tuning, setTuning] = useState<boolean>(false);

  const centerMhz = liveMetrics.centerFreqHz / 1_000_000;
  const sampleRateMsps = liveMetrics.sampleRateHz / 1_000_000;
  const is433 = Math.abs(liveMetrics.centerFreqHz - 433_920_000) < 50_000;
  const is868 = Math.abs(liveMetrics.centerFreqHz - 868_300_000) < 50_000;
  const isSim = (status?.source_mode ?? 'sim') === 'sim';

  const handlePresetTune = async (freqHz: number) => {
    setTuning(true);
    try {
      await onSwitchBand(freqHz);
    } finally {
      setTuning(false);
    }
  };

  const handleCustomSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    const parsed = Number.parseFloat(customMhz);
    if (!Number.isFinite(parsed) || parsed < 1.0) return;
    setTuning(true);
    try {
      await onSwitchBand(Math.round(parsed * 1_000_000));
      setCustomMhz('');
    } finally {
      setTuning(false);
    }
  };

  return (
    <header className="flex flex-wrap items-center justify-between gap-3 border-b border-[#1e2533] bg-[#0d1118] px-3 py-2 select-none">
      <div className="flex items-center gap-3">
        <div className="flex items-center gap-2">
          <span className="inline-block h-3 w-1.5 bg-cyan-400" />
          <span className="font-mono text-sm font-bold tracking-wider text-slate-100">
            SPECTRAGUARD // WIDS
          </span>
        </div>

        <span className="border border-[#1e2533] bg-[#090b10] px-2 py-0.5 font-mono text-[10px] font-semibold tracking-widest text-emerald-400">
          PASSIVE RX ONLY
        </span>

        <div className="flex items-center gap-1.5 border border-[#1e2533] bg-[#090b10] px-2 py-0.5 font-mono text-[11px]">
          <span
            className={`inline-block h-2 w-2 ${
              connectionState === 'connected'
                ? 'bg-emerald-400 shadow-[0_0_6px_#10b981]'
                : connectionState === 'reconnecting'
                ? 'bg-amber-400 animate-pulse'
                : 'bg-red-500'
            }`}
          />
          <span
            className={
              connectionState === 'connected'
                ? 'text-emerald-400'
                : connectionState === 'reconnecting'
                ? 'text-amber-400'
                : 'text-red-400'
            }
          >
            {connectionState === 'connected'
              ? 'WS CONNECTED'
              : connectionState === 'reconnecting'
              ? 'RECONNECTING'
              : 'WS OFFLINE'}
          </span>
        </div>

        <div className="border border-[#1e2533] bg-[#111621] px-2 py-0.5 font-mono text-[11px] font-medium text-cyan-300">
          {isSim ? 'RF CHANNEL SIMULATOR' : 'SIGMF PLAYBACK'}
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-4 font-mono tabular-nums">
        <div className="border-l border-[#1e2533] pl-3">
          <div className="text-[10px] uppercase tracking-wider text-slate-500">CENTER FREQ</div>
          <div className="text-sm font-bold text-cyan-300">{centerMhz.toFixed(3)} MHz</div>
        </div>

        <div className="border-l border-[#1e2533] pl-3">
          <div className="text-[10px] uppercase tracking-wider text-slate-500">SAMPLE RATE</div>
          <div className="text-sm font-semibold text-slate-200">
            {sampleRateMsps.toFixed(3)} MSPS
          </div>
        </div>

        <div className="border-l border-[#1e2533] pl-3">
          <div className="text-[10px] uppercase tracking-wider text-slate-500">NOISE FLOOR</div>
          <div
            className={`text-sm font-semibold ${
              liveMetrics.noiseFloorDbfs > -82.0 ? 'text-red-400' : 'text-emerald-400'
            }`}
          >
            {liveMetrics.noiseFloorDbfs.toFixed(1)} dBFS
          </div>
        </div>

        <div className="border-l border-[#1e2533] pl-3">
          <div className="text-[10px] uppercase tracking-wider text-slate-500">OCCUPANCY</div>
          <div
            className={`text-sm font-semibold ${
              liveMetrics.channelOccupancyPct >= 65.0
                ? 'text-red-400'
                : liveMetrics.channelOccupancyPct >= 25.0
                ? 'text-amber-400'
                : 'text-slate-200'
            }`}
          >
            {liveMetrics.channelOccupancyPct.toFixed(1)}%
          </div>
        </div>

        <div className="border-l border-[#1e2533] pl-3">
          <div className="text-[10px] uppercase tracking-wider text-slate-500">ALERTS / UPTIME</div>
          <div className="text-xs font-medium text-slate-300">
            <span className={activeAlertCount > 0 ? 'text-red-400 font-bold' : 'text-slate-300'}>
              {activeAlertCount} ACT
            </span>{' '}
            <span className="text-slate-600">|</span> {formatUptime(status?.uptime_sec ?? 0)}
          </div>
        </div>
      </div>

      <div className="flex items-center gap-1.5 font-mono text-xs">
        <button
          type="button"
          disabled={tuning}
          onClick={() => void handlePresetTune(433_920_000)}
          className={`border px-2.5 py-1 text-[11px] font-semibold transition-colors ${
            is433
              ? 'border-cyan-400 bg-cyan-950/60 text-cyan-300'
              : 'border-[#1e2533] bg-[#090b10] text-slate-400 hover:border-slate-500 hover:text-slate-200'
          }`}
        >
          433.92 MHz ISM
        </button>

        <button
          type="button"
          disabled={tuning}
          onClick={() => void handlePresetTune(868_300_000)}
          className={`border px-2.5 py-1 text-[11px] font-semibold transition-colors ${
            is868
              ? 'border-cyan-400 bg-cyan-950/60 text-cyan-300'
              : 'border-[#1e2533] bg-[#090b10] text-slate-400 hover:border-slate-500 hover:text-slate-200'
          }`}
        >
          868.30 MHz SRD
        </button>

        <form onSubmit={(e) => void handleCustomSubmit(e)} className="flex items-center">
          <input
            type="text"
            inputMode="decimal"
            value={customMhz}
            onChange={(e) => setCustomMhz(e.target.value)}
            placeholder="MHz..."
            aria-label="Custom center frequency in MHz"
            className="w-20 border border-[#1e2533] bg-[#090b10] px-2 py-1 text-[11px] text-slate-200 placeholder-slate-600 focus:border-cyan-400 focus:outline-none"
          />
          <button
            type="submit"
            disabled={tuning || customMhz.trim().length === 0}
            className="border border-l-0 border-[#1e2533] bg-[#111621] px-2 py-1 text-[11px] font-semibold text-slate-300 hover:border-cyan-400 hover:text-cyan-300 disabled:opacity-40"
          >
            TUNE
          </button>
        </form>
      </div>
    </header>
  );
};