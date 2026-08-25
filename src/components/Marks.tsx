import type { ReactElement } from "react";

/**
 * Provider glyphs are drawn here rather than reusing vendor logos, so the
 * widget never implies endorsement by Anthropic, OpenAI or Google.
 */

export function ClaudeMark() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
      <path
        d="M5 0.6 L6.05 3.6 L9 5 L6.05 6.4 L5 9.4 L3.95 6.4 L1 5 L3.95 3.6 Z"
        fill="var(--claude-to)"
      />
    </svg>
  );
}

export function CodexMark() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
      <rect
        x="0.6"
        y="1.2"
        width="8.8"
        height="7.6"
        rx="1.6"
        fill="none"
        stroke="var(--codex-to)"
        strokeWidth="1"
      />
      <path
        d="M2.6 4 L4.1 5 L2.6 6"
        fill="none"
        stroke="var(--codex-to)"
        strokeWidth="1"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
      <path d="M5.2 6.4 H7.4" stroke="var(--codex-to)" strokeWidth="1" strokeLinecap="round" />
    </svg>
  );
}

/** A mass leaving the ground: the dot has left the line it was resting on. */
export function AntigravityMark() {
  return (
    <svg width="10" height="10" viewBox="0 0 10 10" aria-hidden>
      <circle cx="5" cy="3.1" r="1.7" fill="var(--antigravity-to)" />
      <path
        d="M2.2 6.2 L5 8.2 L7.8 6.2"
        fill="none"
        stroke="var(--antigravity-to)"
        strokeWidth="1"
        strokeLinecap="round"
        strokeLinejoin="round"
        opacity="0.75"
      />
    </svg>
  );
}

export function providerMark(id: "claude" | "codex" | "antigravity"): ReactElement {
  if (id === "claude") return <ClaudeMark />;
  if (id === "codex") return <CodexMark />;
  return <AntigravityMark />;
}
