import React, { useState } from 'react';
import { AlertFeedPanel } from './components/AlertFeedPanel';
import { ForensicInspectorModal } from './components/ForensicInspectorModal';
import { SpectrumWaterfallView } from './components/SpectrumWaterfallView';
import { ThreatLabSidebar } from './components/ThreatLabSidebar';
import { TopHeader } from './components/TopHeader';
import { useTelemetryStream } from './hooks/useTelemetryStream';
import { WidsAlert } from './types/telemetry';

export const App: React.FC = () => {
  const {
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
  } = useTelemetryStream();

  const [selectedAlert, setSelectedAlert] = useState<WidsAlert | null>(null);

  return (
    <div className="flex h-screen w-screen flex-col overflow-hidden bg-[#090b10] text-slate-200">
      <TopHeader
        connectionState={connectionState}
        status={status}
        liveMetrics={liveMetrics}
        activeAlertCount={alerts.length}
        onSwitchBand={switchBand}
      />

      <div className="flex min-h-0 flex-1 flex-col lg:flex-row">
        <main className="min-h-0 min-w-0 flex-1">
          <SpectrumWaterfallView
            latestSpectrumRef={latestSpectrumRef}
            waterfallQueueRef={waterfallQueueRef}
            resetSignalRef={resetSignalRef}
            fftSize={status?.fft_size ?? 2048}
            onResetMaxHold={resetMaxHold}
          />
        </main>

        <ThreatLabSidebar
          sourceMode={status?.source_mode ?? 'sim'}
          baselines={status?.baselines ?? []}
          recentBursts={recentBursts}
          onInjectThreat={injectThreat}
          onAddBaseline={addBaseline}
        />
      </div>

      <AlertFeedPanel
        alerts={alerts}
        onSelectAlert={setSelectedAlert}
        onClearAlerts={clearAlerts}
      />

      <ForensicInspectorModal
        alert={selectedAlert}
        recentBursts={recentBursts}
        onClose={() => setSelectedAlert(null)}
      />
    </div>
  );
};

export default App;