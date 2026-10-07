// The ambient backdrop (the cover art, blurred, behind the whole window) and
// the accent colour taken from the art.

import { h, hashString } from './dom.js';

const FADE_MS = 950;
const BRAND = { h: 336, s: 86 };

/** A soft gradient made from a hue: used when a song has no cover art. */
export function seedGradient(hue) {
  return [
    `radial-gradient(70% 70% at 22% 24%, hsl(${hue} 78% 58%) 0%, transparent 70%)`,
    `radial-gradient(60% 60% at 82% 28%, hsl(${(hue + 48) % 360} 74% 54%) 0%, transparent 70%)`,
    `radial-gradient(80% 70% at 50% 100%, hsl(${(hue + 320) % 360} 72% 46%) 0%, transparent 72%)`,
    `hsl(${hue} 46% 24%)`,
  ].join(', ');
}

/** The hue for a song without usable art, stable for the same title and artist. */
export function seedHue(seed) {
  return hashString(seed) % 360;
}

const BRAND_GRADIENT = [
  'radial-gradient(60% 60% at 18% 20%, #ff9a4d 0%, transparent 70%)',
  'radial-gradient(60% 60% at 85% 25%, #f43f7a 0%, transparent 70%)',
  'radial-gradient(80% 70% at 50% 100%, #7448ff 0%, transparent 72%)',
  '#3a1f5c',
].join(', ');

function setAccent({ h: hue, s }) {
  const root = document.documentElement.style;
  root.setProperty('--accent-h', String(Math.round(hue)));
  root.setProperty('--accent-s', `${Math.round(s)}%`);
}

function rgbToHsl(r, g, b) {
  r /= 255;
  g /= 255;
  b /= 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  if (max === min) {
    return [0, 0, l];
  }
  const d = max - min;
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let hue;
  if (max === r) {
    hue = (g - b) / d + (g < b ? 6 : 0);
  } else if (max === g) {
    hue = (b - r) / d + 2;
  } else {
    hue = (r - g) / d + 4;
  }
  return [hue * 60, s, l];
}

/**
 * The most vivid common hue in the pixels: hues are binned (weighted towards
 * saturated mid tones) and the strongest neighbourhood wins.
 */
function pickAccent(data) {
  const BINS = 36;
  const bins = Array.from({ length: BINS }, () => ({ w: 0, x: 0, y: 0, s: 0 }));
  for (let i = 0; i < data.length; i += 4) {
    if (data[i + 3] < 128) {
      continue;
    }
    const [hue, s, l] = rgbToHsl(data[i], data[i + 1], data[i + 2]);
    if (l < 0.1 || l > 0.94 || s < 0.18) {
      continue;
    }
    const w = s * s * Math.max(0, 1 - Math.abs(l - 0.55) * 1.6);
    if (w <= 0) {
      continue;
    }
    const bin = bins[Math.floor(hue / (360 / BINS)) % BINS];
    const rad = (hue * Math.PI) / 180;
    bin.w += w;
    bin.x += Math.cos(rad) * w;
    bin.y += Math.sin(rad) * w;
    bin.s += s * w;
  }
  let best = -1;
  let bestWeight = 0;
  for (let i = 0; i < BINS; i += 1) {
    const weight = bins[i].w + 0.5 * (bins[(i + 1) % BINS].w + bins[(i + BINS - 1) % BINS].w);
    if (weight > bestWeight) {
      best = i;
      bestWeight = weight;
    }
  }
  if (best < 0 || bestWeight < 4) {
    return null;
  }
  const bin = bins[best];
  if (bin.w <= 0) {
    return null;
  }
  const hue = ((Math.atan2(bin.y, bin.x) * 180) / Math.PI + 360) % 360;
  const saturation = (bin.s / bin.w) * 100;
  return { h: hue, s: Math.min(92, Math.max(58, saturation)) };
}

/**
 * Samples the art for an accent. Cross-origin art without CORS cannot be read
 * (the canvas is tainted, or the image does not load): resolves to null.
 */
async function sampleAccent(url) {
  try {
    const img = new Image();
    img.crossOrigin = 'anonymous';
    img.src = url;
    await img.decode();
    const size = 40;
    const canvas = document.createElement('canvas');
    canvas.width = size;
    canvas.height = size;
    const ctx = canvas.getContext('2d', { willReadFrequently: true });
    ctx.drawImage(img, 0, 0, size, size);
    return pickAccent(ctx.getImageData(0, 0, size, size).data);
  } catch {
    return null;
  }
}

/**
 * Creates the backdrop controller for `container` (the `.backdrop-art` element).
 * `show({ artwork, seed })` crossfades to the new art (or to a gradient from
 * `seed`, the title and artist); `showBrand()` shows the Lyrix colours.
 */
export function createBackdrop(container) {
  let key = null;
  let token = 0;

  function swap(layer) {
    container.append(layer);
    const previous = [...container.children].filter((child) => child !== layer);
    // Two frames so the starting opacity is painted before the fade.
    requestAnimationFrame(() =>
      requestAnimationFrame(() => {
        layer.classList.add('is-visible');
        setTimeout(() => previous.forEach((old) => old.remove()), FADE_MS);
      }),
    );
  }

  function gradientLayer(background) {
    const layer = h('div', { class: 'backdrop-layer' });
    layer.style.background = background;
    return layer;
  }

  return {
    show({ artwork, seed }) {
      const nextKey = artwork ? `art:${artwork}` : `seed:${seed}`;
      if (nextKey === key) {
        return;
      }
      key = nextKey;
      token += 1;
      const mine = token;
      const hue = seedHue(seed);
      const fallback = { h: hue, s: 72 };

      if (!artwork) {
        swap(gradientLayer(seedGradient(hue)));
        setAccent(fallback);
        return;
      }
      const layer = h('div', { class: 'backdrop-layer' });
      const img = h('img', { class: 'backdrop-img', alt: '', decoding: 'async' });
      img.addEventListener('load', () => mine === token && swap(layer));
      img.addEventListener('error', () => {
        if (mine === token) {
          layer.style.background = seedGradient(hue);
          img.remove();
          swap(layer);
        }
      });
      img.src = artwork;
      layer.append(img);
      sampleAccent(artwork).then((accent) => {
        if (mine === token) {
          setAccent(accent || fallback);
        }
      });
    },
    showBrand() {
      if (key === 'brand') {
        return;
      }
      key = 'brand';
      token += 1;
      swap(gradientLayer(BRAND_GRADIENT));
      setAccent(BRAND);
    },
  };
}
