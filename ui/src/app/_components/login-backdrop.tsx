/**
 * Backdrop of the sign-in page: streams of light gather from the whole width of
 * the floor into one beam that rises, as backups converge on the appliance and
 * leave for object storage. Drawn once, deterministic, decorative only.
 */
const WIDTH = 1600, HEIGHT = 900, CENTER = 800, BASE = 540;

// A small seeded generator: the same picture on every render and every server.
function random(seed: number) {
  let state = seed;
  return () => {
    state = (state * 1664525 + 1013904223) % 4294967296;
    return state / 4294967296;
  };
}

type Stream = { d: string; width: number; opacity: number; accent: boolean };

function streams(): Stream[] {
  const next = random(7);
  const result: Stream[] = [];
  for (let index = 0; index < 120; index++) {
    // Spread across the floor, denser near the middle.
    const side = next() < 0.5 ? -1 : 1;
    const spread = Math.pow(next(), 0.7) * 1050;
    const start = CENTER + side * spread;
    const floor = HEIGHT - 70 + next() * 110 - Math.min(spread / 1050, 1) * 120;
    // The beam keeps a little width so its strands stay visible.
    const trunk = CENTER + (next() - 0.5) * 70 + side * Math.min(spread, 400) * 0.02;
    const d = `M${start.toFixed(1)} ${floor.toFixed(1)} `
      + `C${(start + (trunk - start) * 0.62).toFixed(1)} ${(floor - 24).toFixed(1)}, `
      + `${trunk.toFixed(1)} ${(BASE + 150).toFixed(1)}, ${trunk.toFixed(1)} ${BASE} `
      + `L${(trunk + (next() - 0.5) * 6).toFixed(1)} -20`;
    result.push({ d, width: 0.5 + next() * 1.3, opacity: 0.25 + next() * 0.6, accent: index % 29 === 3 });
  }
  return result;
}

function sparks() {
  const next = random(23);
  return Array.from({ length: 170 }, () => {
    const x = next() * WIDTH;
    const distance = Math.abs(x - CENTER) / CENTER;
    const y = HEIGHT - 40 - next() * 210 + distance * 40;
    return { x, y, r: 0.6 + next() * 1.8, opacity: 0.25 + next() * 0.7 };
  });
}

const STREAMS = streams();
const SPARKS = sparks();
// A handful of strands carry a travelling light pulse.
const PULSES = STREAMS.filter((_, index) => index % 17 === 5);

export function LoginBackdrop() {
  return (
    <svg className="lto-login-backdrop" viewBox={`0 0 ${WIDTH} ${HEIGHT}`} preserveAspectRatio="xMidYMid slice" aria-hidden="true" focusable="false">
      <defs>
        <radialGradient id="login-sky" cx="50%" cy="58%" r="75%">
          <stop offset="0" stopColor="#0c4aa0" />
          <stop offset="0.45" stopColor="#072a63" />
          <stop offset="1" stopColor="#020a1c" />
        </radialGradient>
        <linearGradient id="login-stream" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#9ad8ff" />
          <stop offset="0.55" stopColor="#4aa8ff" />
          <stop offset="1" stopColor="#1d5fd0" />
        </linearGradient>
        <linearGradient id="login-fade" x1="0" y1="0" x2="0" y2="1">
          <stop offset="0" stopColor="#fff" stopOpacity="0" />
          <stop offset="0.18" stopColor="#fff" stopOpacity="1" />
        </linearGradient>
        <mask id="login-top-fade"><rect width={WIDTH} height={HEIGHT} fill="url(#login-fade)" /></mask>
        <filter id="login-glow" x="-20%" y="-20%" width="140%" height="140%">
          <feGaussianBlur stdDeviation="6" />
        </filter>
      </defs>
      <rect width={WIDTH} height={HEIGHT} fill="url(#login-sky)" />
      <ellipse cx={CENTER} cy={BASE + 60} rx="520" ry="170" fill="#2f8cff" opacity="0.18" filter="url(#login-glow)" />
      <g mask="url(#login-top-fade)" fill="none" strokeLinecap="round">
        <g filter="url(#login-glow)" opacity="0.55">
          {STREAMS.map((stream, index) => <path key={index} d={stream.d} stroke={stream.accent ? "#ff8a3d" : "#3d9bff"} strokeWidth={stream.width * 3} opacity={stream.opacity} />)}
        </g>
        {STREAMS.map((stream, index) => <path key={index} d={stream.d} stroke={stream.accent ? "#ffa463" : "url(#login-stream)"} strokeWidth={stream.width} opacity={stream.opacity} />)}
        {PULSES.map((stream, index) => <path key={index} className="lto-login-pulse" style={{ animationDelay: `${index * -1.7}s` }} d={stream.d} stroke="#e6f6ff" strokeWidth={stream.width + 0.8} pathLength={1000} />)}
      </g>
      <g fill="#bfe6ff">
        {SPARKS.map((spark, index) => <circle key={index} cx={spark.x.toFixed(1)} cy={spark.y.toFixed(1)} r={spark.r.toFixed(2)} opacity={spark.opacity.toFixed(2)} />)}
      </g>
    </svg>
  );
}
