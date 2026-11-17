import React, { useEffect, useRef, useState } from 'react';
import { SpectrumFrame } from '../types/telemetry';
import {
  buildGradientCss,
  COLORMAP_LUTS,
  COLORMAP_NAMES,
  ColormapName,
  mapDbfsToLutIndex,
} from '../utils/colormaps';

interface SpectrumWaterfallViewProps {
  latestSpectrumRef: React.MutableRefObject<SpectrumFrame | null>;
  waterfallQueueRef: React.MutableRefObject<SpectrumFrame[]>;
  resetSignalRef: React.MutableRefObject<number>;
  fftSize: number;
  onResetMaxHold: () => Promise<void>;
}

interface CrosshairState {
  xNorm: number;
  yNorm: number;
  active: boolean;
}

const WATERFALL_COLS = 512;
const WATERFALL_ROWS = 360;
const AXIS_LEFT_PX = 52;
const AXIS_RIGHT_PX = 12;
const PSD_TOP_PX = 14;
const PSD_BOTTOM_PX = 24;

export const SpectrumWaterfallView: React.FC<SpectrumWaterfallViewProps> = ({
  latestSpectrumRef,
  waterfallQueueRef,
  resetSignalRef,
  fftSize,
  onResetMaxHold,
}) => {
  const [colormap, setColormap] = useState<ColormapName>('Turbo');
  const [minDbfs, setMinDbfs] = useState<number>(-110);
  const [maxDbfs, setMaxDbfs] = useState<number>(-20);

  const colormapRef = useRef<ColormapName>(colormap);
  const minDbfsRef = useRef<number>(minDbfs);
  const maxDbfsRef = useRef<number>(maxDbfs);
  const localMaxHoldRef = useRef<Float32Array | null>(null);
  const lastHandledResetRef = useRef<number>(resetSignalRef.current);
  const crosshairRef = useRef<CrosshairState>({ xNorm: 0.5, yNorm: 0.5, active: false });

  const psdCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const waterfallCanvasRef = useRef<HTMLCanvasElement | null>(null);

  const offscreenCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const offscreenCtxRef = useRef<CanvasRenderingContext2D | null>(null);
  const rowImageDataRef = useRef<ImageData | null>(null);

  useEffect(() => {
    colormapRef.current = colormap;
  }, [colormap]);

  useEffect(() => {
    minDbfsRef.current = minDbfs;
  }, [minDbfs]);

  useEffect(() => {
    maxDbfsRef.current = maxDbfs;
  }, [maxDbfs]);

  useEffect(() => {
    const offscreen = document.createElement('canvas');
    offscreen.width = WATERFALL_COLS;
    offscreen.height = WATERFALL_ROWS;
    const ctx = offscreen.getContext('2d', { alpha: false });
    if (ctx) {
      ctx.fillStyle = '#090b10';
      ctx.fillRect(0, 0, WATERFALL_COLS, WATERFALL_ROWS);
      offscreenCtxRef.current = ctx;
      rowImageDataRef.current = ctx.createImageData(WATERFALL_COLS, 1);
    }
    offscreenCanvasRef.current = offscreen;
  }, []);

  useEffect(() => {
    let rafId = 0;

    const syncCanvasResolution = (canvas: HTMLCanvasElement) => {
      const rect = canvas.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      const targetW = Math.max(1, Math.floor(rect.width * dpr));
      const targetH = Math.max(1, Math.floor(rect.height * dpr));
      if (canvas.width !== targetW || canvas.height !== targetH) {
        canvas.width = targetW;
        canvas.height = targetH;
      }
      return { width: rect.width, height: rect.height, dpr };
    };

    const renderWaterfallRows = () => {
      const offCtx = offscreenCtxRef.current;
      const rowImg = rowImageDataRef.current;
      const queue = waterfallQueueRef.current;
      if (!offCtx || !rowImg || queue.length === 0) return;

      const lut = COLORMAP_LUTS[colormapRef.current];
      const dMin = minDbfsRef.current;
      const dMax = maxDbfsRef.current;
      const pixels = rowImg.data;

      while (queue.length > 0) {
        const frame = queue.shift();
        if (!frame || frame.bins_dbfs.length === 0) continue;

        offCtx.drawImage(
          offCtx.canvas,
          0,
          0,
          WATERFALL_COLS,
          WATERFALL_ROWS - 1,
          0,
          1,
          WATERFALL_COLS,
          WATERFALL_ROWS - 1
        );

        const src = frame.bins_dbfs;
        const srcLen = src.length;
        for (let col = 0; col < WATERFALL_COLS; col++) {
          const srcIdx = Math.min(srcLen - 1, Math.floor((col / WATERFALL_COLS) * srcLen));
          const lutIdx = mapDbfsToLutIndex(src[srcIdx], dMin, dMax) * 4;
          const px = col * 4;
          pixels[px] = lut[lutIdx];
          pixels[px + 1] = lut[lutIdx + 1];
          pixels[px + 2] = lut[lutIdx + 2];
          pixels[px + 3] = 255;
        }
        offCtx.putImageData(rowImg, 0, 0);
      }
    };

    const renderPsdCanvas = () => {
      const canvas = psdCanvasRef.current;
      if (!canvas) return;
      const { width, height, dpr } = syncCanvasResolution(canvas);
      const ctx = canvas.getContext('2d');
      if (!ctx) return;

      ctx.save();
      ctx.scale(dpr, dpr);

      ctx.fillStyle = '#090b10';
      ctx.fillRect(0, 0, width, height);

      const plotLeft = AXIS_LEFT_PX;
      const plotRight = Math.max(plotLeft + 20, width - AXIS_RIGHT_PX);
      const plotTop = PSD_TOP_PX;
      const plotBottom = Math.max(plotTop + 20, height - PSD_BOTTOM_PX);
      const plotW = plotRight - plotLeft;
      const plotH = plotBottom - plotTop;

      const dMin = minDbfsRef.current;
      const dMax = Math.max(dMin + 10, maxDbfsRef.current);
      const dSpan = dMax - dMin;

      const yFromDbfs = (db: number) => {
        const clamped = Math.min(Math.max(db, dMin), dMax);
        return plotBottom - ((clamped - dMin) / dSpan) * plotH;
      };

      const frame = latestSpectrumRef.current;
      const centerFreqHz = frame?.center_freq_hz ?? 868_300_000;
      const sampleRateHz = frame?.sample_rate_hz ?? 2_048_000;
      const startFreqHz = centerFreqHz - sampleRateHz / 2;

      ctx.strokeStyle = '#171d29';
      ctx.lineWidth = 1;
      ctx.font = '10px "JetBrains Mono", monospace';
      ctx.fillStyle = '#64748b';
      ctx.textAlign = 'right';
      ctx.textBaseline = 'middle';

      const dbStep = dSpan <= 50 ? 5 : 10;
      const firstDbTick = Math.ceil(dMin / dbStep) * dbStep;
      for (let db = firstDbTick; db <= dMax; db += dbStep) {
        const y = Math.round(yFromDbfs(db)) + 0.5;
        ctx.beginPath();
        ctx.moveTo(plotLeft, y);
        ctx.lineTo(plotRight, y);
        ctx.stroke();
        ctx.fillText(`${db.toFixed(0)}`, plotLeft - 6, y);
      }

      ctx.textAlign = 'center';
      ctx.textBaseline = 'top';
      const freqDivisions = 8;
      for (let i = 0; i <= freqDivisions; i++) {
        const frac = i / freqDivisions;
        const x = Math.round(plotLeft + frac * plotW) + 0.5;
        ctx.strokeStyle = i === freqDivisions / 2 ? '#263147' : '#171d29';
        ctx.beginPath();
        ctx.moveTo(x, plotTop);
        ctx.lineTo(x, plotBottom);
        ctx.stroke();

        const freqMhz = (startFreqHz + frac * sampleRateHz) / 1_000_000;
        ctx.fillText(freqMhz.toFixed(3), x, plotBottom + 6);
      }

      ctx.strokeStyle = '#1e2533';
      ctx.strokeRect(plotLeft + 0.5, plotTop + 0.5, plotW, plotH);

      if (frame && frame.bins_dbfs.length > 1) {
        const bins = frame.bins_dbfs;
        const n = bins.length;

        if (
          resetSignalRef.current !== lastHandledResetRef.current ||
          !localMaxHoldRef.current ||
          localMaxHoldRef.current.length !== n
        ) {
          lastHandledResetRef.current = resetSignalRef.current;
          localMaxHoldRef.current = Float32Array.from(frame.max_hold_dbfs);
        } else {
          const hold = localMaxHoldRef.current;
          for (let i = 0; i < n; i++) {
            const serverHold = frame.max_hold_dbfs[i] ?? bins[i];
            hold[i] = Math.max(hold[i] - 0.15, bins[i], serverHold);
          }
        }

        const thresholdDb = frame.noise_floor_dbfs + 10.0;
        const threshY = Math.round(yFromDbfs(thresholdDb)) + 0.5;
        if (threshY >= plotTop && threshY <= plotBottom) {
          ctx.save();
          ctx.strokeStyle = '#ef4444';
          ctx.lineWidth = 1;
          ctx.setLineDash([5, 4]);
          ctx.beginPath();
          ctx.moveTo(plotLeft, threshY);
          ctx.lineTo(plotRight, threshY);
          ctx.stroke();
          ctx.fillStyle = '#ef4444';
          ctx.textAlign = 'left';
          ctx.textBaseline = 'bottom';
          ctx.fillText(`WIDS THRESHOLD (${thresholdDb.toFixed(1)} dBFS)`, plotLeft + 6, threshY - 3);
          ctx.restore();
        }

        if (frame.avg_psd_dbfs.length === n) {
          ctx.strokeStyle = '#64748b';
          ctx.lineWidth = 1;
          ctx.beginPath();
          for (let i = 0; i < n; i++) {
            const x = plotLeft + (i / (n - 1)) * plotW;
            const y = yFromDbfs(frame.avg_psd_dbfs[i]);
            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
          }
          ctx.stroke();
        }

        const hold = localMaxHoldRef.current;
        if (hold) {
          ctx.strokeStyle = '#f59e0b';
          ctx.lineWidth = 1.15;
          ctx.beginPath();
          for (let i = 0; i < n; i++) {
            const x = plotLeft + (i / (n - 1)) * plotW;
            const y = yFromDbfs(hold[i]);
            if (i === 0) ctx.moveTo(x, y);
            else ctx.lineTo(x, y);
          }
          ctx.stroke();
        }

        ctx.beginPath();
        for (let i = 0; i < n; i++) {
          const x = plotLeft + (i / (n - 1)) * plotW;
          const y = yFromDbfs(bins[i]);
          if (i === 0) ctx.moveTo(x, y);
          else ctx.lineTo(x, y);
        }
        ctx.strokeStyle = '#22d3ee';
        ctx.lineWidth = 1.4;
        ctx.stroke();

        ctx.lineTo(plotRight, plotBottom);
        ctx.lineTo(plotLeft, plotBottom);
        ctx.closePath();
        const grad = ctx.createLinearGradient(0, plotTop, 0, plotBottom);
        grad.addColorStop(0, 'rgba(34, 211, 238, 0.22)');
        grad.addColorStop(1, 'rgba(34, 211, 238, 0.01)');
        ctx.fillStyle = grad;
        ctx.fill();
      }

      const ch = crosshairRef.current;
      if (ch.active) {
        const cx = plotLeft + ch.xNorm * plotW;
        const cy = plotTop + ch.yNorm * plotH;
        if (cx >= plotLeft && cx <= plotRight && cy >= plotTop && cy <= plotBottom) {
          ctx.save();
          ctx.strokeStyle = 'rgba(226, 232, 240, 0.45)';
          ctx.lineWidth = 1;
          ctx.setLineDash([3, 3]);
          ctx.beginPath();
          ctx.moveTo(cx, plotTop);
          ctx.lineTo(cx, plotBottom);
          ctx.moveTo(plotLeft, cy);
          ctx.lineTo(plotRight, cy);
          ctx.stroke();

          const cursorFreqHz = startFreqHz + ch.xNorm * sampleRateHz;
          const offsetKhz = (cursorFreqHz - centerFreqHz) / 1_000;
          const cursorDb = dMax - ch.yNorm * dSpan;

          let traceReadout = '';
          if (frame && frame.bins_dbfs.length > 0) {
            const binIdx = Math.min(
              frame.bins_dbfs.length - 1,
              Math.max(0, Math.round(ch.xNorm * (frame.bins_dbfs.length - 1)))
            );
            const liveDb = frame.bins_dbfs[binIdx];
            const holdDb = localMaxHoldRef.current?.[binIdx] ?? liveDb;
            traceReadout = ` | LIVE ${liveDb.toFixed(1)} | MAX ${holdDb.toFixed(1)} dBFS`;
          }

          const label = `${(cursorFreqHz / 1_000_000).toFixed(4)} MHz (${offsetKhz >= 0 ? '+' : ''}${offsetKhz.toFixed(1)} kHz) | ${cursorDb.toFixed(1)} dBFS${traceReadout}`;
          ctx.font = '10px "JetBrains Mono", monospace';
          const textW = ctx.measureText(label).width;
          const boxW = textW + 12;
          const boxH = 18;
          const boxX = cx + 10 + boxW > plotRight ? cx - boxW - 10 : cx + 10;
          const boxY = Math.max(plotTop + 4, Math.min(plotBottom - boxH - 4, cy - 22));

          ctx.fillStyle = 'rgba(9, 11, 16, 0.92)';
          ctx.fillRect(boxX, boxY, boxW, boxH);
          ctx.strokeStyle = '#334155';
          ctx.setLineDash([]);
          ctx.strokeRect(boxX + 0.5, boxY + 0.5, boxW, boxH);

          ctx.fillStyle = '#f8fafc';
          ctx.textAlign = 'left';
          ctx.textBaseline = 'middle';
          ctx.fillText(label, boxX + 6, boxY + boxH / 2);
          ctx.restore();
        }
      }

      ctx.restore();
    };

    const renderWaterfallCanvas = () => {
      const canvas = waterfallCanvasRef.current;
      const offscreen = offscreenCanvasRef.current;
      if (!canvas || !offscreen) return;

      const { width, height, dpr } = syncCanvasResolution(canvas);
      const ctx = canvas.getContext('2d');
      if (!ctx) return;

      ctx.save();
      ctx.scale(dpr, dpr);

      ctx.fillStyle = '#090b10';
      ctx.fillRect(0, 0, width, height);

      const plotLeft = AXIS_LEFT_PX;
      const plotRight = Math.max(plotLeft + 20, width - AXIS_RIGHT_PX);
      const plotTop = 4;
      const plotBottom = Math.max(plotTop + 20, height - 8);
      const plotW = plotRight - plotLeft;
      const plotH = plotBottom - plotTop;

      ctx.imageSmoothingEnabled = false;
      ctx.drawImage(offscreen, plotLeft, plotTop, plotW, plotH);

      ctx.strokeStyle = '#1e2533';
      ctx.lineWidth = 1;
      ctx.strokeRect(plotLeft + 0.5, plotTop + 0.5, plotW, plotH);

      ctx.font = '10px "JetBrains Mono", monospace';
      ctx.fillStyle = '#64748b';
      ctx.textAlign = 'right';
      ctx.textBaseline = 'middle';
      ctx.fillText('0.0s', plotLeft - 6, plotTop + 8);
      ctx.fillText('-7.5s', plotLeft - 6, plotTop + plotH * 0.5);
      ctx.fillText('-15.0s', plotLeft - 6, plotBottom - 8);

      const ch = crosshairRef.current;
      if (ch.active) {
        const cx = plotLeft + ch.xNorm * plotW;
        if (cx >= plotLeft && cx <= plotRight) {
          ctx.save();
          ctx.strokeStyle = 'rgba(226, 232, 240, 0.4)';
          ctx.setLineDash([3, 3]);
          ctx.beginPath();
          ctx.moveTo(cx, plotTop);
          ctx.lineTo(cx, plotBottom);
          ctx.stroke();
          ctx.restore();
        }
      }

      ctx.restore();
    };

    const tick = () => {
      renderWaterfallRows();
      renderPsdCanvas();
      renderWaterfallCanvas();
      rafId = window.requestAnimationFrame(tick);
    };

    rafId = window.requestAnimationFrame(tick);
    return () => {
      window.cancelAnimationFrame(rafId);
    };
  }, [latestSpectrumRef, resetSignalRef, waterfallQueueRef]);

  const handlePointerMove = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const plotLeft = AXIS_LEFT_PX;
    const plotRight = Math.max(plotLeft + 20, rect.width - AXIS_RIGHT_PX);
    const plotTop = PSD_TOP_PX;
    const plotBottom = Math.max(plotTop + 20, rect.height - PSD_BOTTOM_PX);
    const xNorm = (e.clientX - rect.left - plotLeft) / (plotRight - plotLeft);
    const yNorm = (e.clientY - rect.top - plotTop) / (plotBottom - plotTop);

    crosshairRef.current = {
      xNorm: Math.min(Math.max(xNorm, 0), 1),
      yNorm: Math.min(Math.max(yNorm, 0), 1),
      active: xNorm >= 0 && xNorm <= 1 && yNorm >= 0 && yNorm <= 1,
    };
  };

  const handleWaterfallPointerMove = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const plotLeft = AXIS_LEFT_PX;
    const plotRight = Math.max(plotLeft + 20, rect.width - AXIS_RIGHT_PX);
    const xNorm = (e.clientX - rect.left - plotLeft) / (plotRight - plotLeft);

    crosshairRef.current = {
      xNorm: Math.min(Math.max(xNorm, 0), 1),
      yNorm: crosshairRef.current.yNorm,
      active: xNorm >= 0 && xNorm <= 1,
    };
  };

  const handlePointerLeave = () => {
    crosshairRef.current = { ...crosshairRef.current, active: false };
  };

  return (
    <section className="flex h-full min-h-0 flex-col bg-[#090b10]">
      <div className="flex flex-wrap items-center justify-between gap-2 border-b border-[#1e2533] bg-[#0d1118] px-3 py-1.5 font-mono text-[11px]">
        <div className="flex items-center gap-4">
          <span className="font-semibold tracking-wider text-slate-300">
            REAL-TIME PSD &amp; WATERFALL ({fftSize}-PT BLACKMAN-HARRIS)
          </span>
          <div className="hidden items-center gap-3 text-[10px] sm:flex">
            <span className="flex items-center gap-1 text-cyan-300">
              <span className="inline-block h-0.5 w-3 bg-cyan-400" /> LIVE PSD
            </span>
            <span className="flex items-center gap-1 text-amber-400">
              <span className="inline-block h-0.5 w-3 bg-amber-400" /> MAX HOLD
            </span>
            <span className="flex items-center gap-1 text-slate-400">
              <span className="inline-block h-0.5 w-3 bg-slate-500" /> AVG
            </span>
            <span className="flex items-center gap-1 text-red-400">
              <span className="inline-block h-0.5 w-3 border-t border-dashed border-red-400" /> WIDS
              LIMIT
            </span>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-3 tabular-nums">
          <div className="flex items-center gap-1.5">
            <label htmlFor="wf-colormap" className="text-[10px] text-slate-500">
              COLORMAP
            </label>
            <select
              id="wf-colormap"
              value={colormap}
              onChange={(e) => setColormap(e.target.value as ColormapName)}
              className="border border-[#1e2533] bg-[#090b10] px-1.5 py-0.5 text-[11px] text-slate-200 focus:border-cyan-400 focus:outline-none"
            >
              {COLORMAP_NAMES.map((name) => (
                <option key={name} value={name}>
                  {name}
                </option>
              ))}
            </select>
          </div>

          <div className="flex items-center gap-1.5">
            <span className="text-[10px] text-slate-500">MIN</span>
            <input
              type="range"
              min={-130}
              max={-40}
              step={1}
              value={minDbfs}
              onChange={(e) => {
                const val = Number(e.target.value);
                setMinDbfs(Math.min(val, maxDbfs - 10));
              }}
              className="w-20"
            />
            <span className="w-14 text-right text-[10px] text-slate-300">{minDbfs} dBFS</span>
          </div>

          <div className="flex items-center gap-1.5">
            <span className="text-[10px] text-slate-500">MAX</span>
            <input
              type="range"
              min={-80}
              max={0}
              step={1}
              value={maxDbfs}
              onChange={(e) => {
                const val = Number(e.target.value);
                setMaxDbfs(Math.max(val, minDbfs + 10));
              }}
              className="w-20"
            />
            <span className="w-14 text-right text-[10px] text-slate-300">{maxDbfs} dBFS</span>
          </div>

          <div
            className="hidden h-2.5 w-16 border border-[#1e2533] md:block"
            style={{ background: buildGradientCss(colormap) }}
            title={`${minDbfs} dBFS to ${maxDbfs} dBFS`}
          />

          <button
            type="button"
            onClick={() => void onResetMaxHold()}
            className="border border-[#1e2533] bg-[#111621] px-2 py-0.5 text-[10px] font-semibold text-amber-400 hover:border-amber-400"
          >
            RESET MAX-HOLD
          </button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col">
        <div className="relative min-h-[160px] flex-[52] border-b border-[#1e2533]">
          <canvas
            ref={psdCanvasRef}
            onMouseMove={handlePointerMove}
            onMouseLeave={handlePointerLeave}
            className="block h-full w-full cursor-crosshair"
          />
        </div>
        <div className="relative min-h-[140px] flex-[48]">
          <canvas
            ref={waterfallCanvasRef}
            onMouseMove={handleWaterfallPointerMove}
            onMouseLeave={handlePointerLeave}
            className="block h-full w-full cursor-crosshair"
          />
        </div>
      </div>
    </section>
  );
};