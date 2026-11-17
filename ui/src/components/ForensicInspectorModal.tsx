import React, { useEffect, useMemo, useRef } from 'react';
import { DetectedBurst, WidsAlert } from '../types/telemetry';

interface ForensicInspectorModalProps {
  alert: WidsAlert | null;
  recentBursts: DetectedBurst[];
  onClose: () => void;
}

interface HexByteCell {
  index: number;
  refByte: string;
  offByte: string;
  matches: boolean;
}

function splitHexBytes(hex: string | undefined): string[] {
  if (!hex) return [];
  const clean = hex.trim().toUpperCase();
  const out: string[] = [];
  for (let i = 0; i < clean.length; i += 2) {
    out.push(clean.slice(i, i + 2).padEnd(2, '0'));
  }
  return out;
}

export const ForensicInspectorModal: React.FC<ForensicInspectorModalProps> = ({
  alert,
  recentBursts,
  onClose,
}) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  const referenceBurst = useMemo(() => {
    if (!alert?.evidence.reference_burst_id) return undefined;
    return recentBursts.find((b) => b.id === alert.evidence.reference_burst_id);
  }, [alert, recentBursts]);

  const offendingBurst = useMemo(() => {
    if (!alert?.evidence.offending_burst_id) return undefined;
    return recentBursts.find((b) => b.id === alert.evidence.offending_burst_id);
  }, [alert, recentBursts]);

  const refEnvelope =
    alert?.evidence.reference_burst_envelope ?? referenceBurst?.envelope_signature ?? [];
  const offEnvelope =
    alert?.evidence.offending_burst_envelope ?? offendingBurst?.envelope_signature ?? [];

  const hexComparison = useMemo<HexByteCell[]>(() => {
    const refBytes = splitHexBytes(referenceBurst?.bitstream_hex);
    const offBytes = splitHexBytes(alert?.evidence.bitstream_hex ?? offendingBurst?.bitstream_hex);
    const len = Math.max(refBytes.length, offBytes.length);
    const cells: HexByteCell[] = [];
    for (let i = 0; i < len; i++) {
      const r = refBytes[i] ?? '--';
      const o = offBytes[i] ?? '--';
      cells.push({
        index: i,
        refByte: r,
        offByte: o,
        matches: r !== '--' && o !== '--' ? r === o : r === '--',
      });
    }
    return cells;
  }, [alert, offendingBurst, referenceBurst]);

  useEffect(() => {
    if (!alert) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [alert, onClose]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !alert) return;

    const rect = canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    const width = Math.max(1, Math.floor(rect.width));
    const height = Math.max(1, Math.floor(rect.height));
    canvas.width = width * dpr;
    canvas.height = height * dpr;

    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    ctx.save();
    ctx.scale(dpr, dpr);

    ctx.fillStyle = '#090b10';
    ctx.fillRect(0, 0, width, height);

    const padLeft = 42;
    const padRight = 14;
    const padTop = 14;
    const padBottom = 22;
    const plotW = Math.max(20, width - padLeft - padRight);
    const plotH = Math.max(20, height - padTop - padBottom);

    ctx.strokeStyle = '#171d29';
    ctx.lineWidth = 1;
    ctx.font = '10px "JetBrains Mono", monospace';
    ctx.fillStyle = '#64748b';
    ctx.textAlign = 'right';
    ctx.textBaseline = 'middle';

    for (let step = 0; step <= 4; step++) {
      const val = step * 0.25;
      const y = Math.round(padTop + plotH - val * plotH) + 0.5;
      ctx.beginPath();
      ctx.moveTo(padLeft, y);
      ctx.lineTo(padLeft + plotW, y);
      ctx.stroke();
      ctx.fillText(val.toFixed(2), padLeft - 6, y);
    }

    ctx.textAlign = 'center';
    ctx.textBaseline = 'top';
    for (let step = 0; step <= 8; step++) {
      const frac = step / 8;
      const x = Math.round(padLeft + frac * plotW) + 0.5;
      ctx.beginPath();
      ctx.moveTo(x, padTop);
      ctx.lineTo(x, padTop + plotH);
      ctx.stroke();
      ctx.fillText(`${Math.round(frac * 128)}`, x, padTop + plotH + 6);
    }

    ctx.strokeStyle = '#1e2533';
    ctx.strokeRect(padLeft + 0.5, padTop + 0.5, plotW, plotH);

    const drawEnvelope = (series: number[], color: string, dashed: boolean) => {
      if (series.length < 2) return;
      ctx.save();
      ctx.strokeStyle = color;
      ctx.lineWidth = dashed ? 1.5 : 2.0;
      if (dashed) ctx.setLineDash([5, 3]);
      ctx.beginPath();
      for (let i = 0; i < series.length; i++) {
        const x = padLeft + (i / (series.length - 1)) * plotW;
        const y = padTop + plotH - Math.min(Math.max(series[i], 0), 1) * plotH;
        if (i === 0) ctx.moveTo(x, y);
        else ctx.lineTo(x, y);
      }
      ctx.stroke();
      ctx.restore();
    };

    if (refEnvelope.length > 0) {
      drawEnvelope(refEnvelope, '#10b981', false);
    }
    if (offEnvelope.length > 0) {
      drawEnvelope(offEnvelope, '#ef4444', refEnvelope.length > 0);
    }

    ctx.restore();
  }, [alert, offEnvelope, refEnvelope]);

  if (!alert) return null;

  const ev = alert.evidence;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/80 p-4"
      onClick={onClose}
    >
      <div
        className="flex max-h-[90vh] w-full max-w-4xl flex-col border border-[#334155] bg-[#0d1118] font-mono shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center justify-between border-b border-[#1e2533] bg-[#111621] px-4 py-2.5">
          <div className="flex items-center gap-3">
            <span className="border border-red-500/70 bg-red-950/60 px-2 py-0.5 text-[10px] font-bold uppercase text-red-300">
              {alert.severity}
            </span>
            <span className="text-xs font-bold text-slate-100">
              FORENSIC BURST INSPECTOR // ALERT #{alert.id} — {alert.category}
            </span>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="border border-[#1e2533] bg-[#090b10] px-2.5 py-1 text-[11px] font-semibold text-slate-300 hover:border-cyan-400 hover:text-cyan-300"
          >
            CLOSE [ESC]
          </button>
        </div>

        <div className="min-h-0 flex-1 space-y-3 overflow-y-auto p-4 text-xs tabular-nums">
          <div className="border border-[#1e2533] bg-[#090b10] px-3 py-2 text-slate-200">
            <div className="font-semibold text-cyan-300">{alert.title}</div>
            {ev.deviation_details && (
              <div className="mt-1 text-[11px] text-slate-400">{ev.deviation_details}</div>
            )}
          </div>

          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-6">
            <div className="border border-[#1e2533] bg-[#090b10] p-2">
              <div className="text-[9px] text-slate-500">CORRELATION (r)</div>
              <div className="text-sm font-bold text-cyan-300">
                {ev.correlation_score !== undefined ? ev.correlation_score.toFixed(4) : 'N/A'}
              </div>
            </div>

            <div className="border border-[#1e2533] bg-[#090b10] p-2">
              <div className="text-[9px] text-slate-500">INTER-ARRIVAL</div>
              <div className="text-sm font-bold text-amber-300">
                {ev.inter_arrival_ms !== undefined ? `${ev.inter_arrival_ms.toFixed(1)} ms` : 'N/A'}
              </div>
            </div>

            <div className="border border-[#1e2533] bg-[#090b10] p-2">
              <div className="text-[9px] text-slate-500">BURST DURATION</div>
              <div className="text-sm font-bold text-slate-200">
                {ev.duration_us !== undefined
                  ? `${(ev.duration_us / 1_000).toFixed(2)} ms`
                  : 'N/A'}
              </div>
            </div>

            <div className="border border-[#1e2533] bg-[#090b10] p-2">
              <div className="text-[9px] text-slate-500">FREQ OFFSET</div>
              <div className="text-sm font-bold text-slate-200">
                {ev.freq_offset_hz !== undefined
                  ? `${ev.freq_offset_hz >= 0 ? '+' : ''}${(ev.freq_offset_hz / 1_000).toFixed(1)} kHz`
                  : 'WIDEBAND'}
              </div>
            </div>

            <div className="border border-[#1e2533] bg-[#090b10] p-2">
              <div className="text-[9px] text-slate-500">EST. BAUD RATE</div>
              <div className="text-sm font-bold text-slate-200">
                {ev.estimated_baud_rate ? `${ev.estimated_baud_rate} Bd` : 'N/A'}
              </div>
            </div>

            <div className="border border-[#1e2533] bg-[#090b10] p-2">
              <div className="text-[9px] text-slate-500">PEAK SNR / DELTA</div>
              <div className="text-sm font-bold text-red-400">+{alert.snr_db.toFixed(1)} dB</div>
            </div>
          </div>

          <div className="border border-[#1e2533] bg-[#090b10] p-2.5">
            <div className="mb-2 flex flex-wrap items-center justify-between gap-2 text-[10px]">
              <span className="font-bold tracking-wider text-slate-300">
                NORMALIZED BASEBAND AMPLITUDE ENVELOPE (128-PT SIGNATURE)
              </span>
              <div className="flex items-center gap-4">
                {refEnvelope.length > 0 && (
                  <span className="flex items-center gap-1.5 text-emerald-400">
                    <span className="inline-block h-0.5 w-4 bg-emerald-400" />
                    REFERENCE BURST {ev.reference_burst_id ? `#${ev.reference_burst_id}` : ''}
                  </span>
                )}
                {offEnvelope.length > 0 && (
                  <span className="flex items-center gap-1.5 text-red-400">
                    <span className="inline-block h-0.5 w-4 border-t-2 border-dashed border-red-400" />
                    OFFENDING BURST {ev.offending_burst_id ? `#${ev.offending_burst_id}` : ''}
                  </span>
                )}
              </div>
            </div>

            {refEnvelope.length === 0 && offEnvelope.length === 0 ? (
              <div className="py-8 text-center text-[11px] text-slate-500">
                NO DISCRETE BURST ENVELOPE ATTACHED TO WIDEBAND SPECTRUM ALERT
              </div>
            ) : (
              <div className="h-48 w-full">
                <canvas ref={canvasRef} className="block h-full w-full" />
              </div>
            )}
          </div>

          {hexComparison.length > 0 && (
            <div className="border border-[#1e2533] bg-[#090b10] p-2.5">
              <div className="mb-2 flex items-center justify-between text-[10px]">
                <span className="font-bold tracking-wider text-slate-300">
                  DEMODULATED OOK BITSTREAM HEX COMPARISON
                </span>
                <span className="text-slate-500">
                  {referenceBurst?.bitstream_hex
                    ? 'GREEN = BIT-IDENTICAL | AMBER = SYMBOL VARIANCE'
                    : 'CAPTURED SYMBOL HEX'}
                </span>
              </div>

              <div className="grid grid-cols-1 gap-2 md:grid-cols-2">
                {referenceBurst?.bitstream_hex && (
                  <div className="border border-[#1e2533] bg-[#0d1118] p-2">
                    <div className="mb-1 text-[9px] text-emerald-400">
                      REFERENCE BURST #{referenceBurst.id}
                    </div>
                    <div className="flex flex-wrap gap-1 text-[11px]">
                      {hexComparison.map((cell) => (
                        <span
                          key={`ref-${cell.index}`}
                          className="border border-emerald-500/30 bg-emerald-950/30 px-1 py-0.5 text-emerald-300"
                        >
                          {cell.refByte}
                        </span>
                      ))}
                    </div>
                  </div>
                )}

                <div
                  className={`border border-[#1e2533] bg-[#0d1118] p-2 ${
                    referenceBurst?.bitstream_hex ? '' : 'md:col-span-2'
                  }`}
                >
                  <div className="mb-1 text-[9px] text-red-400">
                    OFFENDING BURST {ev.offending_burst_id ? `#${ev.offending_burst_id}` : ''}
                  </div>
                  <div className="flex flex-wrap gap-1 text-[11px]">
                    {hexComparison.map((cell) => (
                      <span
                        key={`off-${cell.index}`}
                        className={`border px-1 py-0.5 ${
                          cell.matches
                            ? 'border-emerald-500/40 bg-emerald-950/40 text-emerald-300'
                            : 'border-amber-500/50 bg-amber-950/40 text-amber-300'
                        }`}
                      >
                        {cell.offByte}
                      </span>
                    ))}
                  </div>
                </div>
              </div>
            </div>
          )}
        </div>
      </div>
    </div>
  );
};