import React, { useState } from 'react';
import {
  DetectedBurst,
  DeviceBaselineProfile,
  InjectThreatPayload,
  ModulationType,
} from '../types/telemetry';

interface ThreatLabSidebarProps {
  sourceMode: string;
  baselines: DeviceBaselineProfile[];
  recentBursts: DetectedBurst[];
  onInjectThreat: (payload: InjectThreatPayload) => Promise<string>;
  onAddBaseline: (profile: DeviceBaselineProfile) => Promise<void>;
}

export const ThreatLabSidebar: React.FC<ThreatLabSidebarProps> = ({
  sourceMode,
  baselines,
  recentBursts,
  onInjectThreat,
  onAddBaseline,
}) => {
  const isSim = sourceMode === 'sim';

  const [jamPowerOffsetDb, setJamPowerOffsetDb] = useState<number>(24);
  const [cwOffsetKhz, setCwOffsetKhz] = useState<number>(120);
  const [replayCount, setReplayCount] = useState<number>(4);
  const [rogueOffsetKhz, setRogueOffsetKhz] = useState<number>(-140);
  const [rogueBaud, setRogueBaud] = useState<number>(12500);
  const [feedback, setFeedback] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null);
  const [busy, setBusy] = useState<boolean>(false);

  const [showAddForm, setShowAddForm] = useState<boolean>(false);
  const [newName, setNewName] = useState<string>('OT-CustomSensor-04');
  const [newNodeId, setNewNodeId] = useState<number>(4);
  const [newOffsetKhz, setNewOffsetKhz] = useState<number>(-220);
  const [newMod, setNewMod] = useState<ModulationType>('Ook');
  const [newBaud, setNewBaud] = useState<number>(4800);

  const trigger = async (payload: InjectThreatPayload, label: string) => {
    setBusy(true);
    setFeedback(null);
    try {
      await onInjectThreat(payload);
      setFeedback({ kind: 'ok', text: `INJECTED: ${label}` });
    } catch (err) {
      setFeedback({
        kind: 'err',
        text: err instanceof Error ? err.message : 'Injection failed',
      });
    } finally {
      setBusy(false);
    }
  };

  const handleCreateBaseline = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newName.trim()) return;
    await onAddBaseline({
      name: newName.trim(),
      node_id: newNodeId,
      expected_offset_hz: newOffsetKhz * 1_000,
      offset_tolerance_hz: 15_000,
      modulation: newMod,
      expected_baud_rate: newBaud,
      baud_tolerance_pct: 35.0,
      min_duration_us: 3_000,
      max_duration_us: 45_000,
      expected_period_ms: 500,
    });
    setShowAddForm(false);
    setNewNodeId((prev) => prev + 1);
  };

  const latestBurstNs = recentBursts[0]?.timestamp_ns ?? 0;
  const isNodeActive = (profile: DeviceBaselineProfile): boolean => {
    for (let i = 0; i < Math.min(recentBursts.length, 15); i++) {
      const b = recentBursts[i];
      if (latestBurstNs - b.timestamp_ns > 3_000_000_000) break;
      if (
        Math.abs(b.center_offset_hz - profile.expected_offset_hz) <= profile.offset_tolerance_hz &&
        b.modulation_hint === profile.modulation
      ) {
        return true;
      }
    }
    return false;
  };

  return (
    <aside className="flex h-full min-h-0 w-full flex-col border-l border-[#1e2533] bg-[#0d1118] lg:w-[360px] xl:w-[390px]">
      <div className="border-b border-[#1e2533] p-3">
        <div className="mb-2 flex items-center justify-between">
          <span className="font-mono text-[11px] font-bold tracking-wider text-slate-200">
            RF THREAT LAB // SCENARIO INJECTOR
          </span>
          <span
            className={`border px-1.5 py-0.5 font-mono text-[9px] font-semibold ${
              isSim
                ? 'border-cyan-500/50 bg-cyan-950/40 text-cyan-300'
                : 'border-slate-700 bg-slate-900 text-slate-500'
            }`}
          >
            {isSim ? 'SIM READY' : 'READ-ONLY CAPTURE'}
          </span>
        </div>

        {feedback && (
          <div
            className={`mb-2 border px-2 py-1 font-mono text-[10px] ${
              feedback.kind === 'ok'
                ? 'border-emerald-500/50 bg-emerald-950/40 text-emerald-300'
                : 'border-red-500/50 bg-red-950/40 text-red-300'
            }`}
          >
            {feedback.text}
          </div>
        )}

        <div className="space-y-2 font-mono text-[11px]">
          <div className="border border-[#1e2533] bg-[#090b10] p-2">
            <div className="mb-1.5 flex items-center justify-between text-[10px]">
              <span className="text-slate-400">BARRAGE NOISE (5000 ms)</span>
              <span className="tabular-nums text-red-400">+{jamPowerOffsetDb} dB</span>
            </div>
            <div className="flex items-center gap-2">
              <input
                type="range"
                min={12}
                max={36}
                step={2}
                value={jamPowerOffsetDb}
                disabled={!isSim || busy}
                onChange={(e) => setJamPowerOffsetDb(Number(e.target.value))}
                className="flex-1"
              />
              <button
                type="button"
                disabled={!isSim || busy}
                onClick={() =>
                  void trigger(
                    {
                      threat: 'broadband_jammer',
                      duration_ms: 5000,
                      power_offset_db: jamPowerOffsetDb,
                    },
                    `BROADBAND JAMMER (+${jamPowerOffsetDb} dB, 5s)`
                  )
                }
                className="border border-red-500/60 bg-red-950/40 px-2.5 py-1 text-[10px] font-semibold text-red-300 hover:bg-red-900/50 disabled:opacity-40"
              >
                TRIGGER BROADBAND JAMMING
              </button>
            </div>
          </div>

          <div className="border border-[#1e2533] bg-[#090b10] p-2">
            <div className="mb-1.5 flex items-center justify-between text-[10px]">
              <span className="text-slate-400">NARROWBAND CW/FM (3000 ms)</span>
              <span className="tabular-nums text-amber-400">
                {cwOffsetKhz >= 0 ? '+' : ''}
                {cwOffsetKhz} kHz
              </span>
            </div>
            <div className="flex items-center gap-2">
              <input
                type="number"
                value={cwOffsetKhz}
                disabled={!isSim || busy}
                onChange={(e) => setCwOffsetKhz(Number(e.target.value))}
                className="w-20 border border-[#1e2533] bg-[#0d1118] px-1.5 py-1 text-[10px] tabular-nums text-slate-200 focus:border-amber-400 focus:outline-none"
              />
              <button
                type="button"
                disabled={!isSim || busy}
                onClick={() =>
                  void trigger(
                    {
                      threat: 'narrowband_jammer',
                      freq_offset_hz: cwOffsetKhz * 1_000,
                      duration_ms: 3000,
                      power_db: -14.0,
                    },
                    `NARROWBAND CW JAMMER (${cwOffsetKhz} kHz)`
                  )
                }
                className="flex-1 border border-amber-500/60 bg-amber-950/40 px-2 py-1 text-[10px] font-semibold text-amber-300 hover:bg-amber-900/50 disabled:opacity-40"
              >
                TRIGGER NARROWBAND CW JAMMER
              </button>
            </div>
          </div>

          <div className="border border-[#1e2533] bg-[#090b10] p-2">
            <div className="mb-1.5 flex items-center justify-between text-[10px]">
              <span className="text-slate-400">FRAME CAPTURE REPLAY</span>
              <span className="tabular-nums text-cyan-300">{replayCount}x @ 85 ms</span>
            </div>
            <div className="flex items-center gap-2">
              <input
                type="number"
                min={1}
                max={12}
                value={replayCount}
                disabled={!isSim || busy}
                onChange={(e) => setReplayCount(Math.max(1, Math.min(12, Number(e.target.value))))}
                className="w-16 border border-[#1e2533] bg-[#0d1118] px-1.5 py-1 text-[10px] tabular-nums text-slate-200 focus:border-cyan-400 focus:outline-none"
              />
              <button
                type="button"
                disabled={!isSim || busy}
                onClick={() =>
                  void trigger(
                    {
                      threat: 'replay_attack',
                      repeat_count: replayCount,
                      interval_ms: 85,
                    },
                    `REPLAY ATTACK (${replayCount}x bursts)`
                  )
                }
                className="flex-1 border border-red-500/60 bg-red-950/30 px-2 py-1 text-[10px] font-semibold text-red-200 hover:bg-red-900/40 disabled:opacity-40"
              >
                EXECUTE REPLAY ATTACK
              </button>
            </div>
          </div>

          <div className="border border-[#1e2533] bg-[#090b10] p-2">
            <div className="mb-1.5 flex items-center justify-between text-[10px]">
              <span className="text-slate-400">UNALLOCATED OOK EMITTER</span>
              <span className="tabular-nums text-amber-300">
                {rogueOffsetKhz} kHz / {rogueBaud} Bd
              </span>
            </div>
            <div className="flex items-center gap-1.5">
              <input
                type="number"
                value={rogueOffsetKhz}
                disabled={!isSim || busy}
                onChange={(e) => setRogueOffsetKhz(Number(e.target.value))}
                aria-label="Rogue offset in kHz"
                className="w-16 border border-[#1e2533] bg-[#0d1118] px-1.5 py-1 text-[10px] tabular-nums text-slate-200 focus:border-amber-400 focus:outline-none"
              />
              <input
                type="number"
                value={rogueBaud}
                disabled={!isSim || busy}
                onChange={(e) => setRogueBaud(Number(e.target.value))}
                aria-label="Rogue baud rate"
                className="w-16 border border-[#1e2533] bg-[#0d1118] px-1.5 py-1 text-[10px] tabular-nums text-slate-200 focus:border-amber-400 focus:outline-none"
              />
              <button
                type="button"
                disabled={!isSim || busy}
                onClick={() =>
                  void trigger(
                    {
                      threat: 'rogue_transmitter',
                      freq_offset_hz: rogueOffsetKhz * 1_000,
                      baud_rate: rogueBaud,
                      duration_ms: 1500,
                    },
                    `ROGUE OOK EMITTER (${rogueOffsetKhz} kHz, ${rogueBaud} Bd)`
                  )
                }
                className="flex-1 border border-amber-500/60 bg-amber-950/30 px-2 py-1 text-[10px] font-semibold text-amber-200 hover:bg-amber-900/40 disabled:opacity-40"
              >
                SPAWN ROGUE EMITTER
              </button>
            </div>
          </div>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col p-3">
        <div className="mb-2 flex items-center justify-between">
          <span className="font-mono text-[11px] font-bold tracking-wider text-slate-200">
            AUTHORIZED NODE BASELINES ({baselines.length})
          </span>
          <button
            type="button"
            onClick={() => setShowAddForm((prev) => !prev)}
            className="border border-[#1e2533] bg-[#111621] px-2 py-0.5 font-mono text-[10px] font-semibold text-cyan-300 hover:border-cyan-400"
          >
            {showAddForm ? 'CANCEL' : '+ ADD PROFILE'}
          </button>
        </div>

        {showAddForm && (
          <form
            onSubmit={(e) => void handleCreateBaseline(e)}
            className="mb-2 space-y-1.5 border border-[#1e2533] bg-[#090b10] p-2 font-mono text-[10px]"
          >
            <div className="grid grid-cols-3 gap-1.5">
              <input
                type="text"
                value={newName}
                onChange={(e) => setNewName(e.target.value)}
                placeholder="Node Name"
                className="col-span-2 border border-[#1e2533] bg-[#0d1118] px-1.5 py-1 text-slate-200 focus:border-cyan-400 focus:outline-none"
              />
              <input
                type="number"
                value={newNodeId}
                onChange={(e) => setNewNodeId(Number(e.target.value))}
                placeholder="ID"
                className="border border-[#1e2533] bg-[#0d1118] px-1.5 py-1 text-slate-200 focus:border-cyan-400 focus:outline-none"
              />
            </div>
            <div className="grid grid-cols-3 gap-1.5">
              <input
                type="number"
                value={newOffsetKhz}
                onChange={(e) => setNewOffsetKhz(Number(e.target.value))}
                placeholder="Offset kHz"
                className="border border-[#1e2533] bg-[#0d1118] px-1.5 py-1 text-slate-200 focus:border-cyan-400 focus:outline-none"
              />
              <select
                value={newMod}
                onChange={(e) => setNewMod(e.target.value as ModulationType)}
                className="border border-[#1e2533] bg-[#0d1118] px-1 py-1 text-slate-200 focus:border-cyan-400 focus:outline-none"
              >
                <option value="Ook">OOK</option>
                <option value="Fsk2">2-FSK</option>
              </select>
              <input
                type="number"
                value={newBaud}
                onChange={(e) => setNewBaud(Number(e.target.value))}
                placeholder="Baud"
                className="border border-[#1e2533] bg-[#0d1118] px-1.5 py-1 text-slate-200 focus:border-cyan-400 focus:outline-none"
              />
            </div>
            <button
              type="submit"
              className="w-full border border-cyan-500/60 bg-cyan-950/40 py-1 font-semibold text-cyan-300 hover:bg-cyan-900/50"
            >
              COMMIT ALLOWLIST PROFILE
            </button>
          </form>
        )}

        <div className="min-h-0 flex-1 overflow-auto border border-[#1e2533] bg-[#090b10]">
          <table className="w-full border-collapse text-left font-mono text-[10px] tabular-nums">
            <thead className="sticky top-0 border-b border-[#1e2533] bg-[#111621] text-slate-400">
              <tr>
                <th className="px-2 py-1.5 font-semibold">NODE</th>
                <th className="px-2 py-1.5 font-semibold">OFFSET</th>
                <th className="px-2 py-1.5 font-semibold">PHY</th>
                <th className="px-2 py-1.5 text-right font-semibold">STATUS</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-[#1e2533]">
              {baselines.map((profile) => {
                const active = isNodeActive(profile);
                return (
                  <tr key={`${profile.node_id}-${profile.name}`} className="hover:bg-[#111621]/70">
                    <td className="px-2 py-1.5">
                      <div className="font-semibold text-slate-200">{profile.name}</div>
                      <div className="text-[9px] text-slate-500">
                        ID #{profile.node_id} | {profile.expected_period_ms} ms
                      </div>
                    </td>
                    <td className="px-2 py-1.5 text-cyan-300">
                      {profile.expected_offset_hz >= 0 ? '+' : ''}
                      {(profile.expected_offset_hz / 1_000).toFixed(0)} kHz
                    </td>
                    <td className="px-2 py-1.5 text-slate-300">
                      <div>{profile.modulation.toUpperCase()}</div>
                      <div className="text-[9px] text-slate-500">{profile.expected_baud_rate} Bd</div>
                    </td>
                    <td className="px-2 py-1.5 text-right">
                      <span
                        className={`inline-block border px-1.5 py-0.5 text-[9px] font-semibold ${
                          active
                            ? 'border-emerald-500/60 bg-emerald-950/50 text-emerald-300'
                            : 'border-slate-700 bg-slate-900/70 text-slate-400'
                        }`}
                      >
                        {active ? 'TX BURST' : 'ALLOWLIST'}
                      </span>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </div>
    </aside>
  );
};