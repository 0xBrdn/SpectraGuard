import React, { useMemo, useState } from 'react';
import { AlertSeverity, WidsAlert } from '../types/telemetry';

interface AlertFeedPanelProps {
  alerts: WidsAlert[];
  onSelectAlert: (alert: WidsAlert) => void;
  onClearAlerts: () => Promise<void>;
}

type FilterOption = 'ALL' | AlertSeverity;

function formatUtcTime(isoString: string): string {
  const date = new Date(isoString);
  if (Number.isNaN(date.getTime())) return isoString;
  return date.toISOString().replace('T', ' ').replace('Z', ' UTC');
}

function severityBadgeClass(severity: AlertSeverity): string {
  switch (severity) {
    case 'Critical':
      return 'border-red-500 bg-red-950/70 text-red-300';
    case 'Warning':
      return 'border-amber-500 bg-amber-950/70 text-amber-300';
    case 'Info':
      return 'border-cyan-500 bg-cyan-950/70 text-cyan-300';
  }
}

export const AlertFeedPanel: React.FC<AlertFeedPanelProps> = ({
  alerts,
  onSelectAlert,
  onClearAlerts,
}) => {
  const [filter, setFilter] = useState<FilterOption>('ALL');

  const counts = useMemo(() => {
    let critical = 0;
    let warning = 0;
    let info = 0;
    for (const a of alerts) {
      if (a.severity === 'Critical') critical += 1;
      else if (a.severity === 'Warning') warning += 1;
      else info += 1;
    }
    return { total: alerts.length, critical, warning, info };
  }, [alerts]);

  const filteredAlerts = useMemo(() => {
    if (filter === 'ALL') return alerts;
    return alerts.filter((a) => a.severity === filter);
  }, [alerts, filter]);

  return (
    <section className="flex h-[250px] shrink-0 flex-col border-t border-[#1e2533] bg-[#0d1118]">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-[#1e2533] px-3 py-1.5 font-mono text-[11px]">
        <div className="flex items-center gap-3">
          <span className="font-bold tracking-wider text-slate-200">
            WIDS SECURITY ALERT FEED
          </span>

          <div className="flex items-center gap-1 text-[10px] tabular-nums">
            <button
              type="button"
              onClick={() => setFilter('ALL')}
              className={`border px-2 py-0.5 font-semibold ${
                filter === 'ALL'
                  ? 'border-slate-400 bg-slate-800 text-slate-100'
                  : 'border-[#1e2533] bg-[#090b10] text-slate-400 hover:text-slate-200'
              }`}
            >
              ALL ({counts.total})
            </button>
            <button
              type="button"
              onClick={() => setFilter('Critical')}
              className={`border px-2 py-0.5 font-semibold ${
                filter === 'Critical'
                  ? 'border-red-400 bg-red-950/70 text-red-200'
                  : 'border-[#1e2533] bg-[#090b10] text-red-400 hover:border-red-500/60'
              }`}
            >
              CRITICAL ({counts.critical})
            </button>
            <button
              type="button"
              onClick={() => setFilter('Warning')}
              className={`border px-2 py-0.5 font-semibold ${
                filter === 'Warning'
                  ? 'border-amber-400 bg-amber-950/70 text-amber-200'
                  : 'border-[#1e2533] bg-[#090b10] text-amber-400 hover:border-amber-500/60'
              }`}
            >
              WARNING ({counts.warning})
            </button>
            <button
              type="button"
              onClick={() => setFilter('Info')}
              className={`border px-2 py-0.5 font-semibold ${
                filter === 'Info'
                  ? 'border-cyan-400 bg-cyan-950/70 text-cyan-200'
                  : 'border-[#1e2533] bg-[#090b10] text-cyan-400 hover:border-cyan-500/60'
              }`}
            >
              INFO ({counts.info})
            </button>
          </div>
        </div>

        <div className="flex items-center gap-3">
          <span className="hidden text-[10px] text-slate-500 md:inline">
            SELECT ALERT ROW FOR BASEBAND ENVELOPE &amp; BITSTREAM FORENSICS
          </span>
          <button
            type="button"
            disabled={alerts.length === 0}
            onClick={() => void onClearAlerts()}
            className="border border-[#1e2533] bg-[#111621] px-2.5 py-0.5 text-[10px] font-semibold text-slate-300 hover:border-slate-400 disabled:opacity-40"
          >
            ACKNOWLEDGE / CLEAR
          </button>
        </div>
      </div>

      <div className="min-h-0 flex-1 overflow-auto bg-[#090b10]">
        <table className="w-full border-collapse text-left font-mono text-[11px] tabular-nums">
          <thead className="sticky top-0 border-b border-[#1e2533] bg-[#111621] text-[10px] text-slate-400">
            <tr>
              <th className="px-3 py-1.5 font-semibold">ID</th>
              <th className="px-3 py-1.5 font-semibold">TIMESTAMP (UTC)</th>
              <th className="px-3 py-1.5 font-semibold">SEVERITY</th>
              <th className="px-3 py-1.5 font-semibold">CATEGORY</th>
              <th className="px-3 py-1.5 font-semibold">FREQ (MHz)</th>
              <th className="px-3 py-1.5 font-semibold">SNR / DELTA</th>
              <th className="px-3 py-1.5 font-semibold">CONF</th>
              <th className="px-3 py-1.5 font-semibold">EVENT SUMMARY</th>
              <th className="px-3 py-1.5 text-right font-semibold">COUNT</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-[#1e2533]">
            {filteredAlerts.length === 0 ? (
              <tr>
                <td colSpan={9} className="px-3 py-8 text-center text-xs text-slate-500">
                  NO ACTIVE WIDS ANOMALIES IN CURRENT WINDOW — SPECTRUM BASELINE NOMINAL
                </td>
              </tr>
            ) : (
              filteredAlerts.map((alert) => (
                <tr
                  key={alert.id}
                  onClick={() => onSelectAlert(alert)}
                  className="cursor-pointer transition-colors hover:bg-[#141b29]"
                >
                  <td className="px-3 py-1.5 text-slate-400">#{alert.id}</td>
                  <td className="px-3 py-1.5 text-slate-300">
                    {formatUtcTime(alert.last_updated_utc)}
                  </td>
                  <td className="px-3 py-1.5">
                    <span
                      className={`inline-block border px-1.5 py-0.5 text-[9px] font-bold uppercase ${severityBadgeClass(
                        alert.severity
                      )}`}
                    >
                      {alert.severity}
                    </span>
                  </td>
                  <td className="px-3 py-1.5 font-semibold text-slate-200">{alert.category}</td>
                  <td className="px-3 py-1.5 text-cyan-300">
                    {(alert.center_freq_hz / 1_000_000).toFixed(4)}
                  </td>
                  <td className="px-3 py-1.5 text-amber-300">+{alert.snr_db.toFixed(1)} dB</td>
                  <td className="px-3 py-1.5 text-slate-200">
                    {alert.confidence_pct.toFixed(1)}%
                  </td>
                  <td className="max-w-md truncate px-3 py-1.5 text-slate-300">{alert.title}</td>
                  <td className="px-3 py-1.5 text-right text-slate-400">
                    x{alert.occurrence_count}
                  </td>
                </tr>
              ))
            )}
          </tbody>
        </table>
      </div>
    </section>
  );
};