import * as React from "react";
import { motion, type Transition } from "framer-motion";

const SPRING: Transition = { type: "spring", stiffness: 120, damping: 20 };
const INSTANT: Transition = { duration: 0 };

const SHEEN_SWEEP = 1.6;
const SHEEN_PAUSE = 3.5;

// The sweep is sized from a rounded fill so a drifting percentage never
// re-targets the animation mid-flight, and the travel distance stays constant.
const SHEEN_STEP = 10;
const SHEEN_FILL_RATIO = 0.45;
const SHEEN_TRAVEL_END = `${(1 / SHEEN_FILL_RATIO) * 100 + 20}%`;

// Under this fill the sweep is only a few pixels wide and reads as a flicker.
const SHEEN_MIN_FILL = 30;

// A five stop gradient squeezed into a sliver leaves one bright dot, so the
// core fades in with the fill instead of appearing all at once.
const BAND_FADE_START = 15;
const BAND_FADE_END = 40;

export interface FluxBarProps {
  /** Fill percentage, 0 to 100. */
  value: number;
  palette: "claude" | "codex";
  /** Combined result of the animation setting, reduced motion and occlusion. */
  motion: boolean;
  /** Draw the bar as inactive when the snapshot is older than its window. */
  stale?: boolean;
}

/**
 * The fill is drawn with two opposing translations rather than an animated
 * width: the outer layer slides left and carries an undistorted pill cap, the
 * inner layer slides back by the same amount so the gradient stays anchored to
 * the track. Both stay on the compositor.
 */
export function FluxBar({ value, palette, motion: motionOn, stale }: FluxBarProps) {
  const p = Math.min(100, Math.max(0, Number.isFinite(value) ? value : 0));
  const shift = 100 - p;

  const from = `var(--${palette}-from)`;
  const to = `var(--${palette}-to)`;
  const core = `var(--${palette}-core)`;
  const glowColor = `var(--${palette}-glow)`;

  const danger = p >= 90;
  const blend = (c: string) => (danger ? `color-mix(in srgb, ${c} 60%, var(--danger))` : c);

  const bandStrength = Math.min(
    1,
    Math.max(0, (p - BAND_FADE_START) / (BAND_FADE_END - BAND_FADE_START)),
  );
  const litCore = `color-mix(in oklab, ${core} ${Math.round(bandStrength * 100)}%, ${to})`;

  const gradient = stale
    ? "linear-gradient(90deg, var(--text-secondary), var(--text-secondary))"
    : `linear-gradient(90deg, ${blend(from)} 0%, ${blend(to)} 35%, ${blend(litCore)} 55%, ${blend(to)} 78%, ${blend(from)} 100%)`;

  // Scaled from the reference bar, which is over three times taller.
  const glass = stale
    ? "none"
    : "inset 0 0.45px 0 rgba(255,255,255,0.5), inset 0 -0.6px 1px rgba(0,0,0,0.28)";

  const full = p >= 100;

  const sheenSpan = Math.min(100, Math.max(SHEEN_STEP, Math.ceil(p / SHEEN_STEP) * SHEEN_STEP));
  const sheenWidth = sheenSpan * SHEEN_FILL_RATIO;
  const showSheen = motionOn && !stale && p >= SHEEN_MIN_FILL;

  return (
    <div className="fd-track-wrap">
      {/* Kept outside the track so the track's overflow does not clip it. */}
      <motion.div
        aria-hidden
        className="fd-glow"
        style={{
          boxShadow: stale ? "none" : `0 0 3px 0 ${glowColor}`,
          transformOrigin: "left center",
        }}
        initial={false}
        animate={{
          scaleX: p / 100,
          opacity: full && motionOn ? [0.6, 1, 0.6] : 1,
        }}
        transition={
          motionOn
            ? {
                scaleX: SPRING,
                opacity: full ? { duration: 1.2, repeat: Infinity, ease: "easeInOut" } : INSTANT,
              }
            : INSTANT
        }
      />

      <div
        className="fd-track"
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(p)}
      >
        <motion.div
          className="fd-clip"
          initial={false}
          animate={{ x: `-${shift}%` }}
          transition={motionOn ? SPRING : INSTANT}
        >
          <motion.div
            className="fd-fill"
            style={{
              backgroundImage: gradient,
              backgroundSize: `${Math.max(p, 0.001)}% 100%`,
              backgroundRepeat: "no-repeat",
              boxShadow: glass,
            }}
            initial={false}
            animate={{ x: `${shift}%` }}
            transition={motionOn ? SPRING : INSTANT}
          >
            {showSheen && (
              <motion.span
                aria-hidden
                className="fd-sheen"
                style={{ width: `${sheenWidth}%` }}
                initial={{ x: "-110%" }}
                animate={{ x: SHEEN_TRAVEL_END }}
                transition={{
                  duration: SHEEN_SWEEP,
                  ease: "linear",
                  repeat: Infinity,
                  repeatDelay: SHEEN_PAUSE,
                }}
              />
            )}
          </motion.div>
        </motion.div>
      </div>
    </div>
  );
}

export default React.memo(FluxBar);
