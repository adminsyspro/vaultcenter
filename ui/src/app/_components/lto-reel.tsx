export function LtoReel() {
 return (
<div className="lto-reel-window" aria-hidden="true">
        <svg viewBox="0 0 120 120" className="lto-reel">
          <circle cx="60" cy="60" r="55" className="lto-tape-pack" />
          {[48, 44, 40, 36].map(r => <circle key={r} cx="60" cy="60" r={r} className="lto-tape-layer" />)}
          <g className="lto-reel-spin">
            <circle cx="60" cy="60" r="30" className="lto-reel-flange" />
            {[0, 120, 240].map(angle => <path key={angle} d="M55 35 L65 35 L65 48 L55 48 Z" transform={`rotate(${angle} 60 60)`} className="lto-reel-spoke" />)}
            <circle cx="60" cy="60" r="12" className="lto-reel-hub" />
            <path d="M57 52h6v16h-6zM52 57h16v6H52z" className="lto-reel-key" />
          </g>
        </svg>
        <span className="lto-case-detail" />
      </div>
 );
}
