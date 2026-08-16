import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/**
 * Countdown formats: seconds under an hour, minutes under a day, hours beyond.
 */
export function formatCountdown(msLeft: number): string {
  if (msLeft <= 0) return "0m";
  const totalSec = Math.floor(msLeft / 1000);
  const days = Math.floor(totalSec / 86400);
  const hours = Math.floor((totalSec % 86400) / 3600);
  const minutes = Math.floor((totalSec % 3600) / 60);
  const seconds = totalSec % 60;

  if (totalSec < 3600) return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
  if (totalSec < 86400) return `${hours}h ${minutes}m`;
  return `${days}d ${hours}h`;
}

export function formatClock(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "--:--";
  return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

export function minutesSince(iso: string, now: number): number {
  const t = new Date(iso).getTime();
  if (Number.isNaN(t)) return 0;
  return Math.max(0, Math.floor((now - t) / 60000));
}

/**
 * Ticks once a second only while a countdown is inside its final hour, so an
 * idle widget wakes the webview once a minute instead.
 */
export function tickInterval(resetTimes: number[], now: number): number {
  const anyUnderHour = resetTimes.some((t) => t - now > 0 && t - now < 3_600_000);
  return anyUnderHour ? 1000 : 60_000;
}
