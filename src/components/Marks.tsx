import type { ReactElement } from "react";

/**
 * Provider glyphs are drawn here rather than reusing vendor logos, so the
 * widget never implies endorsement by Anthropic or OpenAI.
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

export function providerMark(id: "claude" | "codex"): ReactElement {
  return id === "claude" ? <ClaudeMark /> : <CodexMark />;
}
