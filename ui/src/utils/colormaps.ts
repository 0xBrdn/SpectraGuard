export type ColormapName = 'Inferno' | 'Viridis' | 'Turbo' | 'Phosphor Green';

export const COLORMAP_NAMES: ColormapName[] = [
  'Inferno',
  'Viridis',
  'Turbo',
  'Phosphor Green',
];

type RgbStop = [number, number, number, number];

const INFERNO_STOPS: RgbStop[] = [
  [0.0, 2, 2, 11],
  [0.14, 27, 12, 65],
  [0.29, 74, 12, 107],
  [0.43, 120, 28, 109],
  [0.57, 165, 44, 96],
  [0.71, 207, 68, 70],
  [0.86, 237, 105, 37],
  [0.94, 251, 155, 6],
  [1.0, 252, 255, 164],
];

const VIRIDIS_STOPS: RgbStop[] = [
  [0.0, 68, 1, 84],
  [0.16, 72, 40, 120],
  [0.33, 62, 73, 137],
  [0.5, 49, 104, 142],
  [0.66, 38, 130, 142],
  [0.82, 53, 183, 121],
  [0.92, 144, 215, 67],
  [1.0, 253, 231, 37],
];

const TURBO_STOPS: RgbStop[] = [
  [0.0, 48, 18, 59],
  [0.14, 69, 91, 205],
  [0.28, 62, 154, 254],
  [0.42, 24, 214, 203],
  [0.56, 72, 248, 130],
  [0.7, 164, 252, 60],
  [0.82, 238, 207, 58],
  [0.92, 251, 128, 34],
  [1.0, 122, 4, 3],
];

const PHOSPHOR_STOPS: RgbStop[] = [
  [0.0, 3, 10, 6],
  [0.25, 6, 38, 20],
  [0.5, 12, 92, 45],
  [0.75, 24, 178, 84],
  [0.9, 96, 238, 142],
  [1.0, 215, 255, 225],
];

function buildLutFromStops(stops: RgbStop[]): Uint8ClampedArray {
  const lut = new Uint8ClampedArray(256 * 4);
  for (let i = 0; i < 256; i++) {
    const t = i / 255;
    let s0 = stops[0];
    let s1 = stops[stops.length - 1];
    for (let j = 0; j < stops.length - 1; j++) {
      if (t >= stops[j][0] && t <= stops[j + 1][0]) {
        s0 = stops[j];
        s1 = stops[j + 1];
        break;
      }
    }
    const span = Math.max(s1[0] - s0[0], 1e-6);
    const local = Math.min(Math.max((t - s0[0]) / span, 0), 1);
    const base = i * 4;
    lut[base] = Math.round(s0[1] + (s1[1] - s0[1]) * local);
    lut[base + 1] = Math.round(s0[2] + (s1[2] - s0[2]) * local);
    lut[base + 2] = Math.round(s0[3] + (s1[3] - s0[3]) * local);
    lut[base + 3] = 255;
  }
  return lut;
}

export const COLORMAP_LUTS: Record<ColormapName, Uint8ClampedArray> = {
  Inferno: buildLutFromStops(INFERNO_STOPS),
  Viridis: buildLutFromStops(VIRIDIS_STOPS),
  Turbo: buildLutFromStops(TURBO_STOPS),
  'Phosphor Green': buildLutFromStops(PHOSPHOR_STOPS),
};

export function mapDbfsToLutIndex(valueDbfs: number, minDbfs: number, maxDbfs: number): number {
  const span = Math.max(maxDbfs - minDbfs, 1);
  const norm = (valueDbfs - minDbfs) / span;
  if (norm <= 0) return 0;
  if (norm >= 1) return 255;
  return Math.round(norm * 255);
}

export function buildGradientCss(colormap: ColormapName): string {
  const lut = COLORMAP_LUTS[colormap];
  const stops: string[] = [];
  for (let step = 0; step <= 8; step++) {
    const idx = Math.min(255, Math.round((step / 8) * 255));
    const base = idx * 4;
    stops.push(`rgb(${lut[base]}, ${lut[base + 1]}, ${lut[base + 2]}) ${(step / 8) * 100}%`);
  }
  return `linear-gradient(90deg, ${stops.join(', ')})`;
}